use archon_kernel::{
    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Command, Edge, EdgeKind,
    FactWriterAssignment, FactWriterId, Need, Node, NodeId, OwnerId, ProviderFactBatch, ProviderId,
    Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::{DeviceSpec, MachineDescription};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LocalExecutor, NodeService};

fn request(kind: ResourceClass, id: u64) -> archon_node::workload::WorkloadSpec {
    archon_node::workload::WorkloadSpec {
        resources: Request {
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
            lifetime: 60,
            priority: 1,
            machine_local: true,
        },
        execution: archon_node::workload::ExecutionSpec {
            command: vec!["true".into()],
            image: None,
            storage: Vec::new(),
            ports: Vec::new(),
            grace_secs: 0,
        },
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
        .apply(Command::ApplyProviderFacts {
            batches: vec![ProviderFactBatch {
                writer: FactWriterId::from_u64(100),
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
        service.cluster.graph.node_fact_writer(machine),
        Some(FactWriterId::from_u64(1))
    );
    assert_eq!(
        service.cluster.graph.node_fact_writer(cpu),
        Some(FactWriterId::from_u64(1))
    );
    assert_eq!(
        service.cluster.graph.node_fact_writer(memory),
        Some(FactWriterId::from_u64(1))
    );
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

fn legacy_device(id: &str, dev: &str) -> DeviceSpec {
    DeviceSpec {
        kind: ResourceClass::Gpu,
        id: id.into(),
        dev: dev.into(),
        host_parent: None,
        access: Vec::new(),
        attrs: Default::default(),
    }
}

fn device_node(service: &NodeService, stable_id: &str) -> NodeId {
    service
        .cluster
        .graph
        .nodes_of_class(ResourceClass::Gpu)
        .iter()
        .copied()
        .find(|id| {
            service
                .cluster
                .graph
                .node(*id)
                .and_then(|node| node.attrs.get("id"))
                .is_some_and(|id| id == stable_id)
        })
        .expect("device is present in legacy Graph")
}

#[test]
fn returning_agent_backfills_only_devices_in_current_provider_inventory() {
    let mut service = NodeService::new();
    let mut legacy = legacy_description();
    legacy.devices = vec![
        legacy_device("gpu-keep", "/dev/gpu-keep"),
        legacy_device("gpu-gone", "/dev/gpu-gone"),
    ];
    let (_local, nodes, edges) = archon_node::discover::build_graph(&legacy, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let kept = device_node(&service, "gpu-keep");
    let gone = device_node(&service, "gpu-gone");

    let mut returning = legacy;
    returning.devices = vec![legacy_device("gpu-keep", "/dev/gpu-keep")];
    service
        .register_agent(
            returning,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("current provider inventory reconciles");

    assert_eq!(
        service.cluster.graph.node_fact_writer(kept),
        Some(FactWriterId::from_u64(2))
    );
    assert_eq!(service.cluster.graph.node_fact_writer(gone), None);
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(kept, CapacityDimension::Count),
        Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        })
    );
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(gone, CapacityDimension::Count),
        None
    );
    assert_eq!(
        service.cluster.node_state(gone),
        Some(archon_kernel::NodeState::Unavailable)
    );
}

#[test]
fn returning_device_fact_change_never_overwrites_conflicting_provider_contract() {
    let mut service = NodeService::new();
    let mut legacy = legacy_description();
    legacy.devices = vec![legacy_device("gpu-1", "/dev/gpu-old")];
    let (_local, nodes, edges) = archon_node::discover::build_graph(&legacy, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let gpu = device_node(&service, "gpu-1");
    service
        .cluster
        .apply(Command::ApplyResourceFacts {
            nodes: Vec::new(),
            edges: Vec::new(),
            claim_bindings: vec![ClaimBindingUpdate {
                node: gpu,
                dimension: CapacityDimension::Count,
                binding: Some(ClaimBinding {
                    provider: ProviderId::from_u64(99),
                    scope: BindingScope::Exclusive,
                }),
            }],
        })
        .unwrap();

    let mut returning = legacy;
    returning.devices = vec![legacy_device("gpu-1", "/dev/gpu-new")];
    let error = service
        .register_agent(
            returning,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("device fact refresh must not steal another provider contract");
    assert!(error.to_string().contains("already belongs to provider"));
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(gpu, CapacityDimension::Count)
            .expect("conflicting provider remains recorded")
            .provider,
        ProviderId::from_u64(99)
    );
    assert_eq!(
        service
            .cluster
            .graph
            .node(gpu)
            .and_then(|node| node.attrs.get("dev"))
            .map(String::as_str),
        Some("/dev/gpu-old")
    );
    assert_eq!(
        service.cluster.node_state(machine),
        Some(archon_kernel::NodeState::Unavailable)
    );
}

#[test]
fn returning_host_contract_conflict_prevents_device_reconciliation_mutation() {
    let mut service = NodeService::new();
    let mut legacy = legacy_description();
    legacy.devices = vec![legacy_device("gpu-1", "/dev/gpu-old")];
    let (_local, nodes, edges) = archon_node::discover::build_graph(&legacy, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let cpu = service.cluster.graph.nodes_of_class(ResourceClass::Cpu)[0];
    let gpu = device_node(&service, "gpu-1");
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

    let mut returning = legacy;
    returning.devices = vec![legacy_device("gpu-1", "/dev/gpu-new")];
    let error = service
        .register_agent(
            returning,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("host provider conflict must fail before device reconciliation");
    assert!(error.to_string().contains("already belongs to provider"));
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count)
            .expect("conflicting host provider remains recorded")
            .provider,
        ProviderId::from_u64(99)
    );
    assert_eq!(
        service
            .cluster
            .graph
            .node(gpu)
            .and_then(|node| node.attrs.get("dev"))
            .map(String::as_str),
        Some("/dev/gpu-old")
    );
    assert_eq!(
        service.cluster.node_state(machine),
        Some(archon_kernel::NodeState::Unavailable)
    );
}

#[test]
fn returning_host_writer_conflict_prevents_device_fact_mutation() {
    let mut service = NodeService::new();
    let mut legacy = legacy_description();
    legacy.devices = vec![legacy_device("gpu-1", "/dev/gpu-old")];
    let (_local, nodes, edges) = archon_node::discover::build_graph(&legacy, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let cpu = service.cluster.graph.nodes_of_class(ResourceClass::Cpu)[0];
    let gpu = device_node(&service, "gpu-1");
    service
        .cluster
        .apply(Command::AdoptFactWriters {
            assignments: vec![FactWriterAssignment {
                writer: FactWriterId::from_u64(77),
                nodes: vec![cpu],
                edges: Vec::new(),
            }],
        })
        .unwrap();

    let mut returning = legacy;
    returning.devices = vec![legacy_device("gpu-1", "/dev/gpu-new")];
    let error = service
        .register_agent(
            returning,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("discovery writer conflict must fail before device reconciliation");
    assert!(error.to_string().contains("discovery writer"));
    assert_eq!(
        service.cluster.graph.node_fact_writer(cpu),
        Some(FactWriterId::from_u64(77))
    );
    assert_eq!(
        service
            .cluster
            .graph
            .node(gpu)
            .and_then(|node| node.attrs.get("dev"))
            .map(String::as_str),
        Some("/dev/gpu-old")
    );
    assert_eq!(
        service.cluster.node_state(machine),
        Some(archon_kernel::NodeState::Unavailable)
    );
}
