use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archon_kernel::{
    CapacityDimension, Filter, LeaseId, Need, OwnerId, Request, RequestClass, RequestId,
    ResourceClass, TopologyConstraint, TopologyRelation, qty,
};
use archon_node::discover::{DeviceSpec, HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, DeviceAccess, ExecutionCapabilities, LeaseLimits,
    RuntimeCapabilities,
};
use archon_node::service::{LeaseExecutor, NodeService};

const GIB: u64 = 1 << 30;

#[derive(Clone, Debug)]
enum Event {
    Prepare,
    Activate {
        machine: String,
        limits: LeaseLimits,
        devices: Vec<DeviceAccess>,
    },
}

struct RecordingExecutor {
    machine: String,
    events: Arc<Mutex<Vec<Event>>>,
}

impl LeaseExecutor for RecordingExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        match request {
            AgentRequest::Prepare { binding, .. } => {
                self.events.lock().expect("event lock").push(Event::Prepare);
                Ok(AgentResponse::Prepared {
                    binding,
                    handle: binding,
                })
            }
            AgentRequest::Activate {
                binding,
                limits,
                devices,
                ..
            } => {
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::Activate {
                        machine: self.machine.clone(),
                        limits,
                        devices,
                    });
                Ok(AgentResponse::Activated { binding })
            }
            AgentRequest::Release { binding, .. } => Ok(AgentResponse::Released { binding }),
            AgentRequest::Fence { binding, .. } => Ok(AgentResponse::Fenced { binding }),
            AgentRequest::Status { lease } => Ok(AgentResponse::Running {
                lease,
                running: true,
                exit_code: None,
            }),
            AgentRequest::Logs { lease } => Ok(AgentResponse::Logs {
                lease,
                output: self.machine.clone(),
            }),
            other => Err(format!("unexpected request in proof executor: {other:?}")),
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

#[test]
fn distributed_activation_is_prepared_atomically_and_scoped_per_machine() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut service = NodeService::new();
    for (member, dev) in [("a", "/dev/gpu-a"), ("b", "/dev/gpu-b")] {
        service
            .register_agent_with_capabilities(
                description(member, dev),
                Box::new(RecordingExecutor {
                    machine: member.into(),
                    events: events.clone(),
                }),
                caps(),
            )
            .expect("register distributed proof agent");
    }

    service.submit(distributed_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    service
        .drive(Duration::from_secs(2))
        .expect("settle prepare/activate effects");

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
    let first_activate = events
        .iter()
        .position(|event| matches!(event, Event::Activate { .. }))
        .expect("activation events");
    assert_eq!(
        events[..first_activate]
            .iter()
            .filter(|event| matches!(event, Event::Prepare))
            .count(),
        6,
        "all six resource bindings must prepare before any member starts"
    );

    for member in ["a", "b"] {
        let activations: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::Activate {
                    machine,
                    limits,
                    devices,
                } if machine == member => Some((*limits, devices)),
                _ => None,
            })
            .collect();
        assert_eq!(activations.len(), 3, "one activation ack per binding");
        for (limits, devices) in activations {
            assert_eq!(limits.cpu_count, 1);
            assert_eq!(limits.memory_bytes, GIB);
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0].dev, format!("/dev/gpu-{member}"));
        }
    }
}
