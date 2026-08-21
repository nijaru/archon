use archon_kernel::{Dimension, KindUsage, NodeKind, OwnerId, qty};
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

fn cpu_request(id: u64, count: u64, priority: u32) -> archon_kernel::Request {
    // 3-CPU requests span the two 2-CPU machines by design.
    cpu_request_local(id, count, priority, count <= 2)
}

fn cpu_request_local(
    id: u64,
    count: u64,
    priority: u32,
    machine_local: bool,
) -> archon_kernel::Request {
    archon_kernel::Request {
        id: archon_kernel::RequestId::from_u64(id),
        class: archon_kernel::RequestClass::Service,
        needs: vec![archon_kernel::Need {
            kind: archon_kernel::NodeKind::Cpu,
            quantity: qty(Dimension::Count, count),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        machine_local,
        lifetime: 100,
        priority,
    }
}

#[test]
fn fair_share_serves_small_owners_first() {
    let mut world = boot();
    world.enqueue(cpu_request(1, 3, 10), OwnerId::from_u64(1));
    world.enqueue(cpu_request(2, 1, 10), OwnerId::from_u64(2));
    world.enqueue(cpu_request(3, 1, 10), OwnerId::from_u64(3));
    // Plain admission lets the big owner take 3 of 4 CPUs.
    assert_eq!(
        world
            .admit_next(archon_kernel::LeaseId::from_u64(1), OwnerId::from_u64(1))
            .unwrap()
            .unwrap(),
        archon_kernel::RequestId::from_u64(1)
    );
    assert_eq!(
        world
            .admit_next(archon_kernel::LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap()
            .unwrap(),
        archon_kernel::RequestId::from_u64(2)
    );
    assert!(
        world
            .admit_next(archon_kernel::LeaseId::from_u64(3), OwnerId::from_u64(3))
            .unwrap()
            .is_none()
    );
}

#[test]
fn fair_share_ceiling_lets_small_owners_through() {
    let mut world = boot();
    // Owner 1 already holds 1 CPU, so its 2-CPU request is over a 2-CPU
    // fair-share ceiling and is skipped.
    world
        .place(
            &cpu_request(1, 1, 1),
            archon_kernel::LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            None,
            100,
            20,
        )
        .unwrap();
    world
        .bind_enforced(archon_kernel::LeaseId::from_u64(1), 100)
        .unwrap();
    world.deliver_all().unwrap();
    world
        .activate_lease(archon_kernel::LeaseId::from_u64(1))
        .unwrap();
    world.deliver_all().unwrap();
    // The 2-CPU request intentionally spreads across both machines.
    world.enqueue(cpu_request_local(2, 2, 10, false), OwnerId::from_u64(1));
    world.enqueue(cpu_request(3, 1, 10), OwnerId::from_u64(2));
    world.enqueue(cpu_request(4, 1, 10), OwnerId::from_u64(3));
    // Fair-share ceiling of 2 CPUs per owner: owner 1 is over budget and is
    // skipped, so both small owners are served.
    let fair_share = KindUsage::from([(NodeKind::Cpu, qty(Dimension::Count, 2))]);
    assert_eq!(
        world
            .admit_next_fair(
                archon_kernel::LeaseId::from_u64(2),
                OwnerId::from_u64(2),
                &fair_share,
            )
            .unwrap()
            .unwrap(),
        archon_kernel::RequestId::from_u64(3)
    );
    assert_eq!(
        world
            .admit_next_fair(
                archon_kernel::LeaseId::from_u64(3),
                OwnerId::from_u64(3),
                &fair_share,
            )
            .unwrap()
            .unwrap(),
        archon_kernel::RequestId::from_u64(4)
    );
    // The big owner is still queued; it is not admitted yet.
    assert_eq!(world.queue.len(), 1);
    assert_eq!(
        world.queue[0].request.id,
        archon_kernel::RequestId::from_u64(2)
    );
    world.deliver_all().unwrap();
    world
        .activate_lease(archon_kernel::LeaseId::from_u64(2))
        .unwrap();
    world
        .activate_lease(archon_kernel::LeaseId::from_u64(3))
        .unwrap();
    world.deliver_all().unwrap();
    // The requesting owner's own 1-CPU lease still counts against its
    // budget, so its 2-CPU request stays blocked until that lease is
    // released.
    world
        .release_lease(archon_kernel::LeaseId::from_u64(1))
        .unwrap();
    world.deliver_all().unwrap();
    assert_eq!(
        world
            .admit_next_fair(
                archon_kernel::LeaseId::from_u64(4),
                OwnerId::from_u64(1),
                &fair_share,
            )
            .unwrap()
            .unwrap(),
        archon_kernel::RequestId::from_u64(2)
    );
}

#[test]
fn empty_ceiling_matches_plain_admission() {
    let mut world = boot();
    world.enqueue(cpu_request(1, 1, 10), OwnerId::from_u64(1));
    world.enqueue(cpu_request(2, 1, 10), OwnerId::from_u64(2));
    let empty = KindUsage::new();
    assert_eq!(
        world
            .admit_next_fair(
                archon_kernel::LeaseId::from_u64(1),
                OwnerId::from_u64(1),
                &empty,
            )
            .unwrap()
            .unwrap(),
        archon_kernel::RequestId::from_u64(1)
    );
    assert_eq!(
        world
            .admit_next_fair(
                archon_kernel::LeaseId::from_u64(2),
                OwnerId::from_u64(2),
                &empty,
            )
            .unwrap()
            .unwrap(),
        archon_kernel::RequestId::from_u64(2)
    );
}
