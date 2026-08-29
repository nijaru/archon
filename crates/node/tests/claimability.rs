use archon_kernel::{
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

fn add_custom_resource(service: &mut NodeService, binding: Option<ClaimBinding>) -> ResourceClass {
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
                .map(|binding| {
                    vec![ClaimBindingUpdate {
                        node: id,
                        dimension: CapacityDimension::Count,
                        binding: Some(binding),
                    }]
                })
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
    assert!(
        error
            .to_string()
            .contains("no registered resource provider")
    );
    assert!(service.cluster.leases.is_empty());
    assert_eq!(service.queue_len(), 1);
}

fn legacy_description() -> MachineDescription {
    MachineDescription {
        instance_id: "legacy-claimability-agent".into(),
        name: "legacy-claimability-agent".into(),
        cpus: 1,
        memory_bytes: 1 << 30,
        host_nodes: Vec::new(),
        devices: Vec::new(),
    }
}

#[test]
fn returning_agent_backfills_claim_contracts_missing_from_older_graph_state() {
    let mut service = NodeService::new();
    let description = legacy_description();
    let (_local, nodes, edges) = archon_node::discover::build_graph(&description, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();

    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let cpu = service.cluster.graph.nodes_of_class(ResourceClass::Cpu)[0];
    let memory = service.cluster.graph.nodes_of_class(ResourceClass::Memory)[0];
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count),
        None
    );

    let returned = service
        .register_agent(
            description,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("validated returning agent backfills current provider contracts");
    assert_eq!(returned, machine);
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count),
        Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        })
    );
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(memory, CapacityDimension::Bytes),
        Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::IndependentShare,
        })
    );
}

#[test]
fn returning_agent_never_overwrites_a_conflicting_claim_provider() {
    let mut service = NodeService::new();
    let description = legacy_description();
    let (_local, nodes, edges) = archon_node::discover::build_graph(&description, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let cpu = service.cluster.graph.nodes_of_class(ResourceClass::Cpu)[0];
    service
        .cluster
        .apply(Command::ApplyResourceFacts {
            nodes: Vec::new(),
            edges: Vec::new(),
            claim_bindings: vec![ClaimBindingUpdate {
                node: cpu,
                dimension: CapacityDimension::Count,
                binding: Some(ClaimBinding {
                    provider: ProviderId::from_u64(99),
                    scope: BindingScope::Exclusive,
                }),
            }],
        })
        .unwrap();

    let error = service
        .register_agent(
            description,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("returning agent must not steal another provider's claim contract");
    assert!(error.to_string().contains("already belongs to provider"));
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count)
            .expect("conflicting ownership remains recorded")
            .provider,
        ProviderId::from_u64(99)
    );
    assert_eq!(
        service.cluster.node_state(machine),
        Some(archon_kernel::NodeState::Unavailable)
    );
}
