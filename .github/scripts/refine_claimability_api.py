from pathlib import Path

# ResourceClass no longer answers whether a resource is enforceable. That is
# now an explicit per-node/per-dimension provider fact.
path = Path("crates/kernel/src/types.rs")
text = path.read_text()
start = text.index("    pub const fn is_enforced(self) -> bool {")
end = text.index("\n    }", start) + len("\n    }")
text = text[:start] + text[end:]
path.write_text(text)

# The extensibility proof must explicitly name a provider contract for the
# custom dimension; custom identifiers are not claimable by construction.
path = Path("crates/kernel/tests/extensible.rs")
text = path.read_text()
text = text.replace(
    '''use archon_kernel::{
    CapacityDimension, Cluster, Command, Edge, EdgeKind, Graph, IdentifierError, Need, NodeId,
    Request, RequestClass, ResourceClass, TopologyRelation, qty,
};''',
    '''use archon_kernel::{
    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Edge,
    EdgeKind, Graph, IdentifierError, Need, NodeId, ProviderId, Request, RequestClass,
    ResourceClass, TopologyRelation, qty,
};''',
    1,
)
text = text.replace('    assert!(class.is_enforced());\n', '', 1)
old = '''        .apply(Command::ApplyGraph {
            nodes: vec![
                archon_kernel::Node {
                    id: machine,
                    kind: ResourceClass::Machine,
                    attrs: Default::default(),
                    capacity: Default::default(),
                },
                archon_kernel::Node {
                    id: device,
                    kind: class,
                    attrs: Default::default(),
                    capacity: qty(dimension, 16),
                },
            ],
            edges: vec![archon_kernel::Edge {
                from: machine,
                to: device,
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
'''
new = '''        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                archon_kernel::Node {
                    id: machine,
                    kind: ResourceClass::Machine,
                    attrs: Default::default(),
                    capacity: Default::default(),
                },
                archon_kernel::Node {
                    id: device,
                    kind: class,
                    attrs: Default::default(),
                    capacity: qty(dimension, 16),
                },
            ],
            edges: vec![archon_kernel::Edge {
                from: machine,
                to: device,
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: vec![ClaimBindingUpdate {
                node: device,
                dimension,
                binding: Some(ClaimBinding {
                    provider: ProviderId::from_u64(42),
                    scope: BindingScope::Exclusive,
                }),
            }],
        })
'''
if old not in text:
    raise SystemExit("extensible custom ApplyGraph block not found")
text = text.replace(old, new, 1)
path.write_text(text)
