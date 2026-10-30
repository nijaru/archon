//! Recovery joins real endpoint-generation evidence with member status. These
//! in-process agents perform no execution and use channels to order old replies.

use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archon_kernel::{
    BindingScope, CapacityDimension, EndpointPhase, LeaseId, LeaseState, Need, NodeId, OwnerId,
    Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::discover::MachineDescription;
use archon_node::protocol::{
    AgentRequest, AgentResponse, EnumeratedEndpoint, ExecutionCapabilities, RuntimeCapabilities,
};
use archon_node::service::{AgentClient, NodeService, RecoveryEvent};
use archon_node::workload::{ExecutionSpec, WorkloadSpec};

const WAIT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct State {
    endpoints: Vec<EnumeratedEndpoint>,
    starts: usize,
    activates: usize,
    failed_fences: BTreeSet<u64>,
    fences: Vec<(u64, u64, BindingScope, u64)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Point {
    Enumeration,
    Status,
    Fence,
}

struct Pause {
    point: Point,
    entered: Sender<()>,
    release: Receiver<()>,
}

struct Agent {
    state: Arc<Mutex<State>>,
    enumeration: Option<Result<Vec<EnumeratedEndpoint>, String>>,
    status: (bool, Option<i32>),
    wrong_status_lease: bool,
    pause: Option<Pause>,
    retired: Option<Sender<()>>,
}

impl Agent {
    fn new(state: &Arc<Mutex<State>>) -> Self {
        Self {
            state: state.clone(),
            enumeration: None,
            status: (true, None),
            wrong_status_lease: false,
            pause: None,
            retired: None,
        }
    }

    fn pause(&mut self, point: Point) -> (Receiver<()>, Sender<()>) {
        let (entered, entry) = channel();
        let (release, released) = channel();
        self.pause = Some(Pause {
            point,
            entered,
            release: released,
        });
        (entry, release)
    }

    fn retiring(&mut self) -> Receiver<()> {
        let (tx, rx) = channel();
        self.retired = Some(tx);
        rx
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if let Some(tx) = &self.retired {
            let _ = tx.send(());
        }
    }
}

impl AgentClient for Agent {
    fn call(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        let point = match request {
            AgentRequest::EnumerateBindings => Some(Point::Enumeration),
            AgentRequest::Status { .. } => Some(Point::Status),
            AgentRequest::Fence { .. } => Some(Point::Fence),
            _ => None,
        };
        let reply = {
            let mut state = self.state.lock().unwrap();
            match request {
                AgentRequest::Prepare {
                    binding,
                    node,
                    provider,
                    scope,
                    fence,
                    ..
                } => {
                    state.endpoints.retain(|endpoint| match scope {
                        BindingScope::Exclusive => {
                            endpoint.scope != scope
                                || endpoint.node != node
                                || endpoint.provider != provider
                        }
                        BindingScope::IndependentShare => endpoint.binding != Some(binding),
                    });
                    state.endpoints.push(EnumeratedEndpoint {
                        binding: Some(binding),
                        node,
                        provider,
                        scope,
                        open: true,
                        accepted_fence: fence,
                        phase: EndpointPhase::Prepared,
                    });
                    Ok(AgentResponse::Prepared {
                        binding,
                        handle: binding,
                    })
                }
                AgentRequest::Activate { binding, .. } => {
                    state.activates += 1;
                    state
                        .endpoints
                        .iter_mut()
                        .find(|e| e.binding == Some(binding))
                        .unwrap()
                        .phase = EndpointPhase::Active;
                    Ok(AgentResponse::Activated { binding })
                }
                AgentRequest::StartExecution { lease, .. } => {
                    state.starts += 1;
                    Ok(AgentResponse::ExecutionStarted { lease })
                }
                AgentRequest::StopExecution { lease, .. } => {
                    Ok(AgentResponse::ExecutionStopFailed {
                        lease,
                        reason: "test retains uncertain execution occupancy".into(),
                    })
                }
                AgentRequest::Status { lease } => Ok(AgentResponse::Running {
                    lease: lease + u64::from(self.wrong_status_lease),
                    running: self.status.0,
                    exit_code: self.status.1,
                }),
                AgentRequest::EnumerateBindings => self
                    .enumeration
                    .clone()
                    .unwrap_or_else(|| Ok(state.endpoints.clone()))
                    .map(|endpoints| AgentResponse::BindingsEnumerated { endpoints }),
                AgentRequest::Fence {
                    binding,
                    node,
                    provider,
                    scope,
                    fence,
                    ..
                } => {
                    state.fences.push((node, provider, scope, fence));
                    if state.failed_fences.contains(&binding) {
                        Err("stray closure unproven".into())
                    } else {
                        for endpoint in &mut state.endpoints {
                            if endpoint.binding == Some(binding) {
                                endpoint.open = false;
                                endpoint.phase = EndpointPhase::Idle;
                            }
                        }
                        Ok(AgentResponse::Fenced { binding })
                    }
                }
                AgentRequest::Logs { lease } => Ok(AgentResponse::Logs {
                    lease,
                    output: String::new(),
                }),
                other => panic!("unexpected test request {other:?}"),
            }
        };
        if let Some(pause) = &self.pause
            && point == Some(pause.point)
        {
            pause.entered.send(()).unwrap();
            pause
                .release
                .recv_timeout(WAIT)
                .expect("release test agent");
            self.pause = None;
        }
        reply
    }
}

fn register(service: &mut NodeService, agent: Agent) -> NodeId {
    service
        .register_agent_with_capabilities(
            MachineDescription {
                instance_id: "gate-agent".into(),
                name: "gate".into(),
                cpus: 4,
                memory_bytes: 1_000,
                host_nodes: vec![],
                devices: vec![],
            },
            Box::new(agent),
            ExecutionCapabilities {
                process: RuntimeCapabilities {
                    available: true,
                    cpu_limit: true,
                    memory_limit: true,
                    device_isolation: false,
                    physical_cpu_placement: false,
                    numa_memory_placement: false,
                },
                container: RuntimeCapabilities::default(),
            },
        )
        .unwrap()
}

fn settle(service: &mut NodeService) {
    service.drive(WAIT).unwrap();
    assert!(service.is_quiescent(), "agent work must finish");
}

fn submit(service: &mut NodeService, id: u64) {
    service.submit(
        WorkloadSpec {
            resources: Request {
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
                        quantity: qty(CapacityDimension::Bytes, 10),
                        filters: vec![],
                    },
                ],
                topology: vec![],
                preferences: vec![],
                data: vec![],
                machine_local: true,
                lifetime: 3_600,
                priority: 1,
            },
            execution: ExecutionSpec {
                command: vec!["never-executed".into()],
                image: None,
                storage: vec![],
                ports: vec![],
                grace_secs: 0,
            },
            keep_alive: false,
        },
        OwnerId::from_u64(1),
    );
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(id)));
}

fn started() -> (NodeService, Arc<Mutex<State>>, NodeId) {
    let state = Arc::new(Mutex::new(State::default()));
    let mut service = NodeService::new();
    let machine = register(&mut service, Agent::new(&state));
    submit(&mut service, 1);
    settle(&mut service);
    assert_eq!(state.lock().unwrap().starts, 1);
    assert_eq!(state.lock().unwrap().endpoints.len(), 2);
    (service, state, machine)
}

fn restart(first: NodeService) -> NodeService {
    let mut recovered = NodeService::new();
    recovered.restore(first.cluster.clone(), first.state_snapshot());
    recovered
}

fn assert_held(service: &mut NodeService, old_session: u64) {
    assert_eq!(
        service.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
    assert!(service.cluster.occupies(LeaseId::from_u64(1)));
    assert!(
        service
            .cluster
            .bindings
            .values()
            .all(|record| record.agent_session == old_session)
    );
    assert!(
        !service
            .take_recovery_events()
            .iter()
            .any(|event| matches!(event, RecoveryEvent::MemberRecovered { .. }))
    );
}

#[test]
fn adoption_requires_every_exact_active_endpoint_and_successful_enumeration() {
    for defect in [
        "empty",
        "missing",
        "node",
        "provider",
        "scope",
        "binding",
        "fence",
        "closed",
        "prepared",
        "duplicate",
        "failed",
    ] {
        let (first, state, machine) = started();
        let old_session = first.cluster.sessions[&machine];
        let truthful = state.lock().unwrap().endpoints.clone();
        let mut endpoints = truthful.clone();
        match defect {
            "empty" => endpoints.clear(),
            "missing" => {
                endpoints.pop();
            }
            "node" => endpoints[0].node = machine.as_u64(),
            "provider" => endpoints[0].provider += 100,
            "scope" => endpoints[0].scope = BindingScope::IndependentShare,
            "binding" => endpoints[0].binding = Some(999),
            "fence" => endpoints[0].accepted_fence += 1,
            "closed" => endpoints[0].open = false,
            "prepared" => endpoints[0].phase = EndpointPhase::Prepared,
            "duplicate" => endpoints.push(endpoints[0].clone()),
            "failed" => {}
            _ => unreachable!(),
        }
        let mut agent = Agent::new(&state);
        agent.enumeration = Some(if defect == "failed" {
            Err("unavailable".into())
        } else {
            Ok(endpoints)
        });
        let mut recovered = restart(first);
        register(&mut recovered, agent);
        settle(&mut recovered);
        assert_held(&mut recovered, old_session);
        assert_eq!(state.lock().unwrap().starts, 1, "{defect}: no respawn");

        // A fresh registration with exact evidence resolves the held member,
        // without another Prepare, Activate, or StartExecution.
        state.lock().unwrap().endpoints = truthful;
        register(&mut recovered, Agent::new(&state));
        settle(&mut recovered);
        let session = recovered.cluster.sessions[&machine];
        assert!(
            recovered
                .cluster
                .bindings
                .values()
                .all(|record| record.agent_session == session)
        );
        assert!(
            recovered
                .take_recovery_events()
                .iter()
                .any(|event| matches!(event, RecoveryEvent::MemberRecovered { .. })),
            "{defect}: new proof recovers"
        );
        let observed = state.lock().unwrap();
        assert_eq!(observed.starts, 1);
        assert_eq!(observed.activates, 2);
    }
}

#[test]
fn status_for_another_lease_is_not_recovery_evidence() {
    let (first, state, _) = started();
    let mut recovered = restart(first);
    let mut agent = Agent::new(&state);
    agent.wrong_status_lease = true;
    register(&mut recovered, agent);
    settle(&mut recovered);
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Revoked
    );
    assert!(
        recovered.cluster.occupies(LeaseId::from_u64(1)),
        "failed stop retains occupancy"
    );
    assert_eq!(state.lock().unwrap().starts, 1);
}

#[test]
fn successful_empty_enumeration_finishes_idle_recovery() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut first = NodeService::new();
    register(&mut first, Agent::new(&state));
    let mut recovered = restart(first);
    register(&mut recovered, Agent::new(&state));
    settle(&mut recovered);
    submit(&mut recovered, 1);
    settle(&mut recovered);
    assert_eq!(state.lock().unwrap().starts, 1);
}

#[test]
fn idle_recovery_failure_is_not_an_empty_enumeration() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut first = NodeService::new();
    register(&mut first, Agent::new(&state));
    let mut recovered = restart(first);
    let mut agent = Agent::new(&state);
    agent.enumeration = Some(Err("unavailable".into()));
    register(&mut recovered, agent);
    settle(&mut recovered);
    submit(&mut recovered, 1);
    settle(&mut recovered);
    assert_eq!(state.lock().unwrap().starts, 0);
    assert!(recovered.cluster.occupies(LeaseId::from_u64(1)));
}

#[test]
fn late_old_enumeration_cannot_supply_missing_current_session_proof() {
    let (first, state, machine) = started();
    let old_session = first.cluster.sessions[&machine];
    let mut recovered = restart(first);
    let mut old = Agent::new(&state);
    let retired = old.retiring();
    let (entered, release) = old.pause(Point::Enumeration);
    register(&mut recovered, old);
    entered.recv_timeout(WAIT).unwrap();
    let mut current = Agent::new(&state);
    current.enumeration = Some(Err("current enumeration unavailable".into()));
    register(&mut recovered, current);
    flush_agent(&mut recovered);
    assert_held(&mut recovered, old_session);
    release.send(()).unwrap();
    retired.recv_timeout(WAIT).unwrap();
    recovered.drive(Duration::ZERO).unwrap();
    assert_held(&mut recovered, old_session);
    assert_eq!(state.lock().unwrap().starts, 1);
}

#[test]
fn late_old_status_cannot_consume_current_member_query() {
    let (first, state, machine) = started();
    let old_session = first.cluster.sessions[&machine];
    let mut recovered = restart(first);
    let mut old = Agent::new(&state);
    old.status = (false, Some(0));
    let retired = old.retiring();
    let (entered, release) = old.pause(Point::Status);
    register(&mut recovered, old);
    entered.recv_timeout(WAIT).unwrap();
    let mut current = Agent::new(&state);
    let (current_entered, current_release) = current.pause(Point::Status);
    register(&mut recovered, current);
    current_entered.recv_timeout(WAIT).unwrap(); // current enumeration already posted
    recovered.drive(Duration::ZERO).unwrap();
    release.send(()).unwrap();
    retired.recv_timeout(WAIT).unwrap();
    recovered.drive(Duration::ZERO).unwrap();
    assert_held(&mut recovered, old_session);
    current_release.send(()).unwrap();
    settle(&mut recovered);
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
    assert!(
        recovered
            .cluster
            .bindings
            .values()
            .all(|record| record.agent_session == recovered.cluster.sessions[&machine])
    );
    assert_eq!(state.lock().unwrap().starts, 1);
}

// A direct query on the serialized worker is a barrier: prior recovery
// answers have reached Inbox before Logs returns. No sleeps or polling.
fn flush_agent(service: &mut NodeService) {
    service
        .request_logs(LeaseId::from_u64(1))
        .unwrap()
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap();
    service.drive(Duration::ZERO).unwrap();
}

#[test]
fn late_old_stray_close_cannot_clear_the_current_sessions_fence() {
    let (first, state, machine) = started();
    let old_session = first.cluster.sessions[&machine];
    let mut stray = state.lock().unwrap().endpoints[1].clone();
    stray.binding = Some(999);
    stray.accepted_fence = 999;
    state.lock().unwrap().endpoints.push(stray);
    let snapshot = state.lock().unwrap().endpoints.clone();
    let mut recovered = restart(first);
    let mut old = Agent::new(&state);
    let retired = old.retiring();
    let (entered, release) = old.pause(Point::Fence);
    register(&mut recovered, old);
    flush_agent(&mut recovered);
    entered.recv_timeout(WAIT).unwrap();
    let mut current = Agent::new(&state);
    current.enumeration = Some(Ok(snapshot));
    let (current_entered, current_release) = current.pause(Point::Fence);
    register(&mut recovered, current);
    flush_agent(&mut recovered);
    current_entered.recv_timeout(WAIT).unwrap();
    release.send(()).unwrap();
    retired.recv_timeout(WAIT).unwrap();
    recovered.drive(Duration::ZERO).unwrap();
    assert_held(&mut recovered, old_session);
    current_release.send(()).unwrap();
    settle(&mut recovered);
    assert!(
        recovered
            .cluster
            .bindings
            .values()
            .all(|record| record.agent_session == recovered.cluster.sessions[&machine])
    );
    assert_eq!(state.lock().unwrap().starts, 1);
}

#[test]
fn work_admitted_during_idle_enumeration_starts_only_after_recovery() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut first = NodeService::new();
    register(&mut first, Agent::new(&state));
    let mut recovered = restart(first);
    let mut old = Agent::new(&state);
    let (entered, release) = old.pause(Point::Enumeration);
    register(&mut recovered, old);
    entered.recv_timeout(WAIT).unwrap();
    submit(&mut recovered, 1);
    assert_eq!(state.lock().unwrap().starts, 0);
    release.send(()).unwrap();
    settle(&mut recovered);
    assert_eq!(state.lock().unwrap().starts, 1);
}

#[test]
fn every_stray_requires_its_own_successful_close_at_reported_coordinates() {
    let state = Arc::new(Mutex::new(State::default()));
    let mut first = NodeService::new();
    let machine = register(&mut first, Agent::new(&state));
    let node = first.cluster.graph.descendants(machine)[0].as_u64();
    {
        let mut observed = state.lock().unwrap();
        observed.endpoints = [999, 1_000]
            .map(|binding| EnumeratedEndpoint {
                node,
                provider: 777,
                scope: BindingScope::IndependentShare,
                binding: Some(binding),
                open: true,
                accepted_fence: binding + 10,
                phase: EndpointPhase::Active,
            })
            .to_vec();
        observed.failed_fences.insert(1_000);
    }
    let mut recovered = restart(first);
    register(&mut recovered, Agent::new(&state));
    settle(&mut recovered);
    submit(&mut recovered, 1);
    settle(&mut recovered);
    let observed = state.lock().unwrap();
    assert_eq!(
        observed.starts, 0,
        "one successful stray ack cannot settle another failure"
    );
    assert_eq!(
        observed.fences,
        vec![
            (node, 777, BindingScope::IndependentShare, 1_009),
            (node, 777, BindingScope::IndependentShare, 1_010),
        ]
    );
}
