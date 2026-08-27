//! Hardware-gated NVIDIA discovery and whole-device lease proof.
//!
//! The test is intentionally skipped when the host has no NVIDIA provider;
//! the deterministic discovery/parser tests cover the portable path.

#![cfg(target_os = "linux")]

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use archon_kernel::{
    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::service::NodeService;

static GPU_LOCK: Mutex<()> = Mutex::new(());

fn gpu_request(id: u64) -> Request {
    let command = if std::env::var_os("ARCHON_CUDA_SMOKE").is_some() {
        "set -e; nvidia-smi --query-gpu=uuid --format=csv,noheader; LD_LIBRARY_PATH=/usr/local/lib/ollama/cuda_v13 \"$ARCHON_CUDA_SMOKE\"; sleep 30"
    } else {
        "nvidia-smi --query-gpu=uuid --format=csv,noheader; sleep 30"
    };
    Request {
        id: RequestId::from_u64(id),
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
        command: vec!["sh".into(), "-c".into(), command.into()],
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

fn cdi_enabled() -> bool {
    matches!(
        std::env::var("ARCHON_CONTAINER_USE_CDI")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes")
    ) && std::env::var_os("ARCHON_CONTAINER_ENGINE").is_some()
        && std::env::var_os("ARCHON_CDI_SPEC_DIR").is_some()
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
    let _gpu = GPU_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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

    service.submit(gpu_request(1), OwnerId::from_u64(1));
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
    let cdi = format!("nvidia.com/gpu={uuid}");
    assert_eq!(access[0].cdi.as_deref(), Some(cdi.as_str()));

    let running = service.is_running(lease);
    if !running {
        let logs = service
            .lease_logs(lease)
            .unwrap_or_else(|err| format!("log error: {err}"));
        panic!(
            "GPU lease workload must run; state={:?}, logs={logs:?}",
            service.cluster.leases.get(&lease).map(|lease| &lease.state)
        );
    }
    assert!(wait_until(Duration::from_secs(2), || {
        service
            .lease_logs(lease)
            .is_ok_and(|logs| logs.contains(&uuid))
    }));
    if std::env::var_os("ARCHON_CUDA_SMOKE").is_some() {
        assert!(wait_until(Duration::from_secs(2), || {
            service
                .lease_logs(lease)
                .is_ok_and(|logs| logs.contains("cuda smoke: devices=1"))
        }));
    }

    service.submit(gpu_request(2), OwnerId::from_u64(2));
    assert_eq!(
        service.admit_one().expect("competing admission"),
        None,
        "a competing whole-device claim must wait",
    );

    service.revoke(lease).expect("revoke");
    assert!(wait_until(Duration::from_secs(2), || {
        let _ = service.drive(Duration::from_millis(20));
        !service.cluster.occupies(lease)
    }));
    assert_eq!(
        service.admit_one().expect("admit queued competitor"),
        Some(RequestId::from_u64(2)),
    );
    let replacement = LeaseId::from_u64(2);
    assert!(
        service.is_running(replacement),
        "queued GPU lease must run after release"
    );
    service.revoke(replacement).expect("revoke replacement");
}

#[test]
fn auto_discovered_nvidia_device_attaches_through_cdi_container() {
    let _gpu = GPU_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !cdi_enabled()
        || std::env::var_os("ARCHON_DEVICES").is_some()
        || Command::new("nvidia-smi").arg("-L").output().is_err()
    {
        eprintln!("skipping: CDI container proof is not configured");
        return;
    }

    let mut service = NodeService::local(None);
    let gpus = service.cluster.graph.nodes_of_class(ResourceClass::Gpu);
    if gpus.is_empty() {
        eprintln!("skipping: nvidia-smi reported no GPUs");
        return;
    }
    assert_eq!(gpus.len(), 1, "test host is expected to expose one GPU");
    let gpu = service.cluster.graph.node(gpus[0]).expect("GPU node");
    let uuid = gpu.attrs.get("uuid").cloned().expect("stable NVIDIA UUID");

    service.submit(
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
            image: Some("docker.io/nvidia/cuda:12.8.1-base-ubuntu24.04".into()),
            storage: vec![],
            ports: vec![],
            lifetime: 3_600,
            priority: 1,
            machine_local: true,
            grace_secs: 0,
            keep_alive: false,
        },
        OwnerId::from_u64(1),
    );
    assert_eq!(
        service.admit_one().expect("admit"),
        Some(RequestId::from_u64(1))
    );
    let lease = LeaseId::from_u64(1);
    let access = service.lease_devices(lease);
    assert_eq!(access.len(), 1);
    let cdi = format!("nvidia.com/gpu={uuid}");
    assert_eq!(access[0].cdi.as_deref(), Some(cdi.as_str()));

    assert!(wait_until(Duration::from_secs(20), || {
        let _ = service.drive(Duration::from_millis(20));
        service
            .lease_logs(lease)
            .is_ok_and(|logs| logs.contains(&uuid))
    }));
    let logs = service.lease_logs(lease).expect("container logs");
    assert!(
        logs.contains(&uuid),
        "CDI container must see the claimed GPU"
    );

    service.revoke(lease).expect("revoke");
    assert!(wait_until(Duration::from_secs(10), || {
        let _ = service.drive(Duration::from_millis(20));
        !service.cluster.occupies(lease)
    }));
}
