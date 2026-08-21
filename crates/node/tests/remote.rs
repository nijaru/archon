//! Remote agent protocol tests: the controller drives a real agent over
//! TCP. The agent runs in a thread here; the same code path serves remote
//! machines.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use fleet_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use fleet_node::agent::LeaseAgent;
use fleet_node::protocol::{read_request, write_response};
use fleet_node::runtime::ProcessRuntime;
use fleet_node::service::NodeService;

/// Serve one controller connection with a fresh agent, like `fleet-node
/// serve` does.
fn spawn_agent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut agent = LeaseAgent::new(ProcessRuntime::new());
            while let Ok(request) = read_request(&mut stream) {
                let response = agent.handle(request);
                if write_response(&mut stream, &response).is_err() {
                    break;
                }
            }
        }
    });
    addr
}

fn request(id: u64, command: Vec<String>) -> Request {
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
        command: command.clone(),
        lifetime: 3_600,
        priority: 1,
    }
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
fn controller_runs_and_kills_a_process_on_a_remote_agent() {
    let addr = spawn_agent();
    let mut service = NodeService::connect(&addr).expect("connect");
    service.submit(
        request(1, vec!["sleep".into(), "30".into()]),
        OwnerId::from_u64(1),
        vec!["sleep".into(), "30".into()],
    );
    service.tick().unwrap();
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let lease = LeaseId::from_u64(1);
    // The process lives in the agent thread's process tree — this same OS,
    // proving the round trip executed something real.
    assert!(
        wait_until(Duration::from_secs(5), || service.is_running(lease)),
        "sleep must run on the remote agent"
    );
    service.revoke(lease).unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || !service.is_running(lease)),
        "revoke must kill the remote process"
    );
}

#[test]
fn stale_sessions_are_rejected_by_the_agent() {
    use fleet_node::protocol::{AgentRequest, AgentResponse};

    let mut agent = LeaseAgent::new(ProcessRuntime::new());
    // Session 2 is accepted and becomes the current generation.
    let response = agent.handle(AgentRequest::Status {
        lease: 1,
        session: 2,
    });
    assert!(matches!(response, AgentResponse::Running { .. }));
    // An older generation is refused.
    let response = agent.handle(AgentRequest::Prepare {
        binding: 1,
        lease: 1,
        session: 1,
        fence: 1,
    });
    match response {
        AgentResponse::Failed { reason, .. } => assert!(reason.contains("stale")),
        other => panic!("expected Failed, got {other:?}"),
    }
}
