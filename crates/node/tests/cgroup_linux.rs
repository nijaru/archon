//! Linux-only cgroup v2 enforcement tests. Run as root (or with a delegated
//! cgroup subtree); skipped elsewhere. These prove that lease claims become
//! real kernel limits and that group kill reclaims everything.

#![cfg(target_os = "linux")]

use std::fs;
use std::time::{Duration, Instant};

use fleet_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use fleet_node::service::NodeService;

const ROOT: &str = "/sys/fs/cgroup/fleet-test";

fn cleanup_root() {
    if let Ok(entries) = fs::read_dir(ROOT) {
        for entry in entries.flatten() {
            let _ = fs::remove_dir(entry.path());
        }
    }
    let _ = fs::remove_dir(ROOT);
}

fn request(id: u64, command: Vec<String>, memory_mib: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![
            Need {
                kind: NodeKind::Cpu,
                quantity: qty(Dimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: NodeKind::Memory,
                quantity: qty(Dimension::Bytes, memory_mib * (1 << 20)),
                filters: vec![],
            },
        ],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: command.clone(),
        lifetime: 3_600,
        priority: 1,
    }
}

fn wait_until(deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    check()
}

#[test]
fn lease_claims_become_kernel_limits() {
    cleanup_root();
    let mut service = NodeService::with_cgroups(ROOT.into());
    let (_local, nodes, edges) = fleet_node::discover::discover();
    service.boot(nodes, edges).expect("boot");
    service.submit(
        request(1, vec!["sleep".into(), "30".into()], 64),
        OwnerId::from_u64(1),
        vec!["sleep".into(), "30".into()],
    );
    service.cluster.set_now(1);
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);

    let group = format!("{ROOT}/lease-{lease}");
    let cpu_max = fs::read_to_string(format!("{group}/cpu.max")).unwrap();
    assert_eq!(cpu_max.trim(), "100000 100000", "1 cpu claim = one core");
    let memory_max = fs::read_to_string(format!("{group}/memory.max")).unwrap();
    assert_eq!(memory_max.trim(), (64 * (1 << 20)).to_string());
    let procs = fs::read_to_string(format!("{group}/cgroup.procs")).unwrap();
    assert!(!procs.trim().is_empty(), "sleep must run inside the group");

    service.revoke(lease).unwrap();
    assert!(!service.is_running(lease), "revoke must kill the group");
    assert!(!fs::exists(&group).unwrap(), "group must be removed");
    cleanup_root();
}

#[test]
fn memory_limit_kills_an_overallocating_process() {
    cleanup_root();
    let mut service = NodeService::with_cgroups(ROOT.into());
    let (_local, nodes, edges) = fleet_node::discover::discover();
    service.boot(nodes, edges).expect("boot");
    // tail /dev/zero allocates without bound; the 16 MiB limit must OOM it.
    service.submit(
        request(1, vec!["tail".into(), "/dev/zero".into()], 16),
        OwnerId::from_u64(1),
        vec!["tail".into(), "/dev/zero".into()],
    );
    service.cluster.set_now(1);
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);

    let died = wait_until(Duration::from_secs(10), || !service.is_running(lease));
    assert!(died, "memory.max must OOM-kill the runaway process");
    let events = fs::read_to_string(format!("{ROOT}/lease-{lease}/memory.events")).unwrap();
    let oom: usize = events
        .lines()
        .find_map(|line| {
            line.strip_prefix("oom_kill ")
                .map(|count| count.trim().parse().unwrap())
        })
        .unwrap_or(0);
    assert!(oom >= 1, "kernel must record an oom_kill, got {events}");

    service.revoke(lease).unwrap();
    cleanup_root();
}
