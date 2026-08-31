use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archon_kernel::{
    CapacityDimension, LeaseId, LeaseState, Need, NodeId, OwnerId, Request, RequestClass,
    RequestId, ResourceClass, qty,
};
use archon_node::discover::{HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities,
};
use archon_node::service::{LeaseExecutor, NodeService};

const GIB: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Activate,
    Fence,
}

struct ReplicaExecutor {
    events: Arc<Mutex<Vec<Event>>>,
}

impl LeaseExecutor for ReplicaExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        match request {
            AgentRequest::Prepare { binding, .. } => Ok(AgentResponse::Prepared {
                binding,
                handle: binding,
            }),
            AgentRequest::Activate { binding, .. } => {
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::Activate);
                Ok(AgentResponse::Activated { binding })
            }
            AgentRequest::Fence { binding, .. } => {
                self.events.lock().expect("event lock").push(Event::Fence);
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
            other => Err(format!("unexpected request in replica executor: {other:?}")),
        }
    }
}

fn description(name: &str) -> MachineDescription {
    MachineDescription {
        instance_id: format!("replica-{name}"),
        name: format!("replica-{name}"),
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
    }
}

fn capabilities() -> ExecutionCapabilities {
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
    }
}

fn register(service: &mut NodeService, name: &str) -> (NodeId, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let machine = service
        .register_agent_with_capabilities(
            description(name),
            Box::new(ReplicaExecutor {
                events: events.clone(),
            }),
            capabilities(),
        )
        .expect("register replica agent");
    (machine, events)
}

fn replica(id: u64) -> archon_node::workload::WorkloadSpec {
    archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(id),
            class: RequestClass::Service,
            needs: vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: Vec::new(),
            }],
            topology: Vec::new(),
            preferences: Vec::new(),
            data: Vec::new(),
            lifetime: 3_600,
            priority: 1,
            machine_local: true,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: vec!["service-member".into()],
            image: None,
            storage: Vec::new(),
            ports: Vec::new(),
            grace_secs: 0,
        },
        keep_alive: true,
    }
}

fn lease_machine(service: &NodeService, lease: LeaseId) -> NodeId {
    let machines: BTreeSet<_> = service.cluster.leases[&lease]
        .allocation
        .claims
        .iter()
        .filter_map(|claim| service.cluster.graph.machine_of(claim.node))
        .collect();
    assert_eq!(
        machines.len(),
        1,
        "a service replica owns one machine-local lease"
    );
    *machines.first().expect("replica machine")
}

fn count(events: &Arc<Mutex<Vec<Event>>>, event: Event) -> usize {
    events
        .lock()
        .expect("event lock")
        .iter()
        .filter(|candidate| **candidate == event)
        .count()
}

#[test]
fn service_replicas_keep_independent_failure_boundaries_and_replace_only_the_failed_member() {
    let mut service = NodeService::new();
    let (alpha, alpha_events) = register(&mut service, "alpha");
    let (beta, beta_events) = register(&mut service, "beta");
    service.cluster.set_now(1);

    service.submit(replica(1), OwnerId::from_u64(1));
    service.submit(replica(2), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(2)));
    service
        .drive(Duration::from_secs(2))
        .expect("activate both replicas");

    let lease_one = LeaseId::from_u64(1);
    let lease_two = LeaseId::from_u64(2);
    assert_eq!(
        BTreeSet::from([
            lease_machine(&service, lease_one),
            lease_machine(&service, lease_two),
        ]),
        BTreeSet::from([alpha, beta]),
        "one-CPU machines force identical replicas onto independent leases"
    );

    let (failed, survivor) = if lease_machine(&service, lease_one) == alpha {
        (lease_one, lease_two)
    } else {
        (lease_two, lease_one)
    };
    let beta_activations = count(&beta_events, Event::Activate);
    assert_eq!(beta_activations, 1);

    service
        .mark_machine_unhealthy(alpha)
        .expect("quarantine failed replica machine");
    service
        .drive(Duration::from_secs(2))
        .expect("fence failed replica");

    assert_eq!(service.cluster.leases[&failed].state, LeaseState::Failed);
    assert_eq!(service.cluster.leases[&survivor].state, LeaseState::Active);
    assert_eq!(lease_machine(&service, survivor), beta);
    assert_eq!(count(&beta_events, Event::Fence), 0);
    assert_eq!(
        count(&beta_events, Event::Activate),
        beta_activations,
        "the healthy replica must not restart when its sibling fails"
    );
    assert!(
        count(&alpha_events, Event::Fence) > 0,
        "failed member authority must fence before reuse"
    );

    let restarts = service.take_restarts();
    assert_eq!(
        restarts.len(),
        1,
        "only the failed keep-alive member is replaced"
    );
    let replacement_id = restarts[0].0.id;
    service.submit(restarts[0].0.clone(), restarts[0].1);
    assert_eq!(
        service.admit_one().unwrap(),
        None,
        "quarantined alpha plus occupied beta leave no safe replacement capacity"
    );

    let (gamma, gamma_events) = register(&mut service, "gamma");
    assert_eq!(service.admit_one().unwrap(), Some(replacement_id));
    service
        .drive(Duration::from_secs(2))
        .expect("activate replacement replica");

    let replacement = LeaseId::from_u64(3);
    assert_eq!(service.cluster.leases[&survivor].state, LeaseState::Active);
    assert_eq!(
        service.cluster.leases[&replacement].state,
        LeaseState::Active
    );
    assert_eq!(lease_machine(&service, survivor), beta);
    assert_eq!(lease_machine(&service, replacement), gamma);
    assert_eq!(count(&gamma_events, Event::Activate), 1);
    assert_eq!(count(&beta_events, Event::Activate), beta_activations);
}
