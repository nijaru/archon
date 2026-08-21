//! Fleet walking skeleton: one machine, real processes under leases.
//!
//! `fleet-node demo` boots a Cluster over this machine's discovered
//! resources, submits a `sleep` workload, proves it is running, then
//! revokes the lease and proves the process died. That end-to-end path —
//! request, admission, lease, binding, execution, fencing — is the product
//! this repository exists to build.

#[cfg(target_os = "linux")]
mod cgroup;
mod discover;
mod runtime;
mod service;

use std::time::Duration;

use fleet_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};

use crate::service::NodeService;

fn main() {
    let mut service = NodeService::new();
    #[cfg(target_os = "linux")]
    if let Ok(root) = std::env::var("FLEET_CGROUP_ROOT") {
        service = NodeService::with_cgroups(root);
        println!("fleet: cgroup v2 enforcement enabled");
    }
    let (local, nodes, edges) = discover::discover();
    service.boot(nodes, edges).expect("boot cluster");
    let cpus = local.cpus.len();
    let memory_gib = {
        let node = service
            .cluster
            .graph
            .node(local.memory)
            .expect("memory node");
        node.capacity
            .iter()
            .find(|(dimension, _)| **dimension == Dimension::Bytes)
            .map(|(_, amount)| amount / (1 << 30))
            .unwrap_or(0)
    };
    println!(
        "fleet: discovered {} ({} cpus, {memory_gib} GiB)",
        service
            .cluster
            .graph
            .node(local.machine)
            .and_then(|node| node.attrs.get("name"))
            .cloned()
            .unwrap_or_else(|| "machine".into()),
        cpus,
    );

    // Submit a real workload: sleep 5 under a 30-second lease.
    let request = Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec!["sleep".into(), "5".into()],
        lifetime: 30,
        priority: 1,
    };
    service.submit(
        request,
        OwnerId::from_u64(1),
        vec!["sleep".into(), "5".into()],
    );
    service.tick().expect("tick");
    let admitted = service.admit_one().expect("admit");
    assert_eq!(admitted, Some(RequestId::from_u64(1)));
    let lease = LeaseId::from_u64(1);
    assert!(
        service.is_running(lease),
        "sleep must be running under the lease"
    );
    println!("fleet: lease 1 active — `sleep 5` is running as a real process");

    // Revoke: the lease fences and the process dies immediately.
    std::thread::sleep(Duration::from_millis(500));
    service.revoke(lease).expect("revoke");
    assert!(
        !service.is_running(lease),
        "process must die with the lease"
    );
    println!("fleet: lease 1 revoked — process terminated");
    println!("fleet: walking skeleton complete");
}
