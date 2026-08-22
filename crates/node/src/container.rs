//! Container execution adapter: runs a lease's command inside an OCI
//! container via an engine CLI (Docker or Podman share the interface).
//! Lease limits become container limits; termination force-removes the
//! container, which kills it regardless of what it is doing.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use archon_kernel::{LeaseId, PortPublish, StorageMount};

use crate::protocol::LeaseLimits;

pub struct ContainerRuntime {
    /// Engine binary; "docker" and "podman" share the CLI surface.
    engine: String,
    /// Per-runtime prefix so concurrent agents never fight over names.
    namespace: String,
    containers: BTreeMap<LeaseId, String>,
}

static CONTAINER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ContainerRuntime {
    pub fn new(engine: String) -> Self {
        let seq = CONTAINER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            engine,
            namespace: format!("{}-{}-{seq}", std::process::id(), lease_namespace_salt()),
            containers: BTreeMap::new(),
        }
    }

    fn name(&self, lease: LeaseId) -> String {
        format!("archon-{}-lease-{}", self.namespace, lease.as_u64())
    }

    pub fn activate(
        &mut self,
        lease: LeaseId,
        image: &str,
        command: &[String],
        limits: &LeaseLimits,
        storage: &[StorageMount],
        ports: &[PortPublish],
    ) -> Result<(), String> {
        if self.containers.contains_key(&lease) {
            return Ok(());
        }
        let name = self.name(lease);
        let count = limits.cpu_count;
        let mut cmd = Command::new(&self.engine);
        cmd.arg("run")
            .arg("-d")
            .arg("--rm")
            .arg("--name")
            .arg(&name)
            .arg("--pull")
            .arg("missing");
        if count > 0 {
            cmd.arg(format!("--cpus={count}"));
        }
        if limits.memory_bytes > 0 {
            cmd.arg(format!("--memory={}b", limits.memory_bytes));
        }
        for mount in storage {
            cmd.arg(format!("--volume={}:{}", mount.host_path, mount.mount_path));
        }
        for port in ports {
            match port.host_port {
                Some(host) => cmd.arg(format!("-p={host}:{}", port.container_port)),
                None => cmd.arg(format!("-p={}", port.container_port)),
            };
        }
        cmd.arg(image).args(command);
        let output = cmd
            .stdin(Stdio::null())
            .output()
            .map_err(|err| format!("run {}: {err}", self.engine))?;
        if !output.status.success() {
            return Err(format!(
                "{} run failed: {}",
                self.engine,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        self.containers.insert(lease, name);
        Ok(())
    }

    /// Whether this lease runs as a container (vs a bare process).
    pub fn is_tracked(&self, lease: LeaseId) -> bool {
        self.containers.contains_key(&lease)
    }

    pub fn terminate(&mut self, lease: LeaseId) -> Result<bool, String> {
        let Some(name) = self.containers.remove(&lease) else {
            return Ok(false);
        };
        let output = Command::new(&self.engine)
            .arg("rm")
            .arg("-f")
            .arg(&name)
            .output()
            .map_err(|err| format!("{} rm: {err}", self.engine))?;
        Ok(output.status.success())
    }

    pub fn is_running(&mut self, lease: LeaseId) -> bool {
        let Some(name) = self.containers.get(&lease) else {
            return false;
        };
        let output = Command::new(&self.engine)
            .arg("inspect")
            .arg("-f")
            .arg("{{.State.Running}}")
            .arg(name)
            .output();
        match output {
            Ok(output) => {
                output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true"
            }
            Err(_) => false,
        }
    }
}

fn lease_namespace_salt() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
}
