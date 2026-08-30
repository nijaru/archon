from pathlib import Path

service = Path('crates/node/src/service.rs')
text = service.read_text()

old = '''    /// CPU and memory claims of a lease, as enforceable limits.\n    fn lease_limits(&self, lease: LeaseId) -> Result<LeaseLimits, Error> {\n        let mut limits = LeaseLimits::default();\n        let claims = &self\n            .cluster\n            .leases\n            .get(&lease)\n            .ok_or(Error::UnknownLease(lease))?\n            .allocation\n            .claims;\n        for claim in claims {\n            match self.cluster.graph.node(claim.node).map(|node| node.kind) {\n                Some(ResourceClass::Cpu) => {\n                    limits.cpu_count += quantity_get(&claim.quantity, CapacityDimension::Count);\n                }\n                Some(ResourceClass::Memory) => {\n                    limits.memory_bytes += quantity_get(&claim.quantity, CapacityDimension::Bytes);\n                }\n                _ => {}\n            }\n        }\n        Ok(limits)\n    }\n'''
new = '''    /// CPU and memory claims of a lease enforced by one machine. A distributed\n    /// Lease may span several agents; no agent may receive another machine's\n    /// resource budget as if it were locally granted.\n    fn lease_limits_on_machine(\n        &self,\n        lease: LeaseId,\n        machine: NodeId,\n    ) -> Result<LeaseLimits, Error> {\n        let mut limits = LeaseLimits::default();\n        let claims = &self\n            .cluster\n            .leases\n            .get(&lease)\n            .ok_or(Error::UnknownLease(lease))?\n            .allocation\n            .claims;\n        for claim in claims {\n            if self.cluster.graph.machine_of(claim.node) != Some(machine) {\n                continue;\n            }\n            match self.cluster.graph.node(claim.node).map(|node| node.kind) {\n                Some(ResourceClass::Cpu) => {\n                    limits.cpu_count += quantity_get(&claim.quantity, CapacityDimension::Count);\n                }\n                Some(ResourceClass::Memory) => {\n                    limits.memory_bytes += quantity_get(&claim.quantity, CapacityDimension::Bytes);\n                }\n                _ => {}\n            }\n        }\n        Ok(limits)\n    }\n'''
assert old in text
text = text.replace(old, new)

old = '''    /// Host device paths bound by a lease's device-kind claims, resolved\n    /// through the graph's `dev` attributes.\n    pub fn lease_devices(&self, lease: LeaseId) -> Vec<crate::protocol::DeviceAccess> {\n        use archon_kernel::ResourceClass;\n        let Some(allocation_lease) = self.cluster.leases.get(&lease) else {\n            return Vec::new();\n        };\n        allocation_lease\n            .allocation\n            .claims\n            .iter()\n            .filter(|claim| {\n                self.cluster.graph.node(claim.node).is_some_and(|node| {\n                    matches!(\n                        node.kind,\n                        ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme\n                    )\n                })\n            })\n            .filter_map(|claim| {\n                self.cluster\n                    .graph\n                    .node(claim.node)\n                    .and_then(|node| crate::protocol::DeviceAccess::from_attrs(&node.attrs))\n            })\n            .collect()\n    }\n'''
new = '''    /// Host device paths bound by a lease's device-kind claims, resolved\n    /// through the graph's `dev` attributes.\n    pub fn lease_devices(&self, lease: LeaseId) -> Vec<crate::protocol::DeviceAccess> {\n        self.lease_devices_for_machine(lease, None)\n    }\n\n    fn lease_devices_on_machine(\n        &self,\n        lease: LeaseId,\n        machine: NodeId,\n    ) -> Vec<crate::protocol::DeviceAccess> {\n        self.lease_devices_for_machine(lease, Some(machine))\n    }\n\n    fn lease_devices_for_machine(\n        &self,\n        lease: LeaseId,\n        machine: Option<NodeId>,\n    ) -> Vec<crate::protocol::DeviceAccess> {\n        use archon_kernel::ResourceClass;\n        let Some(allocation_lease) = self.cluster.leases.get(&lease) else {\n            return Vec::new();\n        };\n        allocation_lease\n            .allocation\n            .claims\n            .iter()\n            .filter(|claim| {\n                machine.is_none_or(|machine| {\n                    self.cluster.graph.machine_of(claim.node) == Some(machine)\n                }) && self.cluster.graph.node(claim.node).is_some_and(|node| {\n                    matches!(\n                        node.kind,\n                        ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme\n                    )\n                })\n            })\n            .filter_map(|claim| {\n                self.cluster\n                    .graph\n                    .node(claim.node)\n                    .and_then(|node| crate::protocol::DeviceAccess::from_attrs(&node.attrs))\n            })\n            .collect()\n    }\n'''
assert old in text
text = text.replace(old, new)

text = text.replace('limits: self.lease_limits(lease)?,', 'limits: self.lease_limits_on_machine(lease, machine)?,', 1)
text = text.replace('devices: self.lease_devices(lease),', 'devices: self.lease_devices_on_machine(lease, machine),', 1)
service.write_text(text)

Path('crates/node/tests/distributed_activation.rs').write_text(r'''use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archon_kernel::{
    CapacityDimension, Filter, LeaseId, Need, OwnerId, Request, RequestClass, RequestId,
    ResourceClass, TopologyConstraint, TopologyRelation, qty,
};
use archon_node::discover::{DeviceSpec, HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, DeviceAccess, ExecutionCapabilities, LeaseLimits, RuntimeCapabilities,
};
use archon_node::service::{LeaseExecutor, NodeService};

const GIB: u64 = 1 << 30;

#[derive(Clone, Debug)]
enum Event {
    Prepare { machine: String },
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
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::Prepare {
                        machine: self.machine.clone(),
                    });
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

fn distributed_request() -> Request {
    Request {
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
        command: vec!["distributed-worker".into()],
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime: 60,
        priority: 1,
        keep_alive: false,
        machine_local: false,
        grace_secs: 0,
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
            .filter(|event| matches!(event, Event::Prepare { .. }))
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
''')
