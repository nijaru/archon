use std::collections::{BTreeMap, BTreeSet};

use archon_kernel::{
    AdmissionPolicy, Allocation, BindingScope, CapacityDimension, Claim, ClaimBinding,
    ClaimBindingUpdate, Cluster, Command, Edge, EdgeKind, Lease, LeaseId, LeaseState, Need, Node,
    NodeId, OwnerId, ProviderId, Quantity, Queued, Request, RequestClass, RequestId, ResourceClass,
    qty,
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
    let mut nodes = vec![node(1, ResourceClass::Machine, Quantity::new())];
    let mut edges = Vec::new();
    let mut bindings = Vec::new();
    for id in 2..=5 {
        nodes.push(node(
            id,
            ResourceClass::Cpu,
            qty(CapacityDimension::Count, 1),
        ));
        edges.push(Edge {
            from: NodeId::from_u64(1),
            to: NodeId::from_u64(id),
            kind: EdgeKind::Contains,
            attrs: Default::default(),
        });
        bindings.push(ClaimBindingUpdate {
            node: NodeId::from_u64(id),
            dimension: CapacityDimension::Count,
            binding: Some(ClaimBinding {
                provider: ProviderId::ENFORCE,
                scope: BindingScope::Exclusive,
            }),
        });
    }
    nodes.push(node(
        6,
        ResourceClass::Gpu,
        qty(CapacityDimension::Count, 1),
    ));
    edges.push(Edge {
        from: NodeId::from_u64(1),
        to: NodeId::from_u64(6),
        kind: EdgeKind::Contains,
        attrs: Default::default(),
    });
    bindings.push(ClaimBindingUpdate {
        node: NodeId::from_u64(6),
        dimension: CapacityDimension::Count,
        binding: Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        }),
    });
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes,
            edges,
            claim_bindings: bindings,
        })
        .unwrap();
    cluster
}

fn request(id: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 10,
        priority,
        keep_alive: false,
        machine_local: true,
        grace_secs: 0,
    }
}

fn active_lease(id: u64, owner: u64, node: u64, kind: ResourceClass) -> Lease {
    let dimension = match kind {
        ResourceClass::Memory => CapacityDimension::Bytes,
        _ => CapacityDimension::Count,
    };
    Lease {
        id: LeaseId::from_u64(id),
        owner: OwnerId::from_u64(owner),
        allocation: Allocation {
            claims: vec![Claim {
                node: NodeId::from_u64(node),
                quantity: qty(dimension, 1),
            }],
            graph_revision: 1,
            explanation: "fixture".into(),
        },
        parent: None,
        expires_at: 100,
        prepare_deadline: 10,
        priority: 1,
        state: LeaseState::Active,
        exit_code: None,
    }
}

fn queue(owner1_priority: u32, owner2_priority: u32) -> Vec<Queued> {
    vec![
        Queued {
            request: request(10, owner1_priority),
            owner: OwnerId::from_u64(1),
            submitted_at: 1,
        },
        Queued {
            request: request(11, owner2_priority),
            owner: OwnerId::from_u64(2),
            submitted_at: 2,
        },
    ]
}

#[test]
fn weighted_fair_share_prefers_lower_dominant_share_within_priority() {
    let cluster = cluster();
    let leases = BTreeMap::from([
        (
            LeaseId::from_u64(1),
            active_lease(1, 1, 2, ResourceClass::Cpu),
        ),
        (
            LeaseId::from_u64(2),
            active_lease(2, 2, 3, ResourceClass::Cpu),
        ),
    ]);
    let occupancy =
        archon_kernel::occupancy_from_leases(leases.values(), &BTreeSet::new(), &BTreeSet::new());
    let policy = AdmissionPolicy {
        fair_share: true,
        owner_weights: BTreeMap::from([(OwnerId::from_u64(1), 1), (OwnerId::from_u64(2), 2)]),
        ..AdmissionPolicy::default()
    };
    let admission = archon_kernel::admit_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue(5, 5),
        &leases,
        &BTreeSet::new(),
    )
    .expect("one CPU remains available");
    assert_eq!(admission.owner, OwnerId::from_u64(2));
}

#[test]
fn explicit_priority_still_precedes_fair_share() {
    let cluster = cluster();
    let leases = BTreeMap::from([
        (
            LeaseId::from_u64(1),
            active_lease(1, 1, 2, ResourceClass::Cpu),
        ),
        (
            LeaseId::from_u64(2),
            active_lease(2, 2, 3, ResourceClass::Cpu),
        ),
    ]);
    let occupancy =
        archon_kernel::occupancy_from_leases(leases.values(), &BTreeSet::new(), &BTreeSet::new());
    let policy = AdmissionPolicy {
        fair_share: true,
        owner_weights: BTreeMap::from([(OwnerId::from_u64(1), 1), (OwnerId::from_u64(2), 100)]),
        ..AdmissionPolicy::default()
    };
    let admission = archon_kernel::admit_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue(10, 1),
        &leases,
        &BTreeSet::new(),
    )
    .expect("one CPU remains available");
    assert_eq!(admission.owner, OwnerId::from_u64(1));
}

#[test]
fn dominant_resource_share_compares_heterogeneous_usage() {
    let cluster = cluster();
    let leases = BTreeMap::from([
        (
            LeaseId::from_u64(1),
            active_lease(1, 1, 6, ResourceClass::Gpu),
        ),
        (
            LeaseId::from_u64(2),
            active_lease(2, 2, 2, ResourceClass::Cpu),
        ),
    ]);
    let occupancy =
        archon_kernel::occupancy_from_leases(leases.values(), &BTreeSet::new(), &BTreeSet::new());
    let policy = AdmissionPolicy {
        fair_share: true,
        ..AdmissionPolicy::default()
    };
    let admission = archon_kernel::admit_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue(5, 5),
        &leases,
        &BTreeSet::new(),
    )
    .expect("CPU capacity remains");
    assert_eq!(admission.owner, OwnerId::from_u64(2));
}

#[test]
fn disabled_fair_share_preserves_legacy_submit_order() {
    let cluster = cluster();
    let leases = BTreeMap::from([
        (
            LeaseId::from_u64(1),
            active_lease(1, 1, 6, ResourceClass::Gpu),
        ),
        (
            LeaseId::from_u64(2),
            active_lease(2, 2, 2, ResourceClass::Cpu),
        ),
    ]);
    let occupancy =
        archon_kernel::occupancy_from_leases(leases.values(), &BTreeSet::new(), &BTreeSet::new());
    let policy = AdmissionPolicy::default();
    let admission = archon_kernel::admit_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue(5, 5),
        &leases,
        &BTreeSet::new(),
    )
    .expect("CPU capacity remains");
    assert_eq!(admission.owner, OwnerId::from_u64(1));
}

#[test]
fn terminal_lease_with_open_binding_stays_charged_to_owner() {
    let cluster = cluster();
    let lease_id = LeaseId::from_u64(1);
    let mut lease = active_lease(1, 1, 2, ResourceClass::Cpu);
    lease.state = LeaseState::Failed;
    let leases = BTreeMap::from([(lease_id, lease)]);

    let charged = archon_kernel::owner_usage(&cluster.graph, &leases, &BTreeSet::from([lease_id]));
    assert_eq!(
        charged[&OwnerId::from_u64(1)][&ResourceClass::Cpu][&CapacityDimension::Count],
        1
    );

    let fenced = archon_kernel::owner_usage(&cluster.graph, &leases, &BTreeSet::new());
    assert!(!fenced.contains_key(&OwnerId::from_u64(1)));
}

#[test]
fn fair_share_ordering_composes_with_backfill_shadow_safety() {
    let cluster = cluster();
    let running_id = LeaseId::from_u64(1);
    let running = active_lease(1, 1, 2, ResourceClass::Cpu);
    let leases = BTreeMap::from([(running_id, running)]);
    let open_bindings = BTreeSet::new();
    let occupancy =
        archon_kernel::occupancy_from_leases(leases.values(), &open_bindings, &BTreeSet::new());

    let mut head = request(20, 10);
    head.needs[0].quantity = qty(CapacityDimension::Count, 4);
    let queue = vec![
        Queued {
            request: head,
            owner: OwnerId::from_u64(3),
            submitted_at: 1,
        },
        Queued {
            request: request(21, 5),
            owner: OwnerId::from_u64(1),
            submitted_at: 2,
        },
        Queued {
            request: request(22, 5),
            owner: OwnerId::from_u64(2),
            submitted_at: 3,
        },
    ];
    let policy = AdmissionPolicy {
        fair_share: true,
        ..AdmissionPolicy::default()
    };
    let admission = archon_kernel::admit_backfill_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &Default::default(),
        &policy,
        &queue,
        &archon_kernel::BackfillCtx {
            now: 0,
            leases: &leases,
            open_bindings: &open_bindings,
        },
    )
    .expect("short lower-share work can backfill before the protected head");
    assert_eq!(admission.owner, OwnerId::from_u64(2));
    assert_eq!(admission.request.id, RequestId::from_u64(22));
}
