//! Container execution tests: a lease whose request carries an image runs
//! as an OCI container with the lease's limits; volumes bind to the host,
//! ports publish onto the host, and lease termination kills the container.
//! Skipped when no container engine is reachable. Each fixture discovers
//! and cleans up only containers carrying its unique command argument.

use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use archon_kernel::{
    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
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
        if let Ok((stream, _)) = listener.accept() {
            let Ok(mut stream) = archon_node::transport::establish_responder(stream, None) else {
                return;
            };
            use archon_node::protocol::AgentRequest;
            let _ = archon_node::protocol::read_greeting(&mut stream);
            let Ok(AgentRequest::Hello) = read_request(&mut stream) else {
                return;
            };
            let description = archon_node::discover::describe();
            let welcome = archon_node::protocol::AgentResponse::Welcome {
                instance_id: "test-agent".into(),
                name: "container-host".into(),
                cpus: description.cpus,
                memory_bytes: description.memory_bytes,
                host_nodes: Vec::new(),
                devices: Vec::new(),
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
    storage: Vec<archon_node::workload::StorageMount>,
    ports: Vec<archon_node::workload::PortPublish>,
    command: Vec<String>,
) -> Option<RequestId> {
    let request = archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(id),
            class: RequestClass::Batch,
            needs: vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            }],
            topology: vec![],
            preferences: vec![],
            data: vec![],
            lifetime: 3_600,
            priority: 1,
            machine_local: true,
        },
        execution: archon_node::workload::ExecutionSpec {
            command,
            image: Some(image.into()),
            storage,
            ports,
            grace_secs: 0,
        },
        keep_alive: false,
    };
    service.submit(request, OwnerId::from_u64(1));
    service.cluster.set_now(1);
    service.admit_one().expect("admit")
}

struct ContainerFixture(String);

impl ContainerFixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        Self(format!(
            "archon-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn command(&self, script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into(), self.0.clone()]
    }

    fn ids(&self) -> Vec<String> {
        let listed = Command::new("docker")
            .args([
                "container",
                "ls",
                "-aq",
                "--no-trunc",
                "--filter",
                "name=archon-",
            ])
            .output();
        let Ok(output) = listed else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|id| {
                serde_json::from_str::<Vec<String>>(&docker_inspect("{{json .Config.Cmd}}", id))
                    .is_ok_and(|args| args.last() == Some(&self.0))
            })
            .map(str::to_owned)
            .collect()
    }

    fn wait_running(&self, service: &mut NodeService) -> String {
        let mut id = None;
        assert!(
            wait_until(Duration::from_secs(30), || {
                service.drive(Duration::from_millis(20)).unwrap();
                id = self
                    .ids()
                    .into_iter()
                    .find(|id| docker_inspect("{{.State.Running}}", id) == "true");
                id.is_some()
            }),
            "the fixture's container must run"
        );
        id.unwrap()
    }
}

impl Drop for ContainerFixture {
    fn drop(&mut self) {
        for id in self.ids() {
            let _ = Command::new("docker").args(["rm", "-f", &id]).output();
        }
    }
}

fn wait_removed(service: &mut NodeService, lease: LeaseId, id: &str) {
    assert!(
        wait_until(Duration::from_secs(20), || {
            service.drive(Duration::from_millis(20)).unwrap();
            if service.cluster.occupies(lease) {
                return false;
            }
            Command::new("docker")
                .args([
                    "container",
                    "ls",
                    "-aq",
                    "--no-trunc",
                    "--filter",
                    &format!("id={id}"),
                ])
                .output()
                .is_ok_and(|out| out.status.success() && out.stdout.is_empty())
        }),
        "revoke must remove {id} and release its capacity"
    );
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
    let first = ContainerFixture::new();
    let second = ContainerFixture::new();

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
            first.command("exec sleep 30"),
        ),
        Some(RequestId::from_u64(1))
    );
    let lease = LeaseId::from_u64(1);
    let name = first.wait_running(&mut service);

    assert_eq!(
        docker_inspect("{{.HostConfig.NanoCpus}}", &name),
        "1000000000",
        "1 cpu claim = 1.0 cpus"
    );

    service.revoke(lease).expect("revoke");
    wait_removed(&mut service, lease, &name);

    // Part 2: volumes round-trip and ports publish.
    let host_dir = std::env::temp_dir().join(format!("archon-vol-{}", std::process::id()));
    std::fs::create_dir_all(&host_dir).expect("create host dir");
    assert_eq!(
        submit_container(
            &mut service,
            2,
            "busybox:latest",
            vec![archon_node::workload::StorageMount {
                host_path: host_dir.display().to_string(),
                mount_path: "/data".into(),
            }],
            vec![archon_node::workload::PortPublish {
                container_port: 8080,
                host_port: None,
            }],
            second.command("echo stored > /data/proof && exec httpd -f -p 8080"),
        ),
        Some(RequestId::from_u64(2))
    );
    let lease2 = LeaseId::from_u64(2);
    let name2 = second.wait_running(&mut service);

    let wrote = wait_until(Duration::from_secs(30), || {
        let _ = service.drive(Duration::from_millis(20));
        host_dir.join("proof").exists()
    });
    assert!(wrote, "the bind mount must reach the host directory");
    let content = std::fs::read_to_string(host_dir.join("proof")).expect("read proof");
    assert_eq!(content.trim(), "stored");
    assert!(
        docker_inspect("{{json .HostConfig.PortBindings}}", &name2).contains("8080/tcp"),
        "port 8080 must be published"
    );

    service.revoke(lease2).expect("revoke");
    wait_removed(&mut service, lease2, &name2);
    let _ = std::fs::remove_dir_all(&host_dir);
}

fn docker_inspect(fmt: &str, name: &str) -> String {
    Command::new("docker")
        .args(["inspect", "-f", fmt, name])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A container submission template with storage mounted at /marker.
fn submit_request_template(
    id: u64,
    command: Vec<String>,
    dir: &std::path::Path,
) -> archon_node::workload::WorkloadSpec {
    archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(id),
            class: RequestClass::Batch,
            needs: vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            }],
            topology: vec![],
            preferences: vec![],
            data: vec![],
            lifetime: 3_600,
            priority: 1,
            machine_local: true,
        },
        execution: archon_node::workload::ExecutionSpec {
            command,
            image: Some("busybox:latest".into()),
            storage: vec![archon_node::workload::StorageMount {
                host_path: dir.display().to_string(),
                mount_path: "/marker".into(),
            }],
            ports: vec![],
            grace_secs: 0,
        },
        keep_alive: false,
    }
}

#[test]
fn container_revoke_drains_within_grace() {
    if !engine_available() {
        eprintln!("skipping: docker not reachable");
        return;
    }
    let fixture = ContainerFixture::new();
    let addr = spawn_agent();
    let mut service = NodeService::new();
    service.register_remote(&addr).expect("register");

    // A container trapping TERM and writing a marker on drain; grace must
    // give it time to finish cleanly.
    let dir = std::env::temp_dir().join(format!("archon-drain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let drain = fixture.command(
        "trap 'echo drained > /marker/done; exit 0' TERM; touch /marker/ready; sleep 60 & wait",
    );
    let mut request = submit_request_template(1, drain, &dir);
    request.execution.grace_secs = 10;
    service.submit(request, OwnerId::from_u64(1));
    service.cluster.set_now(1);
    service.admit_one().expect("admit");
    let lease = LeaseId::from_u64(1);
    let id = fixture.wait_running(&mut service);
    assert!(
        wait_until(Duration::from_secs(5), || dir.join("ready").exists()),
        "TERM handler must be installed"
    );

    // Revoke with drain budget; the trap writes the marker before exit.
    service.revoke(lease).unwrap();
    let drained = wait_until(Duration::from_secs(20), || {
        let _ = service.drive(Duration::from_millis(20));
        dir.join("done").exists()
    });
    assert!(drained, "graceful TERM must reach the container workload");
    wait_removed(&mut service, lease, &id);
    let _ = std::fs::remove_dir_all(&dir);
}
