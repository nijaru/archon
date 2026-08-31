use archon_kernel::{
    Attrs, CapacityDimension, Need, OwnerId, Quantity, Request, RequestClass, RequestId,
    ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::{HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities,
};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};

fn flat(instance: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: instance.into(),
        cpus: 1,
        memory_bytes: 1 << 30,
        host_nodes: Vec::new(),
        devices: Vec::new(),
    }
}

fn normalized(instance: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: instance.into(),
        cpus: 1,
        memory_bytes: 1 << 30,
        host_nodes: vec![
            HostNodeSpec {
                id: "numa/0".into(),
                kind: ResourceClass::Numa,
                parent: None,
                attrs: Attrs::new(),
                capacity: Quantity::new(),
            },
            HostNodeSpec {
                id: "cpu/0".into(),
                kind: ResourceClass::Cpu,
                parent: Some("numa/0".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "memory/0".into(),
                kind: ResourceClass::Memory,
                parent: Some("numa/0".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Bytes, 1 << 30),
            },
        ],
        devices: Vec::new(),
    }
}

fn executable_cpu_request(id: u64) -> archon_node::workload::WorkloadSpec {
    archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(id),
            class: RequestClass::Batch,
            needs: vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: Vec::new(),
            }],
            topology: Vec::new(),
            preferences: Vec::new(),
            data: Vec::new(),
            lifetime: 60,
            priority: 1,
            machine_local: true,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: vec!["true".into()],
            image: None,
            storage: Vec::new(),
            ports: Vec::new(),
            grace_secs: 0,
        },
        keep_alive: false,
    }
}

struct AggregateExecutor {
    inner: LocalExecutor,
}

impl LeaseExecutor for AggregateExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: ExecutionCapabilities {
                    process: RuntimeCapabilities {
                        available: true,
                        cpu_limit: true,
                        memory_limit: true,
                        device_isolation: true,
                        physical_cpu_placement: false,
                        numa_memory_placement: false,
                    },
                    container: RuntimeCapabilities::default(),
                },
            });
        }
        self.inner.execute(request)
    }
}

fn executor() -> Box<dyn LeaseExecutor> {
    Box::new(AggregateExecutor {
        inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
    })
}

#[test]
fn executable_work_skips_normalized_cpu_placement_without_cpuset_support() {
    let mut service = NodeService::new();
    let normalized_machine = service
        .register_agent(normalized("normalized"), executor())
        .expect("normalized registration");
    let flat_machine = service
        .register_agent(flat("flat"), executor())
        .expect("flat registration");

    service.submit(executable_cpu_request(1), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let lease = service.cluster.leases.values().next().expect("lease");
    let selected = service
        .cluster
        .graph
        .machine_of(lease.allocation.claims[0].node)
        .expect("claim machine");
    assert_eq!(selected, flat_machine);
    assert_ne!(selected, normalized_machine);
}

#[test]
fn normalized_cpu_only_machine_stays_unadmitted_until_placement_is_enforceable() {
    let mut service = NodeService::new();
    service
        .register_agent(normalized("normalized-only"), executor())
        .expect("normalized registration");
    service.submit(executable_cpu_request(1), OwnerId::from_u64(1));

    assert_eq!(service.admit_one().unwrap(), None);
    assert!(
        service.cluster.leases.is_empty(),
        "unsupported execution placement must be rejected before Lease authority mutates"
    );
}
