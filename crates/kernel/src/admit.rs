use std::collections::BTreeMap;

use crate::error::Error;
use crate::graph::Graph;
use crate::ids::LeaseId;
use crate::occupancy::{Occupancy, claims_by_node};
use crate::select::select;
use crate::types::{
    Allocation, Quantity, QueuedRequest, Request, quantity_add_assign, quantity_le,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub request: Request,
    pub owner: crate::ids::OwnerId,
    pub submitted_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub request: Request,
    pub allocation: Allocation,
}

/// Plain admission: priority, then submit time, then request id. First
/// feasible request wins; an infeasible head is skipped, never blocking.
pub fn admit(
    graph: &Graph,
    occupancy: &Occupancy,
    queue: &[Queued],
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
) -> Option<Admission> {
    admit_fair(
        graph,
        occupancy,
        quarantine,
        &Quantity::new(),
        queue,
        &BTreeMap::new(),
    )
}

/// Budget-aware admission. `fair_share` is a per-owner ceiling, not a
/// reservation: an owner under budget is never blocked by another owner's
/// consumption. Pass an empty `fair_share` to disable the budget.
pub fn admit_fair(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
    fair_share: &Quantity,
    queue: &[Queued],
    leases: &BTreeMap<LeaseId, crate::types::Lease>,
) -> Option<Admission> {
    let queue: Vec<QueuedRequest> = queue
        .iter()
        .map(|queued| QueuedRequest {
            request: queued.request.clone(),
            owner: queued.owner,
            submitted_at: queued.submitted_at,
        })
        .collect();
    let usage = owner_usage(leases);
    let mut order: Vec<usize> = (0..queue.len()).collect();
    order.sort_by(|&left, &right| {
        let left_request = &queue[left].request;
        let right_request = &queue[right].request;
        right_request
            .priority
            .cmp(&left_request.priority)
            .then(queue[left].submitted_at.cmp(&queue[right].submitted_at))
            .then(left_request.id.cmp(&right_request.id))
            .then(
                usage_total(&usage.get(&queue[left].owner).cloned().unwrap_or_default()).cmp(
                    &usage_total(&usage.get(&queue[right].owner).cloned().unwrap_or_default()),
                ),
            )
    });
    for index in order {
        let queued = &queue[index];
        if !within_budget(
            &usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            fair_share,
        ) {
            continue;
        }
        if let Ok(allocation) = select(graph, occupancy, &queued.request, quarantine) {
            return Some(Admission {
                request: queued.request.clone(),
                allocation,
            });
        }
    }
    None
}

/// Per-owner consumption from active root leases. Only roots are counted so
/// nested children are not double-charged against their owner.
pub fn owner_usage(
    leases: &BTreeMap<LeaseId, crate::types::Lease>,
) -> BTreeMap<crate::ids::OwnerId, Quantity> {
    let mut usage = BTreeMap::new();
    for lease in leases.values() {
        if lease.parent.is_some() {
            continue;
        }
        if !matches!(
            lease.state,
            crate::types::LeaseState::Preparing | crate::types::LeaseState::Active
        ) {
            continue;
        }
        let entry = usage.entry(lease.owner).or_default();
        for (_node, quantity) in claims_by_node(&lease.allocation.claims) {
            quantity_add_assign(entry, &quantity);
        }
    }
    usage
}

fn usage_total(quantity: &Quantity) -> u64 {
    quantity.values().sum()
}

/// Whether `request` fits under `fair_share` for `owner` given current usage.
/// An empty ceiling disables the budget entirely.
pub fn within_budget(usage: &Quantity, request: &Request, fair_share: &Quantity) -> bool {
    if fair_share.is_empty() {
        return true;
    }
    let mut projected = usage.clone();
    for need in &request.needs {
        quantity_add_assign(&mut projected, &need.quantity);
    }
    quantity_le(&projected, fair_share)
}

pub fn refuse_reason(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
) -> Option<Error> {
    select(graph, occupancy, request, quarantine).err()
}
