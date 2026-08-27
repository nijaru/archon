//! Stable device identity: a device keeps its graph node across agent
//! re-registrations even when its host path changes, so live claims keep
//! resolving through the current path.

use archon_kernel::{
    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
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
        }],
    }
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
