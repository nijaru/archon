//! Desired service-group reconciliation: fixed, bounded-elastic, and
//! per-eligible-machine cardinality. Groups replace dead members with
//! ordinary member submissions; the group is the sole desired-state owner;
//! death-replacement is capped and durable across restarts; deliberate
//! scaling never burns the cap and scales down through ordinary revoke.

use std::collections::BTreeSet;
use std::time::Duration;

use archon_kernel::{
    CapacityDimension, LeaseId, Need, NodeId, OwnerId, Request, RequestClass, RequestId,
    ResourceClass, qty,
};
use archon_node::discover::{HostNodeSpec, MachineDescription};
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities,
};
use archon_node::service::{AgentClient, NodeService};
use archon_node::workload::{Cardinality, ServiceGroup, WorkloadSpec};

const GIB: u64 = 1 << 30;

struct GroupExecutor;

impl AgentClient for GroupExecutor {
    fn call(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        match request {
            AgentRequest::Prepare { binding, .. } => Ok(AgentResponse::Prepared {
                binding,
                handle: binding,
            }),
            AgentRequest::Activate { binding, .. } => Ok(AgentResponse::Activated { binding }),
            AgentRequest::StartExecution { lease, .. } => {
                Ok(AgentResponse::ExecutionStarted { lease })
            }
            AgentRequest::StopExecution { lease, .. } => {
                Ok(AgentResponse::ExecutionStopped { lease })
            }
            AgentRequest::Release { binding, .. } => Ok(AgentResponse::Released { binding }),
            AgentRequest::Fence { binding, .. } => Ok(AgentResponse::Fenced { binding }),
            AgentRequest::Status { lease } => Ok(AgentResponse::Running {
                lease,
                running: true,
                exit_code: None,
            }),
            AgentRequest::Logs { lease } => Ok(AgentResponse::Logs {
                lease,
                output: String::new(),
            }),
            other => Err(format!("unexpected request in group proof: {other:?}")),
        }
    }
}

fn description(name: &str, cpus: u64) -> MachineDescription {
    let host_cpus: Vec<HostNodeSpec> = (0..cpus)
        .map(|index| HostNodeSpec {
            id: format!("cpu{index}"),
            kind: ResourceClass::Cpu,
            parent: None,
            attrs: Default::default(),
            capacity: qty(CapacityDimension::Count, 1),
        })
        .collect();
    MachineDescription {
        instance_id: format!("group-{name}"),
        name: format!("group-{name}"),
        cpus,
        memory_bytes: GIB,
        host_nodes: host_cpus
            .into_iter()
            .chain(std::iter::once(HostNodeSpec {
                id: "mem0".into(),
                kind: ResourceClass::Memory,
                parent: None,
                attrs: Default::default(),
                capacity: qty(CapacityDimension::Bytes, GIB),
            }))
            .collect(),
        devices: Vec::new(),
    }
}

fn capabilities() -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: false,
            device_isolation: false,
            physical_cpu_placement: true,
            numa_memory_placement: false,
        },
        container: RuntimeCapabilities::default(),
    }
}

fn register(service: &mut NodeService, name: &str, cpus: u64) -> NodeId {
    service
        .register_agent_with_capabilities(
            description(name, cpus),
            Box::new(GroupExecutor),
            capabilities(),
        )
        .expect("register group agent")
}

fn template(id: u64, cpus: u64) -> WorkloadSpec {
    WorkloadSpec {
        resources: Request {
            id: RequestId::from_u64(id),
            class: RequestClass::Service,
            needs: vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, cpus),
                filters: Vec::new(),
            }],
            topology: Vec::new(),
            preferences: Vec::new(),
            data: Vec::new(),
            lifetime: 3_600,
            priority: 1,
            machine_local: true,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: vec!["group-member".into()],
            image: None,
            storage: Vec::new(),
            ports: Vec::new(),
            grace_secs: 0,
        },
        keep_alive: false,
    }
}

fn settle(service: &mut NodeService) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !service.is_quiescent() {
        let _ = service.drive(Duration::from_millis(20));
    }
}

fn admit_all(service: &mut NodeService) {
    while service.admit_one().expect("admit").is_some() {}
    settle(service);
}

/// Live (non-terminal) leases whose workload belongs to the group.
fn live_members(service: &NodeService, group: &str) -> Vec<LeaseId> {
    service.group_member_leases(group)
}

#[test]
fn group_replaces_dead_members_up_to_desired_count() {
    let mut service = NodeService::new();
    let alpha = register(&mut service, "alpha", 1);
    let beta = register(&mut service, "beta", 1);
    service.cluster.set_now(1);

    service
        .register_service_group(ServiceGroup {
            id: "web".into(),
            owner: OwnerId::from_u64(7),
            cardinality: Cardinality::Fixed(2),
            template: template(0, 1),
        })
        .expect("register");
    let submitted = service.reconcile_service_groups();
    assert_eq!(submitted.len(), 2, "first round submits the desired count");
    admit_all(&mut service);

    // Two live members on the two one-CPU machines.
    let live = live_members(&service, "web");
    assert_eq!(live.len(), 2);
    let machines: BTreeSet<_> = live
        .iter()
        .filter_map(|lease| {
            service
                .cluster
                .leases
                .get(lease)
                .map(|lease| {
                    lease
                        .allocation
                        .claims
                        .iter()
                        .filter_map(|claim| service.cluster.graph.machine_of(claim.node))
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default()
                .into_iter()
                .next()
        })
        .collect();
    assert_eq!(
        machines,
        BTreeSet::from([alpha, beta]),
        "members spread across machines"
    );

    // A settled, fully-live group reconciles to nothing new.
    service.cluster.set_now(2);
    assert!(
        service.reconcile_service_groups().is_empty(),
        "no replacement while every member is live"
    );

    // One member dies: revoke its lease. Reconciliation must submit exactly
    // one replacement after the backoff window.
    let dead = live[0];
    service.revoke(dead).expect("revoke");
    settle(&mut service);
    service.cluster.set_now(60); // beyond the 1s first-round backoff
    let replacements = service.reconcile_service_groups();
    assert_eq!(
        replacements.len(),
        1,
        "exactly one replacement for one dead member"
    );
    admit_all(&mut service);

    let live = live_members(&service, "web");
    assert_eq!(live.len(), 2, "desired count restored");
    assert!(
        !live.contains(&dead),
        "the revoked lease is not resurrected"
    );
    let _ = machines;
}

#[test]
fn group_replacement_is_capped_and_durable_across_restart() {
    let mut service = NodeService::new();
    register(&mut service, "alpha", 1);
    service.cluster.set_now(1);

    service
        .register_service_group(ServiceGroup {
            id: "cap".into(),
            owner: OwnerId::from_u64(1),
            cardinality: Cardinality::Fixed(1),
            template: template(0, 1),
        })
        .expect("register");
    assert_eq!(service.reconcile_service_groups().len(), 1);
    admit_all(&mut service);

    // The member dies and cannot come back: the group replaces it until the
    // cap (5 replacements for desired=1) is exhausted. The initial round is
    // deliberate cardinality, not failure, so it never burns the cap: five
    // death-rounds place, and further rounds submit nothing.
    let initial = live_members(&service, "cap")[0];
    service.revoke(initial).expect("revoke initial member");
    settle(&mut service);
    for round in 0..5 {
        service.cluster.set_now(100 + round * 120);
        let replaced = service.reconcile_service_groups();
        assert_eq!(replaced.len(), 1, "round {round} replaces the dead member");
        admit_all(&mut service);
        let live = live_members(&service, "cap");
        assert_eq!(live.len(), 1, "round {round} restores the desired count");
        if round < 4 {
            service.revoke(live[0]).expect("revoke");
            settle(&mut service);
        }
    }
    service.cluster.set_now(2_000);
    assert!(
        service.reconcile_service_groups().is_empty(),
        "replacement cap reached; no further submissions"
    );

    // Desired state and accounting survive a controller restart: the
    // restored controller neither forgets the group nor resets its cap.
    let state = service.state_snapshot();
    let mut restarted = NodeService::new();
    restarted.restore_state(state);
    restarted.cluster.set_now(3_000);
    assert!(
        restarted.reconcile_service_groups().is_empty(),
        "restart preserves the replacement cap"
    );
}

#[test]
fn group_members_never_double_restart_through_keep_alive() {
    let mut service = NodeService::new();
    register(&mut service, "alpha", 1);
    service.cluster.set_now(1);

    service
        .register_service_group(ServiceGroup {
            id: "solo".into(),
            owner: OwnerId::from_u64(1),
            cardinality: Cardinality::Fixed(1),
            template: {
                let mut spec = template(0, 1);
                spec.keep_alive = true; // even a keep-alive template yields
                // non-keep-alive members
                spec
            },
        })
        .expect("register");
    assert_eq!(service.reconcile_service_groups().len(), 1);
    admit_all(&mut service);

    // Kill the member: the per-member restart path stays silent because the
    // group is the sole desired-state owner.
    let dead = live_members(&service, "solo")[0];
    service.revoke(dead).expect("revoke");
    settle(&mut service);
    assert!(
        service.take_restarts().is_empty(),
        "group members never enter per-member keep-alive restarts"
    );
}

/// Elastic groups steer within [min, max]; scale-up adds members without
/// burning the replacement cap, and scale-down stops the newest members
/// through ordinary revoke.
#[test]
fn elastic_group_scales_within_bounds_and_keeps_the_cap_for_deaths() {
    let mut service = NodeService::new();
    register(&mut service, "alpha", 4);
    service.cluster.set_now(1);

    service
        .register_service_group(ServiceGroup {
            id: "pool".into(),
            owner: OwnerId::from_u64(1),
            cardinality: Cardinality::Elastic {
                min: 1,
                target: 1,
                max: 3,
            },
            template: template(0, 1),
        })
        .expect("register");
    assert_eq!(service.reconcile_service_groups().len(), 1);
    admit_all(&mut service);
    assert_eq!(live_members(&service, "pool").len(), 1);

    // Scale up: target 3 submits two growth members even while the group's
    // backoff window is cold, because growth is not replacement.
    service.scale_service_group("pool", 3).expect("scale");
    assert_eq!(service.reconcile_service_groups().len(), 2);
    admit_all(&mut service);
    assert_eq!(live_members(&service, "pool").len(), 3);

    // Out-of-bounds targets are refused, and non-elastic groups reject
    // scaling outright instead of reinterpreting their cardinality.
    assert!(service.scale_service_group("pool", 4).is_err());
    assert!(service.scale_service_group("pool", 0).is_err());

    // Scale down: the newest members stop through ordinary revoke; the
    // stop path proves member teardown before resource release.
    service.scale_service_group("pool", 1).expect("scale down");
    service.reconcile_service_groups();
    settle(&mut service);
    assert_eq!(
        live_members(&service, "pool").len(),
        1,
        "scale-down leaves exactly the target"
    );

    // Growth never consumed the replacement cap: after a real death the
    // group still has all its replacement budget.
    let dead = live_members(&service, "pool")[0];
    service.revoke(dead).expect("revoke");
    settle(&mut service);
    service.cluster.set_now(120);
    let replaced = service.reconcile_service_groups();
    assert_eq!(replaced.len(), 1, "death still gets a replacement");
    admit_all(&mut service);
    assert_eq!(live_members(&service, "pool").len(), 1);
}

/// A per-eligible-machine group keeps exactly one member on each machine;
/// replacements pin to the same machine rather than drifting elsewhere.
#[test]
fn per_machine_group_maintains_one_member_per_machine() {
    let mut service = NodeService::new();
    let alpha = register(&mut service, "alpha", 1);
    let beta = register(&mut service, "beta", 1);
    service.cluster.set_now(1);

    service
        .register_service_group(ServiceGroup {
            id: "daemon".into(),
            owner: OwnerId::from_u64(1),
            cardinality: Cardinality::PerMachine,
            template: template(0, 1),
        })
        .expect("register");
    assert_eq!(service.reconcile_service_groups().len(), 2);
    admit_all(&mut service);

    let live = live_members(&service, "daemon");
    assert_eq!(live.len(), 2, "one member per machine");
    let machines: BTreeSet<_> = live
        .iter()
        .map(|lease| machine_of(&service, *lease))
        .collect();
    assert_eq!(machines, BTreeSet::from([alpha, beta]));

    // Kill alpha's member: reconciliation replaces it on alpha, not beta,
    // because the dead member's pin names alpha and the new member inherits
    // the machine's need.
    let dead = live
        .iter()
        .copied()
        .find(|lease| machine_of(&service, *lease) == alpha)
        .expect("member on alpha");
    service.revoke(dead).expect("revoke");
    settle(&mut service);
    service.cluster.set_now(120);
    let replaced = service.reconcile_service_groups();
    assert_eq!(replaced.len(), 1, "exactly alpha's member replaced");
    admit_all(&mut service);

    let live = live_members(&service, "daemon");
    assert_eq!(live.len(), 2);
    let machines: BTreeSet<_> = live
        .iter()
        .map(|lease| machine_of(&service, *lease))
        .collect();
    assert_eq!(machines, BTreeSet::from([alpha, beta]));

    // Per-machine groups reject scaling: their cardinality is the machine
    // set, not a steerable number.
    assert!(service.scale_service_group("daemon", 1).is_err());
}

/// A pinned member cannot place anywhere except its machine: with alpha
/// full, its replacement waits in the queue instead of drifting to beta.
#[test]
fn per_machine_pinned_member_waits_for_its_machine() {
    let mut service = NodeService::new();
    register(&mut service, "alpha", 1);
    let beta = register(&mut service, "beta", 1);
    service.cluster.set_now(1);

    // A foreign workload occupies alpha's only CPU first.
    let mut foreign = template(0, 1);
    foreign.resources.id = RequestId::from_u64(999);
    service.submit(foreign, OwnerId::from_u64(42));
    admit_all(&mut service);

    service
        .register_service_group(ServiceGroup {
            id: "daemon".into(),
            owner: OwnerId::from_u64(1),
            cardinality: Cardinality::PerMachine,
            template: template(0, 1),
        })
        .expect("register");
    assert_eq!(service.reconcile_service_groups().len(), 2);
    admit_all(&mut service);

    // Beta's member runs; alpha's member stays queued, pinned to alpha,
    // even though beta is now full too — it must never run on beta.
    let live = live_members(&service, "daemon");
    assert_eq!(live.len(), 1);
    assert_eq!(machine_of(&service, live[0]), beta);
    assert_eq!(
        service.queue_len(),
        1,
        "alpha's pinned member waits for alpha, never drifts to beta"
    );
    let _ = beta;
}

/// Malformed cardinality is refused at registration, before any member
/// exists.
#[test]
fn malformed_cardinality_is_refused_up_front() {
    let mut service = NodeService::new();
    register(&mut service, "alpha", 1);
    service.cluster.set_now(1);

    let broken = |cardinality: Cardinality| ServiceGroup {
        id: "broken".into(),
        owner: OwnerId::from_u64(1),
        cardinality,
        template: template(0, 1),
    };
    assert!(
        service
            .register_service_group(broken(Cardinality::Fixed(0)))
            .is_err()
    );
    assert!(
        service
            .register_service_group(broken(Cardinality::Elastic {
                min: 0,
                target: 1,
                max: 2,
            }))
            .is_err()
    );
    assert!(
        service
            .register_service_group(broken(Cardinality::Elastic {
                min: 2,
                target: 1,
                max: 3,
            }))
            .is_err()
    );
    assert!(
        service
            .register_service_group(broken(Cardinality::Elastic {
                min: 1,
                target: 3,
                max: 2,
            }))
            .is_err()
    );
    assert!(
        service.reconcile_service_groups().is_empty(),
        "nothing was submitted for refused groups"
    );
}

/// The machine a lease's claims live on.
fn machine_of(service: &NodeService, lease: LeaseId) -> NodeId {
    service
        .cluster
        .leases
        .get(&lease)
        .and_then(|lease| {
            lease
                .allocation
                .claims
                .iter()
                .find_map(|claim| service.cluster.graph.machine_of(claim.node))
        })
        .expect("lease has a machine")
}
