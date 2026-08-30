use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::ids::{FactWriterId, NodeId};
use crate::types::{
    Attrs, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind, FactEdge,
    FactWriterAssignment, Node, ProviderFactBatch, Quantity, ResourceClass, TopologyRelation,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct EdgeFactKey {
    from: NodeId,
    to: NodeId,
    kind: EdgeKind,
}

impl EdgeFactKey {
    fn from_edge(edge: &Edge) -> Self {
        Self {
            from: edge.from,
            to: edge.to,
            kind: edge.kind,
        }
    }

    fn from_fact(edge: FactEdge) -> Self {
        Self {
            from: edge.from,
            to: edge.to,
            kind: edge.kind,
        }
    }
}

#[cfg(feature = "serde")]
mod edge_writer_map {
    use super::EdgeFactKey;
    use crate::ids::FactWriterId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(
        map: &BTreeMap<EdgeFactKey, FactWriterId>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        map.iter()
            .map(|(key, writer)| (*key, *writer))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<EdgeFactKey, FactWriterId>, D::Error> {
        Ok(
            Vec::<(EdgeFactKey, FactWriterId)>::deserialize(deserializer)?
                .into_iter()
                .collect(),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Graph {
    pub revision: u64,
    nodes: BTreeMap<NodeId, Node>,
    edges: Vec<Edge>,
    parent: BTreeMap<NodeId, NodeId>,
    children: BTreeMap<NodeId, Vec<NodeId>>,
    by_class: BTreeMap<ResourceClass, Vec<NodeId>>,
    /// Revisioned provider contract for claimable capacity. Capacity without
    /// an entry here remains a placement fact and cannot become Lease
    /// authority merely because it has a positive quantity.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "BTreeMap::is_empty")
    )]
    claim_bindings: BTreeMap<NodeId, BTreeMap<CapacityDimension, ClaimBinding>>,
    /// Authoritative discovery writer for each complete Node fact set. A
    /// legacy Graph may have no entries until a verified provider adopts it.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "BTreeMap::is_empty")
    )]
    node_fact_writers: BTreeMap<NodeId, FactWriterId>,
    /// Authoritative discovery writer for each exact stored Edge tuple.
    #[cfg_attr(
        feature = "serde",
        serde(
            with = "edge_writer_map",
            default,
            skip_serializing_if = "BTreeMap::is_empty"
        )
    )]
    edge_fact_writers: BTreeMap<EdgeFactKey, FactWriterId>,
    /// Non-authoritative observations used for scoring and diagnostics.
    /// They never participate in `revision`, hard filters, Claims, or authority.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "BTreeMap::is_empty")
    )]
    observations: BTreeMap<NodeId, Attrs>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, nodes: Vec<Node>, edges: Vec<Edge>) -> Result<(), Error> {
        if (!nodes.is_empty() || !edges.is_empty()) && self.has_fact_writers() {
            return Err(Error::Refused {
                explanation: "legacy unowned Graph updates are disabled after explicit fact ownership is established".into(),
            });
        }
        self.apply_fact_values(nodes, edges)?;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    fn apply_fact_values(&mut self, nodes: Vec<Node>, edges: Vec<Edge>) -> Result<(), Error> {
        // Validate the complete update against staged state before mutating
        // anything: a rejected fact update must leave Graph state replay-safe.
        let mut staged_nodes = self.nodes.clone();
        for node in nodes {
            if node.attrs.contains_key("health") {
                return Err(Error::Refused {
                    explanation: format!(
                        "resource fact {} uses reserved observation key health",
                        node.id
                    ),
                });
            }
            staged_nodes.insert(node.id, node);
        }
        let mut staged_edges = self.edges.clone();
        for edge in edges {
            if !staged_nodes.contains_key(&edge.from) {
                return Err(Error::UnknownNode(edge.from));
            }
            if !staged_nodes.contains_key(&edge.to) {
                return Err(Error::UnknownNode(edge.to));
            }
            if !staged_edges.iter().any(|existing| {
                existing.from == edge.from && existing.to == edge.to && existing.kind == edge.kind
            }) {
                staged_edges.push(edge);
            } else {
                for existing in staged_edges.iter_mut() {
                    if existing.from == edge.from
                        && existing.to == edge.to
                        && existing.kind == edge.kind
                    {
                        existing.attrs = edge.attrs.clone();
                    }
                }
            }
        }
        validate_contains(&staged_edges)?;
        validate_claim_bindings(&staged_nodes, &self.claim_bindings)?;
        self.nodes = staged_nodes;
        self.edges = staged_edges;
        self.rebuild();
        Ok(())
    }

    /// Atomically apply topology/resource facts together with the provider
    /// contracts that make selected capacity claimable. Removals are staged
    /// before capacity changes, additions after, so a dimension can be
    /// withdrawn or introduced in one Graph revision.
    pub fn apply_resource_facts(
        &mut self,
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        updates: Vec<ClaimBindingUpdate>,
    ) -> Result<(), Error> {
        validate_update_keys(&updates)?;
        let target_revision = self.revision.saturating_add(1);
        let mut staged = self.clone();
        for update in updates.iter().filter(|update| update.binding.is_none()) {
            staged.set_claim_binding(*update)?;
        }
        staged.apply(nodes, edges)?;
        for update in updates.iter().filter(|update| update.binding.is_some()) {
            staged.set_claim_binding(*update)?;
        }
        validate_claim_bindings(&staged.nodes, &staged.claim_bindings)?;
        staged.revision = target_revision;
        *self = staged;
        Ok(())
    }

    /// Apply one or more explicitly-owned provider fragments as a single Graph
    /// revision. Existing unowned legacy facts must be adopted first; an
    /// existing fact owned by a different writer can never be overwritten.
    pub fn apply_provider_facts(
        &mut self,
        batches: Vec<ProviderFactBatch>,
        updates: Vec<ClaimBindingUpdate>,
    ) -> Result<(), Error> {
        validate_update_keys(&updates)?;
        let target_revision = self.revision.saturating_add(1);
        let mut staged = self.clone();
        for update in updates.iter().filter(|update| update.binding.is_none()) {
            staged.set_claim_binding(*update)?;
        }

        let mut seen_nodes = BTreeSet::new();
        let mut seen_edges = BTreeSet::new();
        for batch in batches {
            for node in &batch.nodes {
                if !seen_nodes.insert(node.id) {
                    return Err(Error::Refused {
                        explanation: format!(
                            "resource fact update names node {} more than once",
                            node.id
                        ),
                    });
                }
                match staged.node_fact_writers.get(&node.id).copied() {
                    Some(writer) if writer != batch.writer => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "resource node {} belongs to fact writer {} and cannot be overwritten by {}",
                                node.id, writer, batch.writer
                            ),
                        });
                    }
                    None if staged.nodes.contains_key(&node.id) => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "legacy resource node {} has no fact writer; adopt verified ownership before mutation",
                                node.id
                            ),
                        });
                    }
                    _ => {}
                }
            }
            for edge in &batch.edges {
                let key = EdgeFactKey::from_edge(edge);
                if !seen_edges.insert(key) {
                    return Err(Error::Refused {
                        explanation: format!(
                            "resource fact update names edge {} -> {} {:?} more than once",
                            edge.from, edge.to, edge.kind
                        ),
                    });
                }
                let exists = staged.edges.iter().any(|existing| {
                    existing.from == edge.from
                        && existing.to == edge.to
                        && existing.kind == edge.kind
                });
                match staged.edge_fact_writers.get(&key).copied() {
                    Some(writer) if writer != batch.writer => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "resource edge {} -> {} {:?} belongs to fact writer {} and cannot be overwritten by {}",
                                edge.from, edge.to, edge.kind, writer, batch.writer
                            ),
                        });
                    }
                    None if exists => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "legacy resource edge {} -> {} {:?} has no fact writer; adopt verified ownership before mutation",
                                edge.from, edge.to, edge.kind
                            ),
                        });
                    }
                    _ => {}
                }
            }

            let writer = batch.writer;
            let node_ids: Vec<_> = batch.nodes.iter().map(|node| node.id).collect();
            let edge_keys: Vec<_> = batch.edges.iter().map(EdgeFactKey::from_edge).collect();
            staged.apply_fact_values(batch.nodes, batch.edges)?;
            for node in node_ids {
                staged.node_fact_writers.insert(node, writer);
            }
            for edge in edge_keys {
                staged.edge_fact_writers.insert(edge, writer);
            }
        }

        for update in updates.iter().filter(|update| update.binding.is_some()) {
            staged.set_claim_binding(*update)?;
        }
        validate_claim_bindings(&staged.nodes, &staged.claim_bindings)?;
        staged.revision = target_revision;
        *self = staged;
        Ok(())
    }

    /// Adopt fact-writer provenance for a legacy Graph without modifying facts
    /// or advancing the scheduling revision. Conflicting prior ownership is
    /// rejected atomically.
    pub fn adopt_fact_writers(
        &mut self,
        assignments: Vec<FactWriterAssignment>,
    ) -> Result<(), Error> {
        let mut staged_nodes = self.node_fact_writers.clone();
        let mut staged_edges = self.edge_fact_writers.clone();
        let mut seen_nodes = BTreeSet::new();
        let mut seen_edges = BTreeSet::new();
        for assignment in assignments {
            for node in assignment.nodes {
                if !seen_nodes.insert(node) {
                    return Err(Error::Refused {
                        explanation: format!(
                            "fact writer adoption names node {node} more than once"
                        ),
                    });
                }
                if !self.nodes.contains_key(&node) {
                    return Err(Error::UnknownNode(node));
                }
                match staged_nodes.get(&node).copied() {
                    Some(writer) if writer != assignment.writer => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "resource node {node} already belongs to fact writer {writer}; cannot adopt {}",
                                assignment.writer
                            ),
                        });
                    }
                    _ => {
                        staged_nodes.insert(node, assignment.writer);
                    }
                }
            }
            for edge in assignment.edges {
                let key = EdgeFactKey::from_fact(edge);
                if !seen_edges.insert(key) {
                    return Err(Error::Refused {
                        explanation: format!(
                            "fact writer adoption names edge {} -> {} {:?} more than once",
                            edge.from, edge.to, edge.kind
                        ),
                    });
                }
                if !self.edges.iter().any(|existing| {
                    existing.from == edge.from
                        && existing.to == edge.to
                        && existing.kind == edge.kind
                }) {
                    return Err(Error::Refused {
                        explanation: format!(
                            "cannot adopt missing resource edge {} -> {} {:?}",
                            edge.from, edge.to, edge.kind
                        ),
                    });
                }
                match staged_edges.get(&key).copied() {
                    Some(writer) if writer != assignment.writer => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "resource edge {} -> {} {:?} already belongs to fact writer {}; cannot adopt {}",
                                edge.from, edge.to, edge.kind, writer, assignment.writer
                            ),
                        });
                    }
                    _ => {
                        staged_edges.insert(key, assignment.writer);
                    }
                }
            }
        }
        self.node_fact_writers = staged_nodes;
        self.edge_fact_writers = staged_edges;
        Ok(())
    }

    pub fn node_fact_writer(&self, node: NodeId) -> Option<FactWriterId> {
        self.node_fact_writers.get(&node).copied()
    }

    pub fn edge_fact_writer(&self, edge: FactEdge) -> Option<FactWriterId> {
        self.edge_fact_writers
            .get(&EdgeFactKey::from_fact(edge))
            .copied()
    }

    pub fn node_fact_writers(&self) -> &BTreeMap<NodeId, FactWriterId> {
        &self.node_fact_writers
    }

    pub fn edge_fact_writers(&self) -> Vec<(FactEdge, FactWriterId)> {
        self.edge_fact_writers
            .iter()
            .map(|(edge, writer)| (FactEdge::new(edge.from, edge.to, edge.kind), *writer))
            .collect()
    }

    fn has_fact_writers(&self) -> bool {
        !self.node_fact_writers.is_empty() || !self.edge_fact_writers.is_empty()
    }

    pub fn claim_binding(
        &self,
        node: NodeId,
        dimension: CapacityDimension,
    ) -> Option<ClaimBinding> {
        self.claim_bindings
            .get(&node)
            .and_then(|dimensions| dimensions.get(&dimension))
            .copied()
    }

    /// Resolve the single provider contract required by a Claim. The current
    /// Binding model has one provider/scope per claimed Node, so a quantity
    /// spanning dimensions with different contracts is rejected rather than
    /// silently choosing one.
    pub fn claim_binding_for_quantity(
        &self,
        node: NodeId,
        quantity: &Quantity,
    ) -> Result<ClaimBinding, Error> {
        if self.node(node).is_none() {
            return Err(Error::UnknownNode(node));
        }
        let mut selected = None;
        for (dimension, amount) in quantity {
            if *amount == 0 {
                continue;
            }
            let binding = self
                .claim_binding(node, *dimension)
                .ok_or(Error::UnclaimableNode { node })?;
            match selected {
                Some(existing) if existing != binding => {
                    return Err(Error::Refused {
                        explanation: format!(
                            "claim on {node} spans capacity dimensions with different provider bindings"
                        ),
                    });
                }
                None => selected = Some(binding),
                _ => {}
            }
        }
        selected.ok_or(Error::UnclaimableNode { node })
    }

    pub fn claim_bindings(&self) -> &BTreeMap<NodeId, BTreeMap<CapacityDimension, ClaimBinding>> {
        &self.claim_bindings
    }

    fn set_claim_binding(&mut self, update: ClaimBindingUpdate) -> Result<(), Error> {
        if self.node(update.node).is_none() {
            return Err(Error::UnknownNode(update.node));
        }
        match update.binding {
            Some(binding) => {
                self.claim_bindings
                    .entry(update.node)
                    .or_default()
                    .insert(update.dimension, binding);
            }
            None => {
                if let Some(dimensions) = self.claim_bindings.get_mut(&update.node) {
                    dimensions.remove(&update.dimension);
                    if dimensions.is_empty() {
                        self.claim_bindings.remove(&update.node);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn observation(&self, id: NodeId, key: &str) -> Option<&str> {
        self.observations
            .get(&id)
            .and_then(|attrs| attrs.get(key))
            .map(String::as_str)
    }

    pub fn observations(&self) -> &BTreeMap<NodeId, Attrs> {
        &self.observations
    }

    pub(crate) fn set_observation(&mut self, id: NodeId, key: &str, value: String) -> bool {
        if !self.nodes.contains_key(&id) {
            return false;
        }
        self.observations
            .entry(id)
            .or_default()
            .insert(key.to_string(), value);
        true
    }

    /// Upgrade snapshots written before observations were separated from
    /// hard resource facts. A dedicated observation wins if both forms exist.
    pub(crate) fn migrate_legacy_observations(&mut self) {
        let legacy: Vec<_> = self
            .nodes
            .iter_mut()
            .filter_map(|(id, node)| node.attrs.remove("health").map(|health| (*id, health)))
            .collect();
        for (id, health) in legacy {
            self.observations
                .entry(id)
                .or_default()
                .entry("health".into())
                .or_insert(health);
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    pub fn nodes_of_class(&self, kind: ResourceClass) -> &[NodeId] {
        self.by_class.get(&kind).map_or(&[], Vec::as_slice)
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.parent.get(&id).copied()
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.children.get(&id).map_or(&[], Vec::as_slice)
    }

    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut current = self.parent(id);
        while let Some(node) = current {
            out.push(node);
            current = self.parent(node);
        }
        out
    }

    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = self.children(id).to_vec();
        while let Some(node) = stack.pop() {
            out.push(node);
            stack.extend(self.children(node).iter().copied());
        }
        out
    }

    pub fn ancestor_of_class(&self, id: NodeId, kind: ResourceClass) -> Option<NodeId> {
        if self.node(id).is_some_and(|node| node.kind == kind) {
            return Some(id);
        }
        self.ancestors(id)
            .into_iter()
            .find(|ancestor| self.node(*ancestor).is_some_and(|node| node.kind == kind))
    }

    pub fn machine_of(&self, id: NodeId) -> Option<NodeId> {
        self.ancestor_of_class(id, ResourceClass::Machine)
    }

    /// Whether any node in `id`'s ancestry (including itself) has a
    /// `CachedOn` edge from `data` — i.e. the data is resident near `id`.
    pub fn caches(&self, id: NodeId, data: NodeId) -> bool {
        self.has_edge(data, id, EdgeKind::CachedOn)
            || self
                .ancestors(id)
                .into_iter()
                .any(|ancestor| self.has_edge(data, ancestor, EdgeKind::CachedOn))
    }

    /// The nearest ancestry node (including `id` itself) marked
    /// `health=degraded`, if any. Health is scoring input, never authority.
    pub fn degraded_ancestor(&self, id: NodeId) -> Option<NodeId> {
        let degraded = |node: NodeId| self.observation(node, "health") == Some("degraded");
        if degraded(id) {
            return Some(id);
        }
        self.ancestors(id)
            .into_iter()
            .find(|ancestor| degraded(*ancestor))
    }

    /// Evaluates a placement relationship from containment and sparse
    /// connectivity facts. Ancestor relationships are derived, so adding a
    /// new resource class does not require a new edge or kernel variant.
    pub fn satisfies(&self, left: NodeId, right: NodeId, relation: TopologyRelation) -> bool {
        match relation {
            TopologyRelation::SameAncestor { class } => same_ancestor(self, left, right, class),
            TopologyRelation::DifferentAncestor { class } => match (
                self.ancestor_of_class(left, class),
                self.ancestor_of_class(right, class),
            ) {
                (Some(left), Some(right)) => left != right,
                _ => false,
            },
            TopologyRelation::Contains => {
                self.ancestors(right).contains(&left) || self.ancestors(left).contains(&right)
            }
            TopologyRelation::Connected => self.has_edge(left, right, EdgeKind::Connected),
            TopologyRelation::CachedOn => self.has_edge(left, right, EdgeKind::CachedOn),
        }
    }

    /// Evaluates a sparse stored relationship. Containment relationships are
    /// derived from the hierarchy instead of duplicated as explicit edges.
    pub fn related(&self, left: NodeId, right: NodeId, kind: EdgeKind) -> bool {
        match kind {
            EdgeKind::Contains => {
                self.ancestors(right).contains(&left) || self.ancestors(left).contains(&right)
            }
            EdgeKind::Connected => self.has_edge(left, right, EdgeKind::Connected),
            EdgeKind::CachedOn => self.has_edge(left, right, EdgeKind::CachedOn),
        }
    }

    fn has_edge(&self, left: NodeId, right: NodeId, kind: EdgeKind) -> bool {
        self.edges.iter().any(|edge| {
            edge.kind == kind
                && ((edge.from == left && edge.to == right)
                    || (edge.from == right && edge.to == left))
        })
    }

    fn rebuild(&mut self) {
        self.parent.clear();
        self.children.clear();
        self.by_class.clear();
        for node in self.nodes.values() {
            self.by_class.entry(node.kind).or_default().push(node.id);
        }
        for ids in self.by_class.values_mut() {
            ids.sort();
        }
        for edge in &self.edges {
            if edge.kind != EdgeKind::Contains {
                continue;
            }
            self.parent.insert(edge.to, edge.from);
            self.children.entry(edge.from).or_default().push(edge.to);
        }
        for children in self.children.values_mut() {
            children.sort();
        }
    }
}

fn validate_update_keys(updates: &[ClaimBindingUpdate]) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for update in updates {
        if !seen.insert((update.node, update.dimension)) {
            return Err(Error::Refused {
                explanation: format!(
                    "duplicate claim-binding update for {} dimension {}",
                    update.node, update.dimension
                ),
            });
        }
    }
    Ok(())
}

fn validate_claim_bindings(
    nodes: &BTreeMap<NodeId, Node>,
    bindings: &BTreeMap<NodeId, BTreeMap<CapacityDimension, ClaimBinding>>,
) -> Result<(), Error> {
    for (id, dimensions) in bindings {
        let node = nodes.get(id).ok_or(Error::UnknownNode(*id))?;
        for dimension in dimensions.keys() {
            if node.capacity.get(dimension).copied().unwrap_or(0) == 0 {
                return Err(Error::Refused {
                    explanation: format!(
                        "claim binding for {id} dimension {dimension} has no positive capacity"
                    ),
                });
            }
        }
    }
    Ok(())
}

fn validate_contains(edges: &[Edge]) -> Result<(), Error> {
    let mut parent: BTreeMap<NodeId, NodeId> = BTreeMap::new();
    for edge in edges {
        if edge.kind != EdgeKind::Contains {
            continue;
        }
        if edge.from == edge.to {
            return Err(Error::InvalidTopology {
                reason: format!("node {} contains itself", edge.from),
            });
        }
        let existing = parent.insert(edge.to, edge.from);
        if existing.is_some_and(|previous| previous != edge.from) {
            return Err(Error::InvalidTopology {
                reason: format!("node {} has more than one container", edge.to),
            });
        }
    }
    // A Contains cycle would hang ancestor and descendant walks.
    for start in parent.keys() {
        let mut visited = BTreeSet::new();
        let mut current = Some(*start);
        while let Some(node) = current {
            if !visited.insert(node) {
                return Err(Error::InvalidTopology {
                    reason: format!("Contains cycle through node {start}"),
                });
            }
            current = parent.get(&node).copied();
        }
    }
    Ok(())
}

fn same_ancestor(graph: &Graph, left: NodeId, right: NodeId, kind: ResourceClass) -> bool {
    match (
        graph.ancestor_of_class(left, kind),
        graph.ancestor_of_class(right, kind),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
