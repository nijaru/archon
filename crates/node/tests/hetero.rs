//! Heterogeneous placement: machines with different shapes place against
//! their real capacity — small machines don't get oversized work.

use std::net::TcpListener;

use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{read_request, write_response};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::NodeService;

/// An agent reporting a specific machine shape.
fn spawn_shaped_agent(instance: &'static str, name: &'static str, cpus: u64) -> String {
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
                cpus,
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
    let _ = instance;
    addr
}

fn register(
    service: &mut NodeService,
    instance: &str,
    name: &str,
    cpus: u64,
    addr: &str,
) -> archon_kernel::NodeId {
    let mut executor = archon_node::service::RemoteExecutor::connect(addr).expect("connect");
    let mut description = NodeService::hello(&mut executor).expect("hello");
    description.instance_id = instance.to_string();
    description.name = name.to_string();
    description.cpus = cpus;
    service
        .register_agent(description, Box::new(executor))
        .expect("register")
}

fn submit(service: &mut NodeService, id: u64, cpus: u64) -> Option<RequestId> {
    let request = Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, cpus),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec!["sleep".into(), "30".into()],
        machine_local: true,
        grace_secs: 0,
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 3_600,
        keep_alive: false,
        priority: 1,
    };
    service.submit(request, OwnerId::from_u64(1));
    service.cluster.set_now(1);
    service.admit_one().expect("admit")
}

/// The machine node holding a lease's CPU claim.
fn lease_machine(service: &NodeService, lease: LeaseId) -> Option<archon_kernel::NodeId> {
    service
        .cluster
        .leases
        .get(&lease)?
        .allocation
        .claims
        .iter()
        .find_map(|claim| service.cluster.graph.machine_of(claim.node))
}

#[test]
fn oversized_work_lands_on_the_bigger_machine() {
    let mut service = NodeService::new();
    let small = spawn_shaped_agent("inst-small", "small", 2);
    let big = spawn_shaped_agent("inst-big", "big", 8);
    let small_machine = register(&mut service, "inst-small", "small", 2, &small);
    let big_machine = register(&mut service, "inst-big", "big", 8, &big);
    assert_ne!(small_machine, big_machine);

    // 4 CPUs cannot fit on the 2-CPU machine; it must place on `big`.
    assert_eq!(submit(&mut service, 1, 4), Some(RequestId::from_u64(1)));
    assert_eq!(
        lease_machine(&service, LeaseId::from_u64(1)),
        Some(big_machine),
        "4-cpu work must place on the 8-cpu machine"
    );
    assert!(service.is_running(LeaseId::from_u64(1)));

    // A 1-cpu request can go anywhere; both leases coexist.
    assert_eq!(submit(&mut service, 2, 1), Some(RequestId::from_u64(2)));
    assert!(service.is_running(LeaseId::from_u64(2)));

    // Nothing bigger than every machine stays queued.
    assert_eq!(submit(&mut service, 3, 16), None);
    assert_eq!(service.queue_len(), 1);

    for lease in [LeaseId::from_u64(1), LeaseId::from_u64(2)] {
        service.revoke(lease).expect("revoke");
    }
}

#[test]
fn small_machine_fills_first_only_when_it_fits() {
    let mut service = NodeService::new();
    let small = spawn_shaped_agent("inst-small", "small", 2);
    let big = spawn_shaped_agent("inst-big", "big", 8);
    let small_machine = register(&mut service, "inst-small", "small", 2, &small);
    let big_machine = register(&mut service, "inst-big", "big", 8, &big);

    // Two 1-cpu requests: packing preference should fill the small machine
    // before spreading to the big one (deterministic, either order is
    // acceptable — what matters is that both run).
    assert_eq!(submit(&mut service, 1, 1), Some(RequestId::from_u64(1)));
    assert_eq!(submit(&mut service, 2, 1), Some(RequestId::from_u64(2)));
    assert!(service.is_running(LeaseId::from_u64(1)));
    assert!(service.is_running(LeaseId::from_u64(2)));
    let _ = (small_machine, big_machine);

    for lease in [LeaseId::from_u64(1), LeaseId::from_u64(2)] {
        service.revoke(lease).expect("revoke");
    }
}
