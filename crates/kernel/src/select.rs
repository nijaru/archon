use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::graph::Graph;
use crate::ids::NodeId;
use crate::occupancy::Occupancy;
use crate::types::{
    Allocation, CapacityDimension, Claim, Need, Preference, Request, RequestClass, ResourceClass,
    TopologyRelation, qty, quantity_get,
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
    for need in &request.needs {
        let (claims, need_notes) = select_need(&ctx, need, &picked)?;
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
    Ok(Allocation {
        claims,
        graph_revision: graph.revision,
        explanation: format!(
            "{:?} {:?} selected {} claims{}",
            request.class,
            mode,
            notes.len(),
            if notes.is_empty() {
                String::new()
            } else {
                format!("; {}", notes.join("; "))
            }
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
            match select_need_in(&ctx, need, &picked, Some(&allowed)) {
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
        if !fits || !topology_holds(graph, &picked, request) {
            continue;
        }
        let mut claims = Vec::new();
        for group in &picked {
            claims.extend(group.iter().cloned());
        }
        return Ok(Allocation {
            claims,
            graph_revision: graph.revision,
            explanation: format!(
                "{:?} {:?} selected {} claims on {machine}",
                request.class,
                mode,
                notes.len()
            ),
        });
    }
    Err(Error::Refused {
        explanation: "no single machine hosts every need of this workload".into(),
    })
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
) -> Result<(Vec<Claim>, Vec<String>), Error> {
    select_need_in(ctx, need, already, None)
}

/// `allowed` restricts the candidate nodes (whole-machine placement).
fn select_need_in(
    ctx: &SelectCtx<'_>,
    need: &Need,
    already: &[Vec<Claim>],
    allowed: Option<&std::collections::BTreeSet<NodeId>>,
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
            if topology_holds(graph, &trial, request) {
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
        return select_spread(graph, request, already, need, &ctx, candidates, count);
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
            if topology_holds(graph, &trial, request) {
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
    graph: &Graph,
    request: &Request,
    already: &[Vec<Claim>],
    need: &Need,
    ctx: &ScoreCtx<'_>,
    candidates: Vec<NodeId>,
    count: u64,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
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
        if topology_holds(graph, &trial, request) {
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
    let mut out = Vec::new();
    for id in graph.nodes_of_class(need.kind) {
        let node = graph.node(*id).ok_or(Error::UnknownNode(*id))?;
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
                quantity: need_unit(need),
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

fn topology_holds(graph: &Graph, picked: &[Vec<Claim>], request: &Request) -> bool {
    // Constraints referencing needs not yet selected cannot be evaluated
    // mid-placement; select() refuses malformed requests up front.
    for constraint in &request.topology {
        if constraint.left >= picked.len() || constraint.right >= picked.len() {
            continue;
        }
        for left in &picked[constraint.left] {
            for right in &picked[constraint.right] {
                if !graph.satisfies(left.node, right.node, constraint.relation) {
                    return false;
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    for group in picked {
        for claim in group {
            if !seen.insert(claim.node) {
                return false;
            }
        }
    }
    true
}
