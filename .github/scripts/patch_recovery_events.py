from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    return text.replace(old, new, 1)


service_path = Path("crates/node/src/service.rs")
text = service_path.read_text()

member_status = '''#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MemberStatus {
    running: bool,
    exit_code: Option<i32>,
}
'''
recovery_types = member_status + '''
/// Material controller-restart reconciliation outcomes. These are
/// observational controller events, not kernel authority or replay state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryEvent {
    PreparingLeaseFailed {
        lease: LeaseId,
        machine: NodeId,
    },
    TerminalBindingFenced {
        lease: LeaseId,
        machine: NodeId,
        binding: BindingId,
    },
    MemberRecovered {
        lease: LeaseId,
        machine: NodeId,
        running: bool,
        exit_code: Option<i32>,
    },
    MemberRevokedUnprovable {
        lease: LeaseId,
        machine: NodeId,
    },
    MemberRevokedRebindFailed {
        lease: LeaseId,
        machine: NodeId,
    },
}
'''
text = replace_once(text, member_status, recovery_types, "recovery event type")

field = '''    recovering_members: BTreeSet<(LeaseId, NodeId)>,
    /// Observes every command applied to the cluster; the control plane
'''
field_new = '''    recovering_members: BTreeSet<(LeaseId, NodeId)>,
    /// Material restart-recovery decisions waiting for an observer. This is
    /// controller-local telemetry and is intentionally absent from ServiceState.
    recovery_events: Vec<RecoveryEvent>,
    /// Observes every command applied to the cluster; the control plane
'''
text = replace_once(text, field, field_new, "recovery event field")

init = '''            recovering_machines: BTreeMap::new(),
            recovering_members: BTreeSet::new(),
            command_sink: None,
'''
init_new = '''            recovering_machines: BTreeMap::new(),
            recovering_members: BTreeSet::new(),
            recovery_events: Vec::new(),
            command_sink: None,
'''
text = replace_once(text, init, init_new, "recovery event initialization")

history = '''    pub fn command_history(&self) -> Vec<Command> {
        self.history.clone()
    }

    /// Capture the controller-side state that outlives restarts alongside
'''
history_new = '''    pub fn command_history(&self) -> Vec<Command> {
        self.history.clone()
    }

    /// Drain material controller-restart reconciliation outcomes observed
    /// since the previous call. Recovery events explain decisions but never
    /// participate in authority, replay, or ServiceState restoration.
    pub fn take_recovery_events(&mut self) -> Vec<RecoveryEvent> {
        std::mem::take(&mut self.recovery_events)
    }

    /// Capture the controller-side state that outlives restarts alongside
'''
text = replace_once(text, history, history_new, "recovery event accessor")

terminal = '''                _ => commands.push(Command::FenceBinding { binding }),
'''
terminal_new = '''                _ => {
                    commands.push(Command::FenceBinding { binding });
                    self.recovery_events.push(RecoveryEvent::TerminalBindingFenced {
                        lease: record.lease,
                        machine,
                        binding,
                    });
                }
'''
text = replace_once(text, terminal, terminal_new, "terminal binding event")

preparing = '''        for lease in preparing {
            commands.push(Command::FailLease {
                lease,
                reason: "restart left preparation unproven".into(),
            });
        }
'''
preparing_new = '''        for lease in preparing {
            commands.push(Command::FailLease {
                lease,
                reason: "restart left preparation unproven".into(),
            });
            self.recovery_events
                .push(RecoveryEvent::PreparingLeaseFailed { lease, machine });
        }
'''
text = replace_once(text, preparing, preparing_new, "preparing lease event")

rebind_failure = '''            if let Err(err) = self.rebind_lease_machine(lease, machine) {
                eprintln!("archon: adopting lease {lease} on machine {machine} failed: {err}");
                self.commit_lenient(Command::RevokeLease { lease });
                finished.push(lease);
'''
rebind_failure_new = '''            if let Err(err) = self.rebind_lease_machine(lease, machine) {
                eprintln!("archon: adopting lease {lease} on machine {machine} failed: {err}");
                self.recovery_events
                    .push(RecoveryEvent::MemberRevokedRebindFailed { lease, machine });
                self.commit_lenient(Command::RevokeLease { lease });
                finished.push(lease);
'''
text = replace_once(text, rebind_failure, rebind_failure_new, "rebind failure event")

recovered = '''            self.resolve_observed_completion(lease, finished);
            eprintln!("archon: recovered lease {lease} member on machine {machine}");
        } else {
            self.commit_lenient(Command::RevokeLease { lease });
'''
recovered_new = '''            self.resolve_observed_completion(lease, finished);
            self.recovery_events.push(RecoveryEvent::MemberRecovered {
                lease,
                machine,
                running,
                exit_code,
            });
            eprintln!("archon: recovered lease {lease} member on machine {machine}");
        } else {
            self.recovery_events
                .push(RecoveryEvent::MemberRevokedUnprovable { lease, machine });
            self.commit_lenient(Command::RevokeLease { lease });
'''
text = replace_once(text, recovered, recovered_new, "member recovery events")

service_path.write_text(text)


test_path = Path("crates/node/tests/distributed_completion.rs")
test = test_path.read_text()
test = replace_once(
    test,
    'use archon_node::service::{LeaseExecutor, NodeService};',
    'use archon_node::service::{LeaseExecutor, NodeService, RecoveryEvent};',
    "recovery event import",
)

first_recovery = '''    assert_eq!(binding_sessions(&recovered, a), BTreeSet::from([3]));
    assert_eq!(
        binding_sessions(&recovered, b),
'''
first_recovery_new = '''    assert_eq!(binding_sessions(&recovered, a), BTreeSet::from([3]));
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
'''
test = replace_once(test, first_recovery, first_recovery_new, "first member event assertion")

second_recovery = '''    assert_eq!(binding_sessions(&recovered, b), BTreeSet::from([4]));
    assert_eq!(
        recovered.cluster.leases[&LeaseId::from_u64(1)].state,
'''
second_recovery_new = '''    assert_eq!(binding_sessions(&recovered, b), BTreeSet::from([4]));
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
'''
test = replace_once(test, second_recovery, second_recovery_new, "second member event assertion")

append = r'''

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
'''
if "restart_recovery_explains_unprovable_member_revocation" in test:
    raise SystemExit("unprovable recovery test already present")
test_path.write_text(test + append)
