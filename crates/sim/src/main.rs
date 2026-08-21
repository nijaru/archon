use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_sim::{World, tiny_graph};

fn main() {
    let mut world = World::new();
    let graph = tiny_graph(2);
    world
        .apply_graph(graph.nodes, graph.edges)
        .expect("register graph");
    for machine in &graph.machines {
        world
            .register_agent(machine.machine)
            .expect("register agent");
    }
    world.deliver_all().expect("hello");
    let request = Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Service,
        needs: vec![
            Need {
                kind: NodeKind::Cpu,
                quantity: qty(Dimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: NodeKind::Memory,
                quantity: qty(Dimension::Bytes, archon_sim::GIB),
                filters: vec![],
            },
        ],
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
    world.set_now(1);
    world
        .place(
            &request,
            LeaseId::from_u64(1),
            OwnerId::from_u64(1),
            None,
            100,
            20,
        )
        .expect("place");
    world.bind_enforced(LeaseId::from_u64(1), 1).expect("bind");
    world.deliver_all().expect("prepare");
    world
        .activate_lease(LeaseId::from_u64(1))
        .expect("activate");
    world.deliver_all().expect("activate deliver");
    let digest = world.digest();
    println!(
        "epoch={} revision={} leases={} bindings={}\n{}",
        digest.epoch,
        digest.graph_revision,
        digest.leases.len(),
        digest.bindings.len(),
        world.cluster.leases[&LeaseId::from_u64(1)]
            .allocation
            .explanation
    );
}
