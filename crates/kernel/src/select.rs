use std::collections::BTreeSet;

use crate::error::Error;
use crate::graph::Graph;
use crate::ids::NodeId;
use crate::occupancy::Occupancy;
use crate::types::{Allocation, Claim, Dimension, Need, NodeKind, Request, qty, quantity_get};

pub fn select(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
) -> Result<Allocation, Error> {
    let mut picked: Vec<Vec<Claim>> = Vec::new();
    for need in &request.needs {
        let claims = select_need(graph, occupancy, need, &picked, request)?;
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
            "{:?} request selected {} claims by deterministic first-fit",
            request.class,
            picked.iter().map(Vec::len).sum::<usize>()
        ),
    })
}

fn select_need(
    graph: &Graph,
    occupancy: &Occupancy,
    need: &Need,
    already: &[Vec<Claim>],
    request: &Request,
) -> Result<Vec<Claim>, Error> {
    let candidates = candidates(graph, occupancy, need)?;
    if need.kind == NodeKind::Memory {
        let want = quantity_get(&need.quantity, Dimension::Bytes);
        if want == 0 {
            return Err(Error::Refused {
                explanation: "memory need has zero bytes".into(),
            });
        }
        for node in candidates {
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
                return Ok(vec![claim]);
            }
        }
        return Err(Error::Refused {
            explanation: format!("no memory node has {want} free bytes"),
        });
    }

    let count = quantity_get(&need.quantity, Dimension::Count).max(1);
    let mut chosen = Vec::new();
    for node in candidates {
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
            chosen.push(claim);
        }
    }
    if chosen.len() as u64 != count {
        return Err(Error::Refused {
            explanation: format!("need {:?} x{count} found only {}", need.kind, chosen.len()),
        });
    }
    Ok(chosen)
}

fn candidates(graph: &Graph, occupancy: &Occupancy, need: &Need) -> Result<Vec<NodeId>, Error> {
    let mut out = Vec::new();
    for id in graph.nodes_of_kind(need.kind) {
        let node = graph.node(*id).ok_or(Error::UnknownNode(*id))?;
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
