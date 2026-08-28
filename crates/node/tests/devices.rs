//! Stable device identity: a device keeps its graph node across agent
//! re-registrations even when its host path changes, so live claims keep
//! resolving through the current path.

use archon_kernel::{
    CapacityDimension, LeaseId, LeaseState, Need, NodeState, OwnerId, Request, RequestClass,
    RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::{DeviceSpec, MachineDescription};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LocalExecutor, NodeService};

fn description(instance: &str, gpu_dev: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: "gpu-box".into(),
        cpus: 2,
        memory_bytes: 0,
        devices: vec![DeviceSpec {
            kind: ResourceClass::Gpu,
            id: "gpu0".into(),
            dev: gpu_dev.into(),
            access: Vec::new(),
            attrs: Default::default(),
        }],
    }
}

#[test]
fn provider_support_paths_follow_one_device_claim() {
    let mut service = NodeService::new();
    let mut description = description("inst-paths", "/dev/null");
    description.devices[0].access = vec!["/dev/zero".into()];
    description.devices[0]
        .attrs
        .insert("cdi".into(), "nvidia.com/gpu=gpu0".into());
    service
        .register_agent(
            description,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("registration");
    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let devices = service.lease_devices(LeaseId::from_u64(1));
    assert_eq!(devices[0].dev, "/dev/null");
    assert_eq!(devices[0].paths, vec!["/dev/zero"]);
    assert_eq!(devices[0].cdi.as_deref(), Some("nvidia.com/gpu=gpu0"));
    service.revoke(LeaseId::from_u64(1)).expect("revoke");
}

fn gpu_request() -> Request {
    Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Gpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec!["sleep".into(), "5".into()],
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 3_600,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
    }
}

#[test]
fn device_identity_survives_re_registration_with_new_path() {
    let mut service = NodeService::new();
    service
        .register_agent(
            description("inst-dev", "/dev/gpuA"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("first registration");
    let gpu = *service
        .cluster
        .graph
        .nodes_of_class(ResourceClass::Gpu)
        .first()
        .expect("declared gpu exists");
    assert_eq!(
        service.cluster.graph.node(gpu).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuA".to_string())
    );

    // Admit work that holds the device claim.
    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));

    // The agent returns with the same device id behind a new host path.
    service
        .register_agent(
            description("inst-dev", "/dev/gpuB"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("re-registration");

    // Same node, refreshed path: the live claim still points at gpu0.
    let attrs = &service.cluster.graph.node(gpu).unwrap().attrs;
    assert_eq!(attrs.get("dev"), Some(&"/dev/gpuB".to_string()));
    assert_eq!(attrs.get("id"), Some(&"gpu0".to_string()));
    assert_eq!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Gpu)
            .len(),
        1,
        "no duplicate device node"
    );
    let access = service.lease_devices(LeaseId::from_u64(1));
    assert_eq!(access.len(), 1);
    assert_eq!(access[0].id, "gpu0");
    assert_eq!(access[0].dev, "/dev/gpuB");
}

#[test]
fn re_registration_adds_and_refreshes_without_spurious_revisions() {
    let mut service = NodeService::new();
    service
        .register_agent(
            description("inst-add", "/dev/gpuA"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("first registration");

    // Identical declaration: nothing to reconcile.
    let revision = service.cluster.graph.revision;
    service
        .register_agent(
            description("inst-add", "/dev/gpuA"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("no-op re-registration");
    assert_eq!(
        service.cluster.graph.revision, revision,
        "identical declaration must not advance the graph"
    );

    // One path refresh plus one brand-new device.
    let mut updated = description("inst-add", "/dev/gpuC");
    updated.devices.push(DeviceSpec {
        kind: ResourceClass::Nvme,
        id: "nvme0".into(),
        dev: "/dev/nvme0n1".into(),
        access: Vec::new(),
        attrs: Default::default(),
    });
    service
        .register_agent(
            updated,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("re-registration with changes");
    assert_eq!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Nvme)
            .len(),
        1,
        "new device added"
    );
    let gpus = service.cluster.graph.nodes_of_class(ResourceClass::Gpu);
    assert_eq!(gpus.len(), 1);
    assert_eq!(
        service
            .cluster
            .graph
            .node(gpus[0])
            .unwrap()
            .attrs
            .get("dev"),
        Some(&"/dev/gpuC".to_string()),
        "path refreshed in place"
    );
}

#[test]
fn provider_disappearance_blocks_placement_and_same_id_reappears() {
    let mut service = NodeService::new();
    service
        .register_agent(
            description("inst-disappear", "/dev/gpuA"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("first registration");
    let gpu = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];

    let mut missing = description("inst-disappear", "/dev/gpuA");
    missing.devices.clear();
    service
        .register_agent(
            missing,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("provider reports disappearance");

    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Unavailable)
    );
    assert_eq!(
        service.cluster.graph.nodes_of_class(ResourceClass::Gpu),
        vec![gpu],
        "disappearance preserves the stable identity record"
    );
    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(
        service.admit_one().unwrap(),
        None,
        "missing provider capacity must not accept new claims"
    );

    service
        .register_agent(
            description("inst-disappear", "/dev/gpuB"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("same stable device returns");

    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Schedulable)
    );
    assert_eq!(
        service.cluster.graph.nodes_of_class(ResourceClass::Gpu),
        vec![gpu]
    );
    assert_eq!(
        service.cluster.graph.node(gpu).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuB".to_string())
    );
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    service.revoke(LeaseId::from_u64(1)).expect("cleanup");
}

#[test]
fn provider_disappearance_fails_and_fences_a_live_device_claim() {
    let mut service = NodeService::new();
    service
        .register_agent(
            description("inst-live", "/dev/null"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("first registration");
    let gpu = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];
    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    service
        .drive(std::time::Duration::from_secs(1))
        .expect("activate workload");
    assert_eq!(
        service
            .cluster
            .leases
            .get(&LeaseId::from_u64(1))
            .unwrap()
            .state,
        LeaseState::Active
    );

    let mut missing = description("inst-live", "/dev/null");
    missing.devices.clear();
    service
        .register_agent(
            missing,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("provider reports disappearance");

    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Unavailable)
    );
    assert_eq!(
        service
            .cluster
            .leases
            .get(&LeaseId::from_u64(1))
            .unwrap()
            .state,
        LeaseState::Failed,
        "work using vanished hardware follows the workload-failure path"
    );
    service
        .drive(std::time::Duration::from_secs(1))
        .expect("fence old binding");
    assert!(
        !service.cluster.occupies(LeaseId::from_u64(1)),
        "claim remains occupied until the binding fence is acknowledged"
    );

    service
        .register_agent(
            description("inst-live", "/dev/zero"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("same stable device returns after fencing");
    assert_eq!(
        service.cluster.node_state(gpu),
        Some(NodeState::Schedulable)
    );
    assert_eq!(
        service.cluster.graph.nodes_of_class(ResourceClass::Gpu),
        vec![gpu]
    );
}

#[test]
fn replacement_at_the_same_path_gets_a_new_identity() {
    let mut service = NodeService::new();
    service
        .register_agent(
            description("inst-replace", "/dev/gpuA"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("first registration");
    let original = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];

    let mut replacement = description("inst-replace", "/dev/gpuA");
    replacement.devices[0].id = "gpu1".into();
    service
        .register_agent(
            replacement,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("replacement registration");

    let gpus = service.cluster.graph.nodes_of_class(ResourceClass::Gpu);
    assert_eq!(gpus.len(), 2);
    assert_eq!(
        service.cluster.node_state(original),
        Some(NodeState::Unavailable)
    );
    let fresh = *gpus
        .iter()
        .find(|node| **node != original)
        .expect("new node");
    assert_eq!(
        service.cluster.node_state(fresh),
        Some(NodeState::Schedulable)
    );
    assert_eq!(
        service
            .cluster
            .graph
            .node(original)
            .unwrap()
            .attrs
            .get("id"),
        Some(&"gpu0".to_string())
    );
    assert_eq!(
        service.cluster.graph.node(fresh).unwrap().attrs.get("id"),
        Some(&"gpu1".to_string())
    );
    assert_eq!(
        service.cluster.graph.node(fresh).unwrap().attrs.get("dev"),
        Some(&"/dev/gpuA".to_string()),
        "host-path reuse does not reuse the old provider identity"
    );
}
