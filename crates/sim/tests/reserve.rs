use fleet_kernel::{Dimension, LeaseId, LeaseState, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty};
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

fn gpu_request(id: u64, count: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Gpu,
            quantity: qty(Dimension::Count, count),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        lifetime: 100,
        priority,
    }
}

fn cpu_request(id: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Service,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        lifetime: 100,
        priority,
    }
}

#[test]
fn reservation_blocks_admission_until_released() {
    let mut world = boot();
    world
        .reserve(
            &gpu_request(1, 2, 50),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            1_000,
        )
        .unwrap();
    assert_eq!(world.cluster.leases[&LeaseId::from_u64(1)].state, LeaseState::Reserved);

    world.enqueue(gpu_request(2, 1, 10), OwnerId::from_u64(2));
    assert!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap()
            .is_none(),
        "reserved GPUs must not be admittable"
    );

    world.release_lease(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap(),
        Some(RequestId::from_u64(2))
    );
}

#[test]
fn reservation_expires_and_frees_capacity() {
    let mut world = boot();
    world
        .reserve(
            &gpu_request(1, 2, 50),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            10,
        )
        .unwrap();
    world.enqueue(gpu_request(2, 1, 10), OwnerId::from_u64(2));
    assert!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap()
            .is_none()
    );

    world.set_now(10);
    world.expire_due().unwrap();
    assert_eq!(world.cluster.leases[&LeaseId::from_u64(1)].state, LeaseState::Expired);
    assert_eq!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap(),
        Some(RequestId::from_u64(2))
    );
}

#[test]
fn promoted_reservation_runs_the_ordinary_lifecycle() {
    let mut world = boot();
    world
        .reserve(
            &gpu_request(1, 1, 50),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            1_000,
        )
        .unwrap();
    world.promote(LeaseId::from_u64(1)).unwrap();
    assert_eq!(world.cluster.leases[&LeaseId::from_u64(1)].state, LeaseState::Preparing);
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();
    assert_eq!(world.cluster.leases[&LeaseId::from_u64(1)].state, LeaseState::Active);

    // The promoted lease now enforces exclusivity like any other lease: the
    // remaining GPU on the other machine admits, but the occupied one cannot
    // be double-claimed.
    world.enqueue(gpu_request(2, 2, 10), OwnerId::from_u64(2));
    assert!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap()
            .is_none()
    );
    let active_gpu = world
        .cluster
        .leases
        .get(&LeaseId::from_u64(1))
        .unwrap()
        .allocation
        .claims
        .iter()
        .find(|claim| {
            world.cluster.graph.node(claim.node).unwrap().kind == NodeKind::Gpu
        })
        .unwrap()
        .node;
    assert_eq!(
        world.cluster.occupancy().used_on(active_gpu),
        qty(Dimension::Count, 1),
        "promoted lease must occupy its claimed GPU"
    );
}

#[test]
fn higher_priority_request_preempts_a_reservation() {
    let mut world = boot();
    world
        .reserve(
            &gpu_request(1, 2, 1),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            1_000,
        )
        .unwrap();
    world.enqueue(gpu_request(2, 2, 100), OwnerId::from_u64(2));
    assert!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap()
            .is_none()
    );
    let victims = world.preempt_for(&gpu_request(2, 2, 100)).unwrap().unwrap();
    assert_eq!(victims, vec![LeaseId::from_u64(1)]);
    assert_eq!(world.cluster.leases[&LeaseId::from_u64(1)].state, LeaseState::Revoked);
    assert_eq!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap(),
        Some(RequestId::from_u64(2))
    );
}

#[test]
fn reservation_counts_toward_fair_share_usage() {
    let mut world = boot();
    world
        .reserve(
            &cpu_request(1, 1),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            1_000,
        )
        .unwrap();
    let usage = fleet_kernel::owner_usage(&world.cluster.leases);
    assert_eq!(
        usage.get(&OwnerId::from_u64(1)).map(|q| q.len()).unwrap_or(0),
        1,
        "reserved leases must charge their owner"
    );

    // An owner at its ceiling cannot queue more; another owner is unaffected.
    let fair = fleet_kernel::Quantity::from([(fleet_kernel::Dimension::Count, 1)]);
    world.enqueue(cpu_request(2, 1), OwnerId::from_u64(1));
    assert!(
        world
            .admit_next_fair(LeaseId::from_u64(2), OwnerId::from_u64(1), &fair)
            .unwrap()
            .is_none(),
        "reservation must consume the owner's budget"
    );
    world.enqueue(cpu_request(3, 1), OwnerId::from_u64(2));
    assert_eq!(
        world
            .admit_next_fair(LeaseId::from_u64(3), OwnerId::from_u64(2), &fair)
            .unwrap(),
        Some(RequestId::from_u64(3))
    );
}

#[test]
fn reservation_trace_replays_to_the_same_digest() {
    let mut world = boot();
    world
        .reserve(
            &gpu_request(1, 1, 50),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            1_000,
        )
        .unwrap();
    world.promote(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();
    world
        .reserve(
            &gpu_request(2, 1, 10),
            LeaseId::from_u64(2),
            OwnerId::from_u64(2),
            500,
        )
        .unwrap();
    world.set_now(20);
    world.expire_due().unwrap();
    world.release_lease(LeaseId::from_u64(2)).unwrap();

    let replayed = world.replay_trace().unwrap();
    assert_eq!(format!("{:?}", replayed.digest()), format!("{:?}", world.digest()));
}
