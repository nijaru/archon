use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::ids::NodeId;
use crate::types::{Edge, EdgeKind, Node, NodeKind};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graph {
    pub revision: u64,
    nodes: BTreeMap<NodeId, Node>,
    edges: Vec<Edge>,
    parent: BTreeMap<NodeId, NodeId>,
    children: BTreeMap<NodeId, Vec<NodeId>>,
    by_kind: BTreeMap<NodeKind, Vec<NodeId>>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, nodes: Vec<Node>, edges: Vec<Edge>) -> Result<(), Error> {
        // Validate the complete update against staged state before mutating
        // anything: a rejected ApplyGraph must leave the Graph and the command
        // log consistent for replay.
        let mut staged_nodes = self.nodes.clone();
        for node in nodes {
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
                // Same-tuple edges update attributes in place; topology
                // shape is unchanged, so the forest check below still sees
                // one edge per tuple.
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
        self.nodes = staged_nodes;
        self.edges = staged_edges;
        self.rebuild();
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Set one node attribute without advancing `Graph.revision`. Returns
    /// false when the node is unknown. Used for non-authoritative state such
    /// as health that must never strand in-flight Allocations.
    pub fn set_attr(&mut self, id: NodeId, key: &str, value: String) -> bool {
        match self.nodes.get_mut(&id) {
            Some(node) => {
                node.attrs.insert(key.to_string(), value);
                true
            }
            None => false,
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

    pub fn nodes_of_kind(&self, kind: NodeKind) -> &[NodeId] {
        self.by_kind.get(&kind).map_or(&[], Vec::as_slice)
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

    pub fn ancestor_of_kind(&self, id: NodeId, kind: NodeKind) -> Option<NodeId> {
        if self.node(id).is_some_and(|node| node.kind == kind) {
            return Some(id);
        }
        self.ancestors(id)
            .into_iter()
            .find(|ancestor| self.node(*ancestor).is_some_and(|node| node.kind == kind))
    }

    pub fn machine_of(&self, id: NodeId) -> Option<NodeId> {
        self.ancestor_of_kind(id, NodeKind::Machine)
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
        let degraded = |node: NodeId| {
            self.node(node).and_then(|item| item.attrs.get("health"))
                == Some(&"degraded".to_string())
        };
        if degraded(id) {
            return Some(id);
        }
        self.ancestors(id)
            .into_iter()
            .find(|ancestor| degraded(*ancestor))
    }

    pub fn related(&self, left: NodeId, right: NodeId, kind: EdgeKind) -> bool {
        match kind {
            EdgeKind::Contains => {
                self.ancestors(right).contains(&left) || self.ancestors(left).contains(&right)
            }
            EdgeKind::SameNuma => {
                same_ancestor(self, left, right, NodeKind::Numa)
                    || self.has_edge(left, right, EdgeKind::SameNuma)
            }
            EdgeKind::SamePcie => {
                same_ancestor(self, left, right, NodeKind::PcieRoot)
                    || self.has_edge(left, right, EdgeKind::SamePcie)
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
        self.by_kind.clear();
        for node in self.nodes.values() {
            self.by_kind.entry(node.kind).or_default().push(node.id);
        }
        for ids in self.by_kind.values_mut() {
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

fn same_ancestor(graph: &Graph, left: NodeId, right: NodeId, kind: NodeKind) -> bool {
    match (
        graph.ancestor_of_kind(left, kind),
        graph.ancestor_of_kind(right, kind),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
