use archon_kernel::{
    BindingId, BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command,
    Effect, Error, LeaseId, LeaseState, Need, NodeId, OwnerId, ProviderId, Quantity, Request,
    RequestClass, RequestId, ResourceClass, qty,
};

fn node(id: u64, kind: ResourceClass, capacity: Quantity) -> archon_kernel::Node {
    archon_kernel::Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn graph() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(3, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                archon_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(2),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                archon_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(3),
                    kind: archon_kernel::EdgeKind::Contains,
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

fn cpu_claim(cluster: &Cluster, node: u64) -> archon_kernel::Allocation {
    archon_kernel::Allocation {
        claims: vec![archon_kernel::Claim {
            node: NodeId::from_u64(node),
            quantity: qty(CapacityDimension::Count, 1),
        }],
        graph_revision: cluster.graph.revision,
        explanation: "test".into(),
    }
}

fn open(cluster: &mut Cluster, lease: u64, node: u64, parent: Option<u64>, expires_at: u64) {
    cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(lease),
            owner: OwnerId::from_u64(lease),
            allocation: cpu_claim(cluster, node),
            parent: parent.map(LeaseId::from_u64),
            expires_at,
            prepare_deadline: 1_000,
            priority: 1,
        })
        .unwrap();
}

fn bind(cluster: &mut Cluster, lease: u64, node: u64, binding: u64) {
    cluster
        .apply(Command::OpenBinding {
            binding: BindingId::from_u64(binding),
            lease: LeaseId::from_u64(lease),
            node: NodeId::from_u64(node),
            provider: ProviderId::ENFORCE,
            scope: archon_kernel::BindingScope::Exclusive,
        })
        .unwrap();
    cluster
        .apply(Command::RecordBindingPrepared {
            binding: BindingId::from_u64(binding),
            session: 1,
            provider_handle: binding,
            fence: 1,
        })
        .unwrap();
}

fn bind_and_activate(cluster: &mut Cluster, lease: u64, node: u64, binding: u64) {
    bind(cluster, lease, node, binding);
    cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(lease),
        })
        .unwrap();
}

fn active_root(cluster: &mut Cluster, lease: u64, node: u64, binding: u64) {
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    open(cluster, lease, node, None, 1_000);
    bind_and_activate(cluster, lease, node, binding);
}

#[test]
fn root_activation_requires_bindings() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    open(&mut cluster, 1, 2, None, 1_000);
    let err = cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap_err();
    assert!(matches!(err, Error::BindingsNotPrepared { .. }));
}

#[test]
fn activation_refused_after_prepare_deadline() {
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
            allocation: cpu_claim(&cluster, 2),
            parent: None,
            expires_at: 1_000,
            prepare_deadline: 5,
            priority: 1,
        })
        .unwrap();
    cluster.set_now(6);
    bind(&mut cluster, 1, 2, 1);
    let err = cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap_err();
    assert!(matches!(err, Error::PrepareDeadlinePassed { .. }));
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Preparing
    ));
}

#[test]
fn activation_refused_after_expiry() {
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
            allocation: cpu_claim(&cluster, 2),
            parent: None,
            expires_at: 10,
            prepare_deadline: 1_000,
            priority: 1,
        })
        .unwrap();
    cluster.set_now(10);
    bind(&mut cluster, 1, 2, 1);
    let err = cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap_err();
    assert!(matches!(err, Error::LeaseExpired { .. }));
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Preparing
    ));
}

#[test]
fn promote_refused_after_expiry_or_quarantine() {
    let mut cluster = graph();
    let allocation = cpu_claim(&cluster, 2);
    cluster
        .apply(Command::ReserveLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation,
            expires_at: 10,
            priority: 1,
        })
        .unwrap();
    cluster.set_now(10);
    let err = cluster
        .apply(Command::PromoteLease {
            lease: LeaseId::from_u64(1),
            prepare_deadline: 100,
        })
        .unwrap_err();
    assert!(matches!(err, Error::LeaseExpired { .. }));

    let mut cluster = graph();
    let allocation = cpu_claim(&cluster, 2);
    cluster
        .apply(Command::ReserveLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation,
            expires_at: 1_000,
            priority: 1,
        })
        .unwrap();
    cluster
        .apply(Command::QuarantineNode {
            node: NodeId::from_u64(2),
        })
        .unwrap();
    let err = cluster
        .apply(Command::PromoteLease {
            lease: LeaseId::from_u64(1),
            prepare_deadline: 100,
        })
        .unwrap_err();
    assert!(matches!(err, Error::Quarantined(_)));
}

#[test]
fn release_with_live_descendants_refused_and_expire_cascades() {
    let mut cluster = graph();
    active_root(&mut cluster, 1, 2, 1);
    // Child is accounting-only: it may activate without Bindings.
    open(&mut cluster, 2, 2, Some(1), 1_000);
    cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(2),
        })
        .unwrap();
    let err = cluster
        .apply(Command::ReleaseLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap_err();
    assert!(matches!(err, Error::HasLiveDescendants { .. }));

    cluster.set_now(1_000);
    cluster
        .apply(Command::ExpireLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap();
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Expired
    ));
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Expired
    ));
    // Occupancy persists until the fence ack closes the Binding.
    assert!(cluster.occupies(LeaseId::from_u64(1)));
    cluster
        .apply(Command::RecordBindingFenced {
            binding: BindingId::from_u64(1),
            session: 1,
            fence: 1,
        })
        .unwrap();
    assert!(!cluster.occupies(LeaseId::from_u64(1)));
    assert!(!cluster.occupies(LeaseId::from_u64(2)));
}

#[test]
fn grandchild_capacity_counts_against_new_siblings() {
    let mut cluster = graph();
    active_root(&mut cluster, 1, 2, 1);
    // Child A takes the full CPU; grandchild A1 lives inside A.
    open(&mut cluster, 2, 2, Some(1), 1_000);
    cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(2),
        })
        .unwrap();
    open(&mut cluster, 3, 2, Some(2), 1_000);
    cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(3),
        })
        .unwrap();

    // A still owns the units: a second child of the parent cannot take them.
    let err = cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(4),
            owner: OwnerId::from_u64(4),
            allocation: cpu_claim(&cluster, 2),
            parent: Some(LeaseId::from_u64(1)),
            expires_at: 1_000,
            prepare_deadline: 1_000,
            priority: 1,
        })
        .unwrap_err();
    assert!(matches!(err, Error::Overlap { .. }));

    // And the intermediate child cannot be released while A1 is live.
    let err = cluster
        .apply(Command::ReleaseLease {
            lease: LeaseId::from_u64(2),
        })
        .unwrap_err();
    assert!(matches!(err, Error::HasLiveDescendants { .. }));
}

#[test]
fn duplicate_open_binding_after_active_is_a_noop() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    open(&mut cluster, 1, 2, None, 1_000);
    let open_binding = Command::OpenBinding {
        binding: BindingId::from_u64(1),
        lease: LeaseId::from_u64(1),
        node: NodeId::from_u64(2),
        provider: ProviderId::ENFORCE,
        scope: archon_kernel::BindingScope::Exclusive,
    };
    cluster.apply(open_binding.clone()).unwrap();
    cluster
        .apply(Command::RecordBindingPrepared {
            binding: BindingId::from_u64(1),
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
    cluster
        .apply(Command::ActivateBinding {
            binding: BindingId::from_u64(1),
        })
        .unwrap();
    cluster
        .apply(Command::RecordBindingActive {
            binding: BindingId::from_u64(1),
            session: 1,
            fence: 1,
        })
        .unwrap();
    // Retry after the binding is Active: no new Prepare effect.
    let effects = cluster.apply(open_binding).unwrap();
    assert!(effects.is_empty());
    assert!(matches!(
        cluster.bindings[&BindingId::from_u64(1)].state,
        archon_kernel::BindingState::Active
    ));
}

#[test]
fn binding_acks_validate_the_fence() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    open(&mut cluster, 1, 2, None, 1_000);
    cluster
        .apply(Command::OpenBinding {
            binding: BindingId::from_u64(1),
            lease: LeaseId::from_u64(1),
            node: NodeId::from_u64(2),
            provider: ProviderId::ENFORCE,
            scope: archon_kernel::BindingScope::Exclusive,
        })
        .unwrap();
    let err = cluster
        .apply(Command::RecordBindingPrepared {
            binding: BindingId::from_u64(1),
            session: 1,
            provider_handle: 1,
            fence: 7,
        })
        .unwrap_err();
    assert!(matches!(err, Error::FenceMismatch { .. }));
}

#[test]
fn failed_binding_fails_its_lease() {
    let mut cluster = graph();
    active_root(&mut cluster, 1, 2, 1);
    cluster
        .apply(Command::RecordBindingFailed {
            binding: BindingId::from_u64(1),
            session: 1,
            reason: "device reset".into(),
            fence: 1,
        })
        .unwrap();
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Failed
    ));
    // Occupancy lasts until the failed Binding's fence is acknowledged.
    assert!(cluster.occupies(LeaseId::from_u64(1)));
    cluster
        .apply(Command::RecordBindingFenced {
            binding: BindingId::from_u64(1),
            session: 1,
            fence: 1,
        })
        .unwrap();
    assert!(!cluster.occupies(LeaseId::from_u64(1)));
}

#[test]
fn stale_agent_hello_cannot_reinstate_an_old_session() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 2,
        })
        .unwrap();
    let err = cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        Error::StaleSession {
            expected: 2,
            got: 1
        }
    ));
    // An equal-session retransmission is idempotent and re-emits reconcile.
    let effects = cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 2,
        })
        .unwrap();
    assert!(matches!(effects.as_slice(), [Effect::Reconcile { .. }]));
}

#[test]
fn failed_apply_graph_is_atomic_and_replay_consistent() {
    let mut cluster = graph();
    let before = cluster.digest();
    let err = cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(
                9,
                ResourceClass::Nvme,
                qty(CapacityDimension::Count, 1),
            )],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(9),
                to: NodeId::from_u64(99),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap_err();
    assert!(matches!(err, Error::UnknownNode(_)));
    assert!(cluster.graph.node(NodeId::from_u64(9)).is_none());
    assert_eq!(cluster.graph.revision, before.graph_revision);
    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(replayed.digest(), cluster.digest());
}

#[test]
fn contains_cycles_and_multi_parent_are_rejected_atomically() {
    let mut cluster = graph();
    let before = cluster.digest();
    let err = cluster
        .apply(Command::ApplyGraph {
            nodes: vec![],
            edges: vec![
                archon_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(2),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                archon_kernel::Edge {
                    from: NodeId::from_u64(2),
                    to: NodeId::from_u64(1),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
            ],
        })
        .unwrap_err();
    assert!(matches!(err, Error::InvalidTopology { .. }));
    let err = cluster
        .apply(Command::ApplyGraph {
            nodes: vec![],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(3),
                to: NodeId::from_u64(2),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap_err();
    assert!(matches!(err, Error::InvalidTopology { .. }));
    assert_eq!(cluster.digest(), before);
    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(replayed.digest(), cluster.digest());
}

#[test]
fn child_leases_cannot_open_bindings() {
    let mut cluster = graph();
    active_root(&mut cluster, 1, 2, 1);
    open(&mut cluster, 2, 2, Some(1), 1_000);
    let err = cluster
        .apply(Command::OpenBinding {
            binding: BindingId::from_u64(9),
            lease: LeaseId::from_u64(2),
            node: NodeId::from_u64(2),
            provider: ProviderId::ENFORCE,
            scope: archon_kernel::BindingScope::Exclusive,
        })
        .unwrap_err();
    assert!(matches!(err, Error::ChildBindingRefused { .. }));
}

#[test]
fn health_updates_do_not_strand_in_flight_allocations() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    let revision_before = cluster.graph.revision;
    open(&mut cluster, 1, 2, None, 1_000);
    // Health changes while the lease prepares.
    cluster
        .apply(Command::SetNodeHealth {
            node: NodeId::from_u64(1),
            health: "degraded".into(),
        })
        .unwrap();
    assert_eq!(cluster.graph.revision, revision_before);
    bind_and_activate(&mut cluster, 1, 2, 1);
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    ));
    assert!(
        cluster
            .graph
            .degraded_ancestor(NodeId::from_u64(2))
            .is_some()
    );
    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(replayed.digest(), cluster.digest());
}

#[test]
fn reserve_lease_retries_are_idempotent() {
    let mut cluster = graph();
    let allocation = cpu_claim(&cluster, 2);
    let command = Command::ReserveLease {
        lease: LeaseId::from_u64(1),
        owner: OwnerId::from_u64(1),
        allocation: allocation.clone(),
        expires_at: 1_000,
        priority: 1,
    };
    cluster.apply(command.clone()).unwrap();
    cluster.apply(command).unwrap();
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Reserved
    ));
}

#[test]
fn capacity_cannot_shrink_below_occupied_units() {
    let mut cluster = graph();
    active_root(&mut cluster, 1, 2, 1);
    let before = cluster.digest();
    // Node 2 holds one occupied CPU; shrinking it to zero must be refused.
    let err = cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![node(2, ResourceClass::Cpu, Quantity::new())],
            edges: vec![],
            claim_bindings: vec![ClaimBindingUpdate {
                node: NodeId::from_u64(2),
                dimension: CapacityDimension::Count,
                binding: None,
            }],
        })
        .unwrap_err();
    assert!(matches!(err, Error::CapacityBelowOccupancy { .. }));
    assert_eq!(cluster.digest(), before);
    // Growing capacity is fine.
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(
                2,
                ResourceClass::Cpu,
                qty(CapacityDimension::Count, 4),
            )],
            edges: vec![],
        })
        .unwrap();
    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(replayed.digest(), cluster.digest());
}

#[test]
fn preparing_lease_expires_when_due() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    open(&mut cluster, 1, 2, None, 10);
    bind(&mut cluster, 1, 2, 1);
    cluster.set_now(10);
    cluster
        .apply(Command::ExpireLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap();
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Expired
    ));
}

#[test]
fn backfill_honors_the_owner_ceiling() {
    let mut cluster = graph();
    active_root(&mut cluster, 1, 2, 1);
    let queue = vec![archon_kernel::Queued {
        request: Request {
            id: RequestId::from_u64(2),
            class: RequestClass::Batch,
            needs: vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            }],
            topology: vec![],
            preferences: vec![],
            data: vec![],
            machine_local: true,
            lifetime: 10,
            priority: 1,
        },
        owner: OwnerId::from_u64(1),
        submitted_at: 1,
    }];
    // Owner 1 already holds one CPU; a 1-CPU ceiling blocks its request even
    // though capacity is free.
    let fair =
        archon_kernel::ClassUsage::from([(ResourceClass::Cpu, qty(CapacityDimension::Count, 1))]);
    assert!(cluster.admit_backfill(&queue, &fair).is_none());
    assert!(
        cluster
            .admit_backfill(&queue, &archon_kernel::ClassUsage::new())
            .is_some()
    );
}

#[test]
fn duplicate_node_claims_are_rejected() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    let allocation = archon_kernel::Allocation {
        claims: vec![
            archon_kernel::Claim {
                node: NodeId::from_u64(2),
                quantity: qty(CapacityDimension::Count, 1),
            },
            archon_kernel::Claim {
                node: NodeId::from_u64(2),
                quantity: qty(CapacityDimension::Count, 1),
            },
        ],
        graph_revision: cluster.graph.revision,
        explanation: "dup".into(),
    };
    let err = cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation,
            parent: None,
            expires_at: 1_000,
            prepare_deadline: 1_000,
            priority: 1,
        })
        .unwrap_err();
    assert!(matches!(err, Error::DuplicateClaim { .. }));
}

#[test]
fn rejected_expiry_leaves_descendants_and_log_untouched() {
    let mut cluster = graph();
    active_root(&mut cluster, 1, 2, 1);
    open(&mut cluster, 2, 2, Some(1), 1_000);
    cluster
        .apply(Command::ActivateLease {
            lease: LeaseId::from_u64(2),
        })
        .unwrap();
    let before = cluster.digest();
    // Parent not yet due: the command is refused and mutates nothing.
    let err = cluster
        .apply(Command::ExpireLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap_err();
    assert!(matches!(err, Error::ExpireNotDue));
    assert_eq!(cluster.digest(), before);
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Active
    ));
}

#[test]
fn partial_memory_roots_sum_instead_of_taking_the_max() {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Memory, qty(CapacityDimension::Bytes, 100)),
            ],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(1),
                to: NodeId::from_u64(2),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: vec![ClaimBindingUpdate {
                node: NodeId::from_u64(2),
                dimension: CapacityDimension::Bytes,
                binding: Some(ClaimBinding {
                    provider: ProviderId::ENFORCE,
                    scope: BindingScope::IndependentShare,
                }),
            }],
        })
        .unwrap();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    for lease in 1..=2 {
        cluster
            .apply(Command::OpenLease {
                lease: LeaseId::from_u64(lease),
                owner: OwnerId::from_u64(lease),
                allocation: archon_kernel::Allocation {
                    claims: vec![archon_kernel::Claim {
                        node: NodeId::from_u64(2),
                        quantity: qty(CapacityDimension::Bytes, 40),
                    }],
                    graph_revision: cluster.graph.revision,
                    explanation: "mem".into(),
                },
                parent: None,
                expires_at: 1_000,
                prepare_deadline: 1_000,
                priority: 1,
            })
            .unwrap();
    }
    // 40 + 40 used of 100: a third 40-byte root cannot open.
    let err = cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(3),
            owner: OwnerId::from_u64(3),
            allocation: archon_kernel::Allocation {
                claims: vec![archon_kernel::Claim {
                    node: NodeId::from_u64(2),
                    quantity: qty(CapacityDimension::Bytes, 40),
                }],
                graph_revision: cluster.graph.revision,
                explanation: "mem".into(),
            },
            parent: None,
            expires_at: 1_000,
            prepare_deadline: 1_000,
            priority: 1,
        })
        .unwrap_err();
    assert!(matches!(err, Error::Overlap { .. }));
    // And shrinking capacity below the 80 occupied bytes is refused.
    let err = cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(
                2,
                ResourceClass::Memory,
                qty(CapacityDimension::Bytes, 50),
            )],
            edges: vec![],
        })
        .unwrap_err();
    assert!(matches!(err, Error::CapacityBelowOccupancy { .. }));
}

#[test]
fn promotion_survives_unrelated_graph_updates() {
    let mut cluster = graph();
    let allocation = cpu_claim(&cluster, 2);
    cluster
        .apply(Command::ReserveLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation,
            expires_at: 1_000,
            priority: 1,
        })
        .unwrap();
    // An unrelated additive update advances the revision.
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(
                9,
                ResourceClass::Nvme,
                qty(CapacityDimension::Count, 1),
            )],
            edges: vec![],
        })
        .unwrap();
    cluster
        .apply(Command::PromoteLease {
            lease: LeaseId::from_u64(1),
            prepare_deadline: 1_000,
        })
        .unwrap();
    assert!(matches!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Preparing
    ));
    assert_eq!(
        cluster.leases[&LeaseId::from_u64(1)]
            .allocation
            .graph_revision,
        cluster.graph.revision
    );
}

#[test]
fn data_objects_cannot_be_claimed() {
    let mut cluster = graph();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(
                7,
                ResourceClass::DataObject,
                qty(CapacityDimension::Bytes, 1 << 30),
            )],
            edges: vec![],
        })
        .unwrap();
    let err = cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation: archon_kernel::Allocation {
                claims: vec![archon_kernel::Claim {
                    node: NodeId::from_u64(7),
                    quantity: qty(CapacityDimension::Bytes, 1 << 30),
                }],
                graph_revision: cluster.graph.revision,
                explanation: "data".into(),
            },
            parent: None,
            expires_at: 1_000,
            prepare_deadline: 1_000,
            priority: 1,
        })
        .unwrap_err();
    assert!(matches!(err, Error::UnclaimableNode { .. }));
}

#[test]
fn digest_distinguishes_capacity_and_edge_changes() {
    let mut cluster = graph();
    let before = cluster.digest();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(
                2,
                ResourceClass::Cpu,
                qty(CapacityDimension::Count, 8),
            )],
            edges: vec![],
        })
        .unwrap();
    assert_ne!(cluster.digest(), before, "capacity change must show");
    let after_capacity = cluster.digest();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(2),
                to: NodeId::from_u64(3),
                kind: archon_kernel::EdgeKind::Connected,
                attrs: Default::default(),
            }],
        })
        .unwrap();
    assert_ne!(cluster.digest(), after_capacity, "edge change must show");
}

#[test]
fn completion_records_success_and_failure() {
    let mut cluster = graph();
    active_root(&mut cluster, 1, 2, 1);
    assert!(cluster.occupancy().is_used(NodeId::from_u64(2)));

    // A zero exit completes the lease and frees its claims.
    let effects = cluster
        .apply(Command::CompleteLease {
            lease: LeaseId::from_u64(1),
            exit_code: 0,
        })
        .unwrap();
    assert_eq!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Completed
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Fence { .. })),
        "completion must fence enforcement"
    );
    // Capacity frees when enforcement acknowledges the release.
    cluster
        .apply(Command::RecordBindingReleased {
            binding: BindingId::from_u64(1),
            session: 1,
            fence: 1,
        })
        .unwrap();

    // Completion is terminal: repeating it changes nothing.
    cluster
        .apply(Command::CompleteLease {
            lease: LeaseId::from_u64(1),
            exit_code: 0,
        })
        .unwrap();
    assert_eq!(
        cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Completed
    );

    // A non-zero exit fails the lease instead.
    active_root(&mut cluster, 2, 3, 2);
    cluster
        .apply(Command::CompleteLease {
            lease: LeaseId::from_u64(2),
            exit_code: 3,
        })
        .unwrap();
    assert_eq!(
        cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Failed
    );
    cluster
        .apply(Command::RecordBindingReleased {
            binding: BindingId::from_u64(2),
            session: 1,
            fence: 1,
        })
        .unwrap();
    assert!(!cluster.occupancy().is_used(NodeId::from_u64(3)));

    // Completing an unknown or dead-state lease is rejected, not ignored.
    let mut fresh = graph();
    fresh
        .apply(Command::CompleteLease {
            lease: LeaseId::from_u64(9),
            exit_code: 0,
        })
        .unwrap_err();
    open(&mut fresh, 4, 2, None, 1_000);
    fresh
        .apply(Command::RevokeLease {
            lease: LeaseId::from_u64(4),
        })
        .unwrap();
    fresh
        .apply(Command::CompleteLease {
            lease: LeaseId::from_u64(4),
            exit_code: 0,
        })
        .unwrap_err();
}

#[test]
fn independent_share_bindings_can_coexist_but_exclusive_scope_cannot_mix() {
    let mut cluster = Cluster::new();
    let machine = NodeId::from_u64(1);
    let memory = NodeId::from_u64(2);
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(
                    2,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 1024),
                ),
            ],
            edges: vec![archon_kernel::Edge {
                from: machine,
                to: memory,
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: vec![ClaimBindingUpdate {
                node: memory,
                dimension: CapacityDimension::Bytes,
                binding: Some(ClaimBinding {
                    provider: ProviderId::ENFORCE,
                    scope: BindingScope::IndependentShare,
                }),
            }],
        })
        .unwrap();
    cluster
        .apply(Command::SetAgentSession {
            machine,
            session: 1,
        })
        .unwrap();
    for (lease, binding) in [(1, 1), (2, 2)] {
        let allocation = archon_kernel::Allocation {
            claims: vec![archon_kernel::Claim {
                node: memory,
                quantity: qty(CapacityDimension::Bytes, 256),
            }],
            graph_revision: cluster.graph.revision,
            explanation: "share".into(),
        };
        cluster
            .apply(Command::OpenLease {
                lease: LeaseId::from_u64(lease),
                owner: OwnerId::from_u64(lease),
                allocation,
                parent: None,
                expires_at: 1_000,
                prepare_deadline: 1_000,
                priority: 1,
            })
            .unwrap();
        cluster
            .apply(Command::OpenBinding {
                binding: BindingId::from_u64(binding),
                lease: LeaseId::from_u64(lease),
                node: memory,
                provider: ProviderId::ENFORCE,
                scope: archon_kernel::BindingScope::IndependentShare,
            })
            .expect("independent shares may coexist");
    }
    let allocation = archon_kernel::Allocation {
        claims: vec![archon_kernel::Claim {
            node: memory,
            quantity: qty(CapacityDimension::Bytes, 256),
        }],
        graph_revision: cluster.graph.revision,
        explanation: "exclusive".into(),
    };
    cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(3),
            owner: OwnerId::from_u64(3),
            allocation,
            parent: None,
            expires_at: 1_000,
            prepare_deadline: 1_000,
            priority: 1,
        })
        .unwrap();
    let err = cluster
        .apply(Command::OpenBinding {
            binding: BindingId::from_u64(3),
            lease: LeaseId::from_u64(3),
            node: memory,
            provider: ProviderId::ENFORCE,
            scope: archon_kernel::BindingScope::Exclusive,
        })
        .unwrap_err();
    assert!(
        matches!(err, Error::Refused { explanation } if explanation.contains("IndependentShare"))
    );
}
