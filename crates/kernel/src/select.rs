use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::graph::Graph;
use crate::ids::NodeId;
use crate::occupancy::Occupancy;
use crate::types::{
    Allocation, CapacityDimension, Claim, Need, PlacementExplanation, PlacementReason, Preference,
    Request, RequestClass, ResourceClass, TopologyRelation, qty, quantity_get,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PackMode {
    Pack,
    Spread,
}

struct ScoreCtx<'a> {
    graph: &'a Graph,
    occupancy: &'a Occupancy,
    request: &'a Request,
    already: &'a [Vec<Claim>],
    mode: PackMode,
    memory_want: u64,
}

#[derive(Clone, Copy)]
struct TopologyFailure {
    constraint: usize,
    left: NodeId,
    right: NodeId,
}

#[derive(Default)]
struct TopologyTrace {
    failures: BTreeMap<usize, TopologyFailure>,
}

impl TopologyTrace {
    fn record(&mut self, failure: TopologyFailure) {
        self.failures.entry(failure.constraint).or_insert(failure);
    }
}

pub fn select(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    quarantine: &BTreeSet<NodeId>,
) -> Result<Allocation, Error> {
    // Malformed constraints can never be satisfied; refuse them up front
    // rather than silently placing as if they held.
    if request.topology.iter().any(|constraint| {
        constraint.left >= request.needs.len() || constraint.right >= request.needs.len()
    }) {
        return Err(Error::Refused {
            explanation: "topology constraint names a nonexistent need".into(),
        });
    }
    let mode = pack_mode(request);
    // A machine-local request must land entirely on one machine: pick the
    // anchor once, then satisfy every need within its subtree. Single-need
    // requests already localize per need.
    if request.machine_local && request.needs.len() > 1 {
        return select_one_machine(graph, occupancy, request, mode, quarantine);
    }
    let ctx = SelectCtx {
        graph,
        occupancy,
        request,
        mode,
        quarantine,
    };
    let mut picked: Vec<Vec<Claim>> = Vec::new();
    let mut notes = Vec::new();
    let mut topology_trace = TopologyTrace::default();
    for need in &request.needs {
        let (claims, need_notes) = select_need(&ctx, need, &picked, &mut topology_trace)?;
        notes.extend(need_notes);
        picked.push(claims);
    }
    let mut claims = Vec::new();
    for group in &picked {
        claims.extend(group.iter().cloned());
    }
    if !topology_holds(graph, &picked, request) {
        return Err(Error::Refused {
            explanation: "topology constraints were not satisfied".into(),
        });
    }
    notes.extend(topology_notes(graph, &picked, request, &topology_trace));
    let reasons = placement_reasons(graph, occupancy, request, mode, &picked, &topology_trace)?;
    let claim_count = claims.len();
    Ok(Allocation {
        claims,
        graph_revision: graph.revision,
        explanation: PlacementExplanation::new(
            format!(
                "{:?} {:?} selected {claim_count} claims{}",
                request.class,
                mode,
                if notes.is_empty() {
                    String::new()
                } else {
                    format!("; {}", notes.join("; "))
                }
            ),
            reasons,
        ),
    })
}

/// Whole-machine placement: pick one anchor machine able to host every
/// need, then satisfy each need strictly within its subtree. Deterministic
/// machine order (ascending id); the first machine that fits all needs
/// wins.
fn select_one_machine(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    mode: PackMode,
    quarantine: &BTreeSet<NodeId>,
) -> Result<Allocation, Error> {
    let machines = graph.nodes_of_class(ResourceClass::Machine);
    let mut topology_trace = TopologyTrace::default();
    for &machine in machines {
        if quarantine.contains(&machine)
            || graph
                .ancestors(machine)
                .iter()
                .any(|ancestor| quarantine.contains(ancestor))
        {
            continue;
        }
        let mut allowed = std::collections::BTreeSet::new();
        allowed.insert(machine);
        allowed.extend(graph.descendants(machine));
        let ctx = SelectCtx {
            graph,
            occupancy,
            request,
            mode,
            quarantine,
        };
        let mut picked: Vec<Vec<Claim>> = Vec::new();
        let mut notes = Vec::new();
        let mut fits = true;
        for need in &request.needs {
            match select_need_in(&ctx, need, &picked, Some(&allowed), &mut topology_trace) {
                Ok((claims, need_notes)) => {
                    picked.push(claims);
                    notes.extend(need_notes);
                }
                Err(_) => {
                    fits = false;
                    break;
                }
            }
        }
        if fits && topology_holds(graph, &picked, request) {
            let mut claims = Vec::new();
            for group in &picked {
                claims.extend(group.iter().cloned());
            }
            notes.extend(topology_notes(graph, &picked, request, &topology_trace));
            let reasons =
                placement_reasons(graph, occupancy, request, mode, &picked, &topology_trace)?;
            return Ok(machine_allocation(
                graph, request, mode, machine, claims, notes, reasons,
            ));
        }

        // The ordinary selector is deliberately greedy. A failed greedy
        // attempt can still have a valid allocation when an early need has a
        // locally attractive candidate that conflicts with a later topology
        // constraint. Search this machine with a small, deterministic budget
        // before refusing the complete request.
        if request.topology.is_empty() {
            continue;
        }
        let mut budget = SearchBudget::new(FALLBACK_SEARCH_BUDGET);
        let mut fallback = vec![Vec::new(); request.needs.len()];
        let mut search = MachineSearch {
            graph,
            occupancy,
            request,
            quarantine,
            allowed: &allowed,
            topology_trace: &mut topology_trace,
            budget: &mut budget,
        };
        if search.needs(0, &mut fallback)? {
            let claims = fallback
                .iter()
                .flat_map(|group| group.iter().cloned())
                .collect();
            let mut notes = selection_notes(graph, occupancy, request, mode, &fallback)?;
            notes.extend(topology_notes(graph, &fallback, request, &topology_trace));
            let reasons =
                placement_reasons(graph, occupancy, request, mode, &fallback, &topology_trace)?;
            return Ok(machine_allocation(
                graph, request, mode, machine, claims, notes, reasons,
            ));
        }
    }
    Err(Error::Refused {
        explanation: if request.topology.is_empty() {
            "no single machine hosts every need of this workload".into()
        } else {
            "no single machine satisfies every need and topology constraint of this workload".into()
        },
    })
}

const FALLBACK_SEARCH_BUDGET: usize = 4_096;

struct SearchBudget {
    remaining: usize,
}

impl SearchBudget {
    fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn consume(&mut self) -> bool {
        if self.remaining == 0 {
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
}

struct MachineSearch<'a> {
    graph: &'a Graph,
    occupancy: &'a Occupancy,
    request: &'a Request,
    quarantine: &'a BTreeSet<NodeId>,
    allowed: &'a BTreeSet<NodeId>,
    topology_trace: &'a mut TopologyTrace,
    budget: &'a mut SearchBudget,
}

impl MachineSearch<'_> {
    fn needs(&mut self, index: usize, picked: &mut Vec<Vec<Claim>>) -> Result<bool, Error> {
        if index == self.request.needs.len() {
            return Ok(topology_holds_traced(
                self.graph,
                picked,
                self.request,
                self.topology_trace,
            ));
        }

        let need = &self.request.needs[index];
        let candidates: Vec<_> = candidates(self.graph, self.occupancy, need, self.quarantine)?
            .into_iter()
            .filter(|node| self.allowed.contains(node))
            .collect();
        if candidates.is_empty() {
            return Ok(false);
        }
        let ranked = rank(
            &ScoreCtx {
                graph: self.graph,
                occupancy: self.occupancy,
                request: self.request,
                already: picked,
                mode: pack_mode(self.request),
                memory_want: quantity_get(&consumable_quantity(need), CapacityDimension::Bytes),
            },
            &[],
            &candidates,
        )?;

        if is_consumable_need(need) {
            let wanted = consumable_quantity(need);
            for node in ranked {
                if !self.budget.consume() {
                    return Ok(false);
                }
                let claim = Claim {
                    node,
                    quantity: wanted.clone(),
                };
                if !self.occupancy.can_cover(self.graph, &claim)? {
                    continue;
                }
                picked[index] = vec![claim];
                if topology_holds_traced(self.graph, picked, self.request, self.topology_trace)
                    && self.needs(index + 1, picked)?
                {
                    return Ok(true);
                }
                picked[index].clear();
            }
            return Ok(false);
        }

        let count = quantity_get(&need.quantity, CapacityDimension::Count).max(1);
        let mut chosen = Vec::new();
        self.group(index, &ranked, count, 0, &mut chosen, picked)
    }

    fn group(
        &mut self,
        index: usize,
        ranked: &[NodeId],
        count: u64,
        start: usize,
        chosen: &mut Vec<Claim>,
        picked: &mut Vec<Vec<Claim>>,
    ) -> Result<bool, Error> {
        if chosen.len() as u64 == count {
            return self.needs(index + 1, picked);
        }

        for (position, node) in ranked.iter().enumerate().skip(start) {
            if !self.budget.consume() {
                return Ok(false);
            }
            let claim = Claim {
                node: *node,
                quantity: qty(CapacityDimension::Count, 1),
            };
            if !self.occupancy.can_cover(self.graph, &claim)? {
                continue;
            }
            chosen.push(claim);
            picked[index] = chosen.clone();
            let valid =
                topology_holds_traced(self.graph, picked, self.request, self.topology_trace);
            if valid && self.group(index, ranked, count, position + 1, chosen, picked)? {
                return Ok(true);
            }
            chosen.pop();
            picked[index] = chosen.clone();
        }
        Ok(false)
    }
}

fn machine_allocation(
    graph: &Graph,
    request: &Request,
    mode: PackMode,
    machine: NodeId,
    claims: Vec<Claim>,
    notes: Vec<String>,
    mut reasons: Vec<PlacementReason>,
) -> Allocation {
    reasons.insert(0, PlacementReason::MachineSelected { machine });
    let claim_count = claims.len();
    Allocation {
        claims,
        graph_revision: graph.revision,
        explanation: PlacementExplanation::new(
            format!(
                "{:?} {:?} selected {claim_count} claims on {machine}{}",
                request.class,
                mode,
                if notes.is_empty() {
                    String::new()
                } else {
                    format!("; {}", notes.join("; "))
                }
            ),
            reasons,
        ),
    }
}

fn selection_notes(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    mode: PackMode,
    picked: &[Vec<Claim>],
) -> Result<Vec<String>, Error> {
    let mut notes = Vec::new();
    for (index, group) in picked.iter().enumerate() {
        let need = &request.needs[index];
        let ctx = ScoreCtx {
            graph,
            occupancy,
            request,
            already: &picked[..index],
            mode,
            memory_want: quantity_get(&consumable_quantity(need), CapacityDimension::Bytes),
        };
        let mut chosen = Vec::new();
        for claim in group {
            let score = score_node(&ctx, &chosen, claim.node)?;
            notes.push(format!(
                "{} score={score}{}{}",
                claim.node,
                if request
                    .data
                    .iter()
                    .any(|data| graph.caches(claim.node, *data))
                {
                    " data-local"
                } else {
                    ""
                },
                if graph.degraded_ancestor(claim.node).is_some() {
                    " health-degraded"
                } else {
                    ""
                }
            ));
            chosen.push(claim.clone());
        }
    }
    Ok(notes)
}

fn pack_mode(request: &Request) -> PackMode {
    let mut mode = match request.class {
        RequestClass::Service | RequestClass::Batch => PackMode::Pack,
    };
    for preference in &request.preferences {
        match preference {
            Preference::Pack => mode = PackMode::Pack,
            Preference::Spread => mode = PackMode::Spread,
            Preference::PreferAttr { .. } => {}
        }
    }
    mode
}

struct SelectCtx<'a> {
    graph: &'a Graph,
    occupancy: &'a Occupancy,
    request: &'a Request,
    mode: PackMode,
    quarantine: &'a BTreeSet<NodeId>,
}

fn select_need(
    ctx: &SelectCtx<'_>,
    need: &Need,
    already: &[Vec<Claim>],
    topology_trace: &mut TopologyTrace,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
    select_need_in(ctx, need, already, None, topology_trace)
}

/// `allowed` restricts the candidate nodes (whole-machine placement).
fn select_need_in(
    ctx: &SelectCtx<'_>,
    need: &Need,
    already: &[Vec<Claim>],
    allowed: Option<&std::collections::BTreeSet<NodeId>>,
    topology_trace: &mut TopologyTrace,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
    let SelectCtx {
        graph,
        occupancy,
        request,
        mode,
        quarantine,
    } = *ctx;
    let quarantine: &BTreeSet<NodeId> = quarantine;
    let candidates: Vec<NodeId> = candidates(graph, occupancy, need, quarantine)?
        .into_iter()
        .filter(|node| allowed.is_none_or(|set| set.contains(node)))
        .collect();
    if candidates.is_empty() {
        return Err(Error::Refused {
            explanation: format!("no {:?} node within the allowed set", need.kind),
        });
    }
    if is_consumable_need(need) {
        let wanted = consumable_quantity(need);
        if wanted.is_empty() || wanted.values().all(|amount| *amount == 0) {
            return Err(Error::Refused {
                explanation: if need.kind == ResourceClass::Memory {
                    "memory need has zero bytes".into()
                } else {
                    format!("need {:?} has no positive capacity", need.kind)
                },
            });
        }
        let memory_want = quantity_get(&wanted, CapacityDimension::Bytes);
        let ctx = ScoreCtx {
            graph,
            occupancy,
            request,
            already,
            mode,
            memory_want,
        };
        for node in rank(&ctx, &[], &candidates)? {
            let claim = Claim {
                node,
                quantity: wanted.clone(),
            };
            if !occupancy.can_cover(graph, &claim)? {
                continue;
            }
            let mut trial = already.to_vec();
            trial.push(vec![claim.clone()]);
            if topology_holds_traced(graph, &trial, request, topology_trace) {
                let score = score_node(&ctx, &[], node)?;
                return Ok((
                    vec![claim],
                    vec![format!(
                        "{node} score={score}{}{}",
                        if request.data.iter().any(|data| graph.caches(node, *data)) {
                            " data-local"
                        } else {
                            ""
                        },
                        if graph.degraded_ancestor(node).is_some() {
                            " health-degraded"
                        } else {
                            ""
                        }
                    )],
                ));
            }
        }
        return Err(Error::Refused {
            explanation: if need.kind == ResourceClass::Memory {
                format!("no memory node has {memory_want} free bytes")
            } else {
                format!("no {:?} node has the requested capacity", need.kind)
            },
        });
    }

    // A need's claims are machine-local by default: one workload's compute
    // cannot span hosts. Requests with machine_local=false (explicit
    // multi-member groups) may spread across machines as before.
    let count = quantity_get(&need.quantity, CapacityDimension::Count).max(1);
    let ctx = ScoreCtx {
        graph,
        occupancy,
        request,
        already,
        mode,
        memory_want: 0,
    };
    if !request.machine_local {
        return select_spread(already, need, &ctx, candidates, count, topology_trace);
    }
    let ranked = rank(&ctx, &[], &candidates)?;
    let mut machines: Vec<NodeId> = Vec::new();
    let mut by_machine: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for node in ranked {
        let Some(machine) = graph.machine_of(node) else {
            continue;
        };
        if !by_machine.contains_key(&machine) {
            machines.push(machine);
        }
        by_machine.entry(machine).or_default().push(node);
    }
    for machine in &machines {
        let mut chosen: Vec<Claim> = Vec::new();
        let mut notes = Vec::new();
        for &node in by_machine.get(machine).expect("grouped") {
            if chosen.len() as u64 >= count {
                break;
            }
            let claim = Claim {
                node,
                quantity: qty(CapacityDimension::Count, 1),
            };
            let mut trial = already.to_vec();
            let mut group = chosen.clone();
            group.push(claim.clone());
            trial.push(group);
            if topology_holds_traced(graph, &trial, request, topology_trace) {
                let score = score_node(&ctx, &chosen, node)?;
                let local = request.data.iter().any(|data| graph.caches(node, *data));
                let degraded = graph.degraded_ancestor(node).is_some();
                notes.push(format!(
                    "{node} score={score}{}{}",
                    if local { " data-local" } else { "" },
                    if degraded { " health-degraded" } else { "" }
                ));
                chosen.push(claim);
            }
        }
        if chosen.len() as u64 == count {
            return Ok((chosen, notes));
        }
    }
    Err(Error::Refused {
        explanation: format!(
            "need {:?} x{count} does not fit on any single machine",
            need.kind
        ),
    })
}

/// The legacy spread path: claims may land on any machine, in ranked
/// order, subject only to topology constraints.
fn select_spread(
    already: &[Vec<Claim>],
    need: &Need,
    ctx: &ScoreCtx<'_>,
    candidates: Vec<NodeId>,
    count: u64,
    topology_trace: &mut TopologyTrace,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
    let graph = ctx.graph;
    let request = ctx.request;
    let mut chosen: Vec<Claim> = Vec::new();
    let mut notes = Vec::new();
    let ranked = rank(ctx, &chosen, &candidates)?;
    for node in ranked {
        if chosen.len() as u64 >= count {
            break;
        }
        let claim = Claim {
            node,
            quantity: qty(CapacityDimension::Count, 1),
        };
        let mut trial = already.to_vec();
        let mut group = chosen.clone();
        group.push(claim.clone());
        trial.push(group);
        if topology_holds_traced(graph, &trial, request, topology_trace) {
            let score = score_node(ctx, &chosen, node)?;
            let local = request.data.iter().any(|data| graph.caches(node, *data));
            let degraded = graph.degraded_ancestor(node).is_some();
            notes.push(format!(
                "{node} score={score}{}{}",
                if local { " data-local" } else { "" },
                if degraded { " health-degraded" } else { "" }
            ));
            chosen.push(claim);
        }
    }
    if chosen.len() as u64 != count {
        return Err(Error::Refused {
            explanation: format!("need {:?} x{count} found only {}", need.kind, chosen.len()),
        });
    }
    Ok((chosen, notes))
}

fn candidates(
    graph: &Graph,
    occupancy: &Occupancy,
    need: &Need,
    quarantine: &BTreeSet<NodeId>,
) -> Result<Vec<NodeId>, Error> {
    if need.kind == ResourceClass::DataObject {
        return Err(Error::Refused {
            explanation: "data objects are locality hints, not claimable resources".into(),
        });
    }
    let required = need_unit(need);
    let mut out = Vec::new();
    for id in graph.nodes_of_class(need.kind) {
        let node = graph.node(*id).ok_or(Error::UnknownNode(*id))?;
        if graph.claim_binding_for_quantity(*id, &required).is_err() {
            continue;
        }
        if quarantine.contains(&node.id)
            || graph
                .ancestors(node.id)
                .into_iter()
                .any(|ancestor| quarantine.contains(&ancestor))
        {
            continue;
        }
        if need
            .filters
            .iter()
            .any(|filter| node.attrs.get(&filter.key) != Some(&filter.value))
        {
            continue;
        }
        if occupancy.can_cover(
            graph,
            &Claim {
                node: *id,
                quantity: required.clone(),
            },
        )? {
            out.push(*id);
        }
    }
    Ok(out)
}

fn is_consumable_need(need: &Need) -> bool {
    need.kind == ResourceClass::Memory
        || need
            .quantity
            .keys()
            .any(|dimension| *dimension != CapacityDimension::Count)
}

fn consumable_quantity(need: &Need) -> crate::types::Quantity {
    if need.kind == ResourceClass::Memory {
        qty(
            CapacityDimension::Bytes,
            quantity_get(&need.quantity, CapacityDimension::Bytes),
        )
    } else {
        need.quantity.clone()
    }
}

fn need_unit(need: &Need) -> crate::types::Quantity {
    if is_consumable_need(need) {
        consumable_quantity(need)
    } else {
        qty(CapacityDimension::Count, 1)
    }
}

fn rank(ctx: &ScoreCtx<'_>, chosen: &[Claim], candidates: &[NodeId]) -> Result<Vec<NodeId>, Error> {
    let mut scored = Vec::new();
    for node in candidates {
        scored.push((score_node(ctx, chosen, *node)?, *node));
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    Ok(scored.into_iter().map(|(_, node)| node).collect())
}

fn score_node(ctx: &ScoreCtx<'_>, chosen: &[Claim], node: NodeId) -> Result<i64, Error> {
    let mut score = 0;
    let extras = extras(ctx.already, chosen);
    if let Some(machine) = ctx.graph.machine_of(node) {
        let same_request = extras
            .iter()
            .filter(|claim| ctx.graph.machine_of(claim.node) == Some(machine))
            .count() as i64;
        let load = machine_load(ctx.graph, ctx.occupancy, machine, &extras) as i64;
        match ctx.mode {
            PackMode::Pack => {
                score += 100 * same_request;
                score += 10 * load;
            }
            PackMode::Spread => {
                score -= 100 * same_request;
                score -= 10 * load;
            }
        }
        if extras.iter().any(|claim| {
            ctx.graph.satisfies(
                node,
                claim.node,
                TopologyRelation::SameAncestor {
                    class: ResourceClass::Numa,
                },
            )
        }) && ctx.mode == PackMode::Pack
        {
            score += 50;
        }
    }
    for preference in &ctx.request.preferences {
        if let Preference::PreferAttr { key, value } = preference
            && ctx
                .graph
                .node(node)
                .is_some_and(|item| item.attrs.get(key) == Some(value))
        {
            score += 1_000_000;
        }
    }
    // Data locality: each requested object cached on this node's ancestry
    // outranks pack/spread but not hard attribute preferences. Duplicate
    // data ids count once.
    for data in ctx.request.data.iter().collect::<BTreeSet<_>>() {
        if ctx.graph.caches(node, *data) {
            score += 10_000;
        }
    }
    // Health: degraded ancestry is a strong penalty, not a refusal —
    // degraded resources remain usable for lower-priority workloads.
    if ctx.graph.degraded_ancestor(node).is_some() {
        score -= 20_000;
    }
    if ctx.memory_want > 0 {
        // Bounded so fragmentation preference stays below the locality and
        // health tiers regardless of node size.
        let leftover = (quantity_get(
            &ctx.occupancy.remaining(ctx.graph, node)?,
            CapacityDimension::Bytes,
        )
        .saturating_sub(ctx.memory_want)
            / (1 << 20))
            .min(4_000) as i64;
        match ctx.mode {
            PackMode::Pack => score -= leftover,
            PackMode::Spread => score += leftover,
        }
    }
    Ok(score)
}

fn extras(already: &[Vec<Claim>], chosen: &[Claim]) -> Vec<Claim> {
    let mut out = Vec::new();
    for group in already {
        out.extend(group.iter().cloned());
    }
    out.extend(chosen.iter().cloned());
    out
}

fn machine_load(graph: &Graph, occupancy: &Occupancy, machine: NodeId, extra: &[Claim]) -> u64 {
    graph
        .descendants(machine)
        .into_iter()
        .filter(|id| occupancy.is_used(*id) || extra.iter().any(|claim| claim.node == *id))
        .count() as u64
}

fn topology_failure(
    graph: &Graph,
    picked: &[Vec<Claim>],
    request: &Request,
) -> Option<TopologyFailure> {
    // Constraints referencing needs not yet selected cannot be evaluated
    // mid-placement; select() refuses malformed requests up front.
    for (index, constraint) in request.topology.iter().enumerate() {
        if constraint.left >= picked.len() || constraint.right >= picked.len() {
            continue;
        }
        for left in &picked[constraint.left] {
            for right in &picked[constraint.right] {
                if !graph.satisfies(left.node, right.node, constraint.relation) {
                    return Some(TopologyFailure {
                        constraint: index,
                        left: left.node,
                        right: right.node,
                    });
                }
            }
        }
    }
    None
}

fn claims_are_unique(picked: &[Vec<Claim>]) -> bool {
    let mut seen = BTreeSet::new();
    picked
        .iter()
        .flat_map(|group| group.iter())
        .all(|claim| seen.insert(claim.node))
}

fn topology_holds_traced(
    graph: &Graph,
    picked: &[Vec<Claim>],
    request: &Request,
    trace: &mut TopologyTrace,
) -> bool {
    if !claims_are_unique(picked) {
        return false;
    }
    match topology_failure(graph, picked, request) {
        Some(failure) => {
            trace.record(failure);
            false
        }
        None => true,
    }
}

fn topology_holds(graph: &Graph, picked: &[Vec<Claim>], request: &Request) -> bool {
    claims_are_unique(picked) && topology_failure(graph, picked, request).is_none()
}

fn claim_nodes(claims: &[Claim]) -> String {
    claims
        .iter()
        .map(|claim| claim.node.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn ancestor_nodes(graph: &Graph, claims: &[Claim], class: ResourceClass) -> String {
    claims
        .iter()
        .filter_map(|claim| graph.ancestor_of_class(claim.node, class))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|node| node.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn placement_reasons(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    mode: PackMode,
    picked: &[Vec<Claim>],
    trace: &TopologyTrace,
) -> Result<Vec<PlacementReason>, Error> {
    let mut reasons = Vec::new();
    for (index, group) in picked.iter().enumerate() {
        let need = &request.needs[index];
        let ctx = ScoreCtx {
            graph,
            occupancy,
            request,
            already: &picked[..index],
            mode,
            memory_want: quantity_get(&consumable_quantity(need), CapacityDimension::Bytes),
        };
        let mut chosen = Vec::new();
        for claim in group {
            let data_local = request
                .data
                .iter()
                .copied()
                .filter(|data| graph.caches(claim.node, *data))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            reasons.push(PlacementReason::CandidateScore {
                node: claim.node,
                score: score_node(&ctx, &chosen, claim.node)?,
                data_local,
                degraded_ancestor: graph.degraded_ancestor(claim.node),
            });
            chosen.push(claim.clone());
        }
    }
    for (index, failure) in &trace.failures {
        let Some(constraint) = request.topology.get(*index) else {
            continue;
        };
        let Some(left) = picked.get(constraint.left) else {
            continue;
        };
        let Some(right) = picked.get(constraint.right) else {
            continue;
        };
        reasons.push(PlacementReason::TopologyConstraint {
            index: *index,
            relation: constraint.relation,
            selected_left: left.iter().map(|claim| claim.node).collect(),
            selected_right: right.iter().map(|claim| claim.node).collect(),
            rejected: Some((failure.left, failure.right)),
        });
    }
    Ok(reasons)
}

fn topology_notes(
    graph: &Graph,
    picked: &[Vec<Claim>],
    request: &Request,
    trace: &TopologyTrace,
) -> Vec<String> {
    let mut notes = Vec::new();
    for (index, failure) in &trace.failures {
        let Some(constraint) = request.topology.get(*index) else {
            continue;
        };
        let Some(left) = picked.get(constraint.left) else {
            continue;
        };
        let Some(right) = picked.get(constraint.right) else {
            continue;
        };
        let selected = match constraint.relation {
            TopologyRelation::SameAncestor { class } => format!(
                "selected need[{}]={} and need[{}]={} under {class} {}",
                constraint.left,
                claim_nodes(left),
                constraint.right,
                claim_nodes(right),
                ancestor_nodes(graph, left, class),
            ),
            TopologyRelation::DifferentAncestor { class } => format!(
                "selected need[{}]={} under {class} {} and need[{}]={} under {class} {}",
                constraint.left,
                claim_nodes(left),
                ancestor_nodes(graph, left, class),
                constraint.right,
                claim_nodes(right),
                ancestor_nodes(graph, right, class),
            ),
            TopologyRelation::Contains => format!(
                "selected containment-related need[{}]={} and need[{}]={}",
                constraint.left,
                claim_nodes(left),
                constraint.right,
                claim_nodes(right),
            ),
            TopologyRelation::Connected => format!(
                "selected connected need[{}]={} and need[{}]={}",
                constraint.left,
                claim_nodes(left),
                constraint.right,
                claim_nodes(right),
            ),
            TopologyRelation::CachedOn => format!(
                "selected cache-related need[{}]={} and need[{}]={}",
                constraint.left,
                claim_nodes(left),
                constraint.right,
                claim_nodes(right),
            ),
        };
        let relation = match constraint.relation {
            TopologyRelation::SameAncestor { class } => format!("same-ancestor({class})"),
            TopologyRelation::DifferentAncestor { class } => {
                format!("different-ancestor({class})")
            }
            TopologyRelation::Contains => "contains".into(),
            TopologyRelation::Connected => "connected".into(),
            TopologyRelation::CachedOn => "cached-on".into(),
        };
        notes.push(format!(
            "topology[{index}] {relation} constrained placement (rejected {} vs {}); {selected}",
            failure.left, failure.right
        ));
    }
    notes
}
