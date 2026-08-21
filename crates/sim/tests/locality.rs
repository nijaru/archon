use archon_kernel::{
    Dimension, LeaseId, Need, NodeId, NodeKind, OwnerId, Preference, Request, RequestClass,
    RequestId, qty,
};
use archon_sim::{World, tiny_graph};

fn boot() -> (World, NodeId) {
    let mut world = World::new();
    let mut graph = tiny_graph(2);
    let dataset = graph.with_dataset(0, 10_000);
    world.apply_graph(graph.nodes, graph.edges).unwrap();
    for machine in &graph.machines {
        world.register_agent(machine.machine).unwrap();
    }
    world.deliver_all().unwrap();
    world.set_now(1);
    (world, dataset)
}

fn cpu_request(id: u64, data: Vec<NodeId>) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Service,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![Preference::Spread],
        data,
        command: vec![],
        machine_local: true,
        image: None,
        lifetime: 100,
        keep_alive: false,
        priority: 10,
    }
}

fn hold_cpu(world: &mut World, lease: u64) {
    hold_cpu_at(world, lease, 1_000 + lease);
}

fn hold_cpu_at(world: &mut World, lease: u64, binding: u64) {
    let request = Request {
        id: RequestId::from_u64(lease),
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
        image: None,
        lifetime: 100,
        keep_alive: false,
        priority: 10,
    };
    world
        .place(
            &request,
            LeaseId::from_u64(lease),
            OwnerId::from_u64(lease),
            None,
            100,
            20,
        )
        .unwrap();
    world
        .bind_enforced(LeaseId::from_u64(lease), binding)
        .unwrap();
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(lease)).unwrap();
    world.deliver_all().unwrap();
}

fn placed_node(world: &World, lease: u64) -> NodeId {
    world.cluster.leases[&LeaseId::from_u64(lease)]
        .allocation
        .claims[0]
        .node
}

#[test]
fn locality_outranks_spread() {
    let (mut world, dataset) = boot();
    // Load one CPU on the caching machine so plain Spread would avoid it.
    hold_cpu(&mut world, 1);
    world.enqueue(cpu_request(2, vec![dataset]), OwnerId::from_u64(2));
    assert_eq!(
        world
            .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
            .unwrap(),
        Some(RequestId::from_u64(2))
    );
    let node = placed_node(&world, 2);
    let caching = world.cluster.graph.caches(node, dataset);
    assert!(caching, "locality must beat spread-away-from-load");
    assert!(
        world.cluster.leases[&LeaseId::from_u64(2)]
            .allocation
            .explanation
            .contains("data-local")
    );
}

#[test]
fn without_data_spread_avoids_the_loaded_machine() {
    let (mut world, _dataset) = boot();
    hold_cpu(&mut world, 1);
    world.enqueue(cpu_request(2, vec![]), OwnerId::from_u64(2));
    world
        .admit_next(LeaseId::from_u64(2), OwnerId::from_u64(2))
        .unwrap()
        .unwrap();
    let node = placed_node(&world, 2);
    assert!(
        !world.cluster.graph.caches(node, NodeId::from_u64(10_000)),
        "spread should place on the unloaded, non-caching machine"
    );
}

#[test]
fn locality_is_soft_and_never_blocks() {
    let (mut world, dataset) = boot();
    // Occupy both CPUs on the caching machine.
    hold_cpu(&mut world, 1);
    hold_cpu_at(&mut world, 3, 2_000);
    world.enqueue(cpu_request(4, vec![dataset]), OwnerId::from_u64(4));
    assert_eq!(
        world
            .admit_next(LeaseId::from_u64(4), OwnerId::from_u64(4))
            .unwrap(),
        Some(RequestId::from_u64(4)),
        "missing locality must refuse nothing"
    );
    assert!(!world.cluster.graph.caches(placed_node(&world, 4), dataset));
}
