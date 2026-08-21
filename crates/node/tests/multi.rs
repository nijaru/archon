//! Multi-node tests: one controller, several dial-in agents. Placement
//! spans machines; a re-registering agent gets its live work re-driven.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{read_request, write_response};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::NodeService;

/// A named in-process agent: serves one controller connection, then dies
/// with its socket (like a real agent process would).
fn spawn_named_agent(name: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Connect-out handshake: the controller asks, we describe.
            use archon_node::protocol::AgentRequest;
            let Ok(AgentRequest::Hello) = read_request(&mut stream) else {
                return;
            };
            let description = archon_node::discover::describe();
            let welcome = archon_node::protocol::AgentResponse::Welcome {
                name: name.to_string(),
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

fn submit_sleep(service: &mut NodeService, id: u64) -> RequestId {
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
        command: vec!["sleep".into(), "30".into()],
        lifetime: 3_600,
        priority: 1,
    };
    service.submit(
        request,
        OwnerId::from_u64(1),
        vec!["sleep".into(), "30".into()],
    );
    service.cluster.set_now(1);
    service.admit_one().expect("admit").expect("admitted")
}

#[test]
fn two_agents_register_and_both_machines_take_work() {
    let mut service = NodeService::new();
    let alpha = spawn_named_agent("alpha");
    let beta = spawn_named_agent("beta");
    let m1 = service.register_remote(&alpha).expect("register alpha");
    let m2 = service.register_remote(&beta).expect("register beta");
    assert_ne!(m1, m2, "distinct agents must get distinct machine nodes");

    // Both machines' capacity is schedulable: 2 requests fit concurrently.
    let r1 = submit_sleep(&mut service, 1);
    let r2 = submit_sleep(&mut service, 2);
    assert_eq!(r1, RequestId::from_u64(1));
    assert_eq!(r2, RequestId::from_u64(2));
    assert!(service.is_running(LeaseId::from_u64(1)));
    assert!(service.is_running(LeaseId::from_u64(2)));

    // Revoke both so the agents' processes do not outlive the test.
    for lease in [LeaseId::from_u64(1), LeaseId::from_u64(2)] {
        service.revoke(lease).expect("revoke");
    }
}

#[test]
fn agent_re_registration_reconciles_live_work() {
    let mut service = NodeService::new();
    let first = spawn_named_agent("worker");
    service.register_remote(&first).expect("register");
    submit_sleep(&mut service, 1);
    let lease = LeaseId::from_u64(1);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !service.is_running(lease) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        service.is_running(lease),
        "work must run on the first agent"
    );

    // The first agent process dies (its socket drops, its children die).
    drop(first);
    std::thread::sleep(Duration::from_millis(50));

    // A fresh agent process registers under the same machine name.
    let second = spawn_named_agent("worker");
    service.register_remote(&second).expect("re-register");

    // Reconcile must have respawned the lease's work on the new agent.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !service.is_running(lease) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        service.is_running(lease),
        "reconciliation must respawn live work on the fresh agent"
    );
    service.revoke(lease).expect("revoke");
}
