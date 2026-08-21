use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
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

fn request(id: u64, class: RequestClass, kind: NodeKind, count: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class,
        needs: vec![Need {
            kind,
            quantity: qty(Dimension::Count, count),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        lifetime: 100,
        priority,
    }
}

fn take(world: &mut World, lease: u64) -> RequestId {
    world
        .admit_next(LeaseId::from_u64(lease), OwnerId::from_u64(lease))
        .unwrap()
        .expect("queue should admit")
}

#[test]
fn priority_then_time() {
    let mut world = boot();
    world.enqueue(
        request(1, RequestClass::Service, NodeKind::Cpu, 1, 1),
        OwnerId::from_u64(1),
    );
    world.set_now(2);
    world.enqueue(
        request(2, RequestClass::Service, NodeKind::Cpu, 1, 10),
        OwnerId::from_u64(2),
    );
    assert_eq!(take(&mut world, 1), RequestId::from_u64(2));
    assert_eq!(take(&mut world, 2), RequestId::from_u64(1));
}

#[test]
fn waits_then_places_after_release() {
    let mut world = boot();
    world.enqueue(
        request(1, RequestClass::Batch, NodeKind::Gpu, 1, 1),
        OwnerId::from_u64(1),
    );
    world.enqueue(
        request(2, RequestClass::Batch, NodeKind::Gpu, 1, 1),
        OwnerId::from_u64(2),
    );
    world.enqueue(
        request(3, RequestClass::Batch, NodeKind::Gpu, 1, 1),
        OwnerId::from_u64(3),
    );
    take(&mut world, 1);
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();
    take(&mut world, 2);
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(2)).unwrap();
    world.deliver_all().unwrap();
    assert!(
        world
            .admit_next(LeaseId::from_u64(3), OwnerId::from_u64(3))
            .unwrap()
            .is_none()
    );
    assert_eq!(world.queue.len(), 1);
    world.release_lease(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();
    assert_eq!(take(&mut world, 3), RequestId::from_u64(3));
}

#[test]
fn waiting_large_request_does_not_block_other_kind() {
    let mut world = boot();
    world.enqueue(
        request(1, RequestClass::Batch, NodeKind::Gpu, 4, 100),
        OwnerId::from_u64(1),
    );
    world.enqueue(
        request(2, RequestClass::Service, NodeKind::Cpu, 1, 1),
        OwnerId::from_u64(2),
    );
    assert_eq!(take(&mut world, 1), RequestId::from_u64(2));
    assert_eq!(world.queue[0].request.id, RequestId::from_u64(1));
    assert_eq!(world.queue[0].request.class, RequestClass::Batch);
}
