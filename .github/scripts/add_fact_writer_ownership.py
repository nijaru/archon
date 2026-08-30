from pathlib import Path
import re


def replace_once(text, old, new, label):
    n = text.count(old)
    if n != 1:
        raise SystemExit(f"{label}: expected one match, found {n}")
    return text.replace(old, new, 1)


def sub_once(text, pattern, repl, label):
    out, n = re.subn(pattern, repl, text, count=1, flags=re.S)
    if n != 1:
        raise SystemExit(f"{label}: expected one regex match, found {n}")
    return out


def patch(path, fn):
    p = Path(path)
    old = p.read_text()
    new = fn(old)
    if old == new:
        raise SystemExit(f"{path}: no changes")
    p.write_text(new)


def ids(text):
    text = replace_once(text, "id!(ProviderId);", "id!(ProviderId);\nid!(FactWriterId);", "FactWriterId")
    return text


def types(text):
    text = replace_once(
        text,
        "use crate::ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId};",
        "use crate::ids::{BindingId, FactWriterId, LeaseId, NodeId, OwnerId, ProviderId};",
        "types id import",
    )
    marker = "/// A placement relationship evaluated against the graph's containment and\n"
    addition = '''/// Stable identity for one stored edge fact. Fact-writer ownership is
/// tracked on the exact stored tuple rather than on derived topology predicates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FactEdge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

impl FactEdge {
    pub const fn new(from: NodeId, to: NodeId, kind: EdgeKind) -> Self {
        Self { from, to, kind }
    }
}

/// One provider/discovery writer's atomic contribution to revisioned Graph
/// facts. Node ownership is deliberately coarse in this contract: one writer
/// owns the complete fact set for a Node. Providers compose by adding their
/// own Nodes/Edges that reference another writer's Nodes; same-Node multi-writer
/// augmentation requires an explicit future composition rule.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProviderFactBatch {
    pub writer: FactWriterId,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Upgrade-only adoption of already-persisted facts that predate explicit
/// discovery ownership. Adoption never changes a fact or Graph revision and
/// can only fill an unowned writer slot; an existing different writer wins.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FactWriterAssignment {
    pub writer: FactWriterId,
    pub nodes: Vec<NodeId>,
    pub edges: Vec<FactEdge>,
}

'''
    return replace_once(text, marker, addition + marker, "fact writer types")


def command(text):
    text = replace_once(
        text,
        "use crate::types::{Allocation, BindingScope, ClaimBindingUpdate, Edge, Node, NodeState};",
        "use crate::types::{\n    Allocation, BindingScope, ClaimBindingUpdate, Edge, FactWriterAssignment, Node, NodeState,\n    ProviderFactBatch,\n};",
        "command imports",
    )
    marker = '''    /// Apply provider-authored resource facts and their claim contracts in one
    /// Graph revision. Capacity omitted from `claim_bindings` is placement-only.
    ApplyResourceFacts {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        claim_bindings: Vec<ClaimBindingUpdate>,
    },
'''
    addition = marker + '''    /// Apply explicitly-owned provider facts and claim contracts in one Graph
    /// revision. `ProviderFactBatch.writer` is discovery/fact provenance and
    /// is intentionally independent from `ClaimBinding.provider` enforcement.
    ApplyProviderFacts {
        batches: Vec<ProviderFactBatch>,
        claim_bindings: Vec<ClaimBindingUpdate>,
    },
    /// Adopt writer provenance for legacy facts after the caller has verified
    /// them against a current authoritative provider inventory.
    AdoptFactWriters {
        assignments: Vec<FactWriterAssignment>,
    },
'''
    return replace_once(text, marker, addition, "command variants")


def graph(text):
    text = replace_once(
        text,
        "use crate::ids::NodeId;",
        "use crate::ids::{FactWriterId, NodeId};",
        "graph id import",
    )
    text = replace_once(
        text,
        "    Attrs, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind, Node, Quantity,\n    ResourceClass, TopologyRelation,",
        "    Attrs, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind, FactEdge,\n    FactWriterAssignment, Node, ProviderFactBatch, Quantity, ResourceClass, TopologyRelation,",
        "graph type imports",
    )
    marker = "#[derive(Clone, Debug, Default, PartialEq, Eq)]\n#[cfg_attr(feature = \"serde\", derive(serde::Serialize, serde::Deserialize))]\npub struct Graph {"
    prefix = '''#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
        Ok(Vec::<(EdgeFactKey, FactWriterId)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

'''
    text = replace_once(text, marker, prefix + marker, "edge writer key")
    field_marker = '''    claim_bindings: BTreeMap<NodeId, BTreeMap<CapacityDimension, ClaimBinding>>,
    /// Non-authoritative observations used for scoring and diagnostics.'''
    fields = '''    claim_bindings: BTreeMap<NodeId, BTreeMap<CapacityDimension, ClaimBinding>>,
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
    /// Non-authoritative observations used for scoring and diagnostics.'''
    text = replace_once(text, field_marker, fields, "graph writer fields")

    apply_pattern = r'''    pub fn apply\(&mut self, nodes: Vec<Node>, edges: Vec<Edge>\) -> Result<\(\), Error> \{.*?\n    \}\n\n    /// Atomically apply topology/resource facts'''
    apply_repl = '''    pub fn apply(&mut self, nodes: Vec<Node>, edges: Vec<Edge>) -> Result<(), Error> {
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

    /// Atomically apply topology/resource facts'''
    text = sub_once(text, apply_pattern, apply_repl, "graph apply refactor")

    insert_marker = '''    pub fn claim_binding(
        &self,
        node: NodeId,
        dimension: CapacityDimension,
    ) -> Option<ClaimBinding> {'''
    methods = '''    /// Apply one or more explicitly-owned provider fragments as a single Graph
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
                        explanation: format!("resource fact update names node {} more than once", node.id),
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
                        explanation: format!("fact writer adoption names node {node} more than once"),
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
            .map(|(edge, writer)| {
                (
                    FactEdge::new(edge.from, edge.to, edge.kind),
                    *writer,
                )
            })
            .collect()
    }

    fn has_fact_writers(&self) -> bool {
        !self.node_fact_writers.is_empty() || !self.edge_fact_writers.is_empty()
    }

'''
    text = replace_once(text, insert_marker, methods + insert_marker, "provider fact methods")
    return text


def cluster(text):
    text = replace_once(
        text,
        "use crate::ids::{BindingId, LeaseId, NodeId};",
        "use crate::ids::{BindingId, FactWriterId, LeaseId, NodeId};",
        "cluster ids",
    )
    text = replace_once(
        text,
        "    Binding, BindingScope, BindingState, ClaimBinding, Lease, LeaseState, NodeState, Quantity,",
        "    Binding, BindingScope, BindingState, ClaimBinding, FactEdge, FactWriterAssignment, Lease,\n    LeaseState, NodeState, ProviderFactBatch, Quantity,",
        "cluster types",
    )
    text = replace_once(
        text,
        "    pub graph_observations: BTreeMap<NodeId, crate::types::Attrs>,\n    pub leases:",
        "    pub graph_observations: BTreeMap<NodeId, crate::types::Attrs>,\n    pub graph_node_fact_writers: BTreeMap<NodeId, FactWriterId>,\n    pub graph_edge_fact_writers: Vec<(FactEdge, FactWriterId)>,\n    pub leases:",
        "digest writer fields",
    )
    text = replace_once(
        text,
        "            graph_observations: self.graph.observations().clone(),\n            leases:",
        "            graph_observations: self.graph.observations().clone(),\n            graph_node_fact_writers: self.graph.node_fact_writers().clone(),\n            graph_edge_fact_writers: self.graph.edge_fact_writers(),\n            leases:",
        "digest writer values",
    )
    dispatch_old = '''            Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings,
            } => self.apply_resource_facts(nodes.clone(), edges.clone(), claim_bindings.clone()),
            command @ Command::ReserveLease { .. } => self.reserve_lease(command),'''
    dispatch_new = '''            Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings,
            } => self.apply_resource_facts(nodes.clone(), edges.clone(), claim_bindings.clone()),
            Command::ApplyProviderFacts {
                batches,
                claim_bindings,
            } => self.apply_provider_facts(batches.clone(), claim_bindings.clone()),
            Command::AdoptFactWriters { assignments } => {
                self.adopt_fact_writers(assignments.clone())
            }
            command @ Command::ReserveLease { .. } => self.reserve_lease(command),'''
    text = replace_once(text, dispatch_old, dispatch_new, "cluster dispatch writers")
    marker = "    fn reserve_lease(&mut self, command: &Command) -> Result<Vec<Effect>, Error> {"
    methods = '''    fn apply_provider_facts(
        &mut self,
        batches: Vec<ProviderFactBatch>,
        claim_bindings: Vec<crate::types::ClaimBindingUpdate>,
    ) -> Result<Vec<Effect>, Error> {
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        let mut staged = self.graph.clone();
        staged.apply_provider_facts(batches, claim_bindings)?;
        let occupancy = self.occupancy();
        if let Some(node) = occupancy.exceeds_capacity(&staged)? {
            return Err(Error::CapacityBelowOccupancy { node });
        }
        self.graph = staged;
        Ok(Vec::new())
    }

    fn adopt_fact_writers(
        &mut self,
        assignments: Vec<FactWriterAssignment>,
    ) -> Result<Vec<Effect>, Error> {
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        let mut staged = self.graph.clone();
        staged.adopt_fact_writers(assignments)?;
        self.graph = staged;
        Ok(Vec::new())
    }

'''
    return replace_once(text, marker, methods + marker, "cluster writer methods")


def lib(text):
    text = replace_once(
        text,
        "pub use ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId, RequestId};",
        "pub use ids::{BindingId, FactWriterId, LeaseId, NodeId, OwnerId, ProviderId, RequestId};",
        "lib ids",
    )
    text = replace_once(
        text,
        "    Allocation, Attrs, Binding, BindingScope, BindingState, CapacityDimension, Claim, ClaimBinding,\n    ClaimBindingUpdate, Edge, EdgeKind, Endpoint, EndpointPhase, Filter, IdentifierError, Lease,",
        "    Allocation, Attrs, Binding, BindingScope, BindingState, CapacityDimension, Claim, ClaimBinding,\n    ClaimBindingUpdate, Edge, EdgeKind, Endpoint, EndpointPhase, FactEdge, FactWriterAssignment,\n    Filter, IdentifierError, Lease,",
        "lib type export 1",
    )
    text = replace_once(
        text,
        "    LeaseState, Need, Node, NodeState, PortPublish, Preference, Quantity, Queued, Request,",
        "    LeaseState, Need, Node, NodeState, PortPublish, Preference, ProviderFactBatch, Quantity, Queued, Request,",
        "lib type export 2",
    )
    return text


def discover(text):
    text = replace_once(
        text,
        "    Attrs, BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind, Node,\n    ProviderId, Quantity, ResourceClass, qty, quantity_get,",
        "    Attrs, BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind,\n    FactEdge, FactWriterAssignment, FactWriterId, Node, ProviderFactBatch, ProviderId, Quantity,\n    ResourceClass, qty, quantity_get,",
        "discover imports",
    )
    marker = "pub(crate) const HOST_ID_ATTR: &str = \"archon.host-id\";\npub(crate) const DEVICE_HOST_PARENT_ATTR: &str = \"archon.host-parent\";"
    repl = marker + '''
pub(crate) const HOST_FACT_WRITER: FactWriterId = FactWriterId::from_u64(1);
pub(crate) const DEVICE_FACT_WRITER: FactWriterId = FactWriterId::from_u64(2);'''
    text = replace_once(text, marker, repl, "discover writer constants")
    marker2 = "/// Validate the complete provider-normalized machine inventory before it can\n"
    helpers = '''/// Split one validated machine graph into independently-owned provider fact
/// fragments while retaining one atomic Cluster update. The current Agent
/// aggregates host discovery and device discovery as two writers; the kernel
/// contract supports additional writers without changing ownership semantics.
pub(crate) fn provider_fact_batches(nodes: Vec<Node>, edges: Vec<Edge>) -> Vec<ProviderFactBatch> {
    let mut by_node = BTreeMap::new();
    for node in &nodes {
        let writer = if matches!(
            node.kind,
            ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
        ) {
            DEVICE_FACT_WRITER
        } else {
            HOST_FACT_WRITER
        };
        by_node.insert(node.id, writer);
    }
    let mut host = ProviderFactBatch {
        writer: HOST_FACT_WRITER,
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    let mut device = ProviderFactBatch {
        writer: DEVICE_FACT_WRITER,
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    for node in nodes {
        if by_node[&node.id] == DEVICE_FACT_WRITER {
            device.nodes.push(node);
        } else {
            host.nodes.push(node);
        }
    }
    for edge in edges {
        let writer = by_node.get(&edge.to).copied().unwrap_or(HOST_FACT_WRITER);
        if writer == DEVICE_FACT_WRITER {
            device.edges.push(edge);
        } else {
            host.edges.push(edge);
        }
    }
    [host, device]
        .into_iter()
        .filter(|batch| !batch.nodes.is_empty() || !batch.edges.is_empty())
        .collect()
}

/// Expected ownership for the currently-reported portion of one machine's
/// legacy Graph. Omitted device tombstones are deliberately excluded: a
/// current provider must not acquire authority over a resource it did not
/// report merely because a stale Node remains persisted.
pub(crate) fn current_fact_writer_assignments(
    graph: &archon_kernel::Graph,
    machine: archon_kernel::NodeId,
    devices: &[DeviceSpec],
) -> Vec<FactWriterAssignment> {
    let current_devices: std::collections::BTreeSet<_> =
        devices.iter().map(|device| device.id.as_str()).collect();
    let mut host_nodes = Vec::new();
    let mut device_nodes = Vec::new();
    let mut included = BTreeMap::new();
    let mut ids = graph.descendants(machine);
    ids.push(machine);
    for id in ids {
        let Some(node) = graph.node(id) else { continue };
        let writer = if matches!(
            node.kind,
            ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
        ) {
            if !node
                .attrs
                .get("id")
                .is_some_and(|stable| current_devices.contains(stable.as_str()))
            {
                continue;
            }
            DEVICE_FACT_WRITER
        } else {
            HOST_FACT_WRITER
        };
        included.insert(id, writer);
        if writer == DEVICE_FACT_WRITER {
            device_nodes.push(id);
        } else {
            host_nodes.push(id);
        }
    }
    let mut host_edges = Vec::new();
    let mut device_edges = Vec::new();
    for edge in graph.edges() {
        if edge.kind != EdgeKind::Contains {
            continue;
        }
        let Some(writer) = included.get(&edge.to).copied() else {
            continue;
        };
        let fact = FactEdge::new(edge.from, edge.to, edge.kind);
        if writer == DEVICE_FACT_WRITER {
            device_edges.push(fact);
        } else {
            host_edges.push(fact);
        }
    }
    [
        FactWriterAssignment {
            writer: HOST_FACT_WRITER,
            nodes: host_nodes,
            edges: host_edges,
        },
        FactWriterAssignment {
            writer: DEVICE_FACT_WRITER,
            nodes: device_nodes,
            edges: device_edges,
        },
    ]
    .into_iter()
    .filter(|assignment| !assignment.nodes.is_empty() || !assignment.edges.is_empty())
    .collect()
}

'''
    return replace_once(text, marker2, helpers + marker2, "discover writer helpers")


def service(text):
    text = replace_once(
        text,
        "    Allocation, BindingId, CapacityDimension, Cluster, Command, Effect, Error, LeaseId, NodeId,\n    OwnerId, ProviderId, Queued, Request, RequestId, ResourceClass, quantity_get,",
        "    Allocation, BindingId, CapacityDimension, Cluster, Command, Effect, Error, FactEdge,\n    FactWriterAssignment, LeaseId, NodeId, OwnerId, ProviderFactBatch, ProviderId, Queued, Request,\n    RequestId, ResourceClass, quantity_get,",
        "service imports",
    )
    old_seq = '''                if let Err(err) = self.preflight_claim_contracts(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_device_subtree(machine, &description.devices) {'''
    new_seq = '''                if let Err(err) = self.preflight_fact_writers(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.preflight_claim_contracts(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_fact_writers(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_device_subtree(machine, &description.devices) {'''
    text = replace_once(text, old_seq, new_seq, "registration writer order")
    old_new = '''                let (_local, nodes, edges) = crate::discover::build_graph(&description, base);
                let claim_bindings = crate::discover::claim_bindings(&nodes);
                self.commit(Command::ApplyResourceFacts {
                    nodes,
                    edges,
                    claim_bindings,
                })?;'''
    new_new = '''                let (_local, nodes, edges) = crate::discover::build_graph(&description, base);
                let claim_bindings = crate::discover::claim_bindings(&nodes);
                let batches = crate::discover::provider_fact_batches(nodes, edges);
                self.commit(Command::ApplyProviderFacts {
                    batches,
                    claim_bindings,
                })?;'''
    text = replace_once(text, old_new, new_new, "new registration provider facts")
    marker = "    fn missing_claim_contracts(\n"
    helpers = '''    fn missing_fact_writer_assignments(
        &self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<Vec<FactWriterAssignment>, Error> {
        let expected = crate::discover::current_fact_writer_assignments(
            &self.cluster.graph,
            machine,
            devices,
        );
        let mut missing = Vec::new();
        for assignment in expected {
            let mut nodes = Vec::new();
            let mut edges = Vec::new();
            for node in assignment.nodes {
                match self.cluster.graph.node_fact_writer(node) {
                    None => nodes.push(node),
                    Some(writer) if writer == assignment.writer => {}
                    Some(writer) => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "resource node {node} already belongs to discovery writer {writer}; returning agent expects {}",
                                assignment.writer
                            ),
                        });
                    }
                }
            }
            for edge in assignment.edges {
                match self.cluster.graph.edge_fact_writer(edge) {
                    None => edges.push(edge),
                    Some(writer) if writer == assignment.writer => {}
                    Some(writer) => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "resource edge {} -> {} {:?} already belongs to discovery writer {writer}; returning agent expects {}",
                                edge.from, edge.to, edge.kind, assignment.writer
                            ),
                        });
                    }
                }
            }
            if !nodes.is_empty() || !edges.is_empty() {
                missing.push(FactWriterAssignment {
                    writer: assignment.writer,
                    nodes,
                    edges,
                });
            }
        }
        Ok(missing)
    }

    fn preflight_fact_writers(
        &self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        self.missing_fact_writer_assignments(machine, devices)
            .map(|_| ())
    }

    fn reconcile_fact_writers(
        &mut self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        let assignments = self.missing_fact_writer_assignments(machine, devices)?;
        if !assignments.is_empty() {
            self.commit(Command::AdoptFactWriters { assignments })?;
        }
        Ok(())
    }

'''
    text = replace_once(text, marker, helpers + marker, "service writer helpers")
    old_claim = '''            self.commit(Command::ApplyResourceFacts {
                nodes: Vec::new(),
                edges: Vec::new(),
                claim_bindings: missing,
            })?;'''
    new_claim = '''            self.commit(Command::ApplyProviderFacts {
                batches: Vec::new(),
                claim_bindings: missing,
            })?;'''
    text = replace_once(text, old_claim, new_claim, "claim-only provider command")
    old_device = '''            self.commit(Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings,
            })?;'''
    new_device = '''            self.commit(Command::ApplyProviderFacts {
                batches: vec![ProviderFactBatch {
                    writer: crate::discover::DEVICE_FACT_WRITER,
                    nodes,
                    edges,
                }],
                claim_bindings,
            })?;'''
    text = replace_once(text, old_device, new_device, "device provider command")
    return text


def kernel_test(_text):
    return _text


patch("crates/kernel/src/ids.rs", ids)
patch("crates/kernel/src/types.rs", types)
patch("crates/kernel/src/command.rs", command)
patch("crates/kernel/src/graph.rs", graph)
patch("crates/kernel/src/cluster.rs", cluster)
patch("crates/kernel/src/lib.rs", lib)
patch("crates/node/src/discover.rs", discover)
patch("crates/node/src/service.rs", service)

Path("crates/kernel/tests/fact_writers.rs").write_text(r'''use archon_kernel::{
    CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Edge, EdgeKind, FactEdge,
    FactWriterAssignment, FactWriterId, Node, NodeId, ProviderFactBatch, ProviderId, Quantity,
    ResourceClass, BindingScope, qty,
};

fn node(id: u64, kind: ResourceClass, capacity: Quantity) -> Node {
    Node { id: NodeId::from_u64(id), kind, attrs: Default::default(), capacity }
}

fn edge(from: u64, to: u64) -> Edge {
    Edge {
        from: NodeId::from_u64(from),
        to: NodeId::from_u64(to),
        kind: EdgeKind::Contains,
        attrs: Default::default(),
    }
}

#[test]
fn provider_fragments_compose_without_conflating_binding_provider() {
    let host = FactWriterId::from_u64(10);
    let device = FactWriterId::from_u64(20);
    let gpu = NodeId::from_u64(2);
    let mut cluster = Cluster::new();
    cluster.apply(Command::ApplyProviderFacts {
        batches: vec![
            ProviderFactBatch {
                writer: host,
                nodes: vec![node(1, ResourceClass::Machine, Quantity::new())],
                edges: vec![],
            },
            ProviderFactBatch {
                writer: device,
                nodes: vec![node(2, ResourceClass::Gpu, qty(CapacityDimension::Count, 1))],
                edges: vec![edge(1, 2)],
            },
        ],
        claim_bindings: vec![ClaimBindingUpdate {
            node: gpu,
            dimension: CapacityDimension::Count,
            binding: Some(ClaimBinding {
                provider: ProviderId::from_u64(99),
                scope: BindingScope::Exclusive,
            }),
        }],
    }).unwrap();

    assert_eq!(cluster.graph.node_fact_writer(NodeId::from_u64(1)), Some(host));
    assert_eq!(cluster.graph.node_fact_writer(gpu), Some(device));
    assert_eq!(cluster.graph.edge_fact_writer(FactEdge::new(NodeId::from_u64(1), gpu, EdgeKind::Contains)), Some(device));
    assert_eq!(cluster.graph.claim_binding(gpu, CapacityDimension::Count).unwrap().provider, ProviderId::from_u64(99));
}

#[test]
fn another_writer_cannot_overwrite_an_owned_node() {
    let first = FactWriterId::from_u64(1);
    let other = FactWriterId::from_u64(2);
    let mut cluster = Cluster::new();
    cluster.apply(Command::ApplyProviderFacts {
        batches: vec![ProviderFactBatch {
            writer: first,
            nodes: vec![node(1, ResourceClass::Machine, Quantity::new())],
            edges: vec![],
        }],
        claim_bindings: vec![],
    }).unwrap();
    let revision = cluster.graph.revision;
    let mut changed = node(1, ResourceClass::Machine, Quantity::new());
    changed.attrs.insert("writer".into(), "other".into());
    let err = cluster.apply(Command::ApplyProviderFacts {
        batches: vec![ProviderFactBatch { writer: other, nodes: vec![changed], edges: vec![] }],
        claim_bindings: vec![],
    }).unwrap_err();
    assert!(err.to_string().contains("cannot be overwritten"));
    assert_eq!(cluster.graph.revision, revision);
    assert!(cluster.graph.node(NodeId::from_u64(1)).unwrap().attrs.is_empty());
}

#[test]
fn owner_can_refresh_its_complete_node_facts() {
    let writer = FactWriterId::from_u64(7);
    let mut cluster = Cluster::new();
    cluster.apply(Command::ApplyProviderFacts {
        batches: vec![ProviderFactBatch {
            writer,
            nodes: vec![node(1, ResourceClass::Gpu, qty(CapacityDimension::Count, 1))],
            edges: vec![],
        }],
        claim_bindings: vec![],
    }).unwrap();
    let revision = cluster.graph.revision;
    let mut changed = node(1, ResourceClass::Gpu, qty(CapacityDimension::Count, 1));
    changed.attrs.insert("path".into(), "/dev/new".into());
    cluster.apply(Command::ApplyProviderFacts {
        batches: vec![ProviderFactBatch { writer, nodes: vec![changed], edges: vec![] }],
        claim_bindings: vec![],
    }).unwrap();
    assert_eq!(cluster.graph.revision, revision + 1);
    assert_eq!(cluster.graph.node(NodeId::from_u64(1)).unwrap().attrs.get("path").map(String::as_str), Some("/dev/new"));
}

#[test]
fn provider_can_attach_owned_edge_to_another_writers_node() {
    let host = FactWriterId::from_u64(1);
    let device = FactWriterId::from_u64(2);
    let mut cluster = Cluster::new();
    cluster.apply(Command::ApplyProviderFacts {
        batches: vec![ProviderFactBatch {
            writer: host,
            nodes: vec![node(1, ResourceClass::Machine, Quantity::new())],
            edges: vec![],
        }],
        claim_bindings: vec![],
    }).unwrap();
    cluster.apply(Command::ApplyProviderFacts {
        batches: vec![ProviderFactBatch {
            writer: device,
            nodes: vec![node(2, ResourceClass::Gpu, qty(CapacityDimension::Count, 1))],
            edges: vec![edge(1, 2)],
        }],
        claim_bindings: vec![],
    }).unwrap();
    assert_eq!(cluster.graph.node_fact_writer(NodeId::from_u64(1)), Some(host));
    assert_eq!(cluster.graph.edge_fact_writer(FactEdge::new(NodeId::from_u64(1), NodeId::from_u64(2), EdgeKind::Contains)), Some(device));
}

#[test]
fn legacy_facts_can_be_adopted_without_revising_or_mutating_them() {
    let mut cluster = Cluster::new();
    cluster.apply(Command::ApplyGraph {
        nodes: vec![
            node(1, ResourceClass::Machine, Quantity::new()),
            node(2, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
        ],
        edges: vec![edge(1, 2)],
    }).unwrap();
    let revision = cluster.graph.revision;
    let writer = FactWriterId::from_u64(5);
    cluster.apply(Command::AdoptFactWriters {
        assignments: vec![FactWriterAssignment {
            writer,
            nodes: vec![NodeId::from_u64(2)],
            edges: vec![FactEdge::new(NodeId::from_u64(1), NodeId::from_u64(2), EdgeKind::Contains)],
        }],
    }).unwrap();
    assert_eq!(cluster.graph.revision, revision);
    assert_eq!(cluster.graph.node_fact_writer(NodeId::from_u64(2)), Some(writer));
    let digest = cluster.digest();
    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(digest, replayed.digest());
}

#[test]
fn unowned_legacy_mutation_is_disabled_after_adoption() {
    let mut cluster = Cluster::new();
    cluster.apply(Command::ApplyGraph {
        nodes: vec![node(1, ResourceClass::Machine, Quantity::new())],
        edges: vec![],
    }).unwrap();
    cluster.apply(Command::AdoptFactWriters {
        assignments: vec![FactWriterAssignment {
            writer: FactWriterId::from_u64(1),
            nodes: vec![NodeId::from_u64(1)],
            edges: vec![],
        }],
    }).unwrap();
    let err = cluster.apply(Command::ApplyGraph {
        nodes: vec![node(2, ResourceClass::Machine, Quantity::new())],
        edges: vec![],
    }).unwrap_err();
    assert!(err.to_string().contains("legacy unowned Graph updates are disabled"));
}
''')
