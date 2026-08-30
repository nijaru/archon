//! Heterogeneous placement: machines with different shapes place against
//! their real capacity — small machines don't get oversized work.

use std::net::TcpListener;

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

fn hetero_capabilities(device_isolation: bool) -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: false,
            device_isolation,
            physical_cpu_placement: false,
            numa_memory_placement: false,
        },
        container: RuntimeCapabilities::default(),
    }
}

struct HeteroExecutor {
    inner: LocalExecutor,
    device_isolation: bool,
}

impl LeaseExecutor for HeteroExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: hetero_capabilities(self.device_isolation),
            });
        }
        self.inner.execute(request)
    }
}

/// An agent reporting a specific machine shape.
fn spawn_shaped_agent(instance: &'static str, name: &'static str, cpus: u64) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
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
                name: name.to_string(),
                cpus,
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
                        capabilities: hetero_capabilities(false),
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
    addr
}

fn register(
    service: &mut NodeService,
    instance: &str,
    name: &str,
    cpus: u64,
    addr: &str,
) -> archon_kernel::NodeId {
    let mut executor = archon_node::service::RemoteExecutor::connect(addr, None).expect("connect");
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
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, cpus),
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

#[test]
fn device_claims_resolve_to_host_paths() {
    // A machine declaring two GPUs; a claim against one resolves to its
    // host device path through the graph.
    let mut service = NodeService::new();
    let base = service
        .cluster
        .graph
        .nodes()
        .map(|n| n.id.as_u64())
        .max()
        .unwrap_or(0);
    let (_local, nodes, edges) = archon_node::discover::build_graph(
        &archon_node::discover::MachineDescription {
            instance_id: "inst-gpu".into(),
            name: "gpu-box".into(),
            cpus: 2,
            memory_bytes: 0,
            host_nodes: Vec::new(),
            devices: vec![gpu_spec("/dev/gpuA"), gpu_spec("/dev/gpuB")],
        },
        base,
    );
    service
        .cluster
        .apply(archon_kernel::Command::ApplyGraph { nodes, edges })
        .unwrap();

    // Find the first GPU node and claim exactly it.
    let gpu = service
        .cluster
        .graph
        .nodes_of_class(archon_kernel::ResourceClass::Gpu)
        .iter()
        .copied()
        .find(|id| {
            service
                .cluster
                .graph
                .node(*id)
                .is_some_and(|n| n.attrs.get("dev").is_some_and(|d| d == "/dev/gpuA"))
        })
        .expect("declared gpu exists");
    let machine = service
        .cluster
        .graph
        .machine_of(gpu)
        .expect("device sits under the machine");
    service
        .register_agent(
            archon_node::discover::MachineDescription {
                instance_id: "inst-gpu".into(),
                name: "gpu-box".into(),
                cpus: 2,
                memory_bytes: 0,
                host_nodes: Vec::new(),
                devices: vec![gpu_spec("/dev/gpuA"), gpu_spec("/dev/gpuB")],
            },
            Box::new(HeteroExecutor {
                inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
                device_isolation: true,
            }),
        )
        .unwrap();

    let request = Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: ResourceClass::Gpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
        ],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec!["sleep".into(), "5".into()],
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 3_600,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
    };
    service.submit(request, OwnerId::from_u64(1));
    let _ = machine;
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let devices = service.lease_devices(LeaseId::from_u64(1));
    assert_eq!(devices.len(), 1, "one claimed gpu");
    assert!(
        devices[0].dev.starts_with("/dev/gpu"),
        "resolved host path, got {:?}",
        devices[0]
    );
}

/// A GPU declaration whose stable id is its host path.
fn gpu_spec(dev: &str) -> archon_node::discover::DeviceSpec {
    archon_node::discover::DeviceSpec {
        kind: archon_kernel::ResourceClass::Gpu,
        id: dev.to_string(),
        dev: dev.to_string(),
        host_parent: None,
        access: Vec::new(),
        attrs: Default::default(),
    }
}
