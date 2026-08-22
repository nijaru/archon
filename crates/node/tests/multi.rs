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

/// A named in-process agent with a stable instance id: serves one
/// controller connection, then dies with its socket.
fn spawn_named_agent(_instance_id: &'static str, name: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use archon_node::protocol::{AgentRequest, AgentResponse};
            let Ok(AgentRequest::Hello) = read_request(&mut stream) else {
                return;
            };
            let description = archon_node::discover::describe();
            let welcome = AgentResponse::Welcome {
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

/// Register an executor whose machine carries this instance id and name,
/// mirroring what the control plane does for dial-in agents.
fn commands_of(service: &NodeService) -> Vec<archon_kernel::Command> {
    service.command_history()
}

fn register_instance(
    service: &mut NodeService,
    instance_id: &str,
    name: &str,
    addr: &str,
) -> archon_kernel::NodeId {
    let mut executor = archon_node::service::RemoteExecutor::connect(addr).expect("connect");
    let mut description = NodeService::hello(&mut executor).expect("hello");
    description.instance_id = instance_id.to_string();
    description.name = name.to_string();
    service
        .register_agent(description, Box::new(executor))
        .expect("register")
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
        machine_local: true,
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 3_600,
        keep_alive: false,
        priority: 1,
    };
    service.submit(request, OwnerId::from_u64(1));
    service.cluster.set_now(1);
    service.admit_one().expect("admit").expect("admitted")
}

#[test]
fn two_agents_register_and_both_machines_take_work() {
    let mut service = NodeService::new();
    let alpha = spawn_named_agent("inst-alpha", "alpha");
    let beta = spawn_named_agent("inst-beta", "beta");
    let m1 = register_instance(&mut service, "inst-alpha", "alpha", &alpha);
    let m2 = register_instance(&mut service, "inst-beta", "beta", &beta);
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
    let first = spawn_named_agent("inst-worker", "worker");
    register_instance(&mut service, "inst-worker", "worker", &first);
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

    // A fresh agent process with the SAME instance id re-registers: it is
    // the same machine, reconciled — not a duplicate.
    let second = spawn_named_agent("inst-worker", "worker");
    let machine = register_instance(&mut service, "inst-worker", "worker", &second);
    assert_eq!(
        service
            .cluster
            .graph
            .node(machine)
            .and_then(|n| n.attrs.get("name")),
        Some(&"worker".to_string())
    );

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

    // A DIFFERENT instance id claiming the same display name is a distinct
    // machine, not a takeover — no flap.
    let machines_before = service.cluster.graph.nodes_of_kind(NodeKind::Machine).len();
    let impostor = spawn_named_agent("inst-worker-2", "worker");
    register_instance(&mut service, "inst-worker-2", "worker", &impostor);
    let machines_after = service.cluster.graph.nodes_of_kind(NodeKind::Machine).len();
    assert_eq!(machines_after, machines_before + 1);

    // Control-plane restart: replay restores the graph including agent_id
    // attrs, so the same instance re-registers as the same machine — not a
    // duplicate graph fragment.
    let mut recovered = NodeService::new();
    recovered.replay(commands_of(&service)).expect("replay");
    let machines_before_replay = recovered
        .cluster
        .graph
        .nodes_of_kind(NodeKind::Machine)
        .len();
    let replacement = spawn_named_agent("inst-worker-2", "worker");
    register_instance(&mut recovered, "inst-worker-2", "worker", &replacement);
    let machines_after_replay = recovered
        .cluster
        .graph
        .nodes_of_kind(NodeKind::Machine)
        .len();
    assert_eq!(
        machines_after_replay, machines_before_replay,
        "re-registration after restart must match, not duplicate"
    );
}
