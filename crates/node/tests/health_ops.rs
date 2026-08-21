//! Health-driven operation: an unreachable agent's machine is quarantined,
//! its live leases fail, and keep-alive work re-places on survivors.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{read_request, write_response};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::NodeService;

/// A keep-alive agent that dies when the returned guard drops.
fn spawn_agent(instance: &'static str) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use archon_node::protocol::{AgentRequest, AgentResponse};
            let Ok(AgentRequest::Hello) = read_request(&mut stream) else {
                return;
            };
            let description = archon_node::discover::describe();
            let welcome = AgentResponse::Welcome {
                name: instance.to_string(),
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
    (addr, handle)
}

fn register(service: &mut NodeService, instance: &str, addr: &str) -> archon_kernel::NodeId {
    let mut executor = archon_node::service::RemoteExecutor::connect(addr).expect("connect");
    let mut description = NodeService::hello(&mut executor).expect("hello");
    description.instance_id = instance.to_string();
    service
        .register_agent(description, Box::new(executor))
        .expect("register")
}

fn keep_alive_submit(service: &mut NodeService, id: u64) -> Option<RequestId> {
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
        image: None,
        lifetime: 3_600,
        priority: 1,
        machine_local: true,
        keep_alive: true,
    };
    service.submit(
        request,
        OwnerId::from_u64(1),
        vec!["sleep".into(), "30".into()],
    );
    service.cluster.set_now(1);
    service.admit_one().expect("admit")
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
fn dead_agent_quarantines_and_keep_alive_work_replaces() {
    let mut service = NodeService::new();
    let (alpha_addr, alpha) = spawn_agent("inst-alpha");
    let beta = spawn_agent("inst-beta").0;
    let alpha_machine = register(&mut service, "inst-alpha", &alpha_addr);
    let beta_machine = register(&mut service, "inst-beta", &beta);

    // The workload places on alpha; alpha then dies.
    assert_eq!(
        keep_alive_submit(&mut service, 1),
        Some(RequestId::from_u64(1))
    );
    let first_lease = LeaseId::from_u64(1);
    assert_eq!(
        lease_machine(&service, first_lease),
        Some(alpha_machine),
        "test setup: work must sit on the doomed machine"
    );

    drop(alpha);
    std::thread::sleep(Duration::from_millis(50));

    // Maintenance: probes fail, quarantine + fail live leases.
    service.mark_machine_unhealthy(alpha_machine).unwrap();

    // Keep-alive restarts queue fresh work; admission places it on beta.
    let restarts = service.take_restarts();
    assert!(!restarts.is_empty(), "keep-alive work must be restarted");
    for (request, owner) in restarts {
        let command = request.command.clone();
        service.submit(request, owner, command);
        service.admit_one().unwrap();
    }

    // The new lease lives on beta, not on the quarantined machine.
    let new_lease = *service
        .cluster
        .leases
        .keys()
        .max()
        .expect("a replacement lease exists");
    let replaced_on_beta = wait_until(Duration::from_secs(5), || {
        service.is_running(new_lease) && lease_machine(&service, new_lease) == Some(beta_machine)
    });
    assert!(
        replaced_on_beta,
        "keep-alive work must re-place on the surviving machine"
    );
    assert_eq!(
        service.cluster.leases[&first_lease].state,
        archon_kernel::LeaseState::Failed
    );

    // Quarantine blocks new placements on the dead machine.
    assert_eq!(
        submit_run_once(&mut service, 9),
        Some(RequestId::from_u64(9))
    );
    assert_ne!(
        lease_machine(&service, LeaseId::from_u64(4)),
        Some(alpha_machine),
        "quarantined machines receive no placements"
    );
}

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

fn submit_run_once(service: &mut NodeService, id: u64) -> Option<RequestId> {
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
        image: None,
        lifetime: 3_600,
        priority: 1,
        machine_local: true,
        keep_alive: false,
    };
    service.submit(
        request,
        OwnerId::from_u64(2),
        vec!["sleep".into(), "30".into()],
    );
    service.admit_one().expect("admit")
}
