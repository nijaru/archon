use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archon_kernel::{
    CapacityDimension, Filter, LeaseId, LeaseState, Need, NodeId, OwnerId, Request, RequestClass,
    RequestId, ResourceClass, qty,
};
use archon_node::discover::{HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities,
};
use archon_node::service::{AgentClient, NodeService, RecoveryEvent};

const GIB: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorkReply {
    running: bool,
    exit_code: Option<i32>,
}

impl WorkReply {
    const RUNNING: Self = Self {
        running: true,
        exit_code: None,
    };

    const fn exited(code: i32) -> Self {
        Self {
            running: false,
            exit_code: Some(code),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Activate,
    Status,
    Fence,
    Release,
}

struct StatusExecutor {
    status: Arc<Mutex<WorkReply>>,
    events: Arc<Mutex<Vec<Event>>>,
}

impl AgentClient for StatusExecutor {
    fn call(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        match request {
            AgentRequest::Prepare { binding, .. } => Ok(AgentResponse::Prepared {
                binding,
                handle: binding,
            }),
            AgentRequest::Activate { binding, .. } => {
                self.events
                    .lock()
                    .expect("event lock")
                    .push(Event::Activate);
                Ok(AgentResponse::Activated { binding })
            }
            AgentRequest::Release { binding, .. } => {
                self.events.lock().expect("event lock").push(Event::Release);
                Ok(AgentResponse::Released { binding })
            }
            AgentRequest::Fence { binding, .. } => {
                self.events.lock().expect("event lock").push(Event::Fence);
                Ok(AgentResponse::Fenced { binding })
            }
            AgentRequest::Status { lease } => {
                self.events.lock().expect("event lock").push(Event::Status);
                let status = *self.status.lock().expect("status lock");
                Ok(AgentResponse::Running {
                    lease,
                    running: status.running,
                    exit_code: status.exit_code,
                })
            }
            AgentRequest::Logs { lease } => Ok(AgentResponse::Logs {
                lease,
                output: String::new(),
            }),
            other => Err(format!("unexpected request in status executor: {other:?}")),
        }
    }
}

fn attrs(member: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("member".into(), member.into())])
}

fn description(member: &str) -> MachineDescription {
    MachineDescription {
        instance_id: format!("completion-{member}"),
        name: format!("completion-{member}"),
        cpus: 1,
        memory_bytes: GIB,
        host_nodes: vec![
            HostNodeSpec {
                id: "cpu0".into(),
                kind: ResourceClass::Cpu,
                parent: None,
                attrs: attrs(member),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "mem0".into(),
                kind: ResourceClass::Memory,
                parent: None,
                attrs: attrs(member),
                capacity: qty(CapacityDimension::Bytes, GIB),
            },
        ],
        devices: Vec::new(),
    }
}

fn caps() -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: true,
            device_isolation: true,
            physical_cpu_placement: true,
            numa_memory_placement: true,
        },
        container: RuntimeCapabilities::default(),
    }
}

fn cpu_need(member: &str) -> Need {
    Need {
        kind: ResourceClass::Cpu,
        quantity: qty(CapacityDimension::Count, 1),
        filters: vec![Filter {
            key: "member".into(),
            value: member.into(),
        }],
    }
}

fn request() -> archon_node::workload::WorkloadSpec {
    archon_node::workload::WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(1),
            class: RequestClass::Batch,
            needs: vec![cpu_need("a"), cpu_need("b")],
            topology: Vec::new(),
            preferences: Vec::new(),
            data: Vec::new(),
            lifetime: 60,
            priority: 1,
            machine_local: false,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: vec!["rigid-member".into()],
            image: None,
            storage: Vec::new(),
            ports: Vec::new(),
            grace_secs: 0,
        },
        keep_alive: false,
    }
}

type SharedStatus = Arc<Mutex<WorkReply>>;
type SharedEvents = Arc<Mutex<Vec<Event>>>;

fn register(
    service: &mut NodeService,
    member: &str,
    status: SharedStatus,
    events: SharedEvents,
) -> NodeId {
    service
        .register_agent_with_capabilities(
            description(member),
            Box::new(StatusExecutor { status, events }),
            caps(),
        )
        .expect("register proof agent")
}

fn active_service(
    a_status: SharedStatus,
    b_status: SharedStatus,
) -> (NodeService, NodeId, NodeId, SharedEvents, SharedEvents) {
    let mut service = NodeService::new();
    let a_events = Arc::new(Mutex::new(Vec::new()));
    let b_events = Arc::new(Mutex::new(Vec::new()));
    let a = register(&mut service, "a", a_status, a_events.clone());
    let b = register(&mut service, "b", b_status, b_events.clone());
    service.submit(request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    service
        .drive(Duration::from_secs(2))
        .expect("activate distributed lease");
    assert_eq!(
        service.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
    (service, a, b, a_events, b_events)
}

fn poll(service: &mut NodeService) {
    service
        .collect_completions()
        .expect("dispatch status polls");
    service
        .drive(Duration::from_secs(2))
        .expect("absorb status polls");
}

#[test]
fn distributed_success_waits_for_every_member() {
    let a_status = Arc::new(Mutex::new(WorkReply::exited(0)));
    let b_status = Arc::new(Mutex::new(WorkReply::RUNNING));
    let (mut service, _, _, _, _) = active_service(a_status, b_status.clone());

    poll(&mut service);
    assert_eq!(
        service.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active,
        "one successful member must not complete a rigid multi-machine lease"
    );

    *b_status.lock().expect("status lock") = WorkReply::exited(0);
    poll(&mut service);
    let lease = &service.cluster.leases[&LeaseId::from_u64(1)];
    assert_eq!(lease.state, LeaseState::Completed);
    assert_eq!(lease.exit_code, Some(0));
}

#[test]
fn distributed_failure_fails_the_root_and_fences_other_members() {
    let a_status = Arc::new(Mutex::new(WorkReply::exited(7)));
    let b_status = Arc::new(Mutex::new(WorkReply::RUNNING));
    let (mut service, _, _, _, b_events) = active_service(a_status, b_status);

    poll(&mut service);
    let lease = &service.cluster.leases[&LeaseId::from_u64(1)];
    assert_eq!(lease.state, LeaseState::Failed);
    assert_eq!(lease.exit_code, Some(7));
    assert!(
        service
            .cluster
            .bindings_for(LeaseId::from_u64(1))
            .into_iter()
            .all(|binding| service.cluster.bindings[&binding].state.is_closed()),
        "a failed rigid member must fence the whole allocation before reuse"
    );
    assert!(
        b_events.lock().expect("event lock").contains(&Event::Fence),
        "the still-running sibling must receive a fence"
    );
}

fn binding_sessions(service: &NodeService, machine: NodeId) -> BTreeSet<u64> {
    service
        .cluster
        .bindings
        .values()
        .filter(|binding| service.cluster.graph.machine_of(binding.node) == Some(machine))
        .map(|binding| binding.agent_session)
        .collect()
}

#[test]
fn restart_recovery_is_member_scoped_and_aggregates_exit_state() {
    let running_a = Arc::new(Mutex::new(WorkReply::RUNNING));
    let running_b = Arc::new(Mutex::new(WorkReply::RUNNING));
    let (service, a, b, _, _) = active_service(running_a, running_b);
    let cluster = service.cluster.clone();
    let state = service.state_snapshot();
    let old_a = binding_sessions(&service, a);
    let old_b = binding_sessions(&service, b);
    assert_eq!(old_a, BTreeSet::from([1]));
    assert_eq!(old_b, BTreeSet::from([2]));
    drop(service);

    let mut recovered = NodeService::new();
    recovered.restore(cluster, state);

    let a_status = Arc::new(Mutex::new(WorkReply::exited(0)));
    let a_events = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(register(&mut recovered, "a", a_status, a_events.clone()), a);
    recovered
        .drive(Duration::from_secs(2))
        .expect("recover first member");
    assert_eq!(binding_sessions(&recovered, a), BTreeSet::from([3]));
    assert_eq!(
        recovered.take_recovery_events(),
        vec![RecoveryEvent::MemberRecovered {
            lease: LeaseId::from_u64(1),
            machine: a,
            running: false,
            exit_code: Some(0),
        }]
    );
    assert_eq!(
        binding_sessions(&recovered, b),
        old_b,
        "recovering one machine must not rewrite a sibling machine's binding session"
    );
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active,
        "one member exiting successfully while detached is not whole-job completion"
    );

    let b_status = Arc::new(Mutex::new(WorkReply::RUNNING));
    let b_events = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(register(&mut recovered, "b", b_status, b_events.clone()), b);
    recovered
        .drive(Duration::from_secs(2))
        .expect("recover second member");
    assert_eq!(binding_sessions(&recovered, b), BTreeSet::from([4]));
    assert_eq!(
        recovered.take_recovery_events(),
        vec![RecoveryEvent::MemberRecovered {
            lease: LeaseId::from_u64(1),
            machine: b,
            running: true,
            exit_code: None,
        }]
    );
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Active
    );
    assert!(
        !a_events
            .lock()
            .expect("event lock")
            .contains(&Event::Activate)
            && !b_events
                .lock()
                .expect("event lock")
                .contains(&Event::Activate),
        "recovery must adopt proven members rather than re-executing them"
    );
}

#[test]
fn restart_recovery_explains_unprovable_member_revocation() {
    let running_a = Arc::new(Mutex::new(WorkReply::RUNNING));
    let running_b = Arc::new(Mutex::new(WorkReply::RUNNING));
    let (service, a, _, _, _) = active_service(running_a, running_b);
    let cluster = service.cluster.clone();
    let state = service.state_snapshot();
    drop(service);

    let mut recovered = NodeService::new();
    recovered.restore(cluster, state);
    let unknown = Arc::new(Mutex::new(WorkReply {
        running: false,
        exit_code: None,
    }));
    let events = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(register(&mut recovered, "a", unknown, events), a);
    recovered
        .drive(Duration::from_secs(2))
        .expect("resolve unprovable recovery member");

    assert_eq!(
        recovered.take_recovery_events(),
        vec![RecoveryEvent::MemberRevokedUnprovable {
            lease: LeaseId::from_u64(1),
            machine: a,
        }]
    );
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
        LeaseState::Revoked
    );
}
