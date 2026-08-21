use fleet_kernel::{
    BindingId, Cluster, Command, Dimension, Error, LeaseId, LeaseState, NodeId, NodeKind,
    OwnerId, ProviderId, Quantity, qty,
};

fn node(id: u64, kind: NodeKind, capacity: Quantity) -> fleet_kernel::Node {
    fleet_kernel::Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn graph() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, NodeKind::Machine, Quantity::new()),
                node(2, NodeKind::Cpu, qty(Dimension::Count, 1)),
                node(3, NodeKind::Cpu, qty(Dimension::Count, 1)),
            ],
            edges: vec![
                fleet_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(2),
                    kind: fleet_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                fleet_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(3),
                    kind: fleet_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
            ],
        })
        .unwrap();
    cluster
}

fn cpu_claim(cluster: &Cluster, node: u64) -> fleet_kernel::Allocation {
    fleet_kernel::Allocation {
        claims: vec![fleet_kernel::Claim {
            node: NodeId::from_u64(node),
            quantity: qty(Dimension::Count, 1),
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
        fleet_kernel::BindingState::Active
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
    assert!(matches!(err, Error::StaleSession { expected: 2, got: 1 }));
    let err = cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 2,
        })
        .unwrap_err();
    assert!(matches!(err, Error::StaleSession { expected: 2, got: 2 }));
}

#[test]
fn failed_apply_graph_is_atomic_and_replay_consistent() {
    let mut cluster = graph();
    let before = cluster.digest();
    let err = cluster
        .apply(Command::ApplyGraph {
            nodes: vec![node(9, NodeKind::Nvme, qty(Dimension::Count, 1))],
            edges: vec![fleet_kernel::Edge {
                from: NodeId::from_u64(9),
                to: NodeId::from_u64(99),
                kind: fleet_kernel::EdgeKind::Contains,
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
            edges: vec![fleet_kernel::Edge {
                from: NodeId::from_u64(1),
                to: NodeId::from_u64(2),
                kind: fleet_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }, fleet_kernel::Edge {
                from: NodeId::from_u64(2),
                to: NodeId::from_u64(1),
                kind: fleet_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap_err();
    assert!(matches!(err, Error::InvalidTopology { .. }));
    let err = cluster
        .apply(Command::ApplyGraph {
            nodes: vec![],
            edges: vec![fleet_kernel::Edge {
                from: NodeId::from_u64(3),
                to: NodeId::from_u64(2),
                kind: fleet_kernel::EdgeKind::Contains,
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
    assert!(cluster.graph.degraded_ancestor(NodeId::from_u64(2)).is_some());
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
