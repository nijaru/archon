use fleet_kernel::{
    Dimension, LeaseId, LeaseState, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use fleet_sim::{World, tiny_graph};

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

/// A two-member request: two GPUs and two CPUs that must land together as
/// one atomic allocation spanning both machines.
fn multi_member(id: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![
            Need {
                kind: NodeKind::Gpu,
                quantity: qty(Dimension::Count, 2),
                filters: vec![],
            },
            Need {
                kind: NodeKind::Cpu,
                quantity: qty(Dimension::Count, 2),
                filters: vec![],
            },
        ],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        lifetime: 100,
        priority,
    }
}

fn gpu_hold(world: &mut World, lease: u64, expires_at: u64) {
    let request = Request {
        id: RequestId::from_u64(lease),
        class: RequestClass::Service,
        needs: vec![Need {
            kind: NodeKind::Gpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        lifetime: 100,
        priority: 50,
    };
    world
        .place(
            &request,
            LeaseId::from_u64(lease),
            OwnerId::from_u64(lease),
            None,
            expires_at,
            20,
        )
        .unwrap();
    world
        .bind_enforced(LeaseId::from_u64(lease), 1_000)
        .unwrap();
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(lease)).unwrap();
    world.deliver_all().unwrap();
}

#[test]
fn multi_member_request_places_atomically_or_not_at_all() {
    let mut world = boot();
    gpu_hold(&mut world, 1, 1_000);

    // One member's worth of resources is free, but the request needs both:
    // selection must refuse entirely, never place half the members.
    world.enqueue(multi_member(2, 100), OwnerId::from_u64(2));
    assert!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap()
            .is_none()
    );
    assert_eq!(world.queue.len(), 1);

    // Backfill may run short non-delaying work while the blocked request waits.
    world.set_now(2);
    let short = Request {
        id: RequestId::from_u64(3),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        lifetime: 5,
        priority: 1,
    };
    world.enqueue(short, OwnerId::from_u64(3));
    let admission = world
        .admit_next_backfill(LeaseId::from_u64(3), OwnerId::from_u64(3))
        .unwrap();
    assert_eq!(admission, Some(RequestId::from_u64(3)));
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(3)).unwrap();
    world.deliver_all().unwrap();

    // When the held GPU frees and the backfilled short job expires, the request
    // lands as one lease with all four claims and one activation.
    world.release_lease(LeaseId::from_u64(1)).unwrap();
    world.set_now(7);
    world.expire_due().unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world
            .admit_next(LeaseId::from_u64(4), OwnerId::from_u64(2))
            .unwrap(),
        Some(RequestId::from_u64(2))
    );
    world.deliver_all().unwrap();
    let lease = &world.cluster.leases[&LeaseId::from_u64(4)];
    assert_eq!(lease.state, LeaseState::Preparing);
    assert_eq!(lease.allocation.claims.len(), 4);
    world.activate_lease(LeaseId::from_u64(4)).unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(4)].state,
        LeaseState::Active
    );
}
