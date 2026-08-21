//! Container execution tests: a lease whose request carries an image runs
//! as an OCI container with the lease's limits, and lease termination kills
//! the container. Skipped when no container engine is reachable.

use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{read_request, write_response};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::NodeService;

fn engine_available() -> bool {
    Command::new("docker")
        .arg("info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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

fn container_request(id: u64, image: &str) -> Request {
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
        command: vec!["sleep".into(), "30".into()],
        image: Some(image.into()),
        lifetime: 3_600,
        keep_alive: false,
        priority: 1,
        machine_local: true,
    }
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
fn image_request_runs_as_a_container_and_revoke_kills_it() {
    if !engine_available() {
        eprintln!("skipping: docker not reachable");
        return;
    }
    let addr = spawn_agent();
    let mut service = NodeService::new();
    service.register_remote(&addr).expect("register");

    service.submit(
        container_request(1, "busybox:latest"),
        OwnerId::from_u64(1),
        vec!["sleep".into(), "30".into()],
    );
    service.cluster.set_now(1);
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let lease = LeaseId::from_u64(1);
    let name = format!("archon-lease-{}", lease.as_u64());

    let running = wait_until(Duration::from_secs(30), || {
        let output = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Running}}", &name])
            .output();
        matches!(
            output,
            Ok(output) if String::from_utf8_lossy(&output.stdout).trim() == "true"
        )
    });
    assert!(running, "the lease's container must be running");

    // The container's limits carry the lease's claims.
    let output = Command::new("docker")
        .args(["inspect", "-f", "{{.HostConfig.NanoCpus}}", &name])
        .output()
        .expect("inspect cpus");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1000000000",
        "1 cpu claim = 1.0 cpus"
    );

    service.revoke(lease).expect("revoke");
    let gone = wait_until(Duration::from_secs(10), || {
        !Command::new("docker")
            .args(["inspect", &name])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(true)
    });
    assert!(gone, "revoke must remove the container");
}
