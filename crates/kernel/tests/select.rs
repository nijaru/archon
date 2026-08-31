use archon_kernel::{
    Allocation, BindingScope, CapacityDimension, Claim, ClaimBinding, ClaimBindingUpdate, Cluster,
    Command, Edge, EdgeKind, LeaseId, Need, Node, NodeId, OwnerId, ProviderId, Quantity, Request,
    RequestClass, RequestId, ResourceClass, TopologyConstraint, TopologyRelation, qty,
};

fn node(id: u64, kind: ResourceClass, capacity: Quantity) -> Node {
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

fn claim_binding(node: u64, dimension: CapacityDimension) -> ClaimBindingUpdate {
    ClaimBindingUpdate {
        node: NodeId::from_u64(node),
        dimension,
        binding: Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        }),
    }
}

fn graph() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Numa, Quantity::new()),
                node(3, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(4, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(5, ResourceClass::Memory, qty(CapacityDimension::Bytes, 8)),
                node(6, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                contain(1, 2),
                contain(2, 3),
                contain(2, 4),
                contain(2, 5),
                contain(2, 6),
            ],
            claim_bindings: vec![
                claim_binding(3, CapacityDimension::Count),
                claim_binding(4, CapacityDimension::Count),
                claim_binding(5, CapacityDimension::Bytes),
                claim_binding(6, CapacityDimension::Count),
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
        machine_local: true,
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
    assert_eq!(cluster.graph.nodes_of_class(ResourceClass::Cpu).len(), 2);
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
                    kind: ResourceClass::Cpu,
                    quantity: qty(CapacityDimension::Count, 2),
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
            quantity: qty(CapacityDimension::Count, 1),
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
                    quantity: qty(CapacityDimension::Count, 1),
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
            scope: archon_kernel::BindingScope::Exclusive,
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
                    quantity: qty(CapacityDimension::Count, 1),
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

#[test]
fn machine_local_multi_need_stays_on_one_machine() {
    // Two machines, asymmetric: machine 1 has 1 CPU + 2 GiB, machine 2 has
    // 2 CPUs + 1 GiB. A request wanting 1 CPU + 2 GiB fits only machine 1;
    // the old per-need selection could split CPU and memory across hosts.
    let mut cluster = Cluster::new();
    let cpu1 = NodeId::from_u64(10);
    let mem1 = NodeId::from_u64(11);
    let m1 = NodeId::from_u64(1);
    let cpu2 = NodeId::from_u64(20);
    let mem2 = NodeId::from_u64(21);
    let m2 = NodeId::from_u64(2);
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(10, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(
                    11,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 2 << 30),
                ),
                node(2, ResourceClass::Machine, Quantity::new()),
                node(20, ResourceClass::Cpu, qty(CapacityDimension::Count, 2)),
                node(
                    21,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 1 << 30),
                ),
            ],
            edges: vec![
                edge(m1, cpu1),
                edge(m1, mem1),
                edge(m2, cpu2),
                edge(m2, mem2),
            ],
            claim_bindings: vec![
                claim_binding(10, CapacityDimension::Count),
                claim_binding(11, CapacityDimension::Bytes),
                claim_binding(20, CapacityDimension::Count),
                claim_binding(21, CapacityDimension::Bytes),
            ],
        })
        .unwrap();
    cluster.set_now(1);

    let request = Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: ResourceClass::Memory,
                quantity: qty(CapacityDimension::Bytes, (2 << 30) - 1),
                filters: vec![],
            },
        ],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        lifetime: 100,
        priority: 1,
        machine_local: true,
    };
    let allocation = archon_kernel::select(
        &cluster.graph,
        &cluster.occupancy(),
        &request,
        &Default::default(),
    )
    .expect("must place on the one machine that fits");
    let machines: std::collections::BTreeSet<NodeId> = allocation
        .claims
        .iter()
        .filter_map(|claim| cluster.graph.machine_of(claim.node))
        .collect();
    assert_eq!(
        machines.len(),
        1,
        "machine_local claims must share one machine, got {machines:?}"
    );
}

fn edge(from: NodeId, to: NodeId) -> archon_kernel::Edge {
    archon_kernel::Edge {
        from,
        to,
        kind: archon_kernel::EdgeKind::Contains,
        attrs: Default::default(),
    }
}

#[test]
fn explanation_reports_topology_that_changes_machine_choice() {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(100, ResourceClass::Machine, Quantity::new()),
                node(101, ResourceClass::Numa, Quantity::new()),
                node(102, ResourceClass::Numa, Quantity::new()),
                node(103, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(104, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
                node(200, ResourceClass::Machine, Quantity::new()),
                node(201, ResourceClass::Numa, Quantity::new()),
                node(202, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(203, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                contain(100, 101),
                contain(100, 102),
                contain(101, 103),
                contain(102, 104),
                contain(200, 201),
                contain(201, 202),
                contain(201, 203),
            ],
            claim_bindings: vec![
                claim_binding(103, CapacityDimension::Count),
                claim_binding(104, CapacityDimension::Count),
                claim_binding(202, CapacityDimension::Count),
                claim_binding(203, CapacityDimension::Count),
            ],
        })
        .unwrap();
    let mut request = request(
        RequestClass::Batch,
        vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: ResourceClass::Gpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
        ],
    );
    request.topology.push(TopologyConstraint {
        left: 0,
        right: 1,
        relation: TopologyRelation::SameAncestor {
            class: ResourceClass::Numa,
        },
    });

    let allocation = cluster
        .allocate(&request)
        .expect("second machine satisfies NUMA locality");
    assert!(
        allocation
            .claims
            .iter()
            .all(|claim| { cluster.graph.machine_of(claim.node) == Some(NodeId::from_u64(200)) })
    );
    assert!(
        allocation
            .explanation
            .contains("topology[0] same-ancestor(numa) constrained placement")
    );
    assert!(allocation.explanation.contains("rejected"));
    assert!(allocation.explanation.contains("under numa"));
    assert!(allocation.explanation.reasons.iter().any(|reason| {
        matches!(
            reason,
            archon_kernel::PlacementReason::MachineSelected { machine }
                if *machine == NodeId::from_u64(200)
        )
    }));
    assert!(allocation.explanation.reasons.iter().any(|reason| {
        matches!(
            reason,
            archon_kernel::PlacementReason::TopologyConstraint {
                index: 0,
                relation: TopologyRelation::SameAncestor { class },
                selected_left,
                selected_right,
                rejected: Some((left, right)),
            } if *class == ResourceClass::Numa
                && selected_left == &vec![NodeId::from_u64(202)]
                && selected_right == &vec![NodeId::from_u64(203)]
                && *left == NodeId::from_u64(103)
                && *right == NodeId::from_u64(104)
        )
    }));
}

#[test]
fn explanation_omits_topology_that_did_not_change_candidate_choice() {
    let cluster = graph();
    let mut request = request(
        RequestClass::Batch,
        vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: ResourceClass::Gpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
        ],
    );
    request.topology.push(TopologyConstraint {
        left: 0,
        right: 1,
        relation: TopologyRelation::SameAncestor {
            class: ResourceClass::Numa,
        },
    });

    let allocation = cluster
        .allocate(&request)
        .expect("first choice already satisfies topology");
    assert!(!allocation.explanation.contains("topology[0]"));
}
