use archon_kernel::{
    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Edge,
    EdgeKind, Graph, IdentifierError, Need, NodeId, ProviderId, Request, RequestClass,
    ResourceClass, TopologyRelation, qty,
};

fn request(kind: ResourceClass, dimension: CapacityDimension, amount: u64) -> Request {
    Request {
        id: archon_kernel::RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind,
            quantity: qty(dimension, amount),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        lifetime: 10,
        priority: 1,
        machine_local: true,
    }
}

#[test]
fn identifiers_are_validated_and_preserve_well_known_values() {
    let class = ResourceClass::new("vendor.example/accelerator").unwrap();
    let dimension = CapacityDimension::new("vendor.example/vram-bytes").unwrap();

    assert_eq!(class.as_str(), "vendor.example/accelerator");
    assert_eq!(class.default_dimension(), None);
    assert_eq!(dimension.as_str(), "vendor.example/vram-bytes");
    assert_eq!(ResourceClass::Gpu.as_str(), "gpu");
    assert_eq!(CapacityDimension::Bytes.as_str(), "bytes");

    assert_eq!(ResourceClass::new(""), Err(IdentifierError::Empty));
    assert_eq!(
        ResourceClass::new("Vendor/gpu"),
        Err(IdentifierError::InvalidCharacter { index: 0 })
    );
    assert_eq!(
        ResourceClass::new("-gpu"),
        Err(IdentifierError::InvalidBoundary)
    );
    assert_eq!(
        ResourceClass::new(&"a".repeat(64)),
        Err(IdentifierError::TooLong)
    );
}

#[test]
fn generic_topology_relations_derive_from_containment() {
    let mut graph = Graph::new();
    let left_machine = NodeId::from_u64(1);
    let right_machine = NodeId::from_u64(2);
    let left_device = NodeId::from_u64(3);
    let right_device = NodeId::from_u64(4);
    graph
        .apply(
            vec![
                archon_kernel::Node {
                    id: left_machine,
                    kind: ResourceClass::Machine,
                    attrs: Default::default(),
                    capacity: Default::default(),
                },
                archon_kernel::Node {
                    id: right_machine,
                    kind: ResourceClass::Machine,
                    attrs: Default::default(),
                    capacity: Default::default(),
                },
                archon_kernel::Node {
                    id: left_device,
                    kind: ResourceClass::Gpu,
                    attrs: Default::default(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
                archon_kernel::Node {
                    id: right_device,
                    kind: ResourceClass::Gpu,
                    attrs: Default::default(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
            ],
            vec![
                Edge {
                    from: left_machine,
                    to: left_device,
                    kind: EdgeKind::Contains,
                    attrs: Default::default(),
                },
                Edge {
                    from: right_machine,
                    to: right_device,
                    kind: EdgeKind::Contains,
                    attrs: Default::default(),
                },
            ],
        )
        .unwrap();

    assert!(graph.satisfies(
        left_device,
        left_machine,
        TopologyRelation::SameAncestor {
            class: ResourceClass::Machine,
        }
    ));
    assert!(graph.satisfies(
        left_device,
        right_device,
        TopologyRelation::DifferentAncestor {
            class: ResourceClass::Machine,
        }
    ));
    assert!(!graph.satisfies(
        left_device,
        right_device,
        TopologyRelation::SameAncestor {
            class: ResourceClass::Machine,
        }
    ));
}

#[test]
fn dynamic_resource_class_and_capacity_dimension_are_schedulable() {
    let class = ResourceClass::new("vendor.example/accelerator").unwrap();
    let dimension = CapacityDimension::new("vendor.example/vram-bytes").unwrap();
    let machine = NodeId::from_u64(1);
    let device = NodeId::from_u64(2);

    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyResourceFacts {
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
        .unwrap();

    let allocation = cluster.allocate(&request(class, dimension, 8)).unwrap();
    assert_eq!(allocation.claims.len(), 1);
    assert_eq!(allocation.claims[0].node, device);
    assert_eq!(allocation.claims[0].quantity.get(&dimension), Some(&8));
    assert_eq!(cluster.graph.nodes_of_class(class), &[device]);
}
