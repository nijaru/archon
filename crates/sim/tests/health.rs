use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_sim::{World, tiny_graph};

fn boot(degraded_machine: Option<usize>) -> World {
    let mut world = World::new();
    let mut graph = tiny_graph(2);
    if let Some(index) = degraded_machine {
        graph.with_degraded(index);
    }
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
        class: RequestClass::Service,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        machine_local: true,
        lifetime: 100,
        priority: 10,
    }
}

fn machine_of(world: &World, lease: u64) -> archon_kernel::NodeId {
    let claim = world.cluster.leases[&LeaseId::from_u64(lease)]
        .allocation
        .claims[0]
        .node;
    world.cluster.graph.machine_of(claim).unwrap()
}

fn admit_on(world: &mut World, request_id: u64, lease: u64) {
    world.enqueue(cpu_request(request_id), OwnerId::from_u64(lease));
    assert_eq!(
        world
            .admit_next(LeaseId::from_u64(lease), OwnerId::from_u64(lease))
            .unwrap(),
        Some(RequestId::from_u64(request_id))
    );
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(lease)).unwrap();
    world.deliver_all().unwrap();
}

#[test]
fn healthy_machine_is_preferred() {
    let mut world = boot(Some(0));
    admit_on(&mut world, 1, 1);
    let machines: Vec<_> = world
        .cluster
        .graph
        .nodes_of_kind(archon_kernel::NodeKind::Machine)
        .to_vec();
    let degraded = machines[0];
    assert_eq!(
        machine_of(&world, 1),
        machines[1],
        "must avoid the degraded machine"
    );
    assert_ne!(degraded, machines[1]);
}

#[test]
fn degraded_stays_usable_when_healthy_is_full() {
    let mut world = boot(Some(0));
    // Both CPUs of the healthy machine go first.
    admit_on(&mut world, 1, 1);
    admit_on(&mut world, 2, 2);
    // The degraded machine still serves the request.
    admit_on(&mut world, 3, 3);
    let claim = world.cluster.leases[&LeaseId::from_u64(3)]
        .allocation
        .claims[0]
        .node;
    assert!(
        world.cluster.graph.degraded_ancestor(claim).is_some(),
        "degraded capacity must remain usable"
    );
    assert!(
        world.cluster.leases[&LeaseId::from_u64(3)]
            .allocation
            .explanation
            .contains("health-degraded")
    );
}

#[test]
fn runtime_degrade_avoids_machine_without_touching_running_leases() {
    let mut world = boot(None);
    admit_on(&mut world, 1, 1);
    let first = machine_of(&world, 1);
    let machines: Vec<_> = world
        .cluster
        .graph
        .nodes_of_kind(archon_kernel::NodeKind::Machine)
        .to_vec();
    let other = *machines.iter().find(|id| **id != first).unwrap();

    // Degrade the machine holding lease 1 at runtime.
    world.set_health(first, "degraded").unwrap();
    assert_eq!(
        world.cluster.graph.revision,
        world.cluster.leases[&LeaseId::from_u64(1)]
            .allocation
            .graph_revision,
        "health updates must not advance the graph revision"
    );
    assert_eq!(
        world.cluster.leases[&LeaseId::from_u64(1)].state,
        archon_kernel::LeaseState::Active,
        "running leases are untouched by health"
    );

    // New placement avoids the degraded machine.
    admit_on(&mut world, 2, 2);
    assert_eq!(machine_of(&world, 2), other);
}
