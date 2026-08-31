use std::collections::BTreeSet;

use crate::Cluster;
use crate::ids::{LeaseId, OwnerId, RequestId};
use crate::select::select;
use crate::types::Request;

/// Result of advisory preemption planning. Planning never releases resource
/// authority; selected victims must still pass through ordinary revoke/fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreemptionOutcome {
    /// The request already fits without evicting any Lease.
    AlreadyFeasible,
    /// A lower-priority victim set makes the request feasible in projection.
    Planned,
    /// Evicting every eligible lower-priority Lease still cannot place it.
    NotFeasible,
}

/// One deterministic candidate considered by the preemption planner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreemptionStep {
    pub rank: usize,
    pub lease: LeaseId,
    pub owner: OwnerId,
    pub priority: u32,
    /// Whether the incoming request becomes feasible after this candidate and
    /// every earlier step are removed from projected occupancy.
    pub feasible_after_eviction: bool,
}

/// Machine-readable preemption reasoning for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreemptionPlan {
    pub request: RequestId,
    pub request_priority: u32,
    pub outcome: PreemptionOutcome,
    /// Victims only when `outcome == Planned`; empty for advisory failures.
    pub victims: Vec<LeaseId>,
    /// Ordered lower-priority candidates inspected by the planner.
    pub steps: Vec<PreemptionStep>,
}

/// Compatibility helper returning only the advisory victim set. `Some([])`
/// means no preemption is needed; `None` means lower-priority eviction cannot
/// make the request feasible.
pub fn preempt_victims(cluster: &Cluster, request: &Request) -> Option<Vec<LeaseId>> {
    let plan = preemption_plan(cluster, request);
    match plan.outcome {
        PreemptionOutcome::AlreadyFeasible | PreemptionOutcome::Planned => Some(plan.victims),
        PreemptionOutcome::NotFeasible => None,
    }
}

/// Build a deterministic advisory preemption plan without changing Cluster
/// state. Resources remain occupied until callers revoke the planned Leases
/// and their Bindings are fenced/released.
pub fn preemption_plan(cluster: &Cluster, request: &Request) -> PreemptionPlan {
    let blocked = cluster.placement_blocked();
    if select(&cluster.graph, &cluster.occupancy(), request, &blocked).is_ok() {
        return PreemptionPlan {
            request: request.id,
            request_priority: request.priority,
            outcome: PreemptionOutcome::AlreadyFeasible,
            victims: Vec::new(),
            steps: Vec::new(),
        };
    }

    let mut candidates: Vec<LeaseId> = cluster
        .leases
        .values()
        .filter(|lease| {
            lease.parent.is_none()
                && lease.priority < request.priority
                && cluster.occupies(lease.id)
        })
        .map(|lease| lease.id)
        .collect();
    candidates.sort_by(|left, right| {
        let left_lease = &cluster.leases[left];
        let right_lease = &cluster.leases[right];
        left_lease
            .priority
            .cmp(&right_lease.priority)
            .then(right.cmp(left))
    });

    let mut projected_victims = Vec::new();
    let mut steps = Vec::new();
    for (rank, candidate) in candidates.into_iter().enumerate() {
        projected_victims.push(candidate);
        let mut except: BTreeSet<LeaseId> = projected_victims.iter().copied().collect();
        for victim in &projected_victims {
            except.extend(cluster.descendants_postorder(*victim));
        }
        let occupancy = cluster.occupancy_except(&except);
        let blocked = cluster.placement_blocked();
        let feasible = select(&cluster.graph, &occupancy, request, &blocked).is_ok();
        let lease = &cluster.leases[&candidate];
        steps.push(PreemptionStep {
            rank,
            lease: candidate,
            owner: lease.owner,
            priority: lease.priority,
            feasible_after_eviction: feasible,
        });
        if feasible {
            return PreemptionPlan {
                request: request.id,
                request_priority: request.priority,
                outcome: PreemptionOutcome::Planned,
                victims: projected_victims,
                steps,
            };
        }
    }

    PreemptionPlan {
        request: request.id,
        request_priority: request.priority,
        outcome: PreemptionOutcome::NotFeasible,
        victims: Vec::new(),
        steps,
    }
}
