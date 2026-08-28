//! Linux-only cgroup v2 enforcement tests. Run as root (or with a delegated
//! cgroup subtree); skipped elsewhere. These prove that lease claims become
//! real kernel limits and that group kill reclaims everything.

#![cfg(target_os = "linux")]

use std::fs;
use std::time::{Duration, Instant};

use archon_kernel::{
    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::protocol::LeaseLimits;
use archon_node::runtime::{ProcessRuntime, WorkStatus};
use archon_node::service::NodeService;

/// Per-test cgroup roots: tests run in parallel threads and must not
/// trample each other's groups.
fn root(name: &str) -> String {
    format!("/sys/fs/cgroup/archon-test-{name}")
}

/// Skip unless this process may create cgroups (root or a delegated
/// subtree). CI runners and developer laptops skip; enforcement hosts run.
fn require_cgroup_writable(name: &str) -> bool {
    let root = root(name);
    match fs::create_dir(&root) {
        Ok(()) => {
            let _ = fs::remove_dir(&root);
            true
        }
        Err(err) => {
            eprintln!("skipping: cannot create cgroups at {root}: {err}");
            false
        }
    }
}

fn cleanup_root(root: &str) {
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let _ = fs::remove_dir(entry.path());
        }
    }
    let _ = fs::remove_dir(root);
}

fn request(id: u64, command: Vec<String>, memory_mib: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: ResourceClass::Memory,
                quantity: qty(CapacityDimension::Bytes, memory_mib * (1 << 20)),
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
    let root = root("first-instruction");
    if !require_cgroup_writable("first-instruction") {
        return;
    }
    cleanup_root(&root);
    let mut runtime = ProcessRuntime::new().with_cgroup_root(root.clone());
    let lease = LeaseId::from_u64(1);
    runtime
        .activate(
            lease,
            &[
                "sh".into(),
                "-c".into(),
                "grep -qx '0::/archon-test-first-instruction/lease-1' /proc/self/cgroup".into(),
            ],
            &LeaseLimits {
                cpu_count: 1,
                memory_bytes: 64 * (1 << 20),
            },
            &[],
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
    cleanup_root(&root);
}

#[test]
fn failed_launch_removes_the_lease_cgroup() {
    let root = root("failed-launch");
    if !require_cgroup_writable("failed-launch") {
        return;
    }
    cleanup_root(&root);
    let mut runtime = ProcessRuntime::new().with_cgroup_root(root.clone());
    let lease = LeaseId::from_u64(1);
    let error = runtime.activate(
        lease,
        &["archon-command-that-does-not-exist".into()],
        &LeaseLimits {
            cpu_count: 1,
            memory_bytes: 64 * (1 << 20),
        },
        &[],
    );
    assert!(error.is_err());
    assert!(!fs::exists(format!("{root}/lease-{}", lease.as_u64())).unwrap());
    cleanup_root(&root);
}

#[test]
fn lease_claims_become_kernel_limits() {
    let root = root("limits");
    if !require_cgroup_writable("limits") {
        return;
    }
    cleanup_root(&root);
    let mut service = NodeService::local(Some(root.clone()));
    service.submit(
        request(1, vec!["sleep".into(), "30".into()], 64),
        OwnerId::from_u64(1),
    );
    service.cluster.set_now(1);
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);

    let group = format!("{root}/lease-{}", lease.as_u64());
    // Activation is asynchronous: drive the service until the group
    // materializes (the same pumping is_running performs).
    let group_file = format!("{group}/cpu.max");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !fs::exists(&group_file).unwrap_or(false) {
        let _ = service.drive(Duration::from_millis(50));
    }
    assert!(
        fs::exists(&group_file).unwrap_or(false),
        "lease group must be created during activation; lease state = {:?}, root exists = {}",
        service.cluster.leases.get(&lease).map(|l| l.state),
        fs::exists(&root).unwrap_or(false),
    );
    let cpu_max = fs::read_to_string(&group_file).unwrap();
    assert_eq!(cpu_max.trim(), "100000 100000", "1 cpu claim = one core");
    let memory_max = fs::read_to_string(format!("{group}/memory.max")).unwrap();
    assert_eq!(memory_max.trim(), (64 * (1 << 20)).to_string());
    let swap_max = fs::read_to_string(format!("{group}/memory.swap.max")).unwrap();
    assert_eq!(
        swap_max.trim(),
        "0",
        "memory claims must not spill into swap"
    );
    let procs = fs::read_to_string(format!("{group}/cgroup.procs")).unwrap();
    assert!(!procs.trim().is_empty(), "sleep must run inside the group");

    service.revoke(lease).unwrap();
    assert!(!service.is_running(lease), "revoke must kill the group");
    assert!(
        wait_until(Duration::from_secs(5), || {
            let _ = service.drive(Duration::from_millis(50));
            !fs::exists(&group).unwrap_or(false)
        }),
        "group must be removed after the fence is acknowledged"
    );
    cleanup_root(&root);
}

#[test]
fn memory_limit_kills_an_overallocating_process() {
    let root = root("oom");
    if !require_cgroup_writable("oom") {
        return;
    }
    cleanup_root(&root);
    let mut service = NodeService::local(Some(root.clone()));
    // Keep a large string live in awk so the 16 MiB limit OOM-kills the
    // workload without depending on a utility's buffering behavior.
    service.submit(
        request(
            1,
            vec![
                "awk".into(),
                "BEGIN { a=sprintf(\"%*s\", 67108864, \"x\"); while (1) system(\"sleep 1\") }"
                    .into(),
            ],
            16,
        ),
        OwnerId::from_u64(1),
    );
    service.cluster.set_now(1);
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);

    let events_path = format!("{root}/lease-{}/memory.events", lease.as_u64());
    let oom_seen = wait_until(Duration::from_secs(10), || {
        // `admit_one` queues asynchronous effects; drive them while waiting
        // so the agent has a chance to create the cgroup and launch awk.
        let _ = service.drive(Duration::from_millis(50));
        fs::read_to_string(&events_path).is_ok_and(|events| {
            events.lines().any(|line| {
                line.strip_prefix("oom_kill ")
                    .and_then(|count| count.trim().parse::<usize>().ok())
                    .is_some_and(|count| count >= 1)
            })
        })
    });
    assert!(oom_seen, "memory.max must OOM-kill the runaway process");
    let events = fs::read_to_string(&events_path).unwrap();
    let oom: usize = events
        .lines()
        .find_map(|line| {
            line.strip_prefix("oom_kill ")
                .map(|count| count.trim().parse().unwrap())
        })
        .unwrap_or(0);
    assert!(oom >= 1, "kernel must record an oom_kill, got {events}");

    service.revoke(lease).unwrap();
    cleanup_root(&root);
}

#[test]
fn claimed_devices_are_enforced_by_cgroup_device_filter() {
    let root = root("devices");
    if !require_cgroup_writable("devices") {
        return;
    }
    cleanup_root(&root);
    let mut runtime = ProcessRuntime::new().with_cgroup_root(root.clone());
    let lease = LeaseId::from_u64(2);
    let devices = vec![archon_node::protocol::DeviceAccess {
        id: "gpu0".into(),
        dev: "/dev/null".into(),
        paths: Vec::new(),
        cdi: None,
    }];
    runtime
        .activate(
            lease,
            &[
                "sh".into(),
                "-c".into(),
                "cat /dev/zero > /dev/null && cat /dev/urandom > /dev/null; test -r /dev/tty0 && echo tty-ok || echo tty-denied".to_string(),
            ],
            &LeaseLimits {
                cpu_count: 1,
                memory_bytes: 64 * (1 << 20),
            },
            &devices,
        )
        .expect("activation with device filter");

    // /dev/null is claimed, so reads/writes through it succeed; the log
    // captures the outcome of probing an unclaimed device node.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = String::new();
    while Instant::now() < deadline {
        output = ProcessRuntime::read_log(lease);
        if output.contains("tty-denied") || output.contains("tty-ok") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        output.contains("tty-denied"),
        "unclaimed devices must be denied by the cgroup-device filter, got: {output}"
    );
}
