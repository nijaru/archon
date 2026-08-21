use archon_kernel::{
    Dimension, LeaseId, LeaseState, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
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

fn gpu_request(id: u64, count: u64, priority: u32) -> Request {
    // 2-GPU requests span the two 1-GPU machines by design.
    let machine_local = count <= 1;
    gpu_request_local(id, count, priority, machine_local)
}

fn gpu_request_local(id: u64, count: u64, priority: u32, machine_local: bool) -> Request {
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
        command: vec![],
        machine_local,
        lifetime: 100,
        keep_alive: false,
        priority,
        image: None,
    }
}

fn occupy(world: &mut World, lease: u64, count: u64, priority: u32) {
    world
        .place(
            &gpu_request(lease, count, priority),
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
fn already_fits_has_no_victims() {
    let world = boot();
    let victims = archon_kernel::preempt_victims(&world.cluster, &gpu_request(9, 1, 10)).unwrap();
    assert!(victims.is_empty());
}

#[test]
fn equal_priority_is_not_preempted() {
    let mut world = boot();
    occupy(&mut world, 1, 2, 5);
    assert!(archon_kernel::preempt_victims(&world.cluster, &gpu_request(9, 2, 5)).is_none());
}

#[test]
fn higher_priority_evicts_then_places() {
    let mut world = boot();
    occupy(&mut world, 1, 2, 1);
    let incoming = gpu_request(9, 2, 10);
    let victims = world.preempt_for(&incoming).unwrap().unwrap();
    assert_eq!(victims, vec![LeaseId::from_u64(1)]);
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Revoked
    );
    assert!(world.cluster.occupies(LeaseId::from_u64(1)));
    world.deliver_all().unwrap();
    assert!(!world.cluster.occupies(LeaseId::from_u64(1)));
    world
        .place(
            &incoming,
            LeaseId::from_u64(9),
            OwnerId::from_u64(9),
            None,
            100,
            20,
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

#[test]
fn cannot_preempt_higher_priority() {
    let mut world = boot();
    occupy(&mut world, 1, 2, 20);
    assert!(archon_kernel::preempt_victims(&world.cluster, &gpu_request(9, 2, 10)).is_none());
}
