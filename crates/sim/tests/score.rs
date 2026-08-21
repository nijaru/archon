use fleet_kernel::{
    Dimension, Filter, LeaseId, Need, NodeKind, OwnerId, Preference, Request, RequestClass,
    RequestId, qty,
};
use fleet_sim::{World, tiny_graph};

fn boot(machines: usize) -> (World, fleet_sim::TinyGraph) {
    let mut world = World::new();
    let graph = tiny_graph(machines);
    world
        .apply_graph(graph.nodes.clone(), graph.edges.clone())
        .unwrap();
    for machine in &graph.machines {
        world.register_agent(machine.machine).unwrap();
    }
    world.deliver_all().unwrap();
    world.set_now(1);
    (world, graph)
}

fn cpu_request(id: u64, preferences: Vec<Preference>) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Service,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences,
        data: vec![],
        command: vec![],
        lifetime: 100,
        priority: 10,
    }
}

fn gpu_request(preferences: Vec<Preference>, filters: Vec<Filter>) -> Request {
    Request {
        id: RequestId::from_u64(9),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Gpu,
            quantity: qty(Dimension::Count, 1),
            filters,
        }],
        topology: vec![],
        preferences,
        data: vec![],
        command: vec![],
        lifetime: 100,
        priority: 10,
    }
}

fn place_cpu(world: &mut World, lease: u64, preferences: Vec<Preference>) {
    world
        .place(
            &cpu_request(lease, preferences),
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

fn claim_machine(world: &World, lease: u64) -> fleet_kernel::NodeId {
    let node = world.cluster.leases[&LeaseId::from_u64(lease)]
        .allocation
        .claims[0]
        .node;
    world.cluster.graph.machine_of(node).unwrap()
}

#[test]
fn pack_colocates_on_the_same_machine() {
    let (mut world, _) = boot(3);
    place_cpu(&mut world, 1, vec![Preference::Pack]);
    place_cpu(&mut world, 2, vec![Preference::Pack]);
    assert_eq!(claim_machine(&world, 1), claim_machine(&world, 2));
    assert!(
        world.cluster.leases[&LeaseId::from_u64(2)]
            .allocation
            .explanation
            .contains("Pack")
    );
}

#[test]
fn spread_separates_machines() {
    let (mut world, _) = boot(3);
    place_cpu(&mut world, 1, vec![Preference::Spread]);
    place_cpu(&mut world, 2, vec![Preference::Spread]);
    assert_ne!(claim_machine(&world, 1), claim_machine(&world, 2));
    assert!(
        world.cluster.leases[&LeaseId::from_u64(2)]
            .allocation
            .explanation
            .contains("Spread")
    );
}

#[test]
fn prefer_attr_is_soft() {
    let (mut world, graph) = boot(2);
    let a100 = graph.machines[1].gpu;
    let allocation = world
        .cluster
        .allocate(&gpu_request(
            vec![Preference::PreferAttr {
                key: "model".into(),
                value: "a100".into(),
            }],
            vec![],
        ))
        .unwrap();
    assert_eq!(allocation.claims[0].node, a100);
    assert!(allocation.explanation.contains("score="));

    world
        .place(
            &gpu_request(
                vec![],
                vec![Filter {
                    key: "model".into(),
                    value: "h100".into(),
                }],
            ),
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            None,
            100,
            20,
        )
        .unwrap();
    world.bind_enforced(LeaseId::from_u64(1), 10).unwrap();
    world.deliver_all().unwrap();
    world.activate_lease(LeaseId::from_u64(1)).unwrap();
    world.deliver_all().unwrap();

    let still = world
        .cluster
        .allocate(&gpu_request(
            vec![Preference::PreferAttr {
                key: "model".into(),
                value: "h100".into(),
            }],
            vec![],
        ))
        .unwrap();
    assert_eq!(still.claims[0].node, a100);
}

#[test]
fn hard_filter_still_refuses() {
    let (world, _) = boot(2);
    let err = world
        .cluster
        .allocate(&gpu_request(
            vec![],
            vec![Filter {
                key: "model".into(),
                value: "mi300".into(),
            }],
        ))
        .unwrap_err();
    assert!(matches!(err, fleet_kernel::Error::Refused { .. }));
}
