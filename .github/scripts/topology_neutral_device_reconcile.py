from pathlib import Path

service_path = Path("crates/node/src/service.rs")
text = service_path.read_text()
old = '''        let mut existing: BTreeMap<String, NodeId> = self
            .cluster
            .graph
            .children(machine)
            .iter()
            .filter_map(|child| {
                let node = self.cluster.graph.node(*child)?;
                if !matches!(
                    node.kind,
                    ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
                ) {
                    return None;
                }
                node.attrs.get("id").map(|id| (id.clone(), *child))
            })
            .collect();
'''
new = '''        let mut existing: BTreeMap<String, NodeId> = self
            .cluster
            .graph
            .descendants(machine)
            .into_iter()
            .filter_map(|child| {
                let node = self.cluster.graph.node(child)?;
                if !matches!(
                    node.kind,
                    ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
                ) {
                    return None;
                }
                node.attrs.get("id").map(|id| (id.clone(), child))
            })
            .collect();
'''
if text.count(old) != 1:
    raise RuntimeError("device reconciliation child scan changed")
service_path.write_text(text.replace(old, new, 1))

tests_path = Path("crates/node/tests/devices.rs")
tests = tests_path.read_text()
old_import = '''use archon_kernel::{
    CapacityDimension, LeaseId, LeaseState, Need, NodeState, OwnerId, Request, RequestClass,
    RequestId, ResourceClass, qty,
};
'''
new_import = '''use archon_kernel::{
    Attrs, CapacityDimension, Command, Edge, EdgeKind, LeaseId, LeaseState, Need, Node, NodeId,
    NodeState, OwnerId, Quantity, Request, RequestClass, RequestId, ResourceClass, qty,
};
'''
if tests.count(old_import) != 1:
    raise RuntimeError("device test imports changed")
tests = tests.replace(old_import, new_import, 1)
marker = "fn nested_device_reconciliation_preserves_topology_parent()"
if marker in tests:
    raise RuntimeError("nested topology reconciliation test already present")
tests += r'''

#[test]
fn nested_device_reconciliation_preserves_topology_parent() {
    let mut service = NodeService::new();
    let machine = NodeId::from_u64(100);
    let numa = NodeId::from_u64(101);
    let gpu = NodeId::from_u64(102);

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
            ],
        })
        .expect("seed nested device topology");

    service
        .register_agent(
            description("inst-nested", "/dev/gpuB"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
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
        .register_agent(
            missing,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("nested device disappearance");
    assert_eq!(service.cluster.node_state(gpu), Some(NodeState::Unavailable));
    assert_eq!(service.cluster.graph.parent(gpu), Some(numa));

    service
        .register_agent(
            description("inst-nested", "/dev/gpuC"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("nested device reappearance");
    assert_eq!(service.cluster.node_state(gpu), Some(NodeState::Schedulable));
    assert_eq!(service.cluster.graph.parent(gpu), Some(numa));
    assert_eq!(
        service.cluster.graph.node(gpu).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuC".to_string())
    );
}
'''
tests_path.write_text(tests)
