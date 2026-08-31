//! Health-driven operation: an unreachable agent's machine is quarantined,
//! its live leases fail, and keep-alive work re-places on survivors.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use archon_kernel::{
    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_request,
    write_response,
};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};

fn lifecycle_capabilities() -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: false,
            device_isolation: false,
            physical_cpu_placement: false,
            numa_memory_placement: false,
        },
        container: RuntimeCapabilities::default(),
    }
}

struct HealthProcessExecutor {
    inner: LocalExecutor,
}

impl LeaseExecutor for HealthProcessExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: lifecycle_capabilities(),
            });
        }
        self.inner.execute(request)
    }
}

fn register_local_health_agent(service: &mut NodeService) {
    let description = archon_node::discover::try_describe().expect("local discovery");
    service
        .register_agent(
            description,
            Box::new(HealthProcessExecutor {
                inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
            }),
        )
        .expect("register health test machine");
}

/// A keep-alive agent that dies when the returned guard drops.
fn spawn_agent(instance: &'static str) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let handle = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            // The control plane always secures links before any frames.
            let Ok(mut stream) = archon_node::transport::establish_responder(stream, None) else {
                return;
            };
            let _ = archon_node::protocol::read_greeting(&mut stream);
            let Ok(AgentRequest::Hello) = read_request(&mut stream) else {
                return;
            };
            let description = archon_node::discover::describe();
            let welcome = AgentResponse::Welcome {
                instance_id: instance.to_string(),
                name: instance.to_string(),
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
                let response = if matches!(request, AgentRequest::Capabilities) {
                    AgentResponse::Capabilities {
                        capabilities: lifecycle_capabilities(),
                    }
                } else {
                    lease_agent.handle(request)
                };
                if write_response(&mut stream, &response).is_err() {
                    break;
                }
            }
        }
    });
    (addr, handle)
}

fn register(service: &mut NodeService, instance: &str, addr: &str) -> archon_kernel::NodeId {
    let mut executor = archon_node::service::RemoteExecutor::connect(addr, None).expect("connect");
    let mut description = NodeService::hello(&mut executor).expect("hello");
    description.instance_id = instance.to_string();
    service
        .register_agent(description, Box::new(executor))
        .expect("register")
}

fn keep_alive_submit(service: &mut NodeService, id: u64) -> Option<RequestId> {
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
            command: vec!["sleep".into(), "30".into()],
            image: None,
            storage: vec![],
            ports: vec![],
            grace_secs: 0,
        },
        keep_alive: true,
    };
    service.submit(request, OwnerId::from_u64(1));
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
        service.submit(request, owner);
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
            command: vec!["sleep".into(), "30".into()],
            image: None,
            storage: vec![],
            ports: vec![],
            grace_secs: 0,
        },
        keep_alive: false,
    };
    service.submit(request, OwnerId::from_u64(2));
    service.admit_one().expect("admit")
}

#[test]
fn restarts_back_off_exponentially() {
    let mut service = NodeService::new();
    register_local_health_agent(&mut service);

    // A keep-alive workload whose command always fails.
    let mut request = archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(1),
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
            command: vec!["sh".into(), "-c".into(), "exit 1".into()],
            image: None,
            storage: vec![],
            ports: vec![],
            grace_secs: 0,
        },
        keep_alive: true,
    };
    service.submit(request.clone(), OwnerId::from_u64(1));
    service.cluster.set_now(10);
    service.admit_one().unwrap();

    // First failure restarts immediately (first attempt has no backoff debt).
    let mut finished = Vec::new();
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(20));
        finished = service.collect_completions().unwrap();
        if !finished.is_empty() {
            break;
        }
    }
    assert_eq!(finished, vec![LeaseId::from_u64(1)]);
    let restarts = service.take_restarts();
    assert_eq!(restarts.len(), 1, "first restart must be immediate");
    request.resources.id = restarts[0].0.id;
    service.submit(restarts[0].0.clone(), restarts[0].1);
    service.admit_one().unwrap();

    // Second failure: backoff of 2s since the last restart at t=10.
    let mut finished = Vec::new();
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(20));
        finished = service.collect_completions().unwrap();
        if !finished.is_empty() {
            break;
        }
    }
    assert_eq!(finished, vec![LeaseId::from_u64(2)]);
    service.cluster.set_now(11);
    assert!(
        service.take_restarts().is_empty(),
        "backoff must hold the restart until the delay elapses"
    );
    service.cluster.set_now(12);
    let restarts = service.take_restarts();
    assert_eq!(restarts.len(), 1, "backoff must release when due");
}

#[test]
fn restart_cap_survives_generations() {
    let mut service = NodeService::new();
    register_local_health_agent(&mut service);

    let request = archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(1),
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
            command: vec!["sh".into(), "-c".into(), "exit 1".into()],
            image: None,
            storage: vec![],
            ports: vec![],
            grace_secs: 0,
        },
        keep_alive: true,
    };
    service.submit(request, OwnerId::from_u64(1));
    service.admit_one().unwrap();

    // Fail, restart, admit; jump the clock past any backoff. After five
    // restarts the lineage must stop, even though every attempt has a
    // fresh request id.
    for (attempt, admitted) in (0..10u64).map(|attempt| (attempt, attempt + 1)) {
        let lease = LeaseId::from_u64(admitted);
        let mut finished = Vec::new();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            finished = service.collect_completions().unwrap();
            if !finished.is_empty() {
                break;
            }
        }
        assert_eq!(finished, vec![lease], "attempt {attempt} must fail");
        service.cluster.set_now(1_000 + attempt * 120);
        let restarts = service.take_restarts();
        if restarts.is_empty() {
            assert!(
                attempt >= 5,
                "the cap must hold at 5, stopped early at {attempt}"
            );
            return;
        }
        assert_eq!(restarts.len(), 1);
        service.submit(restarts[0].0.clone(), restarts[0].1);
        service.admit_one().unwrap();
    }
    panic!("ten attempts should have exhausted the cap");
}
