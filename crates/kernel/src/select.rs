use std::collections::BTreeSet;

use crate::error::Error;
use crate::graph::Graph;
use crate::ids::NodeId;
use crate::occupancy::Occupancy;
use crate::types::{
    Allocation, Claim, Dimension, EdgeKind, Need, NodeKind, Preference, Request, RequestClass, qty,
    quantity_get,
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
    let mode = pack_mode(request);
    let mut picked: Vec<Vec<Claim>> = Vec::new();
    let mut notes = Vec::new();
    for need in &request.needs {
        let (claims, need_notes) =
            select_need(graph, occupancy, need, &picked, request, mode, quarantine)?;
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

fn pack_mode(request: &Request) -> PackMode {
    let mut mode = match request.class {
        RequestClass::Service | RequestClass::Batch | RequestClass::Gang => PackMode::Pack,
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

fn select_need(
    graph: &Graph,
    occupancy: &Occupancy,
    need: &Need,
    already: &[Vec<Claim>],
    request: &Request,
    mode: PackMode,
    quarantine: &BTreeSet<NodeId>,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
    let candidates = candidates(graph, occupancy, need, quarantine)?;
    if need.kind == NodeKind::Memory {
        let want = quantity_get(&need.quantity, Dimension::Bytes);
        if want == 0 {
            return Err(Error::Refused {
                explanation: "memory need has zero bytes".into(),
            });
        }
        let ctx = ScoreCtx {
            graph,
            occupancy,
            request,
            already,
            mode,
            memory_want: want,
        };
        for node in rank(&ctx, &[], &candidates)? {
            let remaining = occupancy.remaining(graph, node)?;
            if quantity_get(&remaining, Dimension::Bytes) < want {
                continue;
            }
            let claim = Claim {
                node,
                quantity: qty(Dimension::Bytes, want),
            };
            let mut trial = already.to_vec();
            trial.push(vec![claim.clone()]);
            if topology_holds(graph, &trial, request) {
                let score = score_node(&ctx, &[], node)?;
                let local = request.data.iter().any(|data| graph.caches(node, *data));
                return Ok((
                    vec![claim],
                    vec![format!(
                        "{node} score={score}{}",
                        if local { " data-local" } else { "" }
                    )],
                ));
            }
        }
        return Err(Error::Refused {
            explanation: format!("no memory node has {want} free bytes"),
        });
    }

    let count = quantity_get(&need.quantity, Dimension::Count).max(1);
    let mut chosen = Vec::new();
    let mut notes = Vec::new();
    let ctx = ScoreCtx {
        graph,
        occupancy,
        request,
        already,
        mode,
        memory_want: 0,
    };
    let ranked = rank(&ctx, &chosen, &candidates)?;
    for node in ranked {
        if chosen.len() as u64 >= count {
            break;
        }
        let claim = Claim {
            node,
            quantity: qty(Dimension::Count, 1),
        };
        let mut trial = already.to_vec();
        let mut group = chosen.clone();
        group.push(claim.clone());
        trial.push(group);
        if topology_holds(graph, &trial, request) {
            let score = score_node(&ctx, &chosen, node)?;
            let local = request.data.iter().any(|data| graph.caches(node, *data));
            notes.push(format!(
                "{node} score={score}{}",
                if local { " data-local" } else { "" }
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
    let mut out = Vec::new();
    for id in graph.nodes_of_kind(need.kind) {
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

fn need_unit(need: &Need) -> crate::types::Quantity {
    if need.kind == NodeKind::Memory {
        qty(
            Dimension::Bytes,
            quantity_get(&need.quantity, Dimension::Bytes),
        )
    } else {
        qty(Dimension::Count, 1)
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
        if extras
            .iter()
            .any(|claim| ctx.graph.related(node, claim.node, EdgeKind::SameNuma))
            && ctx.mode == PackMode::Pack
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
    // outranks pack/spread but not hard attribute preferences.
    for data in &ctx.request.data {
        if ctx.graph.caches(node, *data) {
            score += 10_000;
        }
    }
    if ctx.memory_want > 0 {
        let leftover = quantity_get(&ctx.occupancy.remaining(ctx.graph, node)?, Dimension::Bytes)
            .saturating_sub(ctx.memory_want)
            / (1 << 20);
        match ctx.mode {
            PackMode::Pack => score -= leftover as i64,
            PackMode::Spread => score += leftover as i64,
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
    for constraint in &request.topology {
        if constraint.left >= picked.len() || constraint.right >= picked.len() {
            continue;
        }
        for left in &picked[constraint.left] {
            for right in &picked[constraint.right] {
                if !graph.related(left.node, right.node, constraint.kind) {
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
