from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


def patch_service() -> None:
    path = Path("crates/node/src/service.rs")
    text = path.read_text()
    text = replace_once(
        text,
        "Cluster, Command, Effect, Error, FactEdge,\n    FactWriterAssignment,",
        "Cluster, Command, Effect, Error,\n    FactWriterAssignment,",
        "unused FactEdge import",
    )
    path.write_text(text)


def patch_claimability() -> None:
    path = Path("crates/node/tests/claimability.rs")
    text = path.read_text()
    text = replace_once(
        text,
        "BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Command, Edge, EdgeKind,\n    Need, Node, NodeId, OwnerId, ProviderId, Request, RequestClass, RequestId, ResourceClass, qty,",
        "BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Command, Edge, EdgeKind,\n    FactWriterAssignment, FactWriterId, Need, Node, NodeId, OwnerId, ProviderFactBatch, ProviderId,\n    Request, RequestClass, RequestId, ResourceClass, qty,",
        "claimability writer imports",
    )

    old_apply = '''    service
        .cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![Node {
                id,
                kind,
                attrs: Default::default(),
                capacity: qty(CapacityDimension::Count, 1),
            }],
            edges: vec![Edge {
                from: machine,
                to: id,
                kind: EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: binding
                .map(|binding| {
                    vec![ClaimBindingUpdate {
                        node: id,
                        dimension: CapacityDimension::Count,
                        binding: Some(binding),
                    }]
                })
                .unwrap_or_default(),
        })
        .unwrap();'''
    new_apply = '''    service
        .cluster
        .apply(Command::ApplyProviderFacts {
            batches: vec![ProviderFactBatch {
                writer: FactWriterId::from_u64(100),
                nodes: vec![Node {
                    id,
                    kind,
                    attrs: Default::default(),
                    capacity: qty(CapacityDimension::Count, 1),
                }],
                edges: vec![Edge {
                    from: machine,
                    to: id,
                    kind: EdgeKind::Contains,
                    attrs: Default::default(),
                }],
            }],
            claim_bindings: binding
                .map(|binding| {
                    vec![ClaimBindingUpdate {
                        node: id,
                        dimension: CapacityDimension::Count,
                        binding: Some(binding),
                    }]
                })
                .unwrap_or_default(),
        })
        .unwrap();'''
    text = replace_once(text, old_apply, new_apply, "custom resource explicit writer")

    backfill_anchor = '''    assert_eq!(returned, machine);
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count),'''
    backfill_replacement = '''    assert_eq!(returned, machine);
    assert_eq!(
        service.cluster.graph.node_fact_writer(machine),
        Some(FactWriterId::from_u64(1))
    );
    assert_eq!(
        service.cluster.graph.node_fact_writer(cpu),
        Some(FactWriterId::from_u64(1))
    );
    assert_eq!(
        service.cluster.graph.node_fact_writer(memory),
        Some(FactWriterId::from_u64(1))
    );
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count),'''
    text = replace_once(text, backfill_anchor, backfill_replacement, "legacy host writer backfill assertions")

    device_anchor = '''    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(kept, CapacityDimension::Count),'''
    device_replacement = '''    assert_eq!(
        service.cluster.graph.node_fact_writer(kept),
        Some(FactWriterId::from_u64(2))
    );
    assert_eq!(service.cluster.graph.node_fact_writer(gone), None);
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(kept, CapacityDimension::Count),'''
    text = replace_once(text, device_anchor, device_replacement, "current-only device writer backfill assertions")

    text += r'''

#[test]
fn returning_host_writer_conflict_prevents_device_fact_mutation() {
    let mut service = NodeService::new();
    let mut legacy = legacy_description();
    legacy.devices = vec![legacy_device("gpu-1", "/dev/gpu-old")];
    let (_local, nodes, edges) = archon_node::discover::build_graph(&legacy, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let cpu = service.cluster.graph.nodes_of_class(ResourceClass::Cpu)[0];
    let gpu = device_node(&service, "gpu-1");
    service
        .cluster
        .apply(Command::AdoptFactWriters {
            assignments: vec![FactWriterAssignment {
                writer: FactWriterId::from_u64(77),
                nodes: vec![cpu],
                edges: Vec::new(),
            }],
        })
        .unwrap();

    let mut returning = legacy;
    returning.devices = vec![legacy_device("gpu-1", "/dev/gpu-new")];
    let error = service
        .register_agent(
            returning,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("discovery writer conflict must fail before device reconciliation");
    assert!(error.to_string().contains("discovery writer"));
    assert_eq!(
        service.cluster.graph.node_fact_writer(cpu),
        Some(FactWriterId::from_u64(77))
    );
    assert_eq!(
        service
            .cluster
            .graph
            .node(gpu)
            .and_then(|node| node.attrs.get("dev"))
            .map(String::as_str),
        Some("/dev/gpu-old")
    );
    assert_eq!(
        service.cluster.node_state(machine),
        Some(archon_kernel::NodeState::Unavailable)
    );
}
'''
    path.write_text(text)


patch_service()
patch_claimability()
