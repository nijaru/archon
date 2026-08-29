from pathlib import Path

path = Path("crates/kernel/tests/lifecycle.rs")
text = path.read_text()

text = text.replace(
    '''use archon_kernel::{
    CapacityDimension, Cluster, Command, Edge, EdgeKind, Error, LeaseId, Need, Node, NodeId,
    NodeState, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};''',
    '''use archon_kernel::{
    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Edge,
    EdgeKind, Error, LeaseId, Need, Node, NodeId, NodeState, OwnerId, ProviderId, Request,
    RequestClass, RequestId, ResourceClass, qty,
};''',
    1,
)

old = '''    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                Node {
                    id: machine,
                    kind: ResourceClass::Machine,
                    attrs: Default::default(),
                    capacity: Default::default(),
                },
                Node {
                    id: cpu,
                    kind: ResourceClass::Cpu,
                    attrs: Default::default(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
            ],
            edges: vec![Edge {
                from: machine,
                to: cpu,
                kind: EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
'''
new = '''    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                Node {
                    id: machine,
                    kind: ResourceClass::Machine,
                    attrs: Default::default(),
                    capacity: Default::default(),
                },
                Node {
                    id: cpu,
                    kind: ResourceClass::Cpu,
                    attrs: Default::default(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
            ],
            edges: vec![Edge {
                from: machine,
                to: cpu,
                kind: EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: vec![ClaimBindingUpdate {
                node: cpu,
                dimension: CapacityDimension::Count,
                binding: Some(ClaimBinding {
                    provider: ProviderId::ENFORCE,
                    scope: BindingScope::Exclusive,
                }),
            }],
        })
'''
if old not in text:
    raise SystemExit("lifecycle graph fixture not found")
text = text.replace(old, new, 1)
path.write_text(text)
