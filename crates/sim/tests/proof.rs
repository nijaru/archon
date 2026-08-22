use archon_kernel::{
    BindingId, Dimension, Effect, EndpointOp, Error, LeaseId, LeaseState, Need, NodeId, NodeKind,
    OwnerId, ProviderId, Request, RequestClass, RequestId, TopologyConstraint, qty,
};
use archon_sim::{GIB, World, tiny_graph};

fn service_request() -> Request {
    Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Service,
        needs: vec![
            Need {
                kind: NodeKind::Cpu,
                quantity: qty(Dimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: NodeKind::Memory,
                quantity: qty(Dimension::Bytes, GIB),
                filters: vec![],
            },
        ],
        topology: vec![TopologyConstraint {
            left: 0,
            right: 1,
            kind: archon_kernel::EdgeKind::SameNuma,
        }],
        preferences: vec![],
        data: vec![],
        command: vec![],
        machine_local: true,
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 100,
        keep_alive: false,
        priority: 10,
    }
}

fn boot() -> World {
    let mut world = World::new();
    let graph = tiny_graph(2);
    world.apply_graph(graph.nodes, graph.edges).unwrap();
    for machine in &graph.machines {
        world.register_agent(machine.machine).unwrap();
    }
    world.deliver_all().unwrap();
    world.set_now(1);
    world
}

fn place_ready(world: &mut World, lease: u64) {
    world
        .place(
            &service_request(),
            LeaseId::from_u64(lease),
            OwnerId::from_u64(lease),
            None,
            100,
            20,
        )
        .unwrap();
    world
        .bind_enforced(LeaseId::from_u64(lease), lease * 10)
        .unwrap();
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(lease)).unwrap();
    world.deliver_all().unwrap();
}

#[test]
fn activate_and_release_frees_claims() {
    let mut world = boot();
    place_ready(&mut world, 1);
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
    world.release_lease(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();
    assert!(
        world
            .cluster
            .bindings
            .values()
            .all(|binding| binding.state.is_closed())
    );
    assert!(!world.cluster.occupies(LeaseId::from_u64(1)));
    place_ready(&mut world, 2);
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Active
    );
}

#[test]
fn renew_moves_deadline_only() {
    let mut world = boot();
    place_ready(&mut world, 1);
    let before = world.digest();
    world
        .apply(archon_kernel::Command::RenewLease {
            lease: LeaseId::from_u64(1),
            new_expires_at: 200,
        })
        .unwrap();
    let after = world.digest();
    assert_eq!(after.leases[&LeaseId::from_u64(1)].expires_at, 200);
    assert_eq!(before.bindings, after.bindings);
    assert_eq!(
        before.leases[&LeaseId::from_u64(1)].claims,
        after.leases[&LeaseId::from_u64(1)].claims
    );
}

#[test]
fn expire_occupies_until_fence_ack() {
    let mut world = boot();
    place_ready(&mut world, 1);
    world.set_now(100);
    world.expire_due().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Expired
    );
    assert!(world.cluster.occupies(LeaseId::from_u64(1)));
    world.deliver_all().unwrap();
    assert!(!world.cluster.occupies(LeaseId::from_u64(1)));
}

#[test]
fn old_fence_is_rejected() {
    let mut world = boot();
    place_ready(&mut world, 1);
    let binding = world.cluster.bindings.values().next().cloned().unwrap();
    let endpoint = world
        .endpoints
        .get(&(ProviderId::ENFORCE, binding.node))
        .cloned()
        .unwrap();
    let mut endpoint = endpoint;
    let err = endpoint
        .apply(
            EndpointOp::Prepare,
            binding.id,
            binding.fence - 1,
            binding.agent_session,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        archon_kernel::EndpointError::StaleFence { .. }
    ));
}

#[test]
fn old_session_is_rejected() {
    let mut world = boot();
    place_ready(&mut world, 1);
    let binding = world.cluster.bindings.values().next().cloned().unwrap();
    let machine = world.cluster.graph.machine_of(binding.node).unwrap();
    world.restart_agent(machine).unwrap();
    let err = world
        .apply(archon_kernel::Command::RecordBindingActive {
            binding: binding.id,
            session: binding.agent_session,
            fence: binding.fence,
        })
        .unwrap_err();
    assert!(matches!(err, Error::StaleSession { .. }));
}

#[test]
fn lost_prepare_ack_fails_without_subset() {
    let mut world = boot();
    world
        .place(
            &service_request(),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            None,
            100,
            20,
        )
        .unwrap();
    world.bind_enforced(LeaseId::from_u64(1), 10).unwrap();
    assert!(world.drop_one());
    world.deliver_all().unwrap();
    let err = world.activate_lease(LeaseId::from_u64(1)).unwrap_err();
    assert!(matches!(err, Error::BindingsNotPrepared { .. }));
    world
        .apply(archon_kernel::Command::FailLease {
            lease: LeaseId::from_u64(1),
            reason: "lost prepare".into(),
        })
        .unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Failed
    );
    assert_ne!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
}

#[test]
fn prepare_deadline_fails_partial_prepare() {
    let mut world = boot();
    world
        .place(
            &service_request(),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            None,
            100,
            5,
        )
        .unwrap();
    world.bind_enforced(LeaseId::from_u64(1), 10).unwrap();
    world.drop_one();
    world.deliver_all().unwrap();
    world.set_now(5);
    world.fail_late_prepares().unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Failed
    );
    assert!(
        world
            .cluster
            .bindings
            .values()
            .all(|binding| { binding.lease != LeaseId::from_u64(1) || binding.state.is_closed() })
    );
}

#[test]
fn agent_restart_rejects_old_session_and_keeps_lease() {
    let mut world = boot();
    place_ready(&mut world, 1);
    let binding = world.cluster.bindings.values().next().cloned().unwrap();
    let machine = world.cluster.graph.machine_of(binding.node).unwrap();
    world.restart_agent(machine).unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
    assert!(world.cluster.occupies(LeaseId::from_u64(1)));
    world
        .inject(Effect::Activate {
            binding: binding.id,
            node: binding.node,
            provider: binding.provider,
            fence: binding.fence,
            session: binding.agent_session,
            epoch: world.cluster.epoch,
        })
        .unwrap();
    assert_ne!(world.cluster.sessions[&machine], binding.agent_session);
}

#[test]
fn old_epoch_is_ignored() {
    let mut world = boot();
    place_ready(&mut world, 1);
    let binding = world.cluster.bindings.values().next().cloned().unwrap();
    world.advance_epoch();
    let accepted = world
        .endpoints
        .get(&(ProviderId::ENFORCE, binding.node))
        .unwrap()
        .accepted_fence;
    world
        .inject(Effect::Fence {
            binding: binding.id,
            node: binding.node,
            provider: binding.provider,
            fence: binding.fence,
            session: world.cluster.sessions[&world.cluster.graph.machine_of(binding.node).unwrap()],
            epoch: world.cluster.epoch - 1,
        })
        .unwrap();
    assert_eq!(
        world
            .endpoints
            .get(&(ProviderId::ENFORCE, binding.node))
            .unwrap()
            .accepted_fence,
        accepted
    );
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
}

#[test]
fn parent_revoke_fences_child_first() {
    let mut world = boot();
    place_ready(&mut world, 1);
    let parent = world.cluster.leases[&LeaseId::from_u64(1)]
        .allocation
        .clone();
    world
        .apply(archon_kernel::Command::OpenLease {
            lease: LeaseId::from_u64(2),
            owner: OwnerId::from_u64(2),
            allocation: parent,
            parent: Some(LeaseId::from_u64(1)),
            expires_at: 100,
            prepare_deadline: 20,
            priority: 1,
        })
        .unwrap();
    world
        .apply(archon_kernel::Command::ActivateLease {
            lease: LeaseId::from_u64(2),
        })
        .unwrap();
    world.revoke_lease(LeaseId::from_u64(1)).unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Revoked
    );
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Revoked
    );
    world.deliver_all().unwrap();
    assert!(
        world
            .cluster
            .bindings
            .values()
            .all(|binding| binding.state.is_closed())
    );
}

#[test]
fn quarantine_blocks_new_exclusive_lease() {
    let mut world = boot();
    place_ready(&mut world, 1);
    let machine = world
        .cluster
        .graph
        .machine_of(
            world.cluster.leases[&LeaseId::from_u64(1)]
                .allocation
                .claims[0]
                .node,
        )
        .unwrap();
    let machines: Vec<NodeId> = world
        .cluster
        .graph
        .nodes_of_kind(NodeKind::Machine)
        .to_vec();
    for node in &machines {
        world
            .apply(archon_kernel::Command::QuarantineNode { node: *node })
            .unwrap();
    }
    world.revoke_lease(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();
    let err = world
        .place(
            &service_request(),
            LeaseId::from_u64(3),
            OwnerId::from_u64(3),
            None,
            100,
            20,
        )
        .unwrap_err();
    assert!(matches!(err, Error::Quarantined(_) | Error::Refused { .. }));
    for node in &machines {
        world
            .apply(archon_kernel::Command::UnquarantineNode { node: *node })
            .unwrap();
    }
    world
        .apply(archon_kernel::Command::UnquarantineNode { node: machine })
        .unwrap();
    place_ready(&mut world, 3);
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(3)].state,
        LeaseState::Active
    );
}

#[test]
fn replay_matches_digest() {
    let mut world = boot();
    place_ready(&mut world, 1);
    world
        .apply(archon_kernel::Command::RenewLease {
            lease: LeaseId::from_u64(1),
            new_expires_at: 150,
        })
        .unwrap();
    world.set_now(150);
    world.expire_due().unwrap();
    world.deliver_all().unwrap();
    let replayed = world.replay_trace().unwrap();
    assert_eq!(world.digest(), replayed.digest());
}

#[test]
fn no_agreement_refuses_new_lease() {
    let mut world = boot();
    world.set_agreement(false);
    let err = world
        .place(
            &service_request(),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            None,
            100,
            20,
        )
        .unwrap_err();
    assert!(matches!(err, Error::NotAgreed));
}

#[test]
fn activate_requires_prepared_bindings() {
    let mut world = boot();
    world
        .place(
            &service_request(),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            None,
            100,
            20,
        )
        .unwrap();
    world
        .apply(archon_kernel::Command::OpenBinding {
            binding: BindingId::from_u64(1),
            lease: LeaseId::from_u64(1),
            node: world.cluster.leases[&LeaseId::from_u64(1)]
                .allocation
                .claims[0]
                .node,
            provider: ProviderId::ENFORCE,
        })
        .unwrap();
    let err = world
        .apply(archon_kernel::Command::ActivateLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap_err();
    assert!(matches!(err, Error::BindingsNotPrepared { .. }));
}

#[test]
fn child_stays_within_parent() {
    let mut world = boot();
    place_ready(&mut world, 1);
    let parent = world.cluster.leases[&LeaseId::from_u64(1)]
        .allocation
        .clone();
    world
        .apply(archon_kernel::Command::OpenLease {
            lease: LeaseId::from_u64(2),
            owner: OwnerId::from_u64(2),
            allocation: parent,
            parent: Some(LeaseId::from_u64(1)),
            expires_at: 100,
            prepare_deadline: 20,
            priority: 1,
        })
        .unwrap();
    world
        .apply(archon_kernel::Command::ActivateLease {
            lease: LeaseId::from_u64(2),
        })
        .unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Active
    );
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(2)].parent,
        Some(LeaseId::from_u64(1))
    );
}
