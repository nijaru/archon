use archon_kernel::{
    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Edge,
    EdgeKind, FactEdge, FactWriterAssignment, FactWriterId, Node, NodeId, ProviderFactBatch,
    ProviderId, Quantity, ResourceClass, qty,
};

fn node(id: u64, kind: ResourceClass, capacity: Quantity) -> Node {
    Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn edge(from: u64, to: u64) -> Edge {
    Edge {
        from: NodeId::from_u64(from),
        to: NodeId::from_u64(to),
        kind: EdgeKind::Contains,
        attrs: Default::default(),
    }
}

#[test]
fn provider_fragments_compose_without_conflating_binding_provider() {
    let host = FactWriterId::from_u64(10);
    let device = FactWriterId::from_u64(20);
    let gpu = NodeId::from_u64(2);
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyProviderFacts {
            batches: vec![
                ProviderFactBatch {
                    writer: host,
                    nodes: vec![node(1, ResourceClass::Machine, Quantity::new())],
                    edges: vec![],
                },
                ProviderFactBatch {
                    writer: device,
                    nodes: vec![node(
                        2,
                        ResourceClass::Gpu,
                        qty(CapacityDimension::Count, 1),
                    )],
                    edges: vec![edge(1, 2)],
                },
            ],
            claim_bindings: vec![ClaimBindingUpdate {
                node: gpu,
                dimension: CapacityDimension::Count,
                binding: Some(ClaimBinding {
                    provider: ProviderId::from_u64(99),
                    scope: BindingScope::Exclusive,
                }),
            }],
        })
        .unwrap();

    assert_eq!(
        cluster.graph.node_fact_writer(NodeId::from_u64(1)),
        Some(host)
    );
    assert_eq!(cluster.graph.node_fact_writer(gpu), Some(device));
    assert_eq!(
        cluster
            .graph
            .edge_fact_writer(FactEdge::new(NodeId::from_u64(1), gpu, EdgeKind::Contains)),
        Some(device)
    );
    assert_eq!(
        cluster
            .graph
            .claim_binding(gpu, CapacityDimension::Count)
            .unwrap()
            .provider,
        ProviderId::from_u64(99)
    );
}

#[test]
fn another_writer_cannot_overwrite_an_owned_node() {
    let first = FactWriterId::from_u64(1);
    let other = FactWriterId::from_u64(2);
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyProviderFacts {
            batches: vec![ProviderFactBatch {
                writer: first,
                nodes: vec![node(1, ResourceClass::Machine, Quantity::new())],
                edges: vec![],
            }],
            claim_bindings: vec![],
        })
        .unwrap();
    let revision = cluster.graph.revision;
    let mut changed = node(1, ResourceClass::Machine, Quantity::new());
    changed.attrs.insert("writer".into(), "other".into());
    let err = cluster
        .apply(Command::ApplyProviderFacts {
            batches: vec![ProviderFactBatch {
                writer: other,
                nodes: vec![changed],
                edges: vec![],
            }],
            claim_bindings: vec![],
        })
        .unwrap_err();
    assert!(err.to_string().contains("cannot be overwritten"));
    assert_eq!(cluster.graph.revision, revision);
    assert!(
        cluster
            .graph
            .node(NodeId::from_u64(1))
            .unwrap()
            .attrs
            .is_empty()
    );
}

#[test]
fn owner_can_refresh_its_complete_node_facts() {
    let writer = FactWriterId::from_u64(7);
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyProviderFacts {
            batches: vec![ProviderFactBatch {
                writer,
                nodes: vec![node(
                    1,
                    ResourceClass::Gpu,
                    qty(CapacityDimension::Count, 1),
                )],
                edges: vec![],
            }],
            claim_bindings: vec![],
        })
        .unwrap();
    let revision = cluster.graph.revision;
    let mut changed = node(1, ResourceClass::Gpu, qty(CapacityDimension::Count, 1));
    changed.attrs.insert("path".into(), "/dev/new".into());
    cluster
        .apply(Command::ApplyProviderFacts {
            batches: vec![ProviderFactBatch {
                writer,
                nodes: vec![changed],
                edges: vec![],
            }],
            claim_bindings: vec![],
        })
        .unwrap();
    assert_eq!(cluster.graph.revision, revision + 1);
    assert_eq!(
        cluster
            .graph
            .node(NodeId::from_u64(1))
            .unwrap()
            .attrs
            .get("path")
            .map(String::as_str),
        Some("/dev/new")
    );
}

#[test]
fn provider_can_attach_owned_edge_to_another_writers_node() {
    let host = FactWriterId::from_u64(1);
    let device = FactWriterId::from_u64(2);
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyProviderFacts {
            batches: vec![ProviderFactBatch {
                writer: host,
                nodes: vec![node(1, ResourceClass::Machine, Quantity::new())],
                edges: vec![],
            }],
            claim_bindings: vec![],
        })
        .unwrap();
    cluster
        .apply(Command::ApplyProviderFacts {
            batches: vec![ProviderFactBatch {
                writer: device,
                nodes: vec![node(
                    2,
                    ResourceClass::Gpu,
                    qty(CapacityDimension::Count, 1),
                )],
                edges: vec![edge(1, 2)],
            }],
            claim_bindings: vec![],
        })
        .unwrap();
    assert_eq!(
        cluster.graph.node_fact_writer(NodeId::from_u64(1)),
        Some(host)
    );
    assert_eq!(
        cluster.graph.edge_fact_writer(FactEdge::new(
            NodeId::from_u64(1),
            NodeId::from_u64(2),
            EdgeKind::Contains
        )),
        Some(device)
    );
}

#[test]
fn legacy_facts_can_be_adopted_without_revising_or_mutating_them() {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![edge(1, 2)],
        })
        .unwrap();
    let revision = cluster.graph.revision;
    let writer = FactWriterId::from_u64(5);
    cluster
        .apply(Command::AdoptFactWriters {
            assignments: vec![FactWriterAssignment {
                writer,
                nodes: vec![NodeId::from_u64(2)],
                edges: vec![FactEdge::new(
                    NodeId::from_u64(1),
                    NodeId::from_u64(2),
                    EdgeKind::Contains,
                )],
            }],
        })
        .unwrap();
    assert_eq!(cluster.graph.revision, revision);
    assert_eq!(
        cluster.graph.node_fact_writer(NodeId::from_u64(2)),
        Some(writer)
    );
    let digest = cluster.digest();
    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(digest, replayed.digest());
}

#[test]
fn unowned_legacy_mutation_is_disabled_after_adoption() {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(1, ResourceClass::Machine, Quantity::new())],
            edges: vec![],
        })
        .unwrap();
    cluster
        .apply(Command::AdoptFactWriters {
            assignments: vec![FactWriterAssignment {
                writer: FactWriterId::from_u64(1),
                nodes: vec![NodeId::from_u64(1)],
                edges: vec![],
            }],
        })
        .unwrap();
    let err = cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(2, ResourceClass::Machine, Quantity::new())],
            edges: vec![],
        })
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("legacy unowned Graph updates are disabled")
    );
}
