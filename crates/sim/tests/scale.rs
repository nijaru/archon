//! Scale smoke test: placement and admission stay bounded and correct on a
//! graph far larger than the v0 proofs use. The kernel gate requires
//! resource graph and placement queries to remain bounded; this guards the
//! walk paths against accidental unbounded growth.

use archon_kernel::{
    Dimension, KindUsage, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_sim::{World, tiny_graph};

const MACHINES: usize = 40;
const REQUESTS: u64 = 120;

fn boot() -> (World, Vec<archon_sim::MachineIds>) {
    let mut world = World::new();
    let graph = tiny_graph(MACHINES);
    let machines = graph.machines.clone();
    world.apply_graph(graph.nodes, graph.edges).unwrap();
    for machine in &machines {
        world.register_agent(machine.machine).unwrap();
    }
    world.deliver_all().unwrap();
    world.set_now(1);
    (world, machines)
}

fn gpu_request(id: u64, count: u64) -> Request {
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
        lifetime: 1_000,
        priority: 1,
    }
}

#[test]
fn large_graph_admits_places_and_replays() {
    let (mut world, _machines) = boot();
    assert_eq!(world.cluster.graph.nodes().count(), 1 + MACHINES * 10);

    // Enqueue more requests than the graph can hold at once; every one must
    // eventually place as earlier leases expire.
    for id in 1..=REQUESTS {
        world.enqueue(gpu_request(id, 1), OwnerId::from_u64(id));
    }
    let mut placed = 0;
    for offset in 0..(REQUESTS + 8) {
        let lease = LeaseId::from_u64(offset + 1);
        match world.admit_next(lease, OwnerId::from_u64(1)) {
            Ok(Some(_)) => {
                placed += 1;
                world.deliver_all().unwrap();
                world.activate_lease(lease).unwrap();
                world.deliver_all().unwrap();
            }
            Ok(None) => {
                // Queue head does not fit: expire everything due and retry.
                world.set_now(world.cluster.now + 2_000);
                world.expire_due().unwrap();
                world.deliver_all().unwrap();
            }
            Err(err) => panic!("admission failed at scale: {err}"),
        }
        if world.queue.is_empty() {
            break;
        }
    }
    assert_eq!(placed, REQUESTS, "every request must eventually place");
    assert!(world.queue.is_empty());

    // Exclusive occupancy holds at scale: no GPU hosts two occupying leases.
    let mut gpu_claims = std::collections::BTreeMap::new();
    for lease in world.cluster.leases.values() {
        if !world.cluster.occupies(lease.id) {
            continue;
        }
        for claim in &lease.allocation.claims {
            let previous = gpu_claims.insert(claim.node, lease.id);
            assert!(previous.is_none(), "gpu {claim:?} double-claimed");
        }
    }

    // Replay the whole trace to an identical digest.
    let replayed = world.replay_trace().unwrap();
    assert_eq!(replayed.digest(), world.digest());
}

#[test]
fn fair_share_and_backfill_terminate_at_scale() {
    let (mut world, _machines) = boot();
    let ceiling = KindUsage::from([(NodeKind::Gpu, qty(Dimension::Count, 3))]);
    // One owner floods the queue; the per-kind ceiling caps its concurrency.
    for id in 1..=60 {
        world.enqueue(gpu_request(id, 1), OwnerId::from_u64(1));
    }
    let mut admitted = 0;
    for lease_id in 1..=200u64 {
        if world.queue.is_empty() || admitted >= 60 {
            break;
        }
        let lease = LeaseId::from_u64(lease_id);
        if world
            .admit_next_fair(lease, OwnerId::from_u64(1), &ceiling)
            .unwrap()
            .is_some()
        {
            admitted += 1;
            world.deliver_all().unwrap();
            world.activate_lease(lease).unwrap();
            world.deliver_all().unwrap();
        } else {
            world.set_now(world.cluster.now + 2_000);
            world.expire_due().unwrap();
            world.deliver_all().unwrap();
        }
    }
    assert_eq!(admitted, 60);
}
