//! Stable device identity: a device keeps its graph node across agent
//! re-registrations even when its host path changes, so live claims keep
//! resolving through the current path.

use archon_kernel::{
    Attrs, CapacityDimension, Command, Edge, EdgeKind, LeaseId, LeaseState, Need, Node, NodeId,
    NodeState, OwnerId, Quantity, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::{DeviceSpec, HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities,
};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};

struct DeviceClaimExecutor {
    inner: LocalExecutor,
}

impl LeaseExecutor for DeviceClaimExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: ExecutionCapabilities {
                    process: RuntimeCapabilities {
                        available: true,
                        cpu_limit: false,
                        memory_limit: false,
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
    Box::new(DeviceClaimExecutor {
        inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
    })
}

fn description(instance: &str, gpu_dev: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: "gpu-box".into(),
        cpus: 2,
        memory_bytes: 0,
        host_nodes: Vec::new(),
        devices: vec![DeviceSpec {
            kind: ResourceClass::Gpu,
            id: "gpu0".into(),
            dev: gpu_dev.into(),
            host_parent: None,
            access: Vec::new(),
            attrs: Default::default(),
        }],
    }
}

#[test]
fn provider_support_paths_follow_one_device_claim() {
    let mut service = NodeService::new();
    let mut description = description("inst-paths", "/dev/null");
    description.devices[0].access = vec!["/dev/zero".into()];
    description.devices[0]
        .attrs
        .insert("cdi".into(), "nvidia.com/gpu=gpu0".into());
    service
        .register_agent(description, executor())
        .expect("registration");
    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let devices = service.lease_devices(LeaseId::from_u64(1));
    assert_eq!(devices[0].dev, "/dev/null");
    assert_eq!(devices[0].paths, vec!["/dev/zero"]);
    assert_eq!(devices[0].cdi.as_deref(), Some("nvidia.com/gpu=gpu0"));
    service.revoke(LeaseId::from_u64(1)).expect("revoke");
}

fn gpu_request() -> Request {
    Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Gpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: vec![],
        }],
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
    }
}

#[test]
fn device_identity_survives_re_registration_with_new_path() {
    let mut service = NodeService::new();
    service
        .register_agent(description("inst-dev", "/dev/gpuA"), executor())
        .expect("first registration");
    let gpu = *service
        .cluster
        .graph
        .nodes_of_class(ResourceClass::Gpu)
        .first()
        .expect("declared gpu exists");
    assert_eq!(
        service.cluster.graph.node(gpu).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuA".to_string())
    );

    // Admit work that holds the device claim.
    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));

    // The agent returns with the same device id behind a new host path.
    service
        .register_agent(description("inst-dev", "/dev/gpuB"), executor())
        .expect("re-registration");

    // Same node, refreshed path: the live claim still points at gpu0.
    let attrs = &service.cluster.graph.node(gpu).unwrap().attrs;
    assert_eq!(attrs.get("dev"), Some(&"/dev/gpuB".to_string()));
    assert_eq!(attrs.get("id"), Some(&"gpu0".to_string()));
    assert_eq!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Gpu)
            .len(),
        1,
        "no duplicate device node"
    );
    let access = service.lease_devices(LeaseId::from_u64(1));
    assert_eq!(access.len(), 1);
    assert_eq!(access[0].id, "gpu0");
    assert_eq!(access[0].dev, "/dev/gpuB");
}

#[test]
fn re_registration_adds_and_refreshes_without_spurious_revisions() {
    let mut service = NodeService::new();
    service
        .register_agent(description("inst-add", "/dev/gpuA"), executor())
        .expect("first registration");

    // Identical declaration: nothing to reconcile.
    let revision = service.cluster.graph.revision;
    service
        .register_agent(description("inst-add", "/dev/gpuA"), executor())
        .expect("no-op re-registration");
    assert_eq!(
        service.cluster.graph.revision, revision,
        "identical declaration must not advance the graph"
    );

    // One path refresh plus one brand-new device.
    let mut updated = description("inst-add", "/dev/gpuC");
    updated.devices.push(DeviceSpec {
        kind: ResourceClass::Nvme,
        id: "nvme0".into(),
        dev: "/dev/nvme0n1".into(),
        host_parent: None,
        access: Vec::new(),
        attrs: Default::default(),
    });
    service
        .register_agent(updated, executor())
        .expect("re-registration with changes");
    assert_eq!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Nvme)
            .len(),
        1,
        "new device added"
    );
    let gpus = service.cluster.graph.nodes_of_class(ResourceClass::Gpu);
    assert_eq!(gpus.len(), 1);
    assert_eq!(
        service
            .cluster
            .graph
            .node(gpus[0])
            .unwrap()
            .attrs
            .get("dev"),
        Some(&"/dev/gpuC".to_string()),
        "path refreshed in place"
    );
}

#[test]
fn provider_disappearance_blocks_placement_and_same_id_reappears() {
    let mut service = NodeService::new();
    service
        .register_agent(description("inst-disappear", "/dev/gpuA"), executor())
        .expect("first registration");
    let gpu = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];

    let mut missing = description("inst-disappear", "/dev/gpuA");
    missing.devices.clear();
    service
        .register_agent(missing, executor())
        .expect("provider reports disappearance");

    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Unavailable)
    );
    assert_eq!(
        service.cluster.graph.nodes_of_class(ResourceClass::Gpu),
        vec![gpu],
        "disappearance preserves the stable identity record"
    );
    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(
        service.admit_one().unwrap(),
        None,
        "missing provider capacity must not accept new claims"
    );

    service
        .register_agent(description("inst-disappear", "/dev/gpuB"), executor())
        .expect("same stable device returns");

    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Schedulable)
    );
    assert_eq!(
        service.cluster.graph.nodes_of_class(ResourceClass::Gpu),
        vec![gpu]
    );
    assert_eq!(
        service.cluster.graph.node(gpu).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuB".to_string())
    );
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    service.revoke(LeaseId::from_u64(1)).expect("cleanup");
}

#[test]
fn provider_disappearance_fails_and_fences_a_live_device_claim() {
    let mut service = NodeService::new();
    service
        .register_agent(description("inst-live", "/dev/null"), executor())
        .expect("first registration");
    let gpu = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];
    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    assert_eq!(
        service
            .cluster
            .leases
            .get(&LeaseId::from_u64(1))
            .unwrap()
            .state,
        LeaseState::Preparing,
        "the durable claim is live before process activation settles"
    );

    let mut missing = description("inst-live", "/dev/null");
    missing.devices.clear();
    service
        .register_agent(missing, executor())
        .expect("provider reports disappearance");

    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Unavailable)
    );
    assert_eq!(
        service
            .cluster
            .leases
            .get(&LeaseId::from_u64(1))
            .unwrap()
            .state,
        LeaseState::Failed,
        "work using vanished hardware follows the workload-failure path"
    );
    service
        .drive(std::time::Duration::from_secs(1))
        .expect("fence old binding");
    assert!(
        !service.cluster.occupies(LeaseId::from_u64(1)),
        "claim remains occupied until the binding fence is acknowledged"
    );

    service
        .register_agent(description("inst-live", "/dev/zero"), executor())
        .expect("same stable device returns after fencing");
    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Schedulable)
    );
    assert_eq!(
        service.cluster.graph.nodes_of_class(ResourceClass::Gpu),
        vec![gpu]
    );
}

#[test]
fn replacement_at_the_same_path_gets_a_new_identity() {
    let mut service = NodeService::new();
    service
        .register_agent(description("inst-replace", "/dev/gpuA"), executor())
        .expect("first registration");
    let original = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];

    let mut replacement = description("inst-replace", "/dev/gpuA");
    replacement.devices[0].id = "gpu1".into();
    service
        .register_agent(replacement, executor())
        .expect("replacement registration");

    let gpus = service.cluster.graph.nodes_of_class(ResourceClass::Gpu);
    assert_eq!(gpus.len(), 2);
    assert_eq!(
        service.cluster.node_state(original),
        Some(NodeState::Unavailable)
    );
    let fresh = *gpus
        .iter()
        .find(|node| **node != original)
        .expect("new node");
    assert_eq!(
        service.cluster.node_state(fresh),
        Some(NodeState::Schedulable)
    );
    assert_eq!(
        service
            .cluster
            .graph
            .node(original)
            .unwrap()
            .attrs
            .get("id"),
        Some(&"gpu0".to_string())
    );
    assert_eq!(
        service.cluster.graph.node(fresh).unwrap().attrs.get("id"),
        Some(&"gpu1".to_string())
    );
    assert_eq!(
        service.cluster.graph.node(fresh).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuA".to_string()),
        "host-path reuse does not reuse the old provider identity"
    );
}

#[test]
fn initial_registration_rejects_invalid_device_inventory() {
    let mut service = NodeService::new();
    let mut duplicate = description("inst-invalid", "/dev/gpuA");
    duplicate.devices.push(duplicate.devices[0].clone());
    assert!(service.register_agent(duplicate, executor(),).is_err());
    assert!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Machine)
            .is_empty()
    );

    let mut invalid_kind = description("inst-invalid-kind", "/dev/gpuA");
    invalid_kind.devices[0].kind = ResourceClass::Cpu;
    assert!(service.register_agent(invalid_kind, executor(),).is_err());
    assert!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Machine)
            .is_empty()
    );
}

#[test]
fn nested_device_reconciliation_preserves_topology_parent() {
    let mut service = NodeService::new();
    let machine = NodeId::from_u64(100);
    let numa = NodeId::from_u64(101);
    let gpu = NodeId::from_u64(102);
    let cpu0 = NodeId::from_u64(103);
    let cpu1 = NodeId::from_u64(104);
    let memory = NodeId::from_u64(105);

    let mut machine_attrs = Attrs::new();
    machine_attrs.insert("agent_id".into(), "inst-nested".into());
    machine_attrs.insert("name".into(), "gpu-box".into());
    let mut gpu_attrs = Attrs::new();
    gpu_attrs.insert("id".into(), "gpu0".into());
    gpu_attrs.insert("dev".into(), "/dev/gpuA".into());

    service
        .cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                Node {
                    id: machine,
                    kind: ResourceClass::Machine,
                    attrs: machine_attrs,
                    capacity: Quantity::new(),
                },
                Node {
                    id: numa,
                    kind: ResourceClass::Numa,
                    attrs: Attrs::new(),
                    capacity: Quantity::new(),
                },
                Node {
                    id: gpu,
                    kind: ResourceClass::Gpu,
                    attrs: gpu_attrs,
                    capacity: qty(CapacityDimension::Count, 1),
                },
                Node {
                    id: cpu0,
                    kind: ResourceClass::Cpu,
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
                Node {
                    id: cpu1,
                    kind: ResourceClass::Cpu,
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
                Node {
                    id: memory,
                    kind: ResourceClass::Memory,
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Bytes, 0),
                },
            ],
            edges: vec![
                Edge {
                    from: machine,
                    to: numa,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
                Edge {
                    from: numa,
                    to: gpu,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
                Edge {
                    from: machine,
                    to: cpu0,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
                Edge {
                    from: machine,
                    to: cpu1,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
                Edge {
                    from: machine,
                    to: memory,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
            ],
        })
        .expect("seed nested device topology");

    service
        .register_agent(description("inst-nested", "/dev/gpuB"), executor())
        .expect("reconcile nested device");

    assert_eq!(
        service.cluster.graph.nodes_of_class(ResourceClass::Gpu),
        vec![gpu],
        "stable provider identity must not duplicate a nested device"
    );
    assert_eq!(
        service.cluster.graph.parent(gpu),
        Some(numa),
        "path refresh must preserve provider-independent topology containment"
    );
    assert_eq!(
        service.cluster.graph.node(gpu).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuB".to_string())
    );

    let mut missing = description("inst-nested", "/dev/gpuB");
    missing.devices.clear();
    service
        .register_agent(missing, executor())
        .expect("nested device disappearance");
    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Unavailable)
    );
    assert_eq!(service.cluster.graph.parent(gpu), Some(numa));

    service
        .register_agent(description("inst-nested", "/dev/gpuC"), executor())
        .expect("nested device reappearance");
    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Schedulable)
    );
    assert_eq!(service.cluster.graph.parent(gpu), Some(numa));
    assert_eq!(
        service.cluster.graph.node(gpu).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuC".to_string())
    );
}

#[test]
fn returning_agent_with_changed_host_shape_fails_closed() {
    let mut service = NodeService::new();
    service
        .register_agent(description("inst-host-change", "/dev/gpuA"), executor())
        .expect("first registration");
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let mut changed = description("inst-host-change", "/dev/gpuA");
    changed.cpus = 3;
    let err = service
        .register_agent(changed, executor())
        .expect_err("changed host facts must not be silently accepted");
    assert!(err.to_string().contains("host inventory changed"));
    assert_eq!(
        service.cluster.node_state(machine),
        Some(NodeState::Unavailable)
    );
}

fn normalized_description(instance: &str, parent: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: "normalized-gpu-box".into(),
        cpus: 2,
        memory_bytes: 4096,
        host_nodes: vec![
            HostNodeSpec {
                id: "numa/0".into(),
                kind: ResourceClass::Numa,
                parent: None,
                attrs: Attrs::new(),
                capacity: Quantity::new(),
            },
            HostNodeSpec {
                id: "numa/1".into(),
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
                id: "cpu/1".into(),
                kind: ResourceClass::Cpu,
                parent: Some("numa/1".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "memory/0".into(),
                kind: ResourceClass::Memory,
                parent: Some("numa/0".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Bytes, 2048),
            },
            HostNodeSpec {
                id: "memory/1".into(),
                kind: ResourceClass::Memory,
                parent: Some("numa/1".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Bytes, 2048),
            },
        ],
        devices: vec![DeviceSpec {
            kind: ResourceClass::Gpu,
            id: "gpu0".into(),
            dev: "/dev/gpuA".into(),
            host_parent: Some(parent.into()),
            access: Vec::new(),
            attrs: Attrs::new(),
        }],
    }
}

#[test]
fn explicit_device_host_parent_is_used_for_new_inventory() {
    let mut service = NodeService::new();
    let mut first = normalized_description("inst-parent", "numa/0");
    first.devices.clear();
    service
        .register_agent(first, executor())
        .expect("host registration");
    service
        .register_agent(normalized_description("inst-parent", "numa/0"), executor())
        .expect("new nested device");
    let gpu = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];
    let parent = service.cluster.graph.parent(gpu).expect("GPU parent");
    assert_eq!(
        service
            .cluster
            .graph
            .node(parent)
            .and_then(|node| node.attrs.get("archon.host-id"))
            .map(String::as_str),
        Some("numa/0")
    );
}

#[test]
fn same_device_cannot_silently_move_between_host_domains() {
    let mut service = NodeService::new();
    service
        .register_agent(
            normalized_description("inst-reparent", "numa/0"),
            executor(),
        )
        .expect("first registration");
    let gpu = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];
    let original_parent = service.cluster.graph.parent(gpu);
    let err = service
        .register_agent(
            normalized_description("inst-reparent", "numa/1"),
            executor(),
        )
        .expect_err("hard containment change requires explicit reconciliation");
    assert!(err.to_string().contains("containment"));
    assert_eq!(service.cluster.graph.parent(gpu), original_parent);
}
