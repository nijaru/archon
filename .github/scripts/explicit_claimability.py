from pathlib import Path

# --- kernel types ---------------------------------------------------------
path = Path("crates/kernel/src/types.rs")
text = path.read_text()
anchor = '''pub enum BindingScope {
    #[default]
    Exclusive,
    IndependentShare,
}
'''
insert = anchor + '''
/// The provider contract required to turn one capacity dimension into Lease
/// authority. Absence means the dimension is placement-only and cannot be
/// claimed. The scope is part of the provider contract rather than inferred
/// from the resource class.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClaimBinding {
    pub provider: ProviderId,
    pub scope: BindingScope,
}

/// One atomic resource-fact update to a node/dimension claim contract. `None`
/// removes claimability without deleting the underlying placement fact.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClaimBindingUpdate {
    pub node: NodeId,
    pub dimension: CapacityDimension,
    pub binding: Option<ClaimBinding>,
}
'''
if anchor not in text:
    raise SystemExit("BindingScope anchor not found")
text = text.replace(anchor, insert, 1)
path.write_text(text)

# --- graph ---------------------------------------------------------------
path = Path("crates/kernel/src/graph.rs")
text = path.read_text()
text = text.replace(
    'use crate::types::{Edge, EdgeKind, Node, ResourceClass, TopologyRelation};',
    'use crate::types::{\n    CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind, Node, Quantity,\n    ResourceClass, TopologyRelation,\n};',
    1,
)
text = text.replace(
    '    by_class: BTreeMap<ResourceClass, Vec<NodeId>>,\n}',
    '''    by_class: BTreeMap<ResourceClass, Vec<NodeId>>,
    /// Revisioned provider contract for claimable capacity. Capacity without
    /// an entry here remains a placement fact and cannot become Lease
    /// authority merely because it has a positive quantity.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "BTreeMap::is_empty")
    )]
    claim_bindings: BTreeMap<NodeId, BTreeMap<CapacityDimension, ClaimBinding>>,
}''',
    1,
)
# Plain ApplyGraph may update facts but must preserve/validate existing claim contracts.
old = '''        validate_contains(&staged_edges)?;
        self.nodes = staged_nodes;
        self.edges = staged_edges;
        self.rebuild();
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }
'''
new = '''        validate_contains(&staged_edges)?;
        validate_claim_bindings(&staged_nodes, &self.claim_bindings)?;
        self.nodes = staged_nodes;
        self.edges = staged_edges;
        self.rebuild();
        self.revision = self.revision.saturating_add(1);
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

    pub fn claim_bindings(
        &self,
    ) -> &BTreeMap<NodeId, BTreeMap<CapacityDimension, ClaimBinding>> {
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
'''
if old not in text:
    raise SystemExit("Graph apply tail not found")
text = text.replace(old, new, 1)
# Add validators before validate_contains.
anchor = '\nfn validate_contains(edges: &[Edge]) -> Result<(), Error> {'
helpers = '''
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
'''
if anchor not in text:
    raise SystemExit("validate_contains anchor not found")
text = text.replace(anchor, '\n' + helpers + anchor, 1)
path.write_text(text)

# --- command -------------------------------------------------------------
path = Path("crates/kernel/src/command.rs")
text = path.read_text()
text = text.replace(
    'use crate::types::{Allocation, BindingScope, Edge, Node, NodeState};',
    'use crate::types::{Allocation, BindingScope, ClaimBindingUpdate, Edge, Node, NodeState};',
    1,
)
anchor = '''    ApplyGraph {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
    },
'''
insert = anchor + '''    /// Apply provider-authored resource facts and their claim contracts in one
    /// Graph revision. Capacity omitted from `claim_bindings` is placement-only.
    ApplyResourceFacts {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        claim_bindings: Vec<ClaimBindingUpdate>,
    },
'''
if anchor not in text:
    raise SystemExit("ApplyGraph command anchor not found")
text = text.replace(anchor, insert, 1)
path.write_text(text)

# --- cluster -------------------------------------------------------------
path = Path("crates/kernel/src/cluster.rs")
text = path.read_text()
text = text.replace(
    'use crate::types::{Binding, BindingScope, BindingState, Lease, LeaseState, NodeState, Quantity};',
    'use crate::types::{\n    Binding, BindingScope, BindingState, ClaimBinding, Lease, LeaseState, NodeState, Quantity,\n};',
    1,
)
text = text.replace(
    '    pub graph_edges: Vec<(NodeId, NodeId, crate::types::EdgeKind)>,\n',
    '''    pub graph_edges: Vec<(NodeId, NodeId, crate::types::EdgeKind)>,
    pub graph_claim_bindings:
        BTreeMap<NodeId, BTreeMap<crate::types::CapacityDimension, ClaimBinding>>,
''',
    1,
)
text = text.replace(
    '''            graph_edges: self
                .graph
                .edges()
                .iter()
                .map(|edge| (edge.from, edge.to, edge.kind))
                .collect(),
''',
    '''            graph_edges: self
                .graph
                .edges()
                .iter()
                .map(|edge| (edge.from, edge.to, edge.kind))
                .collect(),
            graph_claim_bindings: self.graph.claim_bindings().clone(),
''',
    1,
)
text = text.replace(
    '            Command::ApplyGraph { nodes, edges } => self.apply_graph(nodes.clone(), edges.clone()),\n',
    '''            Command::ApplyGraph { nodes, edges } => self.apply_graph(nodes.clone(), edges.clone()),
            Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings,
            } => self.apply_resource_facts(
                nodes.clone(),
                edges.clone(),
                claim_bindings.clone(),
            ),
''',
    1,
)
# Add atomic cluster method immediately after apply_graph.
needle = '''        self.graph = staged;
        Ok(Vec::new())
    }

    fn reserve_lease'''
replacement = '''        self.graph = staged;
        Ok(Vec::new())
    }

    fn apply_resource_facts(
        &mut self,
        nodes: Vec<crate::types::Node>,
        edges: Vec<crate::types::Edge>,
        claim_bindings: Vec<crate::types::ClaimBindingUpdate>,
    ) -> Result<Vec<Effect>, Error> {
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        let mut staged = self.graph.clone();
        staged.apply_resource_facts(nodes, edges, claim_bindings)?;
        let occupancy = self.occupancy();
        if let Some(node) = occupancy.exceeds_capacity(&staged)? {
            return Err(Error::CapacityBelowOccupancy { node });
        }
        self.graph = staged;
        Ok(Vec::new())
    }

    fn reserve_lease'''
if needle not in text:
    raise SystemExit("cluster apply_graph tail not found")
text = text.replace(needle, replacement, 1)
# Enforce provider/scope at OpenBinding.
needle = '''        if !lease
            .allocation
            .claims
            .iter()
            .any(|claim| claim.node == node)
        {
            return Err(Error::Invalid("binding node is not in lease claims"));
        }
        if self.node_quarantined(node) {
'''
replacement = '''        let Some(claim) = lease
            .allocation
            .claims
            .iter()
            .find(|claim| claim.node == node)
        else {
            return Err(Error::Invalid("binding node is not in lease claims"));
        };
        let required = self
            .graph
            .claim_binding_for_quantity(node, &claim.quantity)?;
        if required.provider != provider || required.scope != scope {
            return Err(Error::Refused {
                explanation: format!(
                    "binding for {node} must use provider {} with {:?} scope",
                    required.provider, required.scope
                ),
            });
        }
        if self.node_quarantined(node) {
'''
if needle not in text:
    raise SystemExit("open_binding claim check not found")
text = text.replace(needle, replacement, 1)
path.write_text(text)

# --- occupancy -----------------------------------------------------------
path = Path("crates/kernel/src/occupancy.rs")
text = path.read_text()
old = '''    if node.kind == crate::types::ResourceClass::DataObject {
        return Err(Error::UnclaimableNode { node: claim.node });
    }
    let quantity = if claim.quantity.is_empty() {
        node.capacity.clone()
    } else {
        claim.quantity.clone()
    };
    Ok(Claim {
'''
new = '''    let quantity = if claim.quantity.is_empty() {
        node.capacity.clone()
    } else {
        claim.quantity.clone()
    };
    graph.claim_binding_for_quantity(claim.node, &quantity)?;
    Ok(Claim {
'''
if old not in text:
    raise SystemExit("resolve_claim block not found")
text = text.replace(old, new, 1)
path.write_text(text)

# --- selector ------------------------------------------------------------
path = Path("crates/kernel/src/select.rs")
text = path.read_text()
old = '''    let mut out = Vec::new();
    for id in graph.nodes_of_class(need.kind) {
        let node = graph.node(*id).ok_or(Error::UnknownNode(*id))?;
'''
new = '''    let required = need_unit(need);
    let mut out = Vec::new();
    for id in graph.nodes_of_class(need.kind) {
        let node = graph.node(*id).ok_or(Error::UnknownNode(*id))?;
        if graph
            .claim_binding_for_quantity(*id, &required)
            .is_err()
        {
            continue;
        }
'''
if old not in text:
    raise SystemExit("selector candidates block not found")
text = text.replace(old, new, 1)
text = text.replace(
    '''            &Claim {
                node: *id,
                quantity: need_unit(need),
            },
''',
    '''            &Claim {
                node: *id,
                quantity: required.clone(),
            },
''',
    1,
)
path.write_text(text)

# --- exports -------------------------------------------------------------
path = Path("crates/kernel/src/lib.rs")
text = path.read_text()
text = text.replace(
    '    Allocation, Attrs, Binding, BindingScope, BindingState, CapacityDimension, Claim, Edge,\n',
    '    Allocation, Attrs, Binding, BindingScope, BindingState, CapacityDimension, Claim,\n    ClaimBinding, ClaimBindingUpdate, Edge,\n',
    1,
)
path.write_text(text)

# --- provider normalization ---------------------------------------------
path = Path("crates/node/src/discover.rs")
text = path.read_text()
text = text.replace(
    '''use archon_kernel::{
    Attrs, CapacityDimension, Edge, EdgeKind, Node, Quantity, ResourceClass, qty, quantity_get,
};''',
    '''use archon_kernel::{
    Attrs, BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind, Node,
    ProviderId, Quantity, ResourceClass, qty, quantity_get,
};''',
    1,
)
anchor = '''/// Discover this machine and build its graph.
pub fn discover() -> (LocalMachine, Vec<Node>, Vec<Edge>) {
'''
helper = '''/// Current proof-stage providers explicitly declare which normalized
/// capacity they can bind/fence. Unknown/custom capacity remains
/// placement-only until a provider supplies its own claim contract.
pub fn claim_bindings(nodes: &[Node]) -> Vec<ClaimBindingUpdate> {
    nodes
        .iter()
        .filter_map(|node| {
            let (dimension, scope) = match node.kind {
                ResourceClass::Cpu => (CapacityDimension::Count, BindingScope::Exclusive),
                ResourceClass::Memory => (CapacityDimension::Bytes, BindingScope::IndependentShare),
                ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme => {
                    (CapacityDimension::Count, BindingScope::Exclusive)
                }
                _ => return None,
            };
            (node.capacity.get(&dimension).copied().unwrap_or(0) > 0).then_some(
                ClaimBindingUpdate {
                    node: node.id,
                    dimension,
                    binding: Some(ClaimBinding {
                        provider: ProviderId::ENFORCE,
                        scope,
                    }),
                },
            )
        })
        .collect()
}

'''
if anchor not in text:
    raise SystemExit("discover function anchor not found")
text = text.replace(anchor, helper + anchor, 1)
path.write_text(text)

# --- node service --------------------------------------------------------
path = Path("crates/node/src/service.rs")
text = path.read_text()
text = text.replace(
    '    BindingId, BindingScope, CapacityDimension, Cluster, Command, Effect, Error, LeaseId, NodeId,\n',
    '    Allocation, BindingId, CapacityDimension, Cluster, Command, Effect, Error, LeaseId, NodeId,\n',
    1,
)
# Initial registration: claim contracts travel with resource facts.
old = '''                let (_local, nodes, edges) = crate::discover::build_graph(&description, base);
                self.commit(Command::ApplyGraph { nodes, edges })?;
'''
new = '''                let (_local, nodes, edges) = crate::discover::build_graph(&description, base);
                let claim_bindings = crate::discover::claim_bindings(&nodes);
                self.commit(Command::ApplyResourceFacts {
                    nodes,
                    edges,
                    claim_bindings,
                })?;
'''
if old not in text:
    raise SystemExit("initial ApplyGraph not found")
text = text.replace(old, new, 1)
# Device reconciliation: any newly materialized device capacity gets the same
# provider-authored claim contract in this graph revision.
old = '''        if !nodes.is_empty() || !edges.is_empty() {
            self.commit(Command::ApplyGraph { nodes, edges })?;
        }
'''
new = '''        if !nodes.is_empty() || !edges.is_empty() {
            let claim_bindings = crate::discover::claim_bindings(&nodes);
            self.commit(Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings,
            })?;
        }
'''
if old not in text:
    raise SystemExit("device ApplyGraph not found")
text = text.replace(old, new, 1)
# Preflight before ANY lease/controller-side mutation.
needle = '''        let request_id = admission.request.id;
        let lease = LeaseId::from_u64(self.next_lease);
'''
replacement = '''        self.validate_allocation_bindings(&admission.allocation)?;
        let request_id = admission.request.id;
        let lease = LeaseId::from_u64(self.next_lease);
'''
if needle not in text:
    raise SystemExit("admit preflight anchor not found")
text = text.replace(needle, replacement, 1)
# Replace class-inferred binding creation with graph provider contracts.
start = text.index('    fn open_enforced_bindings(&mut self, lease: LeaseId) -> Result<(), Error> {')
end = text.index('\n    fn commit(&mut self, command: Command)', start)
old_block = text[start:end]
new_block = '''    fn validate_allocation_bindings(&self, allocation: &Allocation) -> Result<(), Error> {
        for claim in &allocation.claims {
            let node = self
                .cluster
                .graph
                .node(claim.node)
                .ok_or(Error::UnknownNode(claim.node))?;
            let binding = self
                .cluster
                .graph
                .claim_binding_for_quantity(claim.node, &claim.quantity)?;
            if binding.provider != ProviderId::ENFORCE
                || !matches!(
                    node.kind,
                    ResourceClass::Cpu
                        | ResourceClass::Memory
                        | ResourceClass::Gpu
                        | ResourceClass::Nic
                        | ResourceClass::Nvme
                )
            {
                return Err(Error::Refused {
                    explanation: format!(
                        "no registered resource provider can enforce claims on {} ({})",
                        claim.node, node.kind
                    ),
                });
            }
        }
        Ok(())
    }

    fn open_enforced_bindings(&mut self, lease: LeaseId) -> Result<(), Error> {
        let claims = self
            .cluster
            .leases
            .get(&lease)
            .ok_or(Error::UnknownLease(lease))?
            .allocation
            .claims
            .clone();
        for claim in claims {
            let binding_spec = self
                .cluster
                .graph
                .claim_binding_for_quantity(claim.node, &claim.quantity)?;
            let binding = BindingId::from_u64(self.next_binding);
            self.next_binding += 1;
            self.commit(Command::OpenBinding {
                binding,
                lease,
                node: claim.node,
                provider: binding_spec.provider,
                scope: binding_spec.scope,
            })?;
        }
        Ok(())
    }
'''
text = text[:start] + new_block + text[end:]
path.write_text(text)

# --- kernel claimability tests ------------------------------------------
Path("crates/kernel/tests/claimability.rs").write_text(r'''use archon_kernel::{
    Allocation, BindingId, BindingScope, CapacityDimension, Claim, ClaimBinding,
    ClaimBindingUpdate, Command, Edge, EdgeKind, LeaseId, Need, Node, NodeId, OwnerId, ProviderId,
    Quantity, Request, RequestClass, RequestId, ResourceClass, qty,
};

fn request(kind: ResourceClass) -> Request {
    Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind,
            quantity: qty(CapacityDimension::Count, 1),
            filters: Vec::new(),
        }],
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        command: Vec::new(),
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime: 60,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
    }
}

fn custom_graph(binding: Option<ClaimBinding>) -> archon_kernel::Cluster {
    let mut cluster = archon_kernel::Cluster::new();
    let machine = NodeId::from_u64(1);
    let resource = NodeId::from_u64(2);
    let kind = ResourceClass::new("example.com/special").unwrap();
    let updates = binding
        .map(|binding| vec![ClaimBindingUpdate {
            node: resource,
            dimension: CapacityDimension::Count,
            binding: Some(binding),
        }])
        .unwrap_or_default();
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                Node {
                    id: machine,
                    kind: ResourceClass::Machine,
                    attrs: Default::default(),
                    capacity: Quantity::new(),
                },
                Node {
                    id: resource,
                    kind,
                    attrs: Default::default(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
            ],
            edges: vec![Edge {
                from: machine,
                to: resource,
                kind: EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: updates,
        })
        .unwrap();
    cluster
}

#[test]
fn capacity_without_a_provider_contract_is_placement_only() {
    let kind = ResourceClass::new("example.com/special").unwrap();
    let cluster = custom_graph(None);
    let error = cluster.allocate(&request(kind)).expect_err("must be unclaimable");
    assert!(error.to_string().contains("no"));
}

#[test]
fn custom_capacity_becomes_claimable_only_with_explicit_provider_binding() {
    let kind = ResourceClass::new("example.com/special").unwrap();
    let provider = ProviderId::from_u64(7);
    let cluster = custom_graph(Some(ClaimBinding {
        provider,
        scope: BindingScope::Exclusive,
    }));
    let allocation = cluster.allocate(&request(kind)).expect("explicitly claimable");
    assert_eq!(allocation.claims.len(), 1);
    assert_eq!(
        cluster
            .graph
            .claim_binding_for_quantity(allocation.claims[0].node, &allocation.claims[0].quantity)
            .unwrap()
            .provider,
        provider
    );
}

#[test]
fn kernel_rejects_binding_provider_or_scope_that_disagrees_with_resource_facts() {
    let provider = ProviderId::from_u64(7);
    let mut cluster = custom_graph(Some(ClaimBinding {
        provider,
        scope: BindingScope::Exclusive,
    }));
    let kind = ResourceClass::new("example.com/special").unwrap();
    let allocation = cluster.allocate(&request(kind)).unwrap();
    cluster
        .apply(Command::SetAgentSession {
            machine: NodeId::from_u64(1),
            session: 1,
        })
        .unwrap();
    cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation,
            parent: None,
            expires_at: 60,
            prepare_deadline: 20,
            priority: 1,
        })
        .unwrap();
    let error = cluster
        .apply(Command::OpenBinding {
            binding: BindingId::from_u64(1),
            lease: LeaseId::from_u64(1),
            node: NodeId::from_u64(2),
            provider: ProviderId::from_u64(8),
            scope: BindingScope::Exclusive,
        })
        .expect_err("wrong provider must fail");
    assert!(error.to_string().contains("must use provider"));
}

#[test]
fn withdrawing_claimability_invalidates_old_allocations_and_fresh_claims() {
    let binding = ClaimBinding {
        provider: ProviderId::from_u64(7),
        scope: BindingScope::Exclusive,
    };
    let kind = ResourceClass::new("example.com/special").unwrap();
    let mut cluster = custom_graph(Some(binding));
    let allocation = cluster.allocate(&request(kind)).unwrap();
    let prior_revision = cluster.graph.revision;
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: Vec::new(),
            edges: Vec::new(),
            claim_bindings: vec![ClaimBindingUpdate {
                node: NodeId::from_u64(2),
                dimension: CapacityDimension::Count,
                binding: None,
            }],
        })
        .unwrap();
    assert!(cluster.graph.revision > prior_revision);
    let stale = cluster
        .apply(Command::ReserveLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation,
            expires_at: 60,
            priority: 1,
        })
        .expect_err("old allocation must be stale");
    assert!(matches!(stale, archon_kernel::Error::StaleGraphRevision { .. }));
    let fresh = Allocation {
        claims: vec![Claim {
            node: NodeId::from_u64(2),
            quantity: qty(CapacityDimension::Count, 1),
        }],
        graph_revision: cluster.graph.revision,
        explanation: String::new(),
    };
    let error = cluster
        .apply(Command::ReserveLease {
            lease: LeaseId::from_u64(2),
            owner: OwnerId::from_u64(1),
            allocation: fresh,
            expires_at: 60,
            priority: 1,
        })
        .expect_err("fresh unclaimable claim must fail");
    assert!(matches!(error, archon_kernel::Error::UnclaimableNode { .. }));
}
''')

# --- NodeService pre-authority test -------------------------------------
Path("crates/node/tests/claimability.rs").write_text(r'''use archon_kernel::{
    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Command, Edge, EdgeKind,
    Need, Node, NodeId, OwnerId, ProviderId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::MachineDescription;
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LocalExecutor, NodeService};

fn request(kind: ResourceClass, id: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind,
            quantity: qty(CapacityDimension::Count, 1),
            filters: Vec::new(),
        }],
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        command: vec!["true".into()],
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime: 60,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
    }
}

fn service() -> NodeService {
    let mut service = NodeService::new();
    service
        .register_agent(
            MachineDescription {
                instance_id: "claimability-agent".into(),
                name: "claimability-agent".into(),
                cpus: 1,
                memory_bytes: 1 << 30,
                host_nodes: Vec::new(),
                devices: Vec::new(),
            },
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .unwrap();
    service
}

fn add_custom_resource(
    service: &mut NodeService,
    binding: Option<ClaimBinding>,
) -> ResourceClass {
    let kind = ResourceClass::new("example.com/special").unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let id = NodeId::from_u64(
        service
            .cluster
            .graph
            .nodes()
            .map(|node| node.id.as_u64())
            .max()
            .unwrap()
            + 1,
    );
    service
        .cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![Node {
                id,
                kind,
                attrs: Default::default(),
                capacity: qty(CapacityDimension::Count, 1),
            }],
            edges: vec![Edge {
                from: machine,
                to: id,
                kind: EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: binding
                .map(|binding| vec![ClaimBindingUpdate {
                    node: id,
                    dimension: CapacityDimension::Count,
                    binding: Some(binding),
                }])
                .unwrap_or_default(),
        })
        .unwrap();
    kind
}

#[test]
fn unclaimable_custom_capacity_never_creates_lease_authority() {
    let mut service = service();
    let kind = add_custom_resource(&mut service, None);
    service.submit(request(kind, 1), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), None);
    assert!(service.cluster.leases.is_empty());
    assert_eq!(service.queue_len(), 1);
}

#[test]
fn unregistered_provider_is_refused_before_lease_or_dequeue() {
    let mut service = service();
    let kind = add_custom_resource(
        &mut service,
        Some(ClaimBinding {
            provider: ProviderId::from_u64(99),
            scope: BindingScope::Exclusive,
        }),
    );
    service.submit(request(kind, 1), OwnerId::from_u64(1));
    let error = service
        .admit_one()
        .expect_err("unknown provider must fail before lease mutation");
    assert!(error.to_string().contains("no registered resource provider"));
    assert!(service.cluster.leases.is_empty());
    assert_eq!(service.queue_len(), 1);
}
''')
