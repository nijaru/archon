use std::sync::{Arc, Mutex};
use std::time::Duration;

use archon_kernel::{
    CapacityDimension, LeaseId, LeaseState, Need, OwnerId, Request, RequestClass, RequestId,
    ResourceClass, preempt_victims, qty,
};
use archon_node::discover::{HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities,
};
use archon_node::service::{LeaseExecutor, NodeService};

const GIB: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Activate(u64),
    Fence(u64),
}

struct ProofExecutor {
    events: Arc<Mutex<Vec<Event>>>,
}

impl LeaseExecutor for ProofExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        match request {
            AgentRequest::Prepare { binding, .. } => Ok(AgentResponse::Prepared {
                binding,
                handle: binding,
            }),
            AgentRequest::Activate { binding, lease, .. } => {
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::Activate(lease));
                Ok(AgentResponse::Activated { binding })
            }
            AgentRequest::Fence { binding, lease, .. } => {
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::Fence(lease));
                Ok(AgentResponse::Fenced { binding })
            }
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
            other => Err(format!("unexpected request in preemption proof: {other:?}")),
        }
    }
}

fn boot() -> (NodeService, Arc<Mutex<Vec<Event>>>) {
    let mut service = NodeService::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let description = MachineDescription {
        instance_id: "preemption-machine".into(),
        name: "preemption-machine".into(),
        cpus: 1,
        memory_bytes: GIB,
        host_nodes: vec![
            HostNodeSpec {
                id: "cpu0".into(),
                kind: ResourceClass::Cpu,
                parent: None,
                attrs: Default::default(),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "mem0".into(),
                kind: ResourceClass::Memory,
                parent: None,
                attrs: Default::default(),
                capacity: qty(CapacityDimension::Bytes, GIB),
            },
        ],
        devices: Vec::new(),
    };
    service
        .register_agent_with_capabilities(
            description,
            Box::new(ProofExecutor {
                events: events.clone(),
            }),
            ExecutionCapabilities {
                process: RuntimeCapabilities {
                    available: true,
                    cpu_limit: true,
                    memory_limit: false,
                    device_isolation: false,
                    physical_cpu_placement: true,
                    numa_memory_placement: false,
                },
                container: RuntimeCapabilities::default(),
            },
        )
        .expect("register proof agent");
    (service, events)
}

fn request(id: u64, priority: u32) -> archon_node::workload::WorkloadSpec {
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
            lifetime: 3_600,
            priority,
            machine_local: true,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: vec![format!("job-{id}")],
            image: None,
            storage: Vec::new(),
            ports: Vec::new(),
            grace_secs: 0,
        },
        keep_alive: false,
    }
}

fn activate_low_priority() -> (NodeService, Arc<Mutex<Vec<Event>>>) {
    let (mut service, events) = boot();
    service.submit(request(1, 1), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    service
        .drive(Duration::from_secs(2))
        .expect("activate low-priority lease");
    assert_eq!(
        service.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
    (service, events)
}

#[test]
fn higher_priority_preemption_reuses_capacity_only_after_fence_ack() {
    let (mut service, events) = activate_low_priority();
    let high = request(2, 10);
    service.submit(high.clone(), OwnerId::from_u64(2));
    assert_eq!(
        service.admit_one().unwrap(),
        None,
        "occupied capacity must queue the higher-priority request"
    );

    let victims = preempt_victims(&service.cluster, &high).expect("preemption should make it fit");
    assert_eq!(victims, vec![LeaseId::from_u64(1)]);

    service.revoke(victims[0]).expect("revoke victim lease");
    assert_eq!(
        service.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Revoked
    );
    assert!(
        service.cluster.occupies(LeaseId::from_u64(1)),
        "revocation alone must not free authority while its Binding is open"
    );
    assert_eq!(
        service.admit_one().unwrap(),
        None,
        "the incoming request cannot commit before the victim fence is acknowledged"
    );

    service
        .drive(Duration::from_secs(2))
        .expect("record victim fence acknowledgement");
    assert!(
        !service.cluster.occupies(LeaseId::from_u64(1)),
        "capacity becomes reusable only after fencing closes the Binding"
    );
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(2)));
    service
        .drive(Duration::from_secs(2))
        .expect("activate higher-priority lease");
    assert_eq!(
        service.cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Active
    );

    let events = events.lock().expect("event lock");
    let fence = events
        .iter()
        .position(|event| *event == Event::Fence(1))
        .expect("victim fence event");
    let high_activate = events
        .iter()
        .position(|event| *event == Event::Activate(2))
        .expect("higher-priority activation");
    assert!(
        fence < high_activate,
        "the replacement may activate only after the victim was fenced"
    );
}

#[test]
fn preemption_never_selects_same_or_higher_priority_victims() {
    let (service, _) = activate_low_priority();
    let same_priority = request(2, 1);
    assert_eq!(
        preempt_victims(&service.cluster, &same_priority),
        None,
        "equal-priority work is not a valid victim under the baseline policy"
    );
}
