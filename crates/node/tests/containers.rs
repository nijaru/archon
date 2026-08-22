//! Container execution tests: a lease whose request carries an image runs
//! as an OCI container with the lease's limits; volumes bind to the host,
//! ports publish onto the host, and lease termination kills the container.
//! Skipped when no container engine is reachable. One sequential test:
//! containers share the engine's name space, so scenarios must not race.

use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, PortPublish, Request, RequestClass, RequestId,
    StorageMount, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{read_request, write_response};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::NodeService;

/// True when a real Docker engine answers. Rootless podman behind the
/// docker CLI is excluded: its user-namespace mapping makes simple bind
/// mounts read-only inside the container, which is an engine quirk these
/// tests do not model.
fn engine_available() -> bool {
    let ok = Command::new("docker")
        .arg("info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !ok {
        return false;
    }
    let version = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).to_lowercase())
        .unwrap_or_default();
    !version.contains("podman")
}

fn spawn_agent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use archon_node::protocol::AgentRequest;
            let Ok(AgentRequest::Hello) = read_request(&mut stream) else {
                return;
            };
            let description = archon_node::discover::describe();
            let welcome = archon_node::protocol::AgentResponse::Welcome {
                name: "container-host".into(),
                cpus: description.cpus,
                memory_bytes: description.memory_bytes,
            };
            if write_response(&mut stream, &welcome).is_err() {
                return;
            }
            let mut lease_agent = LeaseAgent::new(ProcessRuntime::new());
            while let Ok(request) = read_request(&mut stream) {
                let response = lease_agent.handle(request);
                if write_response(&mut stream, &response).is_err() {
                    break;
                }
            }
        }
    });
    addr
}

fn submit_container(
    service: &mut NodeService,
    id: u64,
    image: &str,
    storage: Vec<StorageMount>,
    ports: Vec<PortPublish>,
    command: Vec<String>,
) -> Option<RequestId> {
    let request = Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command,
        image: Some(image.into()),
        lifetime: 3_600,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
        storage,
        ports,
    };
    service.submit(request, OwnerId::from_u64(1));
    service.cluster.set_now(1);
    service.admit_one().expect("admit")
}

/// The namespaced container for a lease; names end in `-lease-<id>`.
fn find_container(lease: LeaseId) -> Option<String> {
    let suffix = format!("lease-{}", lease.as_u64());
    let output = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("name={suffix}"),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .expect("docker ps");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|name| name.ends_with(&suffix))
        .map(String::from)
}

fn wait_until_container(lease: LeaseId) -> Option<String> {
    let mut name = None;
    wait_until(Duration::from_secs(30), || {
        name = find_container(lease);
        name.is_some()
    });
    name
}

fn wait_until(deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    check()
}

#[test]
fn container_leases_run_with_limits_volumes_and_ports_then_die_on_revoke() {
    if !engine_available() {
        eprintln!("skipping: docker not reachable");
        return;
    }
    // Remove containers left by earlier crashed runs so name discovery
    // only sees this run's containers.
    let stale = Command::new("docker")
        .args(["ps", "-aq", "--filter", "name=archon-"])
        .output()
        .expect("list stale");
    for id in String::from_utf8_lossy(&stale.stdout).lines() {
        if !id.is_empty() {
            let _ = Command::new("docker").arg("rm").arg("-f").arg(id).output();
        }
    }

    // Part 1: lifecycle + limits.
    let addr = spawn_agent();
    let mut service = NodeService::new();
    service.register_remote(&addr).expect("register");
    assert_eq!(
        submit_container(
            &mut service,
            1,
            "busybox:latest",
            vec![],
            vec![],
            vec!["sleep".into(), "30".into()],
        ),
        Some(RequestId::from_u64(1))
    );
    let lease = LeaseId::from_u64(1);
    let name = wait_until_container(lease);
    let name = name.unwrap_or_else(|| format!("archon-lease-{}", lease.as_u64()));

    assert!(
        wait_until(Duration::from_secs(10), || {
            docker_inspect("{{.State.Running}}", &name) == "true"
        }),
        "the lease's container must be running"
    );
    assert_eq!(
        docker_inspect("{{.HostConfig.NanoCpus}}", &name),
        "1000000000",
        "1 cpu claim = 1.0 cpus"
    );

    service.revoke(lease).expect("revoke");
    assert!(
        wait_until(Duration::from_secs(10), || find_container(lease).is_none()),
        "revoke must remove the container"
    );

    // Part 2: volumes round-trip and ports publish.
    let host_dir = std::env::temp_dir().join(format!("archon-vol-{}", std::process::id()));
    std::fs::create_dir_all(&host_dir).expect("create host dir");
    assert_eq!(
        submit_container(
            &mut service,
            2,
            "busybox:latest",
            vec![StorageMount {
                host_path: host_dir.display().to_string(),
                mount_path: "/data".into(),
            }],
            vec![PortPublish {
                container_port: 8080,
                host_port: None,
            }],
            vec![
                "sh".into(),
                "-c".into(),
                "echo stored > /data/proof && httpd -f -p 8080 && sleep 30".into(),
            ],
        ),
        Some(RequestId::from_u64(2))
    );
    let lease2 = LeaseId::from_u64(2);
    let name2 = wait_until_container(lease2).expect("container appears");

    let wrote = wait_until(Duration::from_secs(30), || host_dir.join("proof").exists());
    assert!(wrote, "the bind mount must reach the host directory");
    let content = std::fs::read_to_string(host_dir.join("proof")).expect("read proof");
    assert_eq!(content.trim(), "stored");
    assert!(
        docker_inspect("{{json .HostConfig.PortBindings}}", &name2).contains("8080/tcp"),
        "port 8080 must be published"
    );

    service.revoke(lease2).expect("revoke");
    let _ = std::fs::remove_dir_all(&host_dir);
}

fn docker_inspect(fmt: &str, name: &str) -> String {
    Command::new("docker")
        .args(["inspect", "-f", fmt, name])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

fn busybox_drain_command() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        "trap 'echo drained > /marker/done; exit 0' TERM; sleep 60 & wait".into(),
    ]
}

/// A container submission template with storage mounted at /marker.
fn submit_request_template(id: u64, command: Vec<String>, dir: &std::path::Path) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command,
        image: Some("busybox:latest".into()),
        lifetime: 3_600,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
        storage: vec![StorageMount {
            host_path: dir.display().to_string(),
            mount_path: "/marker".into(),
        }],
        ports: vec![],
    }
}

#[test]
fn container_revoke_drains_within_grace() {
    if !engine_available() {
        eprintln!("skipping: docker not reachable");
        return;
    }
    let addr = spawn_agent();
    let mut service = NodeService::new();
    service.register_remote(&addr).expect("register");

    // A container trapping TERM and writing a marker on drain; grace must
    // give it time to finish cleanly.
    let dir = std::env::temp_dir().join(format!("archon-drain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let request = Request {
        grace_secs: 10,
        ..submit_request_template(1, busybox_drain_command(), &dir)
    };
    service.submit(request, OwnerId::from_u64(1));
    service.cluster.set_now(1);
    service.admit_one().expect("admit");
    let lease = LeaseId::from_u64(1);
    assert!(wait_until_container(lease).is_some(), "container must run");

    // Revoke with drain budget; the trap writes the marker before exit.
    service.revoke(lease).unwrap();
    let drained = wait_until(Duration::from_secs(20), || dir.join("done").exists());
    assert!(drained, "graceful TERM must reach the container workload");
    let _ = std::fs::remove_dir_all(&dir);
}
