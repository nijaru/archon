use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use archon_kernel::{
    AdmissionPolicy, CapacityDimension, LeaseId, LeaseState, Need, NodeId, OwnerId, Request,
    RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::discover::{DeviceSpec, HostNodeSpec, MachineDescription};
use archon_node::protocol::{AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities};
use archon_node::service::{LeaseExecutor, NodeService};

const GIB: u64 = 1 << 30;

struct ProofExecutor;

impl LeaseExecutor for ProofExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        match request {
            AgentRequest::Prepare { binding, .. } => Ok(AgentResponse::Prepared {
                binding,
                handle: binding,
            }),
            AgentRequest::Activate { binding, .. } => Ok(AgentResponse::Activated { binding }),
            AgentRequest::Fence { binding, .. } => Ok(AgentResponse::Fenced { binding }),
            AgentRequest::Release { binding, .. } => Ok(AgentResponse::Released { binding }),
            AgentRequest::Status { lease } => Ok(AgentResponse::Running {
                lease,
                running: true,
                exit_code: None,
            }),
            AgentRequest::Logs { lease } => Ok(AgentResponse::Logs {
                lease,
                output: String::new(),
            }),
            other => Err(format!("unexpected mixed-workload request: {other:?}")),
        }
    }
}

fn capabilities() -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: true,
            device_isolation: true,
            physical_cpu_placement: true,
            numa_memory_placement: true,
        },
        container: RuntimeCapabilities::default(),
    }
}

fn description(name: &str, gpu: bool) -> MachineDescription {
    MachineDescription {
        instance_id: format!("mixed-{name}"),
        name: format!("mixed-{name}"),
        cpus: 1,
        memory_bytes: GIB,
        host_nodes: vec![
            HostNodeSpec {
                id: "cpu0".into(),
                kind: ResourceClass::Cpu,
                parent: None,
                attrs: BTreeMap::new(),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "mem0".into(),
                kind: ResourceClass::Memory,
                parent: None,
                attrs: BTreeMap::new(),
                capacity: qty(CapacityDimension::Bytes, GIB),
            },
        ],
        devices: if gpu {
            vec![DeviceSpec {
                kind: ResourceClass::Gpu,
                id: "gpu0".into(),
                dev: "/dev/null".into(),
                host_parent: None,
                access: Vec::new(),
                attrs: BTreeMap::from([("provider".into(), "proof".into())]),
            }]
        } else {
            Vec::new()
        },
    }
}

fn request(id: u64, class: RequestClass, priority: u32, gpu: bool) -> Request {
    let mut needs = vec![Need {
        kind: ResourceClass::Cpu,
        quantity: qty(CapacityDimension::Count, 1),
        filters: Vec::new(),
    }];
    if gpu {
        needs.push(Need {
            kind: ResourceClass::Gpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: Vec::new(),
        });
    }
    Request {
        id: RequestId::from_u64(id),
        class,
        needs,
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        command: Vec::new(),
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime: 3_600,
        priority,
        keep_alive: class == RequestClass::Service,
        machine_local: true,
        grace_secs: 0,
    }
}

fn lease_machine(service: &NodeService, lease: LeaseId) -> NodeId {
    let machines: BTreeSet<_> = service.cluster.leases[&lease]
        .allocation
        .claims
        .iter()
        .filter_map(|claim| service.cluster.graph.machine_of(claim.node))
        .collect();
    assert_eq!(machines.len(), 1);
    *machines.first().expect("one machine")
}

#[test]
fn mixed_service_batch_and_accelerator_contention_preserves_authority_boundaries() {
    let mut service = NodeService::new();
    let alpha = service
        .register_agent_with_capabilities(
            description("alpha", true),
            Box::new(ProofExecutor),
            capabilities(),
        )
        .expect("register accelerator machine");
    let beta = service
        .register_agent_with_capabilities(
            description("beta", false),
            Box::new(ProofExecutor),
            capabilities(),
        )
        .expect("register CPU machine");
    service.cluster.set_now(1);

    let fair = AdmissionPolicy {
        fair_share: true,
        ..AdmissionPolicy::default()
    };

    // A low-priority continuous service deterministically takes alpha's CPU.
    service.submit(
        request(1, RequestClass::Service, 1, false),
        OwnerId::from_u64(1),
    );
    assert_eq!(
        service.admit_one_with_policy(&fair).unwrap(),
        Some(RequestId::from_u64(1))
    );
    service.drive(Duration::from_secs(2)).unwrap();
    let service_lease = LeaseId::from_u64(1);
    assert_eq!(lease_machine(&service, service_lease), alpha);
    assert_eq!(service.cluster.leases[&service_lease].state, LeaseState::Active);

    // Equal-priority batch contenders share the remaining CPU. Owner 1 is
    // already consuming half of cluster CPU capacity, so fair-share selects
    // owner 2 even though owner 1 submitted first.
    service.submit(
        request(2, RequestClass::Batch, 5, false),
        OwnerId::from_u64(1),
    );
    service.submit(
        request(3, RequestClass::Batch, 5, false),
        OwnerId::from_u64(2),
    );
    assert_eq!(
        service.admit_one_with_policy(&fair).unwrap(),
        Some(RequestId::from_u64(3))
    );
    service.drive(Duration::from_secs(2)).unwrap();
    let batch_lease = LeaseId::from_u64(2);
    assert_eq!(lease_machine(&service, batch_lease), beta);
    assert_eq!(service.cluster.leases[&batch_lease].state, LeaseState::Active);

    // A higher-priority accelerator job needs alpha's CPU and GPU together.
    // No allocation is legal while both CPUs are occupied.
    let accelerator = request(4, RequestClass::Batch, 10, true);
    service.submit(accelerator.clone(), OwnerId::from_u64(3));
    assert_eq!(service.admit_one_with_policy(&fair).unwrap(), None);

    // Planning may name the low-priority service as a victim, but that alone
    // grants no authority. Ordinary revoke/fence closes its Binding first.
    assert_eq!(
        archon_kernel::preempt_victims(&service.cluster, &accelerator),
        Some(vec![service_lease])
    );
    service.revoke(service_lease).unwrap();
    service.drive(Duration::from_secs(2)).unwrap();
    assert_eq!(
        service.cluster.leases[&service_lease].state,
        LeaseState::Revoked
    );
    assert!(!service.cluster.occupies(service_lease));

    // Priority now admits the accelerator job ahead of the older owner-1
    // batch request. The unrelated owner-2 batch member stays active.
    assert_eq!(
        service.admit_one_with_policy(&fair).unwrap(),
        Some(RequestId::from_u64(4))
    );
    service.drive(Duration::from_secs(2)).unwrap();
    let accelerator_lease = LeaseId::from_u64(3);
    assert_eq!(lease_machine(&service, accelerator_lease), alpha);
    assert_eq!(
        service.cluster.leases[&accelerator_lease].state,
        LeaseState::Active
    );
    assert_eq!(service.cluster.leases[&batch_lease].state, LeaseState::Active);

    let kinds: BTreeSet<_> = service.cluster.leases[&accelerator_lease]
        .allocation
        .claims
        .iter()
        .map(|claim| service.cluster.graph.node(claim.node).unwrap().kind)
        .collect();
    assert_eq!(
        kinds,
        BTreeSet::from([ResourceClass::Cpu, ResourceClass::Gpu])
    );
}
