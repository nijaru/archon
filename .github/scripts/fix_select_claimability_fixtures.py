from pathlib import Path

path = Path("crates/kernel/tests/select.rs")
text = path.read_text()

text = text.replace(
    '''use archon_kernel::{
    Allocation, CapacityDimension, Claim, Cluster, Command, Edge, EdgeKind, LeaseId, Need, Node,
    NodeId, OwnerId, Quantity, Request, RequestClass, RequestId, ResourceClass, TopologyConstraint,
    TopologyRelation, qty,
};''',
    '''use archon_kernel::{
    Allocation, BindingScope, CapacityDimension, Claim, ClaimBinding, ClaimBindingUpdate, Cluster,
    Command, Edge, EdgeKind, LeaseId, Need, Node, NodeId, OwnerId, ProviderId, Quantity, Request,
    RequestClass, RequestId, ResourceClass, TopologyConstraint, TopologyRelation, qty,
};''',
    1,
)

old = '''        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Numa, Quantity::new()),
                node(3, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(4, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(5, ResourceClass::Memory, qty(CapacityDimension::Bytes, 8)),
                node(6, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                contain(1, 2),
                contain(2, 3),
                contain(2, 4),
                contain(2, 5),
                contain(2, 6),
            ],
        })'''
new = '''        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Numa, Quantity::new()),
                node(3, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(4, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(5, ResourceClass::Memory, qty(CapacityDimension::Bytes, 8)),
                node(6, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                contain(1, 2),
                contain(2, 3),
                contain(2, 4),
                contain(2, 5),
                contain(2, 6),
            ],
            claim_bindings: vec![
                claim_binding(3, CapacityDimension::Count),
                claim_binding(4, CapacityDimension::Count),
                claim_binding(5, CapacityDimension::Bytes),
                claim_binding(6, CapacityDimension::Count),
            ],
        })'''
if old not in text:
    raise SystemExit("primary selector fixture not found")
text = text.replace(old, new, 1)

insert_after = '''fn contain(from: u64, to: u64) -> Edge {
    Edge {
        from: NodeId::from_u64(from),
        to: NodeId::from_u64(to),
        kind: EdgeKind::Contains,
        attrs: Default::default(),
    }
}
'''
helper = insert_after + '''
fn claim_binding(node: u64, dimension: CapacityDimension) -> ClaimBindingUpdate {
    ClaimBindingUpdate {
        node: NodeId::from_u64(node),
        dimension,
        binding: Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        }),
    }
}
'''
if insert_after not in text:
    raise SystemExit("contain helper not found")
text = text.replace(insert_after, helper, 1)

old = '''        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(10, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(
                    11,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 2 << 30),
                ),
                node(2, ResourceClass::Machine, Quantity::new()),
                node(20, ResourceClass::Cpu, qty(CapacityDimension::Count, 2)),
                node(
                    21,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 1 << 30),
                ),
            ],
            edges: vec![
                edge(m1, cpu1),
                edge(m1, mem1),
                edge(m2, cpu2),
                edge(m2, mem2),
            ],
        })'''
new = '''        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(10, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(
                    11,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 2 << 30),
                ),
                node(2, ResourceClass::Machine, Quantity::new()),
                node(20, ResourceClass::Cpu, qty(CapacityDimension::Count, 2)),
                node(
                    21,
                    ResourceClass::Memory,
                    qty(CapacityDimension::Bytes, 1 << 30),
                ),
            ],
            edges: vec![
                edge(m1, cpu1),
                edge(m1, mem1),
                edge(m2, cpu2),
                edge(m2, mem2),
            ],
            claim_bindings: vec![
                claim_binding(10, CapacityDimension::Count),
                claim_binding(11, CapacityDimension::Bytes),
                claim_binding(20, CapacityDimension::Count),
                claim_binding(21, CapacityDimension::Bytes),
            ],
        })'''
if old not in text:
    raise SystemExit("machine-local selector fixture not found")
text = text.replace(old, new, 1)

old = '''        .apply(Command::ApplyGraph {
            nodes: vec![
                node(100, ResourceClass::Machine, Quantity::new()),
                node(101, ResourceClass::Numa, Quantity::new()),
                node(102, ResourceClass::Numa, Quantity::new()),
                node(103, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(104, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
                node(200, ResourceClass::Machine, Quantity::new()),
                node(201, ResourceClass::Numa, Quantity::new()),
                node(202, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(203, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                contain(100, 101),
                contain(100, 102),
                contain(101, 103),
                contain(102, 104),
                contain(200, 201),
                contain(201, 202),
                contain(201, 203),
            ],
        })'''
new = '''        .apply(Command::ApplyResourceFacts {
            nodes: vec![
                node(100, ResourceClass::Machine, Quantity::new()),
                node(101, ResourceClass::Numa, Quantity::new()),
                node(102, ResourceClass::Numa, Quantity::new()),
                node(103, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(104, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
                node(200, ResourceClass::Machine, Quantity::new()),
                node(201, ResourceClass::Numa, Quantity::new()),
                node(202, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(203, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                contain(100, 101),
                contain(100, 102),
                contain(101, 103),
                contain(102, 104),
                contain(200, 201),
                contain(201, 202),
                contain(201, 203),
            ],
            claim_bindings: vec![
                claim_binding(103, CapacityDimension::Count),
                claim_binding(104, CapacityDimension::Count),
                claim_binding(202, CapacityDimension::Count),
                claim_binding(203, CapacityDimension::Count),
            ],
        })'''
if old not in text:
    raise SystemExit("topology explanation selector fixture not found")
text = text.replace(old, new, 1)

path.write_text(text)
