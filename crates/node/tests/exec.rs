//! End-to-end tests against real processes: a lease runs a real command and
//! lease termination kills it. These exercise the same seam production
//! enforcement will use.

use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_node::service::NodeService;

fn boot() -> NodeService {
    // Isolate per-run output files from other binaries and prior runs.
    // Safe here: the env is read by this same test process's runtime.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var(
            "ARCHON_LOG_DIR",
            std::env::temp_dir().join(format!("archon-exec-logs-{}", std::process::id())),
        );
    }
    let mut service = NodeService::new();
    service
        .register_local(None)
        .expect("register local machine");
    service
}

fn request(id: u64, command: Vec<String>) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
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

#[test]
fn lease_runs_a_real_process() {
    let mut service = boot();
    service.submit(
        request(1, vec!["sleep".into(), "10".into()]),
        OwnerId::from_u64(1),
    );
    service.tick().unwrap();
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let lease = LeaseId::from_u64(1);
    assert!(service.is_running(lease), "sleep must run under the lease");
    service.revoke(lease).unwrap();
    assert!(!service.is_running(lease), "revoke must kill the process");
}

#[test]
fn lease_expiry_kills_the_process() {
    let mut service = boot();
    service.submit(
        request(1, vec!["sleep".into(), "10".into()]),
        OwnerId::from_u64(1),
    );
    // Shorten the lease so expiry is due immediately: manual clock, since
    // tick() would jump to real wall-clock time.
    service.cluster.set_now(1);
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);
    assert!(service.is_running(lease));
    service.cluster.set_now(3_601);
    let expired = service.expire_due().unwrap();
    assert_eq!(expired, vec![lease]);
    assert!(!service.is_running(lease), "expiry must kill the process");
}

#[test]
fn batch_completion_completes_the_lease_and_frees_claims() {
    let mut service = boot();
    service.submit(
        request(1, vec!["sleep".into(), "0".into()]),
        OwnerId::from_u64(1),
    );
    service.tick().unwrap();
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let lease = LeaseId::from_u64(1);

    // The workload finishes on its own; the controller notices and records
    // completion.
    let mut finished = Vec::new();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        finished = service.collect_completions().unwrap();
        if !finished.is_empty() {
            break;
        }
        service.tick().ok(); // refresh clock for backoff bookkeeping
    }
    assert_eq!(finished, vec![lease], "sleep 0 must be observed exiting");
    let lease_record = &service.cluster.leases[&lease];
    assert_eq!(lease_record.state, archon_kernel::LeaseState::Completed);
    assert!(
        !service
            .cluster
            .occupancy()
            .is_used(archon_kernel::NodeId::from_u64(1)),
        "completed claims must be freed"
    );

    // Idempotent: a second pass changes nothing.
    let again = service.collect_completions().unwrap();
    assert!(again.is_empty());
}

#[test]
fn failed_batch_exit_fails_the_lease() {
    let mut service = boot();
    service.submit(
        request(1, vec!["sh".into(), "-c".into(), "exit 3".into()]),
        OwnerId::from_u64(1),
    );
    service.tick().unwrap();
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);
    let mut finished = Vec::new();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        finished = service.collect_completions().unwrap();
        if !finished.is_empty() {
            break;
        }
        service.tick().ok();
    }
    assert_eq!(finished, vec![lease]);
    assert_eq!(
        service.cluster.leases[&lease].state,
        archon_kernel::LeaseState::Failed
    );
}

#[test]
fn drain_grace_lets_a_workload_finish_cleanly() {
    let mut service = boot();
    let mut req = request(
        1,
        vec![
            "sh".into(),
            "-c".into(),
            "trap 'exit 0' TERM; sleep 30".into(),
        ],
    );
    req.grace_secs = 5;
    service.submit(req, OwnerId::from_u64(1));
    service.tick().unwrap();
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);
    assert!(service.is_running(lease));

    // Revocation drains: TERM arrives, the trap exits 0 well within budget.
    let started = std::time::Instant::now();
    service.revoke(lease).unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "drain must return as soon as the workload exits, took {:?}",
        started.elapsed()
    );
    assert!(!service.is_running(lease));
}

#[test]
fn finished_work_reports_exit_code_and_output() {
    let mut service = boot();
    service.submit(
        request(
            1,
            vec![
                "sh".into(),
                "-c".into(),
                "echo hello-from-job; echo err-line >&2; exit 0".into(),
            ],
        ),
        OwnerId::from_u64(1),
    );
    service.tick().unwrap();
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);

    let mut finished = Vec::new();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        finished = service.collect_completions().unwrap();
        if !finished.is_empty() {
            break;
        }
    }
    assert_eq!(finished, vec![lease]);

    // The kernel records the outcome.
    assert_eq!(service.cluster.leases[&lease].exit_code, Some(0));

    // The agent returns the workload's output, stdout and stderr.
    let logs = service.lease_logs(lease).unwrap();
    assert!(
        logs.contains("hello-from-job") && logs.contains("err-line"),
        "logs must capture both streams, got {logs:?}"
    );
}

#[test]
fn failed_work_records_its_exit_code() {
    let mut service = boot();
    service.submit(
        request(
            1,
            vec!["sh".into(), "-c".into(), "echo before; exit 3".into()],
        ),
        OwnerId::from_u64(1),
    );
    service.tick().unwrap();
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !service.collect_completions().unwrap().is_empty() {
            break;
        }
    }
    assert_eq!(service.cluster.leases[&lease].exit_code, Some(3));
    let logs = service.lease_logs(lease).unwrap();
    assert!(logs.contains("before"), "output survives failure");
}
