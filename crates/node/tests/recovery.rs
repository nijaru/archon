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
    AgentRequest, AgentResponse, EnumeratedEndpoint, ExecutionCapabilities, RuntimeCapabilities,
    read_greeting, read_request, write_response,
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

#[derive(Clone, Copy, Debug, Default)]
enum ReplyMode {
    #[default]
    Normal,
    Failed,
    Unexpected,
    Disconnect,
}

#[derive(Default)]
struct FakeState {
    /// What the next Status answer reports.
    running: bool,
    exit_code: Option<i32>,
    status_mode: ReplyMode,
    stop_mode: ReplyMode,
    stops: u32,
    starts: u32,
    /// Protocol counters: adoption must not mint a second activation.
    activates: u32,
    prepares: u32,
    fences: u32,
    /// Actual endpoint generations, with optional per-test scripted edits.
    endpoints: Vec<EnumeratedEndpoint>,
    /// Count EnumerateBindings requests served.
    enumerations: u32,
    /// Count stray fences the controller asked for (binding ids may be
    /// unknown to the kernel).
    stray_fences: Vec<u64>,
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
                        AgentRequest::Prepare {
                            binding,
                            node,
                            provider,
                            scope,
                            fence,
                            ..
                        } => {
                            let mut state = shared.lock().unwrap();
                            state.prepares += 1;
                            state.endpoints.retain(|endpoint| match scope {
                                archon_kernel::BindingScope::Exclusive => {
                                    endpoint.scope != scope
                                        || endpoint.node != node
                                        || endpoint.provider != provider
                                }
                                archon_kernel::BindingScope::IndependentShare => {
                                    endpoint.binding != Some(binding)
                                }
                            });
                            state.endpoints.push(EnumeratedEndpoint {
                                node,
                                provider,
                                scope,
                                binding: Some(binding),
                                open: true,
                                accepted_fence: fence,
                                phase: archon_kernel::EndpointPhase::Prepared,
                            });
                            AgentResponse::Prepared {
                                binding,
                                handle: {
                                    handle += 1;
                                    handle
                                },
                            }
                        }
                        AgentRequest::Activate { binding, .. } => {
                            let mut state = shared.lock().unwrap();
                            state.activates += 1;
                            state
                                .endpoints
                                .iter_mut()
                                .find(|endpoint| endpoint.binding == Some(binding))
                                .unwrap()
                                .phase = archon_kernel::EndpointPhase::Active;
                            AgentResponse::Activated { binding }
                        }
                        AgentRequest::StartExecution { lease, .. } => {
                            shared.lock().unwrap().starts += 1;
                            AgentResponse::ExecutionStarted { lease }
                        }
                        AgentRequest::StopExecution { lease, .. } => {
                            let mut state = shared.lock().unwrap();
                            state.stops += 1;
                            match state.stop_mode {
                                ReplyMode::Normal => AgentResponse::ExecutionStopped { lease },
                                ReplyMode::Failed => AgentResponse::ExecutionStopFailed {
                                    lease,
                                    reason: "execution ownership unknown".into(),
                                },
                                ReplyMode::Unexpected => AgentResponse::ExecutionStarted { lease },
                                ReplyMode::Disconnect => break,
                            }
                        }
                        AgentRequest::Fence {
                            binding, lease: 0, ..
                        } => {
                            // Stray-generation fence: the endpoint's own
                            // accepted fence with no committed lease.
                            let mut state = shared.lock().unwrap();
                            state.stray_fences.push(binding);
                            state.fences += 1;
                            close_endpoint(&mut state, binding);
                            AgentResponse::Fenced { binding }
                        }
                        AgentRequest::Fence { binding, .. } => {
                            let mut state = shared.lock().unwrap();
                            state.fences += 1;
                            close_endpoint(&mut state, binding);
                            AgentResponse::Fenced { binding }
                        }
                        AgentRequest::Release { binding, .. } => {
                            close_endpoint(&mut shared.lock().unwrap(), binding);
                            AgentResponse::Released { binding }
                        }
                        AgentRequest::Status { lease } => {
                            let state = shared.lock().unwrap();
                            match state.status_mode {
                                ReplyMode::Normal => AgentResponse::Running {
                                    lease,
                                    running: state.running,
                                    exit_code: state.exit_code,
                                },
                                ReplyMode::Failed => AgentResponse::Failed {
                                    binding: 0,
                                    reason: "status unavailable".into(),
                                },
                                ReplyMode::Unexpected => AgentResponse::ExecutionStarted { lease },
                                ReplyMode::Disconnect => break,
                            }
                        }
                        AgentRequest::EnumerateBindings => {
                            let mut state = shared.lock().unwrap();
                            state.enumerations += 1;
                            let endpoints = state.endpoints.clone();
                            AgentResponse::BindingsEnumerated { endpoints }
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

fn close_endpoint(state: &mut FakeState, binding: u64) {
    for endpoint in &mut state.endpoints {
        if endpoint.binding == Some(binding) {
            endpoint.open = false;
            endpoint.phase = archon_kernel::EndpointPhase::Idle;
        }
    }
}

/// These scripted cases use the CPU's exclusive enforcement namespace.
fn cpu_endpoint(node: u64, binding: u64, open: bool, accepted_fence: u64) -> EnumeratedEndpoint {
    EnumeratedEndpoint {
        node,
        provider: archon_kernel::ProviderId::ENFORCE.as_u64(),
        scope: archon_kernel::BindingScope::Exclusive,
        binding: Some(binding),
        open,
        accepted_fence,
        phase: if open {
            archon_kernel::EndpointPhase::Active
        } else {
            archon_kernel::EndpointPhase::Idle
        },
    }
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

    let saved_state = first.state_snapshot();
    // Second controller generation replays the same authority log while
    // the endpoint keeps enforcing its generation.
    let commands = read_log(&path);
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    recovered.restore_state(saved_state);
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

    let saved_state = first.state_snapshot();
    // The endpoint lost the workload (or cannot vouch for it): recovery
    // must revoke through the protocol and fence before reuse.
    state.lock().unwrap().running = false;
    let commands = read_log(&path);
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    recovered.restore_state(saved_state);
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
    recovered.restore_state(first.state_snapshot());
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

/// Read the exclusive binding the first lease opened, for scripting a
/// truthful endpoint picture.
fn first_lease_binding(
    service: &NodeService,
) -> Option<(archon_kernel::BindingId, archon_kernel::NodeId, u64)> {
    service
        .cluster
        .bindings
        .values()
        .find(|record| record.lease == LeaseId::from_u64(1))
        .map(|record| (record.id, record.node, record.fence))
}

#[test]
fn restart_fences_stray_open_generation_from_enumeration() {
    let dir = std::env::temp_dir().join(format!("archon-recovery-stray-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("log.jsonl");

    let (addr, state) = spawn_fake_agent();

    // First generation: one workload admitted and activated.
    let mut first = NodeService::new();
    first.set_command_sink(Some(logged_sink(&path)));
    register(&mut first, &addr, "inst-stray");
    submit_sleep(&mut first, 1);
    state.lock().unwrap().running = true;
    settle(&mut first, 5);
    let (binding, node, fence) = first_lease_binding(&first).expect("lease 1 opened a binding");

    let saved_state = first.state_snapshot();
    // The endpoint still enforces that generation, but the controller log
    // that survives the crash ends the lease with the fence ack lost:
    // the kernel holds the binding open while the lease is terminal.
    // Dropping without settling captures exactly that mid-teardown crash.
    first.revoke(LeaseId::from_u64(1)).expect("revoke");
    drop(first);

    let commands = read_log(&path);
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");

    recovered.restore_state(saved_state);
    // The agent reports the generation still open at the accepted fence.
    state.lock().unwrap().endpoints =
        vec![cpu_endpoint(node.as_u64(), binding.as_u64(), true, fence)];
    state.lock().unwrap().running = false; // explicit StopExecution proves it gone
    register(&mut recovered, &addr, "inst-stray");
    settle(&mut recovered, 5);

    assert!(
        !recovered.cluster.occupies(LeaseId::from_u64(1)),
        "stray generation fenced from enumeration frees claims"
    );
    assert!(
        recovered
            .take_recovery_events()
            .iter()
            .any(|event| matches!(
                event,
                archon_node::service::RecoveryEvent::StrayFenced { binding: b, .. } if *b == binding
            )),
        "enumeration recorded the stray fence decision"
    );
    assert!(
        state.lock().unwrap().fences >= 1,
        "the stray generation fence reached the agent"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restart_frees_claims_the_endpoint_already_closed() {
    let dir = std::env::temp_dir().join(format!("archon-recovery-closed-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("log.jsonl");

    let (addr, state) = spawn_fake_agent();

    let mut first = NodeService::new();
    first.set_command_sink(Some(logged_sink(&path)));
    register(&mut first, &addr, "inst-closed");
    submit_sleep(&mut first, 1);
    state.lock().unwrap().running = true;
    settle(&mut first, 5);
    let (binding, node, fence) = first_lease_binding(&first).expect("lease 1 opened a binding");

    let saved_state = first.state_snapshot();
    // Lease revoked; the endpoint processed the fence, but the
    // acknowledgement was lost before the kernel could record the close.
    // Dropping without settling captures exactly that mid-teardown crash.
    first.revoke(LeaseId::from_u64(1)).expect("revoke");
    drop(first);

    let commands = read_log(&path);
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");

    recovered.restore_state(saved_state);
    // Endpoint reports the generation closed at its accepted fence.
    state.lock().unwrap().endpoints =
        vec![cpu_endpoint(node.as_u64(), binding.as_u64(), false, fence)];
    state.lock().unwrap().running = false;
    register(&mut recovered, &addr, "inst-closed");
    settle(&mut recovered, 5);

    assert!(
        !recovered.cluster.occupies(LeaseId::from_u64(1)),
        "endpoint close plus execution stop frees claims without expiry"
    );
    let record = recovered
        .cluster
        .bindings
        .get(&binding)
        .expect("binding record survives");
    assert_eq!(
        record.state,
        archon_kernel::BindingState::Fenced,
        "the close is recorded as a fence at the accepted generation"
    );
    assert!(
        !state
            .lock()
            .unwrap()
            .stray_fences
            .contains(&binding.as_u64()),
        "no pointless fence was sent to a closed endpoint"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn terminal_recovery_requires_execution_proof_even_when_endpoint_is_closed() {
    // Exercise explicit failure, unexpected responses, and transport loss
    // independently at both proof boundaries. None is evidence of death.
    for endpoint_open in [Some(true), Some(false), None] {
        for (status_mode, stop_mode) in [
            (ReplyMode::Normal, ReplyMode::Failed),
            (ReplyMode::Normal, ReplyMode::Unexpected),
            (ReplyMode::Normal, ReplyMode::Disconnect),
            (ReplyMode::Failed, ReplyMode::Failed),
            (ReplyMode::Unexpected, ReplyMode::Failed),
            (ReplyMode::Disconnect, ReplyMode::Failed),
        ] {
            let (addr, state) = spawn_fake_agent();
            let mut first = NodeService::new();
            register(&mut first, &addr, "proof");
            submit_sleep(&mut first, 1);
            settle(&mut first, 5);
            let (binding, node, fence) = first_lease_binding(&first).unwrap();
            // Replay a crash before any teardown acknowledgement, without
            // allowing the old worker to race the scripted proof failures.
            let saved_state = first.state_snapshot();
            let mut commands = first.command_history();
            commands.push(Command::RevokeLease {
                lease: LeaseId::from_u64(1),
            });
            drop(first);
            {
                let mut fake = state.lock().unwrap();
                fake.status_mode = status_mode;
                fake.stop_mode = stop_mode;
                fake.endpoints = endpoint_open
                    .into_iter()
                    .map(|open| cpu_endpoint(node.as_u64(), binding.as_u64(), open, fence))
                    .collect();
            }
            let mut recovered = NodeService::new();
            recovered.replay(commands).unwrap();
            recovered.restore_state(saved_state);
            recovered.clear_history();
            register(&mut recovered, &addr, "proof");
            settle(&mut recovered, 5);
            assert!(
                recovered.cluster.occupies(LeaseId::from_u64(1)),
                "unproven member must retain occupancy: {endpoint_open:?}/{status_mode:?}/{stop_mode:?}"
            );
            assert!(!recovered.cluster.bindings[&binding].state.is_closed());
            assert_eq!(state.lock().unwrap().fences, 0);
            if !matches!(status_mode, ReplyMode::Disconnect) {
                assert!(
                    state.lock().unwrap().stops > 0,
                    "unknown status requires StopExecution"
                );
            }
            assert!(!recovered.command_history().iter().any(|command| matches!(
                command, Command::RecordBindingFenced { binding: id, .. } if *id == binding
            )));
            assert_eq!(state.lock().unwrap().starts, 1, "recovery never relaunches");

            // A new session with affirmative stop and endpoint proof can
            // settle the held ownership, including after transport loss.
            {
                let mut fake = state.lock().unwrap();
                fake.status_mode = ReplyMode::Normal;
                fake.stop_mode = ReplyMode::Normal;
                fake.endpoints = vec![cpu_endpoint(node.as_u64(), binding.as_u64(), true, fence)];
            }
            register(&mut recovered, &addr, "proof");
            settle(&mut recovered, 5);
            assert!(!recovered.cluster.occupies(LeaseId::from_u64(1)));
            assert!(state.lock().unwrap().stops > 0);
        }
    }
}

#[test]
fn empty_enumeration_does_not_close_terminal_bindings() {
    let (addr, state) = spawn_fake_agent();
    let mut first = NodeService::new();
    register(&mut first, &addr, "empty");
    submit_sleep(&mut first, 1);
    settle(&mut first, 5);
    let mut commands = first.command_history();
    commands.push(Command::RevokeLease {
        lease: LeaseId::from_u64(1),
    });
    let mut recovered = NodeService::new();
    recovered.replay(commands).unwrap();
    recovered.restore_state(first.state_snapshot());
    state.lock().unwrap().exit_code = Some(0);
    state.lock().unwrap().endpoints.clear();
    register(&mut recovered, &addr, "empty");
    settle(&mut recovered, 5);
    assert!(recovered.cluster.occupies(LeaseId::from_u64(1)));
    assert_eq!(state.lock().unwrap().fences, 0);
}

#[test]
fn agent_reregistration_never_relaunches_untracked_execution() {
    let (addr, state) = spawn_fake_agent();
    let mut service = NodeService::new();
    register(&mut service, &addr, "replacement");
    submit_sleep(&mut service, 1);
    settle(&mut service, 5);
    state.lock().unwrap().stop_mode = ReplyMode::Failed;
    register(&mut service, &addr, "replacement");
    settle(&mut service, 5);
    assert_eq!(
        service.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Revoked
    );
    assert!(service.cluster.occupies(LeaseId::from_u64(1)));
    let fake = state.lock().unwrap();
    assert_eq!(fake.starts, 1);
    assert_eq!(fake.activates, 1);
    assert!(fake.stops > 0);
    assert_eq!(fake.fences, 0);
}

#[test]
fn restart_fences_unknown_open_generation_the_kernel_never_committed() {
    let dir = std::env::temp_dir().join(format!("archon-recovery-unknown-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("log.jsonl");

    let (addr, state) = spawn_fake_agent();

    // A clean controller generation with one ordinary workload.
    let mut first = NodeService::new();
    first.set_command_sink(Some(logged_sink(&path)));
    register(&mut first, &addr, "inst-unk");
    submit_sleep(&mut first, 1);
    state.lock().unwrap().running = true;
    settle(&mut first, 5);
    let (_, node, _) = first_lease_binding(&first).expect("lease 1 opened a binding");

    let commands = read_log(&path);
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");

    recovered.restore_state(first.state_snapshot());
    // The endpoint holds an open generation whose binding id was never
    // committed by any controller generation — for example an effect
    // applied just before the controller crashed mid-commit.
    let mut stray = cpu_endpoint(node.as_u64(), 999, true, 42);
    // A distinct provider namespace can coexist with the active CPU endpoint.
    stray.provider = 999;
    state.lock().unwrap().endpoints.push(stray);
    state.lock().unwrap().running = true; // keep lease 1 adoptable
    register(&mut recovered, &addr, "inst-unk");
    settle(&mut recovered, 5);

    assert!(
        state.lock().unwrap().stray_fences.contains(&999),
        "an open generation unknown to the kernel is fenced promptly"
    );
    // The adoptable workload itself is untouched by the stray cleanup.
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active,
        "stray cleanup never disturbs provably-current work"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
