//! Repeatable scale measurements feeding PLAN §5 evidence.
//!
//! Structural metrics (counts, log growth, digests) are deterministic.
//! Wall-clock timings are informational only and must never be asserted:
//! they describe this machine, not the design.

use std::time::Instant;

use archon_kernel::{
    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};

use crate::{World, tiny_graph};

/// One scale point: synthetic cluster size and offered load.
#[derive(Clone, Copy, Debug)]
pub struct ScaleParams {
    pub machines: usize,
    pub requests: u64,
}

/// Deterministic outcome plus clearly-marked informational timings.
#[derive(Clone, Debug)]
pub struct ScaleReport {
    pub machines: usize,
    pub nodes: usize,
    pub requested: u64,
    pub placed: u64,
    pub queue_drained: bool,
    pub replay_matches: bool,
    pub epoch: u64,
    pub graph_revision: u64,
    pub leases: usize,
    pub bindings: usize,
    pub log_commands: usize,
    pub log_per_alloc: f64,
    /// Wall-clock seconds for setup; informational only.
    pub boot_secs: f64,
    /// Wall-clock seconds for the admit/place/activate loop; informational only.
    pub admit_secs: f64,
    /// placed / admit_secs; informational only.
    pub allocs_per_sec: f64,
}

impl std::fmt::Display for ScaleReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "machines={} nodes={} requested={} placed={} drained={} replay={}",
            self.machines,
            self.nodes,
            self.requested,
            self.placed,
            self.queue_drained,
            self.replay_matches
        )?;
        writeln!(
            f,
            "epoch={} revision={} leases={} bindings={} log_commands={} log_per_alloc={:.1}",
            self.epoch,
            self.graph_revision,
            self.leases,
            self.bindings,
            self.log_commands,
            self.log_per_alloc
        )?;
        writeln!(
            f,
            "boot_secs={:.3} (info) admit_secs={:.3} (info) allocs_per_sec={:.0} (info)",
            self.boot_secs, self.admit_secs, self.allocs_per_sec
        )
    }
}

fn gpu_request(id: u64, count: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Gpu,
            quantity: qty(CapacityDimension::Count, count),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        machine_local: true,
        lifetime: 1_000,
        priority: 1,
    }
}

/// Boot a synthetic cluster, offer `params.requests` GPU batch requests,
/// and drive admission to completion. Mirrors the `scale.rs` lifecycle
/// (admit, deliver, activate; expire-and-retry when the head does not fit)
/// so the measured path is the proven one.
pub fn measure_scale(params: &ScaleParams) -> ScaleReport {
    let boot_start = Instant::now();
    let mut world = World::new();
    let graph = tiny_graph(params.machines);
    world
        .apply_graph(graph.nodes, graph.edges)
        .expect("register graph");
    for machine in &graph.machines {
        world
            .register_agent(machine.machine)
            .expect("register agent");
    }
    world.deliver_all().expect("hello");
    world.set_now(1);
    let nodes = world.cluster.graph.nodes().count();
    let boot_secs = boot_start.elapsed().as_secs_f64();

    for id in 1..=params.requests {
        world.enqueue(gpu_request(id, 1), OwnerId::from_u64(id));
    }
    let admit_start = Instant::now();
    let mut placed = 0u64;
    let mut lease_seq = 0u64;
    while !world.queue.is_empty() {
        lease_seq += 1;
        if lease_seq > params.requests + 10_000 {
            panic!("scale runaway: queue never drains");
        }
        let lease = LeaseId::from_u64(lease_seq);
        match world.admit_next(lease, OwnerId::from_u64(1)) {
            Ok(Some(_)) => {
                placed += 1;
                world.deliver_all().expect("deliver");
                world.activate_lease(lease).expect("activate");
                world.deliver_all().expect("activate deliver");
            }
            Ok(None) => {
                // Queue head does not fit: expire everything due and retry.
                world.set_now(world.cluster.now + 2_000);
                world.expire_due().expect("expire");
                world.deliver_all().expect("reap");
            }
            Err(err) => panic!("admission failed at scale: {err}"),
        }
    }
    let admit_secs = admit_start.elapsed().as_secs_f64();

    let digest = world.digest();
    let replay_matches = world
        .replay_trace()
        .map(|replayed| replayed.digest() == digest)
        .unwrap_or(false);
    let log_commands = world.cluster.log.len();
    ScaleReport {
        machines: params.machines,
        nodes,
        requested: params.requests,
        placed,
        queue_drained: world.queue.is_empty(),
        replay_matches,
        epoch: digest.epoch,
        graph_revision: digest.graph_revision,
        leases: digest.leases.len(),
        bindings: digest.bindings.len(),
        log_commands,
        log_per_alloc: log_commands as f64 / placed.max(1) as f64,
        boot_secs,
        admit_secs,
        allocs_per_sec: placed as f64 / admit_secs.max(1e-9),
    }
}
