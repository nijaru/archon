from pathlib import Path

path = Path("crates/sim/src/world.rs")
text = path.read_text()

text = text.replace(
    '''use archon_kernel::{
    Allocation, BindingId, Cluster, Command, Digest, Effect, Endpoint, EndpointOp, Error, LeaseId,
    NodeId, OwnerId, ProviderId, Queued, Request, RequestId,
};''',
    '''use archon_kernel::{
    Allocation, BindingId, BindingScope, ClaimBinding, ClaimBindingUpdate, Cluster, Command,
    Digest, Effect, Endpoint, EndpointOp, Error, LeaseId, NodeId, OwnerId, ProviderId, Queued,
    Request, RequestId,
};''',
    1,
)

old = '''    pub fn apply_graph(
        &mut self,
        nodes: Vec<archon_kernel::Node>,
        edges: Vec<archon_kernel::Edge>,
    ) -> Result<(), Error> {
        self.apply(Command::ApplyGraph { nodes, edges })?;
        Ok(())
    }
'''
new = '''    /// Simulation convenience for the proof-era ENFORCE provider: every
    /// positive capacity dimension in this helper is explicitly published as
    /// an exclusive claim contract in the command trace. Tests that need
    /// placement-only facts or another provider can call `apply_resource_facts`.
    pub fn apply_graph(
        &mut self,
        nodes: Vec<archon_kernel::Node>,
        edges: Vec<archon_kernel::Edge>,
    ) -> Result<(), Error> {
        let claim_bindings = nodes
            .iter()
            .flat_map(|node| {
                node.capacity.iter().filter_map(move |(dimension, amount)| {
                    (*amount > 0).then_some(ClaimBindingUpdate {
                        node: node.id,
                        dimension: *dimension,
                        binding: Some(ClaimBinding {
                            provider: ProviderId::ENFORCE,
                            scope: BindingScope::Exclusive,
                        }),
                    })
                })
            })
            .collect();
        self.apply_resource_facts(nodes, edges, claim_bindings)
    }

    pub fn apply_resource_facts(
        &mut self,
        nodes: Vec<archon_kernel::Node>,
        edges: Vec<archon_kernel::Edge>,
        claim_bindings: Vec<ClaimBindingUpdate>,
    ) -> Result<(), Error> {
        self.apply(Command::ApplyResourceFacts {
            nodes,
            edges,
            claim_bindings,
        })?;
        Ok(())
    }
'''
if old not in text:
    raise SystemExit("World::apply_graph block not found")
text = text.replace(old, new, 1)

old = '''        for node in enforced_under(&self.cluster, machine) {
            self.endpoints
                .entry((ProviderId::ENFORCE, node))
                .and_modify(|endpoint| endpoint.handshake(session))
                .or_insert_with(|| Endpoint::new(ProviderId::ENFORCE, node, session));
        }
'''
new = '''        for (provider, node) in enforced_under(&self.cluster, machine) {
            self.endpoints
                .entry((provider, node))
                .and_modify(|endpoint| endpoint.handshake(session))
                .or_insert_with(|| Endpoint::new(provider, node, session));
        }
'''
if old not in text:
    raise SystemExit("register_agent endpoint block not found")
text = text.replace(old, new, 1)

old = '''        let mut next = start;
        for claim in claims {
            let kind = self
                .cluster
                .graph
                .node(claim.node)
                .ok_or(Error::UnknownNode(claim.node))?
                .kind;
            if !kind.is_enforced() {
                continue;
            }
            let binding = BindingId::from_u64(next);
            next += 1;
            self.apply(Command::OpenBinding {
                binding,
                lease,
                node: claim.node,
                provider: ProviderId::ENFORCE,
                scope: archon_kernel::BindingScope::Exclusive,
            })?;
            bindings.push(binding);
        }
'''
new = '''        for (next, claim) in (start..).zip(claims) {
            let required = self
                .cluster
                .graph
                .claim_binding_for_quantity(claim.node, &claim.quantity)?;
            let binding = BindingId::from_u64(next);
            self.apply(Command::OpenBinding {
                binding,
                lease,
                node: claim.node,
                provider: required.provider,
                scope: required.scope,
            })?;
            bindings.push(binding);
        }
'''
if old not in text:
    raise SystemExit("bind_enforced block not found")
text = text.replace(old, new, 1)

old = '''fn enforced_under(cluster: &Cluster, machine: NodeId) -> Vec<NodeId> {
    let mut nodes = cluster.graph.descendants(machine);
    nodes.push(machine);
    nodes
        .into_iter()
        .filter(|id| {
            cluster
                .graph
                .node(*id)
                .is_some_and(|node| node.kind.is_enforced())
        })
        .collect()
}
'''
new = '''fn enforced_under(cluster: &Cluster, machine: NodeId) -> Vec<(ProviderId, NodeId)> {
    let mut nodes = cluster.graph.descendants(machine);
    nodes.push(machine);
    let nodes: BTreeSet<NodeId> = nodes.into_iter().collect();
    cluster
        .graph
        .claim_bindings()
        .iter()
        .filter(|(node, _)| nodes.contains(node))
        .flat_map(|(node, dimensions)| {
            dimensions
                .values()
                .map(move |binding| (binding.provider, *node))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
'''
if old not in text:
    raise SystemExit("enforced_under block not found")
text = text.replace(old, new, 1)

path.write_text(text)
