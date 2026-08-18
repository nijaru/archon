use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::graph::Graph;
use crate::ids::{LeaseId, NodeId};
use crate::types::{
    Claim, Lease, LeaseState, Quantity, quantity_add_assign, quantity_get, quantity_le,
    quantity_max, quantity_saturating_sub,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Occupancy {
    used: BTreeMap<NodeId, Quantity>,
}

impl Occupancy {
    pub fn remaining(&self, graph: &Graph, node: NodeId) -> Result<Quantity, Error> {
        let capacity = &graph.node(node).ok_or(Error::UnknownNode(node))?.capacity;
        Ok(quantity_saturating_sub(
            capacity,
            self.used.get(&node).unwrap_or(&Quantity::new()),
        ))
    }

    pub fn can_cover(&self, graph: &Graph, claim: &Claim) -> Result<bool, Error> {
        let remaining = self.remaining(graph, claim.node)?;
        Ok(quantity_le(&claim.quantity, &remaining))
    }

    pub fn used_on(&self, node: NodeId) -> Quantity {
        self.used.get(&node).cloned().unwrap_or_default()
    }
}

pub fn lease_occupies(lease: &Lease, open_bindings: bool) -> bool {
    matches!(lease.state, LeaseState::Preparing | LeaseState::Active) || open_bindings
}

pub fn resolve_claim(graph: &Graph, claim: &Claim) -> Result<Claim, Error> {
    let node = graph
        .node(claim.node)
        .ok_or(Error::UnknownNode(claim.node))?;
    let quantity = if claim.quantity.is_empty() {
        node.capacity.clone()
    } else {
        claim.quantity.clone()
    };
    Ok(Claim {
        node: claim.node,
        quantity,
    })
}

pub fn covers(parent: &[Claim], child: &[Claim]) -> bool {
    child.iter().all(|child_claim| {
        parent.iter().any(|parent_claim| {
            parent_claim.node == child_claim.node
                && quantity_le(&child_claim.quantity, &parent_claim.quantity)
        })
    })
}

pub fn claims_by_node(claims: &[Claim]) -> BTreeMap<NodeId, Quantity> {
    let mut out = BTreeMap::new();
    for claim in claims {
        let entry = out.entry(claim.node).or_default();
        *entry = quantity_max(entry, &claim.quantity);
    }
    out
}

pub fn occupancy_from_leases<'a>(
    leases: impl IntoIterator<Item = &'a Lease>,
    open_binding_leases: &BTreeSet<LeaseId>,
    except: &BTreeSet<LeaseId>,
) -> Occupancy {
    let occupying: Vec<&Lease> = leases
        .into_iter()
        .filter(|lease| {
            !except.contains(&lease.id)
                && lease_occupies(lease, open_binding_leases.contains(&lease.id))
        })
        .collect();
    let roots: BTreeSet<LeaseId> = occupying
        .iter()
        .filter(|lease| {
            lease
                .parent
                .is_none_or(|parent| occupying.iter().all(|other| other.id != parent))
        })
        .map(|lease| lease.id)
        .collect();
    let mut used = BTreeMap::new();
    for lease in occupying {
        if !roots.contains(&lease.id) {
            continue;
        }
        for (node, quantity) in claims_by_node(&lease.allocation.claims) {
            let entry = used.entry(node).or_default();
            *entry = quantity_max(entry, &quantity);
        }
    }
    Occupancy { used }
}

pub fn sibling_used(
    leases: &BTreeMap<LeaseId, Lease>,
    open_binding_leases: &BTreeSet<LeaseId>,
    parent: LeaseId,
    except: LeaseId,
) -> Occupancy {
    let mut used = BTreeMap::new();
    for lease in leases.values() {
        if lease.id == except || lease.parent != Some(parent) {
            continue;
        }
        if !lease_occupies(lease, open_binding_leases.contains(&lease.id)) {
            continue;
        }
        for (node, quantity) in claims_by_node(&lease.allocation.claims) {
            quantity_add_assign(used.entry(node).or_default(), &quantity);
        }
    }
    Occupancy { used }
}

pub fn claim_fits(capacity: &Quantity, used: &Quantity, claim: &Quantity) -> bool {
    claim.iter().all(|(dimension, amount)| {
        let cap = quantity_get(capacity, *dimension);
        let taken = quantity_get(used, *dimension);
        cap.saturating_sub(taken) >= *amount
    })
}
