use archon_kernel::{
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
        lifetime: 60,
        priority: 1,
        machine_local: true,
    }
}

fn custom_graph(binding: Option<ClaimBinding>) -> archon_kernel::Cluster {
    let mut cluster = archon_kernel::Cluster::new();
    let machine = NodeId::from_u64(1);
    let resource = NodeId::from_u64(2);
    let kind = ResourceClass::new("example.com/special").unwrap();
    let updates = binding
        .map(|binding| {
            vec![ClaimBindingUpdate {
                node: resource,
                dimension: CapacityDimension::Count,
                binding: Some(binding),
            }]
        })
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
    let error = cluster
        .allocate(&request(kind))
        .expect_err("must be unclaimable");
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
    let allocation = cluster
        .allocate(&request(kind))
        .expect("explicitly claimable");
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
    assert!(matches!(
        stale,
        archon_kernel::Error::StaleGraphRevision { .. }
    ));
    let fresh = Allocation {
        claims: vec![Claim {
            node: NodeId::from_u64(2),
            quantity: qty(CapacityDimension::Count, 1),
        }],
        graph_revision: cluster.graph.revision,
        explanation: String::new().into(),
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
    assert!(matches!(
        error,
        archon_kernel::Error::UnclaimableNode { .. }
    ));
}
