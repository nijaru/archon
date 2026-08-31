//! Controller-restart reconciliation against a scripted agent endpoint:
//! provably-running workloads are adopted without respawn, natural exits
//! complete normally, unprovable ownership revokes through the protocol,
//! and partially prepared leases fail instead of activating.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use archon_kernel::{
    CapacityDimension, Command, LeaseId, LeaseState, Need, OwnerId, Request, RequestClass,
    RequestId, ResourceClass, qty,
};
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_greeting,
    read_request, write_response,
};
use archon_node::service::{NodeService, RemoteAgentClient};

fn recovery_capabilities() -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: false,
            device_isolation: false,
            physical_cpu_placement: false,
            numa_memory_placement: false,
        },
        container: RuntimeCapabilities::default(),
    }
}

#[derive(Default)]
struct FakeState {
    /// What the next Status answer reports.
    running: bool,
    exit_code: Option<i32>,
    /// Protocol counters: adoption must not mint a second activation.
    activates: u32,
    prepares: u32,
    fences: u32,
}

/// An agent endpoint whose live-workload answers are scripted. Serves any
/// number of controller connections concurrently; all share one state, so
/// a recovering controller observes the same endpoint as its predecessor.
fn spawn_fake_agent() -> (String, Arc<Mutex<FakeState>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let state = Arc::new(Mutex::new(FakeState {
        running: false,
        exit_code: None,
        ..Default::default()
    }));
    let shared = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // One thread per controller generation: a recovering
            // controller connects while its predecessor's socket lingers.
            let shared = shared.clone();
            std::thread::spawn(move || {
                let Ok(mut stream) = archon_node::transport::establish_responder(stream, None)
                else {
                    return;
                };
                let _ = read_greeting(&mut stream);
                let mut handle = 100u64;
                while let Ok(request) = read_request(&mut stream) {
                    let response = match request {
                        AgentRequest::Hello => AgentResponse::Welcome {
                            instance_id: "recovery-agent".into(),
                            name: "fake".into(),
                            cpus: 8,
                            memory_bytes: 16 << 30,
                            host_nodes: Vec::new(),
                            devices: Vec::new(),
                        },
                        AgentRequest::Capabilities => AgentResponse::Capabilities {
                            capabilities: recovery_capabilities(),
                        },
                        AgentRequest::Prepare { binding, .. } => {
                            shared.lock().unwrap().prepares += 1;
                            AgentResponse::Prepared {
                                binding,
                                handle: {
                                    handle += 1;
                                    handle
                                },
                            }
                        }
                        AgentRequest::Activate { binding, .. } => {
                            shared.lock().unwrap().activates += 1;
                            AgentResponse::Activated { binding }
                        }
                        AgentRequest::Fence { binding, .. } => {
                            shared.lock().unwrap().fences += 1;
                            AgentResponse::Fenced { binding }
                        }
                        AgentRequest::Release { binding, .. } => {
                            AgentResponse::Released { binding }
                        }
                        AgentRequest::Status { lease } => {
                            let state = shared.lock().unwrap();
                            AgentResponse::Running {
                                lease,
                                running: state.running,
                                exit_code: state.exit_code,
                            }
                        }
                        other => other.failed_response(),
                    };
                    if write_response(&mut stream, &response).is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, state)
}

trait FailedResponse {
    fn failed_response(&self) -> AgentResponse;
}

impl FailedResponse for AgentRequest {
    fn failed_response(&self) -> AgentResponse {
        AgentResponse::Failed {
            binding: 0,
            reason: format!("unexpected request {self:?}"),
        }
    }
}

fn register(service: &mut NodeService, addr: &str, instance_id: &str) -> archon_kernel::NodeId {
    let mut executor = RemoteAgentClient::connect(addr, None).expect("connect");
    let mut description = NodeService::hello(&mut executor).expect("hello");
    description.instance_id = instance_id.to_string();
    service
        .register_agent(description, Box::new(executor))
        .expect("register")
}

/// Read a controller command log without the control-plane crate: the
/// node crate must not depend upward, so parse the JSONL directly.
fn read_log(path: &std::path::Path) -> Vec<Command> {
    std::fs::read_to_string(path)
        .expect("read log")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("parse command"))
        .collect()
}

fn logged_sink(path: &std::path::Path) -> Box<dyn FnMut(&Command) + Send> {
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(std::io::BufWriter::new)
        .expect("open log");
    Box::new(move |command: &Command| {
        use std::io::Write;
        serde_json::to_writer(&mut log, command).expect("write command");
        log.write_all(b"\n").expect("newline");
        log.flush().expect("flush log");
    })
}

fn submit_sleep(service: &mut NodeService, id: u64) {
    let request = archon_node::workload::WorkloadSpec {
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
            machine_local: true,
            lifetime: 3_600,
            priority: 1,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: vec!["sleep".into(), "300".into()],
            image: None,
            storage: vec![],
            ports: vec![],
            grace_secs: 0,
        },
        keep_alive: false,
    };
    service.submit(request, OwnerId::from_u64(1));
    service.cluster.set_now(1);
    service.admit_one().expect("admit").expect("admitted");
}

fn settle(service: &mut NodeService, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline && !service.is_quiescent() {
        let _ = service.drive(Duration::from_millis(20));
    }
}

#[test]
fn restart_adopts_running_workload_without_respawn() {
    let dir = std::env::temp_dir().join(format!("archon-recovery-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("log.jsonl");

    let (addr, state) = spawn_fake_agent();

    // First controller generation: run one workload on the agent.
    let mut first = NodeService::new();
    first.set_command_sink(Some(logged_sink(&path)));
    register(&mut first, &addr, "inst-a");
    submit_sleep(&mut first, 1);
    state.lock().unwrap().running = true; // workload starts once activated
    settle(&mut first, 5);
    assert_eq!(
        first.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
    let old_session = *first.cluster.sessions.values().next().unwrap();
    let activates_before = state.lock().unwrap().activates;
    assert_eq!(activates_before, 1);

    // Second controller generation replays the same authority log while
    // the endpoint keeps enforcing its generation.
    let commands = read_log(&path);
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    register(&mut recovered, &addr, "inst-a");

    // Reconciliation adopts: no second activation may reach the agent.
    settle(&mut recovered, 5);
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active,
        "provably-current work survives the restart"
    );
    assert_eq!(recovered.live_lease_count(), 1);
    let new_session = *recovered.cluster.sessions.values().next().unwrap();
    assert_ne!(new_session, old_session, "sessions stay monotonic");
    for record in recovered.cluster.bindings.values() {
        assert_eq!(
            record.agent_session, new_session,
            "bindings ride the new session"
        );
        assert_eq!(record.state, archon_kernel::BindingState::Active);
    }
    assert_eq!(
        state.lock().unwrap().activates,
        activates_before,
        "adoption must never respawn the workload"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restart_revokes_work_the_endpoint_cannot_prove() {
    let dir = std::env::temp_dir().join(format!("archon-recovery-rv-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("log.jsonl");

    let (addr, state) = spawn_fake_agent();

    let mut first = NodeService::new();
    first.set_command_sink(Some(logged_sink(&path)));
    register(&mut first, &addr, "inst-rv");
    submit_sleep(&mut first, 1);
    state.lock().unwrap().running = true;
    settle(&mut first, 5);

    // The endpoint lost the workload (or cannot vouch for it): recovery
    // must revoke through the protocol and fence before reuse.
    state.lock().unwrap().running = false;
    let commands = read_log(&path);
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    register(&mut recovered, &addr, "inst-rv");
    settle(&mut recovered, 5);

    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Revoked,
        "unprovable ownership ends"
    );
    assert!(!recovered.cluster.occupies(LeaseId::from_u64(1)));
    assert!(
        state.lock().unwrap().fences >= 1,
        "revocation fenced the endpoint"
    );

    // Freed capacity admits fresh work immediately.
    submit_sleep(&mut recovered, 2);
    settle(&mut recovered, 5);
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(2)].state,
        LeaseState::Active,
        "fenced claims become reusable"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restart_fails_partially_prepared_leases_without_activation() {
    let dir = std::env::temp_dir().join(format!("archon-recovery-pp-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("log.jsonl");

    let (addr, state) = spawn_fake_agent();

    // Admit but never absorb the prepare ack: the crash catches the lease
    // mid-preparation, with an opened Binding and no proof anywhere.
    let mut first = NodeService::new();
    first.set_command_sink(Some(logged_sink(&path)));
    register(&mut first, &addr, "inst-pp");
    submit_sleep(&mut first, 1);
    assert_eq!(
        first.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Preparing
    );

    let commands = read_log(&path);
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    register(&mut recovered, &addr, "inst-pp");
    settle(&mut recovered, 5);

    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Failed,
        "partial preparation can never activate after a restart"
    );
    assert!(!recovered.cluster.occupies(LeaseId::from_u64(1)));
    assert_eq!(
        state.lock().unwrap().activates,
        0,
        "a failed preparation must not execute anything"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
