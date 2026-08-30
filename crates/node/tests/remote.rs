//! Remote agent protocol tests: the controller drives a real agent over
//! TCP. The agent runs in a thread here; the same code path serves remote
//! machines.

use archon_kernel::{
    CapacityDimension, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{read_request, write_response};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::NodeService;
use std::net::TcpListener;

/// Serve one controller connection with a fresh agent, like `archon
/// agent` does.
fn spawn_agent(instance_id: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            // The controller secures the link before any frames flow.
            let Ok(mut stream) = archon_node::transport::establish_responder(stream, None) else {
                return;
            };
            let mut agent =
                LeaseAgent::new(ProcessRuntime::new()).with_identity(instance_id.to_string(), None);
            // The controller greets before driving requests.
            let _ = archon_node::protocol::read_greeting(&mut stream);
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
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: command.clone(),
        lifetime: 3_600,
        keep_alive: false,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        image: None,
        storage: vec![],
        ports: vec![],
    }
}

#[test]
fn controller_excludes_unenforced_cpu_work_on_a_remote_agent() {
    let addr = spawn_agent("remote-a");
    let mut service = NodeService::new();
    service
        .register_remote(&addr)
        .expect("register remote agent");
    service.submit(
        request(1, vec!["sleep".into(), "30".into()]),
        OwnerId::from_u64(1),
    );
    service.tick().unwrap();
    assert_eq!(
        service.admit_one().unwrap(),
        None,
        "a remote lifecycle-only process runtime must not receive a CPU Claim"
    );
    assert!(
        service.cluster.leases.is_empty(),
        "remote capability refusal must happen before Lease authority"
    );
}

#[test]
fn controller_preserves_distinct_listening_agent_identities() {
    let first = spawn_agent("remote-a");
    let second = spawn_agent("remote-b");
    let mut service = NodeService::new();
    let first_machine = service
        .register_remote(&first)
        .expect("register first agent");
    let second_machine = service
        .register_remote(&second)
        .expect("register second agent");
    assert_ne!(first_machine, second_machine);
    assert_eq!(
        service
            .cluster
            .graph
            .node(first_machine)
            .and_then(|node| node.attrs.get("agent_id"))
            .map(String::as_str),
        Some("remote-a")
    );
    assert_eq!(
        service
            .cluster
            .graph
            .node(second_machine)
            .and_then(|node| node.attrs.get("agent_id"))
            .map(String::as_str),
        Some("remote-b")
    );
}

#[test]
fn controller_refuses_a_listening_agent_without_stable_identity() {
    let addr = spawn_agent("");
    let mut service = NodeService::new();
    let err = service
        .register_remote(&addr)
        .expect_err("empty identity must fail closed");
    assert!(err.to_string().contains("empty stable instance id"));
}

#[test]
fn stale_sessions_are_rejected_by_the_agent() {
    use archon_node::protocol::{AgentRequest, AgentResponse};

    let mut agent = LeaseAgent::new(ProcessRuntime::new());
    // Session 2 arrives on a mutating request and becomes the current
    // generation.
    let response = agent.handle(AgentRequest::Prepare {
        binding: 1,
        lease: 1,
        node: 1,
        provider: 1,
        scope: archon_kernel::BindingScope::Exclusive,
        session: 2,
        fence: 1,
        epoch: 1,
    });
    assert!(matches!(response, AgentResponse::Prepared { .. }));
    // An older generation is refused.
    let response = agent.handle(AgentRequest::Prepare {
        binding: 2,
        lease: 1,
        node: 1,
        provider: 1,
        scope: archon_kernel::BindingScope::Exclusive,
        session: 1,
        fence: 1,
        epoch: 1,
    });
    match response {
        AgentResponse::Failed { reason, .. } => assert!(reason.contains("stale")),
        other => panic!("expected Failed, got {other:?}"),
    }
}
