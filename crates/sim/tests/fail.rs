use fleet_kernel::{
    Dimension, Error, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
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

fn gpu_request(id: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Gpu,
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

fn occupy(world: &mut World, lease: u64, priority: u32) {
    world
        .place(
            &gpu_request(lease, priority),
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

fn machine_of(world: &World, lease: u64) -> fleet_kernel::NodeId {
    let claim = world.cluster.leases[&LeaseId::from_u64(lease)]
        .allocation
        .claims[0]
        .node;
    world.cluster.graph.machine_of(claim).unwrap()
}

#[test]
fn failed_machine_quarantines_and_releases() {
    let mut world = boot();
    occupy(&mut world, 1, 1);
    let machine = machine_of(&world, 1);
    let victims = world.fail_machine(machine).unwrap();
    assert_eq!(victims, vec![LeaseId::from_u64(1)]);
    assert!(world.cluster.quarantine.contains(&machine));
    // Occupancy persists while the machine is unreachable: no fence acks.
    world.deliver_all().unwrap();
    assert!(world.cluster.occupies(LeaseId::from_u64(1)));
    // A restarted Agent reconciles and acknowledges the fences.
    world.restart_agent(machine).unwrap();
    world.deliver_all().unwrap();
    assert!(!world.cluster.occupies(LeaseId::from_u64(1)));
    let other = world
        .cluster
        .graph
        .nodes_of_kind(NodeKind::Machine)
        .iter()
        .find(|id| **id != machine)
        .copied()
        .unwrap();
    world.fail_machine(other).unwrap();
    world.restart_agent(other).unwrap();
    world.deliver_all().unwrap();
    assert!(
        world
            .cluster
            .graph
            .nodes_of_kind(NodeKind::Machine)
            .iter()
            .all(|id| world.cluster.quarantine.contains(id))
    );
    let err = world
        .place(
            &gpu_request(9, 2),
            LeaseId::from_u64(9),
            OwnerId::from_u64(9),
            None,
            100,
            20,
        )
        .unwrap_err();
    assert!(matches!(err, Error::Quarantined(_) | Error::Refused { .. }));
}

#[test]
fn recovery_unquarantines_and_replaces() {
    let mut world = boot();
    occupy(&mut world, 1, 1);
    let machine = machine_of(&world, 1);
    world.fail_machine(machine).unwrap();
    assert!(world.cluster.occupies(LeaseId::from_u64(1)));
    // Unquarantine is blocked until the restarted Agent fences the bindings.
    assert!(world.unquarantine_machine(machine).is_err());
    world.restart_agent(machine).unwrap();
    world.deliver_all().unwrap();
    assert!(!world.cluster.occupies(LeaseId::from_u64(1)));
    world.unquarantine_machine(machine).unwrap();
    world.deliver_all().unwrap();
    assert!(!world.cluster.quarantine.contains(&machine));
    world
        .place(
            &gpu_request(9, 1),
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
        fleet_kernel::LeaseState::Active
    );
}

#[test]
fn other_machine_keeps_running() {
    let mut world = boot();
    occupy(&mut world, 1, 1);
    let machine = machine_of(&world, 1);
    world.fail_machine(machine).unwrap();
    world.restart_agent(machine).unwrap();
    world.deliver_all().unwrap();
    let other = world
        .cluster
        .graph
        .nodes_of_kind(NodeKind::Machine)
        .iter()
        .find(|id| **id != machine)
        .copied()
        .unwrap();
    world
        .place(
            &gpu_request(9, 1),
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
        fleet_kernel::LeaseState::Active
    );
    let claim = world.cluster.leases[&LeaseId::from_u64(9)]
        .allocation
        .claims[0]
        .node;
    assert_eq!(world.cluster.graph.machine_of(claim), Some(other));
}
