use archon_kernel::{
    CapacityDimension, Cluster, Command, IdentifierError, Need, NodeId, Request, RequestClass,
    ResourceClass, qty,
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
        command: vec![],
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 10,
        priority: 1,
        keep_alive: false,
        machine_local: true,
        grace_secs: 0,
    }
}

#[test]
fn identifiers_are_validated_and_preserve_well_known_values() {
    let class = ResourceClass::new("vendor.example/accelerator").unwrap();
    let dimension = CapacityDimension::new("vendor.example/vram-bytes").unwrap();

    assert_eq!(class.as_str(), "vendor.example/accelerator");
    assert!(class.is_enforced());
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
fn dynamic_resource_class_and_capacity_dimension_are_schedulable() {
    let class = ResourceClass::new("vendor.example/accelerator").unwrap();
    let dimension = CapacityDimension::new("vendor.example/vram-bytes").unwrap();
    let machine = NodeId::from_u64(1);
    let device = NodeId::from_u64(2);

    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
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
        .unwrap();

    let allocation = cluster.allocate(&request(class, dimension, 8)).unwrap();
    assert_eq!(allocation.claims.len(), 1);
    assert_eq!(allocation.claims[0].node, device);
    assert_eq!(allocation.claims[0].quantity.get(&dimension), Some(&8));
    assert_eq!(cluster.graph.nodes_of_class(class), &[device]);
}
