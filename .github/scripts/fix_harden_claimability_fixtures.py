from pathlib import Path

path = Path("crates/kernel/tests/harden.rs")
text = path.read_text()

text = text.replace(
    '''use archon_kernel::{
    BindingId, CapacityDimension, Cluster, Command, Effect, Error, LeaseId, LeaseState, Need,
    NodeId, OwnerId, ProviderId, Quantity, Request, RequestClass, RequestId, ResourceClass, qty,
};''',
    '''use archon_kernel::{
    BindingId, BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command,
    Effect, Error, LeaseId, LeaseState, Need, NodeId, OwnerId, ProviderId, Quantity, Request,
    RequestClass, RequestId, ResourceClass, qty,
};''',
    1,
)

old = '''    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(3, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                archon_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(2),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                archon_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(3),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
            ],
        })
'''
new = '''    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(3, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                archon_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(2),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                archon_kernel::Edge {
                    from: NodeId::from_u64(1),
                    to: NodeId::from_u64(3),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
            ],
            claim_bindings: [2, 3]
                .into_iter()
                .map(|node| ClaimBindingUpdate {
                    node: NodeId::from_u64(node),
                    dimension: CapacityDimension::Count,
                    binding: Some(ClaimBinding {
                        provider: ProviderId::ENFORCE,
                        scope: BindingScope::Exclusive,
                    }),
                })
                .collect(),
        })
'''
if old not in text:
    raise SystemExit("shared hardening graph fixture not found")
text = text.replace(old, new, 1)

old = '''    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Memory, qty(CapacityDimension::Bytes, 100)),
            ],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(1),
                to: NodeId::from_u64(2),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
'''
new = '''    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Memory, qty(CapacityDimension::Bytes, 100)),
            ],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(1),
                to: NodeId::from_u64(2),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: vec![ClaimBindingUpdate {
                node: NodeId::from_u64(2),
                dimension: CapacityDimension::Bytes,
                binding: Some(ClaimBinding {
                    provider: ProviderId::ENFORCE,
                    scope: BindingScope::IndependentShare,
                }),
            }],
        })
'''
if old not in text:
    raise SystemExit("partial memory fixture not found")
text = text.replace(old, new, 1)

old = '''    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(
                    2,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 1024),
                ),
            ],
            edges: vec![archon_kernel::Edge {
                from: machine,
                to: memory,
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
'''
new = '''    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(
                    2,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 1024),
                ),
            ],
            edges: vec![archon_kernel::Edge {
                from: machine,
                to: memory,
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
            claim_bindings: vec![ClaimBindingUpdate {
                node: memory,
                dimension: CapacityDimension::Bytes,
                binding: Some(ClaimBinding {
                    provider: ProviderId::ENFORCE,
                    scope: BindingScope::IndependentShare,
                }),
            }],
        })
'''
if old not in text:
    raise SystemExit("independent share fixture not found")
text = text.replace(old, new, 1)

old = '''    assert!(matches!(err, Error::ResourceBusy { node } if node == memory));
'''
new = '''    assert!(matches!(err, Error::Refused { explanation } if explanation.contains("IndependentShare")));
'''
if old not in text:
    raise SystemExit("exclusive-share mismatch assertion not found")
text = text.replace(old, new, 1)

path.write_text(text)
