//! Hardware-gated NVIDIA discovery and whole-device lease proof.
//!
//! The test is intentionally skipped when the host has no NVIDIA provider;
//! the deterministic discovery/parser tests cover the portable path.

#![cfg(target_os = "linux")]

use std::process::Command;
use std::time::{Duration, Instant};

use archon_kernel::{
    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::service::NodeService;

fn gpu_request() -> Request {
    Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: ResourceClass::Gpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
        ],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![
            "sh".into(),
            "-c".into(),
            "nvidia-smi --query-gpu=uuid --format=csv,noheader; sleep 30".into(),
        ],
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

fn wait_until(deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    check()
}

#[test]
fn auto_discovers_and_runs_a_real_nvidia_device_lease() {
    if std::env::var_os("ARCHON_DEVICES").is_some()
        || Command::new("nvidia-smi").arg("-L").output().is_err()
    {
        eprintln!("skipping: automatic NVIDIA discovery is unavailable or overridden");
        return;
    }

    let cgroup_root = std::env::var("ARCHON_CGROUP_ROOT");
    let mut service = NodeService::local(cgroup_root.ok());
    let gpus = service.cluster.graph.nodes_of_class(ResourceClass::Gpu);
    if gpus.is_empty() {
        eprintln!("skipping: nvidia-smi reported no GPUs");
        return;
    }
    assert_eq!(gpus.len(), 1, "test host is expected to expose one GPU");
    let gpu = service.cluster.graph.node(gpus[0]).expect("GPU node");
    let uuid = gpu.attrs.get("uuid").cloned().expect("stable NVIDIA UUID");
    assert_eq!(gpu.attrs.get("provider"), Some(&"nvidia".into()));
    assert!(gpu.attrs.contains_key("pci_bus_id"));
    assert!(gpu.attrs.contains_key("compute_capability"));
    assert_eq!(gpu.attrs.get("whole_device"), Some(&"true".into()));
    assert!(gpu.attrs.contains_key("cdi"));

    service.submit(gpu_request(), OwnerId::from_u64(1));
    assert_eq!(
        service.admit_one().expect("admit"),
        Some(RequestId::from_u64(1))
    );
    let lease = LeaseId::from_u64(1);
    let access = service.lease_devices(lease);
    assert_eq!(access.len(), 1);
    assert_eq!(access[0].id, uuid);
    assert_eq!(access[0].dev, "/dev/nvidia0");
    assert!(access[0].paths.iter().any(|path| path == "/dev/nvidiactl"));
    assert!(access[0].paths.iter().any(|path| path == "/dev/nvidia-uvm"));

    assert!(service.is_running(lease), "GPU lease workload must run");
    assert!(wait_until(Duration::from_secs(2), || {
        service
            .lease_logs(lease)
            .is_ok_and(|logs| logs.contains(&uuid))
    }));

    service.revoke(lease).expect("revoke");
    assert!(wait_until(Duration::from_secs(2), || !service.is_running(lease)));
}
