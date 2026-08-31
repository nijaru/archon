use std::collections::{BTreeMap, BTreeSet};

use archon_kernel::{
    AdmissionOutcome, AdmissionPolicy, Allocation, BackfillCtx, BindingScope, CapacityDimension,
    Claim, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Edge, EdgeKind, Lease, LeaseId,
    LeaseState, Need, Node, NodeId, OwnerId, ProviderId, Quantity, Queued, Request, RequestClass,
    RequestId, ResourceClass, admit_backfill_with_policy_explained, admit_with_policy_explained,
    occupancy_from_leases, qty,
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
                .map(|id| ClaimBindingUpdate {
                    node: NodeId::from_u64(id),
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

fn request(id: u64, count: u64, priority: u32, lifetime: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, count),
            filters: Vec::new(),
        }],
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        command: Vec::new(),
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime,
        priority,
        keep_alive: false,
        machine_local: true,
        grace_secs: 0,
    }
}

fn active_lease(id: u64, owner: u64, node: u64, priority: u32, expires_at: u64) -> Lease {
    Lease {
        id: LeaseId::from_u64(id),
        owner: OwnerId::from_u64(owner),
        allocation: Allocation {
            claims: vec![Claim {
                node: NodeId::from_u64(node),
                quantity: qty(CapacityDimension::Count, 1),
            }],
            graph_revision: 1,
            explanation: "fixture".into(),
        },
        parent: None,
        expires_at,
        prepare_deadline: 10,
        priority,
        state: LeaseState::Active,
        exit_code: None,
    }
}

#[test]
fn trace_exposes_fair_share_ordering_inputs() {
    let cluster = cluster();
    let leases = BTreeMap::from([(LeaseId::from_u64(1), active_lease(1, 1, 2, 1, 100))]);
    let occupancy = occupancy_from_leases(leases.values(), &BTreeSet::new(), &BTreeSet::new());
    let queue = vec![
        Queued {
            request: request(10, 1, 5, 10),
            owner: OwnerId::from_u64(1),
            submitted_at: 1,
        },
        Queued {
            request: request(11, 1, 5, 10),
            owner: OwnerId::from_u64(2),
            submitted_at: 2,
        },
    ];
    let policy = AdmissionPolicy {
        fair_share: true,
        ..AdmissionPolicy::default()
    };

    let (admission, trace) = admit_with_policy_explained(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue,
        &leases,
        &BTreeSet::new(),
    );

    assert_eq!(admission.unwrap().request.id, RequestId::from_u64(11));
    assert_eq!(trace.candidates.len(), 2);
    assert_eq!(trace.candidates[0].rank, 0);
    assert_eq!(trace.candidates[0].request, RequestId::from_u64(11));
    assert_eq!(trace.candidates[0].owner_weight, Some(1));
    assert_eq!(trace.candidates[0].weighted_dominant_share, Some(0));
    assert_eq!(
        trace.candidates[0].outcome,
        AdmissionOutcome::Selected { backfilled: false }
    );
    assert_eq!(trace.candidates[1].request, RequestId::from_u64(10));
    assert!(
        trace.candidates[1]
            .weighted_dominant_share
            .is_some_and(|share| share > 0)
    );
    assert_eq!(trace.candidates[1].outcome, AdmissionOutcome::NotEvaluated);
}

#[test]
fn trace_distinguishes_quota_from_priority_order() {
    let cluster = cluster();
    let leases = BTreeMap::from([(LeaseId::from_u64(1), active_lease(1, 1, 2, 1, 100))]);
    let occupancy = occupancy_from_leases(leases.values(), &BTreeSet::new(), &BTreeSet::new());
    let queue = vec![
        Queued {
            request: request(20, 1, 10, 10),
            owner: OwnerId::from_u64(1),
            submitted_at: 1,
        },
        Queued {
            request: request(21, 1, 1, 10),
            owner: OwnerId::from_u64(2),
            submitted_at: 2,
        },
    ];
    let policy = AdmissionPolicy {
        owner_ceiling: BTreeMap::from([(ResourceClass::Cpu, qty(CapacityDimension::Count, 1))]),
        ..AdmissionPolicy::default()
    };

    let (admission, trace) = admit_with_policy_explained(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue,
        &leases,
        &BTreeSet::new(),
    );

    assert_eq!(admission.unwrap().request.id, RequestId::from_u64(21));
    assert_eq!(trace.candidates[0].request, RequestId::from_u64(20));
    assert_eq!(trace.candidates[0].priority, 10);
    assert_eq!(trace.candidates[0].outcome, AdmissionOutcome::OverQuota);
    assert_eq!(trace.candidates[1].request, RequestId::from_u64(21));
    assert_eq!(
        trace.candidates[1].outcome,
        AdmissionOutcome::Selected { backfilled: false }
    );
}

#[test]
fn trace_marks_unsatisfiable_head_as_placement_refused() {
    let cluster = cluster();
    let queue = vec![
        Queued {
            request: request(30, 3, 10, 10),
            owner: OwnerId::from_u64(1),
            submitted_at: 1,
        },
        Queued {
            request: request(31, 1, 1, 10),
            owner: OwnerId::from_u64(2),
            submitted_at: 2,
        },
    ];

    let (admission, trace) = admit_with_policy_explained(
        &cluster.graph,
        &cluster.occupancy(),
        &BTreeSet::new(),
        &AdmissionPolicy::default(),
        &queue,
        &BTreeMap::new(),
        &BTreeSet::new(),
    );

    assert_eq!(admission.unwrap().request.id, RequestId::from_u64(31));
    assert!(matches!(
        &trace.candidates[0].outcome,
        AdmissionOutcome::PlacementRefused { explanation } if !explanation.is_empty()
    ));
    assert_eq!(
        trace.candidates[1].outcome,
        AdmissionOutcome::Selected { backfilled: false }
    );
}

#[test]
fn backfill_trace_exposes_non_authoritative_shadow() {
    let cluster = cluster();
    let running = active_lease(1, 1, 2, 1, 10);
    let leases = BTreeMap::from([(LeaseId::from_u64(1), running)]);
    let open_bindings = BTreeSet::new();
    let occupancy = occupancy_from_leases(leases.values(), &open_bindings, &BTreeSet::new());
    let queue = vec![
        Queued {
            request: request(40, 2, 10, 10),
            owner: OwnerId::from_u64(2),
            submitted_at: 1,
        },
        Queued {
            request: request(41, 1, 1, 5),
            owner: OwnerId::from_u64(3),
            submitted_at: 2,
        },
    ];

    let (admission, trace) = admit_backfill_with_policy_explained(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &BTreeMap::new(),
        &AdmissionPolicy::default(),
        &queue,
        &BackfillCtx {
            now: 0,
            leases: &leases,
            open_bindings: &open_bindings,
        },
    );

    assert_eq!(admission.unwrap().request.id, RequestId::from_u64(41));
    assert_eq!(
        trace.candidates[0].outcome,
        AdmissionOutcome::Blocked { shadow: Some(10) }
    );
    assert_eq!(
        trace.candidates[1].outcome,
        AdmissionOutcome::Selected { backfilled: true }
    );
    assert_eq!(leases.len(), 1, "shadow planning must not create a Lease");
}
