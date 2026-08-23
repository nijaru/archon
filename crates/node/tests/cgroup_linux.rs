//! Linux-only cgroup v2 enforcement tests. Run as root (or with a delegated
//! cgroup subtree); skipped elsewhere. These prove that lease claims become
//! real kernel limits and that group kill reclaims everything.

#![cfg(target_os = "linux")]

use std::fs;
use std::time::{Duration, Instant};

use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_node::protocol::LeaseLimits;
use archon_node::runtime::{ProcessRuntime, WorkStatus};
use archon_node::service::NodeService;

const ROOT: &str = "/sys/fs/cgroup/archon-test";

/// Skip unless this process may create cgroups (root or a delegated
/// subtree). CI runners and developer laptops skip; enforcement hosts run.
fn require_cgroup_writable() -> bool {
    match fs::create_dir(ROOT) {
        Ok(()) => {
            let _ = fs::remove_dir(ROOT);
            true
        }
        Err(err) => {
            eprintln!("skipping: cannot create cgroups at {ROOT}: {err}");
            false
        }
    }
}

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
        keep_alive: false,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        image: None,
        storage: vec![],
        ports: vec![],
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
fn first_instruction_runs_inside_the_lease_cgroup() {
    if !require_cgroup_writable() {
        return;
    }
    cleanup_root();
    let mut runtime = ProcessRuntime::new().with_cgroup_root(ROOT.into());
    let lease = LeaseId::from_u64(1);
    runtime
        .activate(
            lease,
            &[
                "sh".into(),
                "-c".into(),
                "grep -qx '0::/archon-test/lease-1' /proc/self/cgroup".into(),
            ],
            &LeaseLimits {
                cpu_count: 1,
                memory_bytes: 64 * (1 << 20),
            },
        )
        .unwrap();

    let mut exit_code = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match runtime.status(lease) {
            WorkStatus::Exited(code) => {
                exit_code = Some(code);
                break;
            }
            WorkStatus::Running => std::thread::sleep(Duration::from_millis(20)),
            WorkStatus::Gone => break,
        }
    }
    assert_eq!(
        exit_code,
        Some(0),
        "the first command must see its lease cgroup"
    );
    runtime.terminate(lease).unwrap();
    cleanup_root();
}

#[test]
fn failed_launch_removes_the_lease_cgroup() {
    if !require_cgroup_writable() {
        return;
    }
    cleanup_root();
    let mut runtime = ProcessRuntime::new().with_cgroup_root(ROOT.into());
    let lease = LeaseId::from_u64(1);
    let error = runtime.activate(
        lease,
        &["archon-command-that-does-not-exist".into()],
        &LeaseLimits {
            cpu_count: 1,
            memory_bytes: 64 * (1 << 20),
        },
    );
    assert!(error.is_err());
    assert!(!fs::exists(format!("{ROOT}/lease-{lease}")).unwrap());
    cleanup_root();
}

#[test]
fn lease_claims_become_kernel_limits() {
    if !require_cgroup_writable() {
        return;
    }
    cleanup_root();
    let mut service = NodeService::local(Some(ROOT.into()));
    service.submit(
        request(1, vec!["sleep".into(), "30".into()], 64),
        OwnerId::from_u64(1),
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
    if !require_cgroup_writable() {
        return;
    }
    cleanup_root();
    let mut service = NodeService::local(Some(ROOT.into()));
    // tail /dev/zero allocates without bound; the 16 MiB limit must OOM it.
    service.submit(
        request(1, vec!["tail".into(), "/dev/zero".into()], 16),
        OwnerId::from_u64(1),
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
