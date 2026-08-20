use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::graph::Graph;
use crate::ids::{LeaseId, NodeId, OwnerId};
use crate::occupancy::{Occupancy, claims_by_node, lease_occupies, occupancy_from_leases};
use crate::select::select;
use crate::types::{
    Allocation, Lease, Quantity, QueuedRequest, Request, quantity_add_assign, quantity_le,
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
    for index in order_queue(&queue, &usage) {
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
            crate::types::LeaseState::Reserved
                | crate::types::LeaseState::Preparing
                | crate::types::LeaseState::Active
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

fn order_queue(queue: &[QueuedRequest], usage: &BTreeMap<OwnerId, Quantity>) -> Vec<usize> {
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
    order
}

/// A queued request that cannot start yet, with the earliest time its
/// capacity frees (shadow) and the claims it would take at that time.
struct BlockedHead {
    shadow: u64,
    shadow_claims: BTreeSet<NodeId>,
}

/// Cluster state the shadow model reads: the clock, the lease table, and the
/// set of leases still occupying through open Bindings.
pub struct BackfillCtx<'a> {
    pub now: u64,
    pub leases: &'a BTreeMap<LeaseId, Lease>,
    pub open_bindings: &'a BTreeSet<LeaseId>,
}

/// EASY-style backfill over the priority queue. The first feasible request in
/// order is admitted as in plain admission. Requests ahead of it that cannot
/// select become blocked heads with a shadow time derived from lease expiry;
/// a later request may start only if it finishes by every blocked head's
/// shadow or claims none of the nodes that head would take. A request that
/// cannot select even after every expiring lease is unsatisfiable: it blocks
/// nobody. The shadow model is optimistic — renewals may push real start
/// times later; backfill never makes them earlier.
pub fn admit_backfill(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<NodeId>,
    fair_share: &Quantity,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> Option<Admission> {
    let BackfillCtx {
        now,
        leases,
        open_bindings,
    } = ctx;
    let queue: Vec<QueuedRequest> = queue
        .iter()
        .map(|queued| QueuedRequest {
            request: queued.request.clone(),
            owner: queued.owner,
            submitted_at: queued.submitted_at,
        })
        .collect();
    let usage = owner_usage(leases);
    let mut blocked: Vec<BlockedHead> = Vec::new();
    for index in order_queue(&queue, &usage) {
        let queued = &queue[index];
        if !within_budget(
            &usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            fair_share,
        ) {
            continue;
        }
        match select(graph, occupancy, &queued.request, quarantine) {
            Ok(allocation) => {
                let claims: BTreeSet<NodeId> =
                    allocation.claims.iter().map(|claim| claim.node).collect();
                let finishes = now.saturating_add(queued.request.lifetime);
                let safe = blocked.iter().all(|head| {
                    finishes <= head.shadow || claims.is_disjoint(&head.shadow_claims)
                });
                if safe {
                    return Some(Admission {
                        request: queued.request.clone(),
                        allocation,
                    });
                }
            }
            Err(_) => {
                if let Some(head) = shadow_head(
                    graph,
                    quarantine,
                    leases,
                    open_bindings,
                    &queued.request,
                    *now,
                ) {
                    blocked.push(head);
                }
            }
        }
    }
    None
}

fn occupying(lease: &Lease, open_bindings: &BTreeSet<LeaseId>) -> bool {
    lease_occupies(lease, open_bindings.contains(&lease.id))
}

/// Earliest lease-expiry event time at which `request` selects, plus the
/// claims it would take then. None means the request can never select.
fn shadow_head(
    graph: &Graph,
    quarantine: &std::collections::BTreeSet<NodeId>,
    leases: &BTreeMap<LeaseId, Lease>,
    open_bindings: &BTreeSet<LeaseId>,
    request: &Request,
    now: u64,
) -> Option<BlockedHead> {
    let mut times: BTreeSet<u64> = leases
        .values()
        .filter(|lease| occupying(lease, open_bindings) && lease.expires_at > now)
        .map(|lease| lease.expires_at)
        .collect();
    times.insert(u64::MAX);
    for shadow in times {
        let except: BTreeSet<LeaseId> = leases
            .values()
            .filter(|lease| occupying(lease, open_bindings) && lease.expires_at <= shadow)
            .map(|lease| lease.id)
            .collect();
        let projected = occupancy_from_leases(leases.values(), open_bindings, &except);
        if let Ok(allocation) = select(graph, &projected, request, quarantine) {
            return Some(BlockedHead {
                shadow,
                shadow_claims: claims_by_node(&allocation.claims).into_keys().collect(),
            });
        }
    }
    None
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
