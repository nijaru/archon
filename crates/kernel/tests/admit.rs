use archon_kernel::{
    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Need,
    Node, NodeId, OwnerId, ProviderId, Quantity, Queued, Request, RequestClass, RequestId,
    ResourceClass, admit, qty,
};
use std::collections::{BTreeMap, BTreeSet};

fn node(id: u64, kind: ResourceClass, capacity: Quantity) -> Node {
    Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn make_cpu_claimable(cluster: &mut Cluster, nodes: &[u64]) {
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: Vec::new(),
            edges: Vec::new(),
            claim_bindings: nodes
                .iter()
                .map(|id| ClaimBindingUpdate {
                    node: NodeId::from_u64(*id),
                    dimension: CapacityDimension::Count,
                    binding: Some(ClaimBinding {
                        provider: ProviderId::ENFORCE,
                        scope: BindingScope::Exclusive,
                    }),
                })
                .collect(),
        })
        .unwrap();
}

fn cluster() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(3, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(1),
                to: NodeId::from_u64(2),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(1),
                to: NodeId::from_u64(3),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap();
    make_cpu_claimable(&mut cluster, &[2, 3]);
    cluster
}

fn cpu_request(id: u64, count: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, count),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        machine_local: true,
        grace_secs: 0,
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 10,
        keep_alive: false,
        priority,
    }
}

#[test]
fn higher_priority_wins() {
    let cluster = cluster();
    let admission = admit(
        &cluster.graph,
        &cluster.occupancy(),
        &[
            Queued {
                request: cpu_request(1, 1, 1),
                owner: OwnerId::from_u64(1),
                submitted_at: 1,
            },
            Queued {
                request: cpu_request(2, 1, 10),
                owner: OwnerId::from_u64(2),
                submitted_at: 2,
            },
        ],
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(admission.request.id, RequestId::from_u64(2));
}

#[test]
fn earlier_submit_breaks_priority_ties() {
    let cluster = cluster();
    let admission = admit(
        &cluster.graph,
        &cluster.occupancy(),
        &[
            Queued {
                request: cpu_request(2, 1, 5),
                owner: OwnerId::from_u64(2),
                submitted_at: 8,
            },
            Queued {
                request: cpu_request(1, 1, 5),
                owner: OwnerId::from_u64(1),
                submitted_at: 3,
            },
        ],
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(admission.request.id, RequestId::from_u64(1));
}

#[test]
fn skips_infeasible_head() {
    let cluster = cluster();
    let admission = admit(
        &cluster.graph,
        &cluster.occupancy(),
        &[
            Queued {
                request: cpu_request(1, 8, 100),
                owner: OwnerId::from_u64(1),
                submitted_at: 1,
            },
            Queued {
                request: cpu_request(2, 1, 1),
                owner: OwnerId::from_u64(2),
                submitted_at: 2,
            },
        ],
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(admission.request.id, RequestId::from_u64(2));
}

fn two_machine_cluster() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(10, ResourceClass::Machine, Quantity::new()),
                node(11, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(12, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(20, ResourceClass::Machine, Quantity::new()),
                node(21, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                archon_kernel::Edge {
                    from: NodeId::from_u64(10),
                    to: NodeId::from_u64(11),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                archon_kernel::Edge {
                    from: NodeId::from_u64(10),
                    to: NodeId::from_u64(12),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                archon_kernel::Edge {
                    from: NodeId::from_u64(20),
                    to: NodeId::from_u64(21),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
            ],
        })
        .unwrap();
    make_cpu_claimable(&mut cluster, &[11, 12, 21]);
    cluster
}

#[test]
fn hard_request_exclusion_selects_a_compatible_machine() {
    let cluster = two_machine_cluster();
    let request = cpu_request(10, 1, 1);
    let queue = vec![Queued {
        request: request.clone(),
        owner: OwnerId::from_u64(1),
        submitted_at: 1,
    }];
    let exclusions = BTreeMap::from([(request.id, BTreeSet::from([NodeId::from_u64(10)]))]);
    let admission = cluster
        .admit_backfill_with_exclusions(&queue, &Default::default(), &exclusions)
        .expect("second machine remains compatible");
    let machine = cluster
        .graph
        .machine_of(admission.allocation.claims[0].node)
        .unwrap();
    assert_eq!(machine, NodeId::from_u64(20));
}

#[test]
fn hard_excluded_head_does_not_create_backfill_debt() {
    let cluster = two_machine_cluster();
    let head = cpu_request(10, 2, 10);
    let later = cpu_request(11, 1, 1);
    let queue = vec![
        Queued {
            request: head.clone(),
            owner: OwnerId::from_u64(1),
            submitted_at: 1,
        },
        Queued {
            request: later.clone(),
            owner: OwnerId::from_u64(2),
            submitted_at: 2,
        },
    ];
    let exclusions = BTreeMap::from([(head.id, BTreeSet::from([NodeId::from_u64(10)]))]);
    let admission = cluster
        .admit_backfill_with_exclusions(&queue, &Default::default(), &exclusions)
        .expect("permanently incompatible head must not block compatible work");
    assert_eq!(admission.request.id, later.id);
}
