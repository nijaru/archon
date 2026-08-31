//! End-to-end tests against real processes: a lease runs a real command and
//! lease termination kills it. These exercise the same seam production
//! enforcement will use.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use archon_kernel::{
    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, LeaseLimits, RuntimeCapabilities,
};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{AgentClient, LocalAgentClient, NodeService};

/// Hosted CI has no delegated cgroup subtree, but this suite is about the
/// higher-level process lifecycle rather than proving kernel CPU isolation.
/// Advertise that guarantee through an explicit test double while forwarding
/// every lifecycle operation to the real ProcessRuntime. `cgroup_linux.rs`
/// remains the proof that production CPU limits are actually enforced.
struct LifecycleProcessExecutor {
    inner: LocalAgentClient,
}

impl AgentClient for LifecycleProcessExecutor {
    fn call(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: ExecutionCapabilities {
                    process: RuntimeCapabilities {
                        available: true,
                        cpu_limit: true,
                        memory_limit: false,
                        device_isolation: false,
                        physical_cpu_placement: false,
                        numa_memory_placement: false,
                    },
                    container: RuntimeCapabilities::default(),
                },
            });
        }
        self.inner.call(request)
    }
}

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
    let description = archon_node::discover::try_describe().expect("local discovery");
    let mut service = NodeService::new();
    service
        .register_agent(
            description,
            Box::new(LifecycleProcessExecutor {
                inner: LocalAgentClient::new(LeaseAgent::new(ProcessRuntime::new())),
            }),
        )
        .expect("register lifecycle test machine");
    service
}

fn request(id: u64, command: Vec<String>) -> archon_node::workload::WorkloadSpec {
    archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(id),
            class: RequestClass::Batch,
            needs: vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            }],
            topology: vec![],
            preferences: vec![],
            data: vec![],
            lifetime: 3_600,
            priority: 1,
            machine_local: true,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: command.clone(),
            image: None,
            storage: vec![],
            ports: vec![],
            grace_secs: 0,
        },
        keep_alive: false,
    }
}

fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
#[test]
fn runtime_child_owner() {
    let Ok(pid_file) = std::env::var("ARCHON_RUNTIME_DEATH_HELPER") else {
        return;
    };
    let command = vec![
        "sh".into(),
        "-c".into(),
        "echo $$ > \"$1\"; exec sleep 120".into(),
        "sh".into(),
        pid_file,
    ];
    let mut runtime = ProcessRuntime::new();
    runtime
        .activate(LeaseId::from_u64(1), &command, &LeaseLimits::default(), &[])
        .expect("spawn process");
    std::thread::sleep(Duration::from_secs(120));
}

#[cfg(target_os = "linux")]
#[test]
fn agent_death_kills_uncontained_workload() {
    let pid_file = std::env::temp_dir().join(format!(
        "archon-runtime-death-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let mut owner = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "runtime_child_owner", "--nocapture"])
        .env("ARCHON_RUNTIME_DEATH_HELPER", &pid_file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn runtime owner");
    let pid_deadline = Instant::now() + Duration::from_secs(5);
    let workload_pid = loop {
        if let Ok(value) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = value.trim().parse::<u32>()
        {
            break pid;
        }
        assert!(
            Instant::now() < pid_deadline,
            "workload did not publish its pid"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(process_alive(workload_pid));

    owner.kill().expect("kill runtime owner");
    let _ = owner.wait();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && process_alive(workload_pid) {
        std::thread::sleep(Duration::from_millis(10));
    }
    if process_alive(workload_pid) {
        let _ = Command::new("kill")
            .args(["-KILL", &workload_pid.to_string()])
            .status();
    }
    let _ = std::fs::remove_file(pid_file);
    assert!(
        !process_alive(workload_pid),
        "agent death must kill its uncontained workload"
    );
}

#[test]
fn dropping_runtime_terminates_uncontained_process() {
    let pid_file = std::env::temp_dir().join(format!(
        "archon-runtime-drop-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let command = vec![
        "sh".into(),
        "-c".into(),
        "echo $$ > \"$1\"; exec sleep 30".into(),
        "sh".into(),
        pid_file.display().to_string(),
    ];
    let lease = LeaseId::from_u64(1);
    let mut runtime = ProcessRuntime::new();
    runtime
        .activate(lease, &command, &LeaseLimits::default(), &[])
        .expect("spawn process");
    let pid_deadline = Instant::now() + Duration::from_secs(2);
    let pid = loop {
        if let Ok(value) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = value.trim().parse::<u32>()
        {
            break pid;
        }
        assert!(
            Instant::now() < pid_deadline,
            "process did not publish its pid"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        process_alive(pid),
        "workload should be alive before runtime drop"
    );

    drop(runtime);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && process_alive(pid) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_alive(pid),
        "dropping the runtime must terminate its process"
    );
    let _ = std::fs::remove_file(pid_file);
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
    req.execution.grace_secs = 5;
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
