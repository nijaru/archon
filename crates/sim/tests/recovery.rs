//! Controller-restart reconciliation: a fresh authority adopts work whose
//! Binding generation is provably still enforced, fences ambiguous or stale
//! generations before reuse, fails partially prepared Leases, and keeps
//! expired-but-unfenced claims occupied until the fences land.

use archon_kernel::{
    BindingId, Dimension, Error, LeaseId, LeaseState, Need, NodeKind, OwnerId, ProviderId, Request,
    RequestClass, RequestId, qty,
};
use archon_sim::{World, tiny_graph};

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

fn cpu_request(id: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 2),
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
        lifetime: 100,
        keep_alive: false,
        priority: 1,
    }
}

/// Admit and fully activate lease `lease` on one machine.
fn occupy(world: &mut World, lease: u64, binding_start: u64) -> (archon_kernel::NodeId, BindingId) {
    world
        .place(
            &cpu_request(lease),
            LeaseId::from_u64(lease),
            OwnerId::from_u64(lease),
            None,
            100,
            20,
        )
        .unwrap();
    let bindings = world
        .bind_enforced(LeaseId::from_u64(lease), binding_start)
        .unwrap();
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(lease)).unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(lease)].state,
        LeaseState::Active
    );
    let claim = world.cluster.leases[&LeaseId::from_u64(lease)]
        .allocation
        .claims[0]
        .node;
    (claim, bindings[0])
}

fn machine_of(world: &World, claim: archon_kernel::NodeId) -> archon_kernel::NodeId {
    world.cluster.graph.machine_of(claim).unwrap()
}

fn endpoint(world: &World, node: archon_kernel::NodeId) -> &archon_kernel::Endpoint {
    &world.endpoints[&(ProviderId::ENFORCE, node)]
}

#[test]
fn controller_restart_preserves_provable_active_work() {
    let mut world = boot();
    let (claim, binding) = occupy(&mut world, 1, 10);
    let machine = machine_of(&world, claim);
    let old_session = world.cluster.sessions[&machine];
    let fence = endpoint(&world, claim).accepted_fence;
    assert!(endpoint(&world, claim).open);

    // The controller crashes; agents and endpoints keep enforcing.
    world.restart_controller().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active,
        "replay must restore the pre-crash authority exactly"
    );

    // The agent re-registers with a fresh monotonic session.
    world.register_agent(machine).unwrap();
    world.deliver_all().unwrap();

    let lease = &world.cluster.leases[&LeaseId::from_u64(1)];
    assert_eq!(lease.state, LeaseState::Active, "provable work is adopted");
    let record = &world.cluster.bindings[&binding];
    assert_eq!(record.state, archon_kernel::BindingState::Active);
    assert_eq!(
        record.fence, fence,
        "adoption must not mint a new generation"
    );
    assert_ne!(
        record.agent_session, old_session,
        "the adopted Binding rides the agent's new session"
    );
    assert!(world.cluster.occupies(LeaseId::from_u64(1)));
    let after = endpoint(&world, claim);
    assert!(after.open && after.binding == Some(binding) && after.accepted_fence == fence);

    // Deterministic replay reproduces the recovered state.
    assert_eq!(world.replay_trace().unwrap().digest(), world.digest());
}

#[test]
fn controller_restart_fences_lost_generations_before_reuse() {
    let mut world = boot();
    let (claim, _binding) = occupy(&mut world, 1, 10);
    let machine = machine_of(&world, claim);
    let key = (ProviderId::ENFORCE, claim);

    // The endpoint's generation becomes unknowable to the recovering
    // controller: no open binding can be proven there.
    let lost = world.endpoints.get_mut(&key).unwrap();
    lost.open = false;
    lost.binding = None;
    lost.phase = archon_kernel::EndpointPhase::Idle;

    world.restart_controller().unwrap();
    world.register_agent(machine).unwrap();
    world.deliver_all().unwrap();

    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Failed,
        "unprovable authority ends at reconciliation"
    );
    assert!(!world.cluster.occupies(LeaseId::from_u64(1)));

    // Only now may an exclusive claim reuse the freed capacity.
    world.set_now(200);
    world
        .place(
            &cpu_request(2),
            LeaseId::from_u64(2),
            OwnerId::from_u64(2),
            None,
            300,
            220,
        )
        .unwrap();
    world.bind_enforced(LeaseId::from_u64(2), 50).unwrap();
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(2)).unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Active
    );
    assert!(world.replay_trace().is_ok());
}

#[test]
fn preparing_lease_at_restart_cannot_activate() {
    let mut world = boot();
    world
        .place(
            &cpu_request(1),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            None,
            100,
            20,
        )
        .unwrap();
    let bindings = world.bind_enforced(LeaseId::from_u64(1), 10).unwrap();
    // Lose the Prepare effect: preparation never lands anywhere.
    while world.drop_one() {}
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Preparing
    );

    world.restart_controller().unwrap();
    let claim = world.cluster.leases[&LeaseId::from_u64(1)]
        .allocation
        .claims[0]
        .node;
    let machine = machine_of(&world, claim);
    world.register_agent(machine).unwrap();
    world.deliver_all().unwrap();

    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Failed,
        "a partially prepared Lease must not become Active after restart"
    );
    for binding in bindings {
        assert_eq!(
            world.cluster.bindings[&binding].state,
            archon_kernel::BindingState::Fenced,
            "its bindings are fenced before anything reuses the claims"
        );
    }
    assert_eq!(world.replay_trace().unwrap().digest(), world.digest());
}

#[test]
fn expired_unfenced_claims_stay_occupied_across_restart() {
    // One machine: every CPU claim conflicts with the expired lease.
    let mut world = World::new();
    let graph = tiny_graph(1);
    world.apply_graph(graph.nodes, graph.edges).unwrap();
    for machine in &graph.machines {
        world.register_agent(machine.machine).unwrap();
    }
    world.deliver_all().unwrap();
    world.set_now(1);

    let (claim, _binding) = occupy(&mut world, 1, 10);
    let machine = machine_of(&world, claim);

    // The lease expires but every fence effect is lost on the wire.
    world.set_now(200);
    world.expire_due().unwrap();
    while world.drop_one() {}
    assert!(
        world.cluster.occupies(LeaseId::from_u64(1)),
        "expiry alone never frees exclusive claims"
    );

    // A conflicting placement is refused until fencing completes.
    let err = world
        .place(
            &cpu_request(9),
            LeaseId::from_u64(9),
            OwnerId::from_u64(9),
            None,
            300,
            220,
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::Overlap { .. } | Error::Refused { .. }),
        "{err:?}"
    );

    // Restart + reconciliation proves the stale generation and fences it.
    world.restart_controller().unwrap();
    world.register_agent(machine).unwrap();
    world.deliver_all().unwrap();
    assert!(!world.cluster.occupies(LeaseId::from_u64(1)));

    world
        .place(
            &cpu_request(9),
            LeaseId::from_u64(9),
            OwnerId::from_u64(9),
            None,
            300,
            220,
        )
        .unwrap();
    world.bind_enforced(LeaseId::from_u64(9), 90).unwrap();
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(9)).unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(9)].state,
        LeaseState::Active
    );
}
