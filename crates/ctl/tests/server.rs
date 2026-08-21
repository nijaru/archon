//! Server integration tests: a real ControlPlane over loopback TCP, driven
//! by the same frames the CLI sends.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fleet_ctl::api::{ClientRequest, ServerResponse, read_response, write_frame};
use fleet_ctl::server::ControlPlane;

fn temp_log(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "fleet-ctl-server-{name}-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

fn spawn_server(name: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let log = temp_log(name);
    std::thread::spawn(move || {
        let link = fleet_ctl::server::AgentLink::Local { cgroup_root: None };
        let mut plane = ControlPlane::boot(link, log).expect("boot");
        plane.serve(listener).ok();
    });
    addr
}

fn roundtrip(stream: &mut TcpStream, request: ClientRequest) -> ServerResponse {
    write_frame(stream, &request).expect("send");
    read_response(stream).expect("receive")
}

fn wait_until(deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    check()
}

#[test]
fn submit_status_revoke_over_the_wire() {
    let addr = spawn_server("lifecycle");
    let mut stream = TcpStream::connect(&addr).expect("connect");

    let response = roundtrip(
        &mut stream,
        ClientRequest::Submit {
            owner: 1,
            cpus: 1,
            memory_mib: 0,
            lifetime_secs: 3_600,
            command: vec!["sleep".into(), "30".into()],
        },
    );
    let ServerResponse::Submitted { lease, .. } = response else {
        panic!("expected Submitted, got {response:?}");
    };
    assert!(lease > 0, "submit must admit on an empty cluster");

    let saw_running = wait_until(Duration::from_secs(5), || {
        matches!(
            roundtrip(&mut stream, ClientRequest::Status),
            ServerResponse::Status { .. }
        )
    });
    assert!(saw_running);

    let response = roundtrip(&mut stream, ClientRequest::Revoke { lease });
    assert!(matches!(response, ServerResponse::Revoked));

    let response = roundtrip(&mut stream, ClientRequest::Status);
    let ServerResponse::Status { leases, .. } = response else {
        panic!("expected Status, got {response:?}");
    };
    let lease_info = leases.iter().find(|info| info.id == lease).expect("lease");
    assert_eq!(lease_info.state, "Revoked");
}

#[test]
fn empty_command_is_rejected() {
    let addr = spawn_server("reject");
    let mut stream = TcpStream::connect(&addr).expect("connect");
    let response = roundtrip(
        &mut stream,
        ClientRequest::Submit {
            owner: 1,
            cpus: 1,
            memory_mib: 0,
            lifetime_secs: 60,
            command: vec![],
        },
    );
    assert!(matches!(response, ServerResponse::Error { .. }));
}
