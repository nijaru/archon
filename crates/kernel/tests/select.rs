use archon_kernel::{
    Allocation, Claim, Cluster, Command, Dimension, Edge, EdgeKind, LeaseId, Need, Node, NodeId,
    NodeKind, OwnerId, Quantity, Request, RequestClass, RequestId, qty,
};

fn node(id: u64, kind: NodeKind, capacity: Quantity) -> Node {
    Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn contain(from: u64, to: u64) -> Edge {
    Edge {
        from: NodeId::from_u64(from),
        to: NodeId::from_u64(to),
        kind: EdgeKind::Contains,
        attrs: Default::default(),
    }
}

fn graph() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, NodeKind::Machine, Quantity::new()),
                node(2, NodeKind::Numa, Quantity::new()),
                node(3, NodeKind::Cpu, qty(Dimension::Count, 1)),
                node(4, NodeKind::Cpu, qty(Dimension::Count, 1)),
                node(5, NodeKind::Memory, qty(Dimension::Bytes, 8)),
                node(6, NodeKind::Gpu, qty(Dimension::Count, 1)),
            ],
            edges: vec![
                contain(1, 2),
                contain(2, 3),
                contain(2, 4),
                contain(2, 5),
                contain(2, 6),
            ],
        })
        .unwrap();
    cluster
}

fn request(class: RequestClass, needs: Vec<Need>) -> Request {
    Request {
        id: RequestId::from_u64(1),
        class,
        needs,
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        machine_local: true,
        image: None,
        lifetime: 10,
        priority: 1,
    }
}

#[test]
fn replay_rebuilds_graph() {
    let cluster = graph();
    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(
        cluster.digest().graph_revision,
        replayed.digest().graph_revision
    );
    assert_eq!(cluster.graph.nodes_of_kind(NodeKind::Cpu).len(), 2);
}

#[test]
fn selects_service_and_batch() {
    let cluster = graph();
    for class in [
        RequestClass::Service,
        RequestClass::Batch,
        RequestClass::Batch,
    ] {
        let allocation = cluster
            .allocate(&request(
                class,
                vec![Need {
                    kind: NodeKind::Cpu,
                    quantity: qty(Dimension::Count, 2),
                    filters: vec![],
                }],
            ))
            .unwrap();
        assert_eq!(allocation.claims.len(), 2);
        assert!(allocation.explanation.contains("Pack"));
    }
}

#[test]
fn exclusive_claims_do_not_overlap() {
    let mut cluster = graph();
    let allocation = Allocation {
        claims: vec![Claim {
            node: NodeId::from_u64(3),
            quantity: qty(Dimension::Count, 1),
        }],
        graph_revision: cluster.graph.revision,
        explanation: "fixed".into(),
    };
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation: allocation.clone(),
            parent: None,
            expires_at: 10,
            prepare_deadline: 5,
            priority: 1,
        })
        .unwrap();
    let err = cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(2),
            owner: OwnerId::from_u64(2),
            allocation,
            parent: None,
            expires_at: 10,
            prepare_deadline: 5,
            priority: 1,
        })
        .unwrap_err();
    assert!(matches!(err, archon_kernel::Error::Overlap { .. }));
}

#[test]
fn child_cannot_escape_parent() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation: Allocation {
                claims: vec![Claim {
                    node: NodeId::from_u64(3),
                    quantity: qty(Dimension::Count, 1),
                }],
                graph_revision: cluster.graph.revision,
                explanation: "parent".into(),
            },
            parent: None,
            expires_at: 10,
            prepare_deadline: 5,
            priority: 1,
        })
        .unwrap();
    // A root lease over an enforced claim needs a prepared Binding before it
    // can activate.
    cluster
        .apply(Command::OpenBinding {
            binding: archon_kernel::BindingId::from_u64(1),
            lease: LeaseId::from_u64(1),
            node: NodeId::from_u64(3),
            provider: archon_kernel::ProviderId::ENFORCE,
        })
        .unwrap();
    cluster
        .apply(Command::RecordBindingPrepared {
            binding: archon_kernel::BindingId::from_u64(1),
            session: 1,
            provider_handle: 1,
            fence: 1,
        })
        .unwrap();
    cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap();
    let err = cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(2),
            owner: OwnerId::from_u64(2),
            allocation: Allocation {
                claims: vec![Claim {
                    node: NodeId::from_u64(6),
                    quantity: qty(Dimension::Count, 1),
                }],
                graph_revision: cluster.graph.revision,
                explanation: "child".into(),
            },
            parent: Some(LeaseId::from_u64(1)),
            priority: 1,
            expires_at: 10,
            prepare_deadline: 5,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        archon_kernel::Error::ChildEscapesParent { .. }
    ));
}
