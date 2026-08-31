use archon_kernel::{
    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Edge,
    EdgeKind, Filter, Need, Node, NodeId, ProviderId, Quantity, Request, RequestClass, RequestId,
    ResourceClass, qty,
};

fn node(id: u64, kind: ResourceClass, capacity: Quantity) -> Node {
    Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn cluster() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(3, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(2),
                    kind: EdgeKind::Contains,
                    attrs: Default::default(),
                },
                Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(3),
                    kind: EdgeKind::Contains,
                    attrs: Default::default(),
                },
            ],
            claim_bindings: [2, 3]
                .into_iter()
                .map(|node| ClaimBindingUpdate {
                    node: NodeId::from_u64(node),
                    dimension: CapacityDimension::Count,
                    binding: Some(ClaimBinding {
                        provider: ProviderId::ENFORCE,
                        scope: BindingScope::Exclusive,
                    }),
                })
                .collect(),
        })
        .unwrap();
    cluster
}

fn request(filters: Vec<Filter>) -> Request {
    Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters,
        }],
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        lifetime: 60,
        priority: 1,
        machine_local: true,
    }
}

#[test]
fn health_is_non_revisioned_observation_not_a_hard_fact() {
    let mut cluster = cluster();
    let cpu = NodeId::from_u64(2);
    let revision = cluster.graph.revision;
    cluster
        .apply(Command::SetNodeHealth {
            node: cpu,
            health: "degraded".into(),
        })
        .unwrap();

    assert_eq!(cluster.graph.revision, revision);
    assert_eq!(cluster.graph.observation(cpu, "health"), Some("degraded"));
    assert!(
        !cluster
            .graph
            .node(cpu)
            .unwrap()
            .attrs
            .contains_key("health")
    );

    let error = cluster
        .allocate(&request(vec![Filter {
            key: "health".into(),
            value: "degraded".into(),
        }]))
        .expect_err("observations must not satisfy hard resource filters");
    assert!(matches!(error, archon_kernel::Error::Refused { .. }));
}

#[test]
fn degraded_observation_still_changes_scoring() {
    let mut cluster = cluster();
    cluster
        .apply(Command::SetNodeHealth {
            node: NodeId::from_u64(2),
            health: "degraded".into(),
        })
        .unwrap();
    let allocation = cluster.allocate(&request(Vec::new())).unwrap();
    assert_eq!(allocation.claims[0].node, NodeId::from_u64(3));
}

#[test]
fn provider_facts_cannot_publish_reserved_health_observation() {
    let mut cluster = Cluster::new();
    let mut machine = node(1, ResourceClass::Machine, Quantity::new());
    machine.attrs.insert("health".into(), "degraded".into());
    let error = cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![machine],
            edges: Vec::new(),
            claim_bindings: Vec::new(),
        })
        .expect_err("health is observation state, not a provider-authored fact");
    assert!(
        error
            .to_string()
            .contains("reserved observation key health")
    );
    assert_eq!(cluster.graph.revision, 0);
}

#[test]
fn observation_replay_is_deterministic_without_changing_revision() {
    let mut cluster = cluster();
    let revision = cluster.graph.revision;
    cluster
        .apply(Command::SetNodeHealth {
            node: NodeId::from_u64(1),
            health: "degraded".into(),
        })
        .unwrap();
    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(cluster.digest(), replayed.digest());
    assert_eq!(replayed.graph.revision, revision);
}
