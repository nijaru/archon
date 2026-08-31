use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archon_kernel::{
    BindingState, CapacityDimension, Filter, LeaseId, LeaseState, Need, OwnerId, Request,
    RequestClass, RequestId, ResourceClass, TopologyConstraint, TopologyRelation, qty,
};
use archon_node::discover::{DeviceSpec, HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, DeviceAccess, ExecutionCapabilities, LeaseLimits,
    RuntimeCapabilities,
};
use archon_node::service::{AgentClient, NodeService};

const GIB: u64 = 1 << 30;

#[derive(Clone, Debug)]
enum Event {
    Prepare,
    Activate { machine: String },
    StartExecution {
        machine: String,
        limits: LeaseLimits,
        devices: Vec<DeviceAccess>,
    },
    Fence { machine: String },
}

struct RecordingAgent {
    machine: String,
    events: Arc<Mutex<Vec<Event>>>,
    fail_execution_start: bool,
}

impl AgentClient for RecordingAgent {
    fn call(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        match request {
            AgentRequest::Prepare { binding, .. } => {
                self.events.lock().expect("event lock").push(Event::Prepare);
                Ok(AgentResponse::Prepared {
                    binding,
                    handle: binding,
                })
            }
            AgentRequest::Activate { binding, .. } => {
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::Activate {
                        machine: self.machine.clone(),
                    });
                Ok(AgentResponse::Activated { binding })
            }
            AgentRequest::StartExecution {
                lease,
                limits,
                devices,
                ..
            } => {
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::StartExecution {
                        machine: self.machine.clone(),
                        limits,
                        devices,
                    });
                if self.fail_execution_start {
                    Ok(AgentResponse::ExecutionFailed {
                        lease,
                        reason: format!("{} refused execution", self.machine),
                    })
                } else {
                    Ok(AgentResponse::ExecutionStarted { lease })
                }
            }
            AgentRequest::Release { binding, .. } => Ok(AgentResponse::Released { binding }),
            AgentRequest::Fence { binding, .. } => {
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::Fence {
                        machine: self.machine.clone(),
                    });
                Ok(AgentResponse::Fenced { binding })
            }
            AgentRequest::Status { lease } => Ok(AgentResponse::Running {
                lease,
                running: true,
                exit_code: None,
            }),
            AgentRequest::Logs { lease } => Ok(AgentResponse::Logs {
                lease,
                output: self.machine.clone(),
            }),
            other => Err(format!("unexpected request in proof agent: {other:?}")),
        }
    }
}

fn attrs(member: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("member".into(), member.into())])
}

fn description(member: &str, dev: &str) -> MachineDescription {
    MachineDescription {
        instance_id: format!("distributed-{member}"),
        name: format!("distributed-{member}"),
        cpus: 1,
        memory_bytes: 2 * GIB,
        host_nodes: vec![
            HostNodeSpec {
                id: "cpu0".into(),
                kind: ResourceClass::Cpu,
                parent: None,
                attrs: attrs(member),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "mem0".into(),
                kind: ResourceClass::Memory,
                parent: None,
                attrs: attrs(member),
                capacity: qty(CapacityDimension::Bytes, 2 * GIB),
            },
        ],
        devices: vec![DeviceSpec {
            kind: ResourceClass::Gpu,
            id: format!("gpu-{member}"),
            dev: dev.into(),
            host_parent: None,
            access: Vec::new(),
            attrs: attrs(member),
        }],
    }
}

fn caps() -> ExecutionCapabilities {
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

fn need(kind: ResourceClass, dimension: CapacityDimension, amount: u64, member: &str) -> Need {
    Need {
        kind,
        quantity: qty(dimension, amount),
        filters: vec![Filter {
            key: "member".into(),
            value: member.into(),
        }],
    }
}

fn same_machine(left: usize, right: usize) -> TopologyConstraint {
    TopologyConstraint {
        left,
        right,
        relation: TopologyRelation::SameAncestor {
            class: ResourceClass::Machine,
        },
    }
}

fn distributed_request() -> archon_node::workload::WorkloadSpec {
    archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(1),
            class: RequestClass::Batch,
            needs: vec![
                need(ResourceClass::Cpu, CapacityDimension::Count, 1, "a"),
                need(ResourceClass::Memory, CapacityDimension::Bytes, GIB, "a"),
                need(ResourceClass::Gpu, CapacityDimension::Count, 1, "a"),
                need(ResourceClass::Cpu, CapacityDimension::Count, 1, "b"),
                need(ResourceClass::Memory, CapacityDimension::Bytes, GIB, "b"),
                need(ResourceClass::Gpu, CapacityDimension::Count, 1, "b"),
            ],
            topology: vec![
                same_machine(0, 1),
                same_machine(0, 2),
                same_machine(3, 4),
                same_machine(3, 5),
            ],
            preferences: Vec::new(),
            data: Vec::new(),
            lifetime: 60,
            priority: 1,
            machine_local: false,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: vec!["distributed-worker".into()],
            image: None,
            storage: Vec::new(),
            ports: Vec::new(),
            grace_secs: 0,
        },
        keep_alive: false,
    }
}

fn register_agents(
    service: &mut NodeService,
    events: &Arc<Mutex<Vec<Event>>>,
    failing_member: Option<&str>,
) {
    for (member, dev) in [("a", "/dev/gpu-a"), ("b", "/dev/gpu-b")] {
        service
            .register_agent_with_capabilities(
                description(member, dev),
                Box::new(RecordingAgent {
                    machine: member.into(),
                    events: events.clone(),
                    fail_execution_start: failing_member == Some(member),
                }),
                caps(),
            )
            .expect("register distributed proof agent");
    }
}

#[test]
fn distributed_activation_is_prepared_atomically_and_scoped_per_machine() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut service = NodeService::new();
    register_agents(&mut service, &events, None);

    service.submit(distributed_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    service
        .drive(Duration::from_secs(2))
        .expect("settle prepare/activate/start effects");

    let lease = service
        .cluster
        .leases
        .get(&LeaseId::from_u64(1))
        .expect("distributed lease");
    let machines: std::collections::BTreeSet<_> = lease
        .allocation
        .claims
        .iter()
        .filter_map(|claim| service.cluster.graph.machine_of(claim.node))
        .collect();
    assert_eq!(machines.len(), 2, "one rigid lease must span both agents");

    let events = events.lock().expect("event lock");
    let first_start = events
        .iter()
        .position(|event| matches!(event, Event::StartExecution { .. }))
        .expect("execution start events");
    assert_eq!(
        events[..first_start]
            .iter()
            .filter(|event| matches!(event, Event::Prepare))
            .count(),
        6,
        "all six resource bindings must prepare before any member starts"
    );
    assert_eq!(
        events[..first_start]
            .iter()
            .filter(|event| matches!(event, Event::Activate { .. }))
            .count(),
        6,
        "all six resource bindings must acknowledge activation before any member starts"
    );

    for member in ["a", "b"] {
        let activations = events
            .iter()
            .filter(|event| {
                matches!(event, Event::Activate { machine } if machine == member)
            })
            .count();
        assert_eq!(activations, 3, "one Provider activation per binding");

        let starts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::StartExecution {
                    machine,
                    limits,
                    devices,
                } if machine == member => Some((*limits, devices)),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 1, "one execution start per workload member");
        let (limits, devices) = starts[0];
        assert_eq!(limits.cpu_count, 1);
        assert_eq!(limits.memory_bytes, GIB);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].dev, format!("/dev/gpu-{member}"));
    }
}

#[test]
fn execution_start_failure_fails_and_fences_the_rigid_root() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut service = NodeService::new();
    register_agents(&mut service, &events, Some("b"));

    service.submit(distributed_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    service
        .drive(Duration::from_secs(2))
        .expect("settle failed member start and fencing");

    let lease = service
        .cluster
        .leases
        .get(&LeaseId::from_u64(1))
        .expect("distributed lease");
    assert_eq!(lease.state, LeaseState::Failed);
    assert!(
        service
            .cluster
            .bindings_for(lease.id)
            .iter()
            .all(|binding| service
                .cluster
                .bindings
                .get(binding)
                .is_some_and(|record| record.state == BindingState::Fenced)),
        "execution-start failure must close every resource Binding through ordinary fencing"
    );

    let events = events.lock().expect("event lock");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Fence { .. }))
            .count(),
        6,
        "all six resource bindings must fence after member execution fails to start"
    );
    assert!(events.iter().any(|event| {
        matches!(event, Event::Fence { machine } if machine == "a")
    }));
    assert!(events.iter().any(|event| {
        matches!(event, Event::Fence { machine } if machine == "b")
    }));
}
