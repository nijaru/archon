from pathlib import Path

admit_path = Path('crates/kernel/src/admit.rs')
text = admit_path.read_text()

marker = '''#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub request: Request,
    pub owner: OwnerId,
    pub allocation: Allocation,
}
'''
insert = marker + '''
/// Machine-readable scheduler evaluation for one queue candidate. The trace
/// records policy/planning state only; it never grants resource authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionCandidateTrace {
    pub rank: usize,
    pub request: RequestId,
    pub owner: OwnerId,
    pub priority: u32,
    pub submitted_at: u64,
    pub owner_weight: Option<u32>,
    pub weighted_dominant_share: Option<u128>,
    pub outcome: AdmissionOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionOutcome {
    NotEvaluated,
    OverQuota,
    PlacementRefused { explanation: String },
    Blocked { shadow: Option<u64> },
    BackfillUnsafe { protected_heads: usize },
    Selected { backfilled: bool },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdmissionTrace {
    pub candidates: Vec<AdmissionCandidateTrace>,
}
'''
if text.count(marker) != 1:
    raise SystemExit('Admission marker changed')
text = text.replace(marker, insert, 1)

start = text.index('pub fn admit_with_policy(')
end = text.index('\n/// Per-owner, per-kind consumption', start)
new_policy = '''pub fn admit_with_policy(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
    policy: &AdmissionPolicy,
    queue: &[Queued],
    leases: &BTreeMap<LeaseId, crate::types::Lease>,
    open_bindings: &BTreeSet<LeaseId>,
) -> Option<Admission> {
    admit_with_policy_explained(
        graph,
        occupancy,
        quarantine,
        policy,
        queue,
        leases,
        open_bindings,
    )
    .0
}

/// Admission plus a typed trace of scheduler ordering and refusal outcomes.
/// The trace is observational policy state: the returned Allocation still has
/// to enter the ordinary conflict-checked Lease/Binding authority path.
pub fn admit_with_policy_explained(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
    policy: &AdmissionPolicy,
    queue: &[Queued],
    leases: &BTreeMap<LeaseId, crate::types::Lease>,
    open_bindings: &BTreeSet<LeaseId>,
) -> (Option<Admission>, AdmissionTrace) {
    let usage = owner_usage(graph, leases, open_bindings);
    let capacity = claimable_capacity(graph);
    let order = order_queue_with_policy(queue, &usage, &capacity, policy);
    let mut trace = build_admission_trace(queue, &order, &usage, &capacity, policy);
    for (rank, index) in order.into_iter().enumerate() {
        let queued = &queue[index];
        if !within_budget(
            usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            &policy.owner_ceiling,
        ) {
            trace.candidates[rank].outcome = AdmissionOutcome::OverQuota;
            continue;
        }
        match select(graph, occupancy, &queued.request, quarantine) {
            Ok(allocation) => {
                trace.candidates[rank].outcome = AdmissionOutcome::Selected { backfilled: false };
                return (
                    Some(Admission {
                        request: queued.request.clone(),
                        owner: queued.owner,
                        allocation,
                    }),
                    trace,
                );
            }
            Err(error) => {
                trace.candidates[rank].outcome = AdmissionOutcome::PlacementRefused {
                    explanation: error.to_string(),
                };
            }
        }
    }
    (None, trace)
}
'''
text = text[:start] + new_policy + text[end:]

order_end_marker = '''    order
}

/// A queued request that cannot start yet:'''
helper = '''    order
}

fn build_admission_trace(
    queue: &[Queued],
    order: &[usize],
    usage: &BTreeMap<OwnerId, ClassUsage>,
    capacity: &ClassUsage,
    policy: &AdmissionPolicy,
) -> AdmissionTrace {
    AdmissionTrace {
        candidates: order
            .iter()
            .enumerate()
            .map(|(rank, index)| {
                let queued = &queue[*index];
                let owner_weight = policy.fair_share.then(|| {
                    policy
                        .owner_weights
                        .get(&queued.owner)
                        .copied()
                        .unwrap_or(1)
                        .max(1)
                });
                let weighted_dominant_share = owner_weight.map(|weight| {
                    weighted_dominant_share(
                        usage.get(&queued.owner).unwrap_or(&ClassUsage::new()),
                        capacity,
                        weight,
                    )
                });
                AdmissionCandidateTrace {
                    rank,
                    request: queued.request.id,
                    owner: queued.owner,
                    priority: queued.request.priority,
                    submitted_at: queued.submitted_at,
                    owner_weight,
                    weighted_dominant_share,
                    outcome: AdmissionOutcome::NotEvaluated,
                }
            })
            .collect(),
    }
}

/// A queued request that cannot start yet:'''
if text.count(order_end_marker) != 1:
    raise SystemExit('order helper marker changed')
text = text.replace(order_end_marker, helper, 1)

start = text.index('pub fn admit_backfill_with_policy(')
end = text.index('\nfn occupying(', start)
new_backfill = '''pub fn admit_backfill_with_policy(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<NodeId>,
    exclusions: &RequestExclusions,
    policy: &AdmissionPolicy,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> Option<Admission> {
    admit_backfill_with_policy_explained(
        graph,
        occupancy,
        quarantine,
        exclusions,
        policy,
        queue,
        ctx,
    )
    .0
}

/// EASY-style backfill admission plus typed queue/planning outcomes. Shadow
/// reservations in this trace remain recomputable scheduler state and never
/// become authoritative Leases.
pub fn admit_backfill_with_policy_explained(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<NodeId>,
    exclusions: &RequestExclusions,
    policy: &AdmissionPolicy,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> (Option<Admission>, AdmissionTrace) {
    let BackfillCtx {
        now,
        leases,
        open_bindings,
    } = ctx;
    let usage = owner_usage(graph, leases, open_bindings);
    let capacity = claimable_capacity(graph);
    let order = order_queue_with_policy(queue, &usage, &capacity, policy);
    let mut trace = build_admission_trace(queue, &order, &usage, &capacity, policy);
    let mut blocked: Vec<BlockedHead> = Vec::new();
    for (rank, index) in order.into_iter().enumerate() {
        let queued = &queue[index];
        if !within_budget(
            usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            &policy.owner_ceiling,
        ) {
            trace.candidates[rank].outcome = AdmissionOutcome::OverQuota;
            continue;
        }
        let mut effective = quarantine.clone();
        let hard = exclusions
            .get(&queued.request.id)
            .cloned()
            .unwrap_or_default();
        effective.extend(hard.iter().copied());
        match select(graph, occupancy, &queued.request, &effective) {
            Ok(allocation) => {
                let claims: BTreeSet<NodeId> =
                    allocation.claims.iter().map(|claim| claim.node).collect();
                let finishes = now.saturating_add(queued.request.lifetime);
                let safe = blocked.iter().all(|head| match head.shadow {
                    Some(shadow) => finishes <= shadow || claims.is_disjoint(&head.shadow_claims),
                    None => claims.is_disjoint(&head.shadow_claims),
                });
                if safe {
                    trace.candidates[rank].outcome = AdmissionOutcome::Selected {
                        backfilled: !blocked.is_empty(),
                    };
                    return (
                        Some(Admission {
                            request: queued.request.clone(),
                            owner: queued.owner,
                            allocation,
                        }),
                        trace,
                    );
                }
                trace.candidates[rank].outcome = AdmissionOutcome::BackfillUnsafe {
                    protected_heads: blocked.len(),
                };
            }
            Err(error) => {
                if let Ok(allocation) = select(graph, occupancy, &queued.request, &hard) {
                    blocked.push(BlockedHead {
                        shadow: None,
                        shadow_claims: claims_by_node(&allocation.claims).into_keys().collect(),
                    });
                    trace.candidates[rank].outcome = AdmissionOutcome::Blocked { shadow: None };
                } else if let Some(head) = shadow_head(
                    graph,
                    &effective,
                    leases,
                    open_bindings,
                    &queued.request,
                    *now,
                ) {
                    let shadow = head.shadow;
                    blocked.push(head);
                    trace.candidates[rank].outcome = AdmissionOutcome::Blocked { shadow };
                } else {
                    trace.candidates[rank].outcome = AdmissionOutcome::PlacementRefused {
                        explanation: error.to_string(),
                    };
                }
            }
        }
    }
    (None, trace)
}
'''
text = text[:start] + new_backfill + text[end:]
admit_path.write_text(text)

lib_path = Path('crates/kernel/src/lib.rs')
lib = lib_path.read_text()
old = '''pub use admit::{
    Admission, AdmissionPolicy, BackfillCtx, ClassUsage, RequestExclusions, admit, admit_backfill,
    admit_backfill_with_exclusions, admit_backfill_with_policy, admit_with_ceiling,
    admit_with_policy, owner_usage, refuse_reason,
};'''
new = '''pub use admit::{
    Admission, AdmissionCandidateTrace, AdmissionOutcome, AdmissionPolicy, AdmissionTrace,
    BackfillCtx, ClassUsage, RequestExclusions, admit, admit_backfill,
    admit_backfill_with_exclusions, admit_backfill_with_policy,
    admit_backfill_with_policy_explained, admit_with_ceiling, admit_with_policy,
    admit_with_policy_explained, owner_usage, refuse_reason,
};'''
if lib.count(old) != 1:
    raise SystemExit('lib export marker changed')
lib_path.write_text(lib.replace(old, new, 1))
