use fleet_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
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

fn request(id: u64, kind: NodeKind, count: u64, lifetime: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind,
            quantity: qty(Dimension::Count, count),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        lifetime,
        priority,
    }
}

fn hold_gpu(world: &mut World, lease: u64, expires_at: u64) {
    world
        .place(
            &request(lease, NodeKind::Gpu, 1, 100, 50),
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
fn backfill_starts_job_that_finishes_before_shadow() {
    let mut world = boot();
    hold_gpu(&mut world, 1, 1_000);
    // Head wants both GPUs; the held one frees at 1_000.
    world.enqueue(request(2, NodeKind::Gpu, 2, 100, 100), OwnerId::from_u64(2));
    // Low-priority job needs one GPU and finishes at 11, long before 1_000.
    world.enqueue(request(3, NodeKind::Gpu, 1, 10, 1), OwnerId::from_u64(3));
    assert_eq!(
        world
            .admit_next_backfill(LeaseId::from_u64(3), OwnerId::from_u64(3))
            .unwrap(),
        Some(RequestId::from_u64(3)),
        "short job must backfill past the blocked head"
    );
    assert_eq!(world.queue.len(), 1);
}

#[test]
fn backfill_skips_job_that_would_delay_the_head() {
    let mut world = boot();
    hold_gpu(&mut world, 1, 1_000);
    world.enqueue(request(2, NodeKind::Gpu, 2, 100, 100), OwnerId::from_u64(2));
    // Long GPU job finishes at 5_001, after the head's shadow, and claims a
    // GPU the head would take: it must wait.
    world.enqueue(request(3, NodeKind::Gpu, 1, 5_000, 1), OwnerId::from_u64(3));
    assert!(
        world
            .admit_next_backfill(LeaseId::from_u64(3), OwnerId::from_u64(3))
            .unwrap()
            .is_none()
    );
    // A short GPU job behind it still backfills.
    world.enqueue(request(4, NodeKind::Gpu, 1, 10, 1), OwnerId::from_u64(4));
    assert_eq!(
        world
            .admit_next_backfill(LeaseId::from_u64(4), OwnerId::from_u64(4))
            .unwrap(),
        Some(RequestId::from_u64(4))
    );
    // Head and the skipped delaying job both remain queued.
    assert_eq!(world.queue.len(), 2);
}

#[test]
fn non_conflicting_job_backfills_past_the_shadow() {
    let mut world = boot();
    hold_gpu(&mut world, 1, 1_000);
    world.enqueue(request(2, NodeKind::Gpu, 2, 100, 100), OwnerId::from_u64(2));
    // CPU-only job never claims a node the GPU head would take.
    world.enqueue(request(3, NodeKind::Cpu, 1, 5_000, 1), OwnerId::from_u64(3));
    assert_eq!(
        world
            .admit_next_backfill(LeaseId::from_u64(3), OwnerId::from_u64(3))
            .unwrap(),
        Some(RequestId::from_u64(3))
    );
}

#[test]
fn unsatisfiable_head_does_not_block() {
    let mut world = boot();
    world.enqueue(request(1, NodeKind::Gpu, 5, 100, 100), OwnerId::from_u64(1));
    world.enqueue(request(2, NodeKind::Gpu, 1, 100, 1), OwnerId::from_u64(2));
    assert_eq!(
        world
            .admit_next_backfill(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap(),
        Some(RequestId::from_u64(2)),
        "a request no cluster state can satisfy must not block others"
    );
}

#[test]
fn reservation_expiry_feeds_the_shadow() {
    let mut world = boot();
    world
        .reserve(
            &request(1, NodeKind::Gpu, 1, 100, 50),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            50,
        )
        .unwrap();
    world.enqueue(request(2, NodeKind::Gpu, 2, 100, 100), OwnerId::from_u64(2));
    // Finishes at 101, past the reservation shadow of 50, on a claimed GPU.
    world.enqueue(request(3, NodeKind::Gpu, 1, 100, 1), OwnerId::from_u64(3));
    assert!(
        world
            .admit_next_backfill(LeaseId::from_u64(3), OwnerId::from_u64(3))
            .unwrap()
            .is_none()
    );
    // Finishes at 11, before 50.
    world.enqueue(request(4, NodeKind::Gpu, 1, 10, 1), OwnerId::from_u64(4));
    assert_eq!(
        world
            .admit_next_backfill(LeaseId::from_u64(4), OwnerId::from_u64(4))
            .unwrap(),
        Some(RequestId::from_u64(4))
    );
}

#[test]
fn unproven_shadow_admits_only_disjoint_jobs() {
    let mut world = boot();
    hold_gpu(&mut world, 1, 1_000);
    // Release without delivering: the lease is Released but still occupies
    // its GPU through the open Binding, with no proven release time.
    world.release_lease(LeaseId::from_u64(1)).unwrap();

    // Head needs both GPUs; capacity frees only when the fence ack lands.
    world.enqueue(request(2, NodeKind::Gpu, 2, 100, 100), OwnerId::from_u64(2));
    // A long GPU job claims a node the head would take: must wait even though
    // the stale occupancy looks overdue.
    world.enqueue(request(3, NodeKind::Gpu, 1, 5_000, 1), OwnerId::from_u64(3));
    assert!(
        world
            .admit_next_backfill(LeaseId::from_u64(3), OwnerId::from_u64(3))
            .unwrap()
            .is_none()
    );
    // A CPU job shares nothing with the head: safe to backfill.
    world.enqueue(request(4, NodeKind::Cpu, 1, 5_000, 1), OwnerId::from_u64(4));
    assert_eq!(
        world
            .admit_next_backfill(LeaseId::from_u64(4), OwnerId::from_u64(4))
            .unwrap(),
        Some(RequestId::from_u64(4))
    );
}

#[test]
fn backfilled_lease_expires_before_shadow_and_frees_capacity() {
    let mut world = boot();
    hold_gpu(&mut world, 1, 1_000);
    world.enqueue(request(2, NodeKind::Gpu, 2, 100, 100), OwnerId::from_u64(2));
    world.enqueue(request(3, NodeKind::Gpu, 1, 10, 1), OwnerId::from_u64(3));
    let admitted = world
        .admit_next_backfill(LeaseId::from_u64(3), OwnerId::from_u64(3))
        .unwrap()
        .unwrap();
    assert_eq!(admitted, RequestId::from_u64(3));
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(3)).unwrap();
    world.deliver_all().unwrap();

    world.set_now(11);
    world.expire_due().unwrap();
    world.deliver_all().unwrap();
    // The backfilled job expired; the head still waits for lease 1 at 1_000.
    assert!(
        world
            .admit_next_backfill(LeaseId::from_u64(4), OwnerId::from_u64(4))
            .unwrap()
            .is_none()
    );
    assert_eq!(world.queue.len(), 1);
}

#[test]
fn quarantine_blocked_head_still_protects_capacity() {
    let mut world = boot();
    let graph_machines: Vec<_> = world
        .cluster
        .graph
        .nodes_of_kind(NodeKind::Machine)
        .to_vec();
    // Quarantine one machine: its GPU and CPUs are hard-filtered.
    world
        .apply(fleet_kernel::Command::QuarantineNode {
            node: graph_machines[0],
        })
        .unwrap();
    // Head needs both GPUs; one is behind quarantine, so it cannot select —
    // but quarantine is temporary, so the head must not be treated as dead.
    world.enqueue(request(1, NodeKind::Gpu, 2, 100, 100), OwnerId::from_u64(1));
    // A long GPU job claims the free GPU: it must wait, because it would
    // delay the head once the machine unquarantines.
    world.enqueue(request(2, NodeKind::Gpu, 1, 5_000, 1), OwnerId::from_u64(2));
    assert!(
        world
            .admit_next_backfill(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap()
            .is_none()
    );
    // Unquarantine: the head places immediately, unobstructed.
    world
        .apply(fleet_kernel::Command::UnquarantineNode {
            node: graph_machines[0],
        })
        .unwrap();
    assert_eq!(
        world
            .admit_next_backfill(LeaseId::from_u64(1), OwnerId::from_u64(1))
            .unwrap(),
        Some(fleet_kernel::RequestId::from_u64(1))
    );
}
