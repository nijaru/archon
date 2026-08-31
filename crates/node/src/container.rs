//! Container execution adapter: runs a lease's command inside an OCI
//! container via an engine CLI (Docker or Podman share the interface).
//! Lease limits become container limits; termination force-removes the
//! container, which kills it regardless of what it is doing.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use archon_kernel::LeaseId;

use crate::workload::{PortPublish, StorageMount};

use crate::protocol::LeaseLimits;

/// Everything one activation needs, bundled so the executor seam stays
/// two arguments wide.
pub struct ContainerConfig<'a> {
    pub image: &'a str,
    pub command: &'a [String],
    pub limits: &'a LeaseLimits,
    pub storage: &'a [StorageMount],
    pub ports: &'a [PortPublish],
    pub devices: &'a [crate::protocol::DeviceAccess],
}

pub struct ContainerRuntime {
    /// Engine binary; "docker" and "podman" share the CLI surface.
    engine: String,
    /// Per-runtime prefix so concurrent agents never fight over names.
    namespace: String,
    /// A recognized Docker/Podman CLI answered --version successfully.
    available: bool,
    containers: BTreeMap<LeaseId, String>,
    /// Engine is podman: volume mounts need the SELinux relabel suffix.
    relabels: bool,
    /// Explicit opt-in for provider-native CDI names. The default remains
    /// direct host-device paths so stale runtime CDI metadata cannot silently
    /// change attachment behavior.
    use_cdi: bool,
    /// Optional Podman CDI search path, used only when explicitly configured.
    cdi_spec_dir: Option<String>,
}

static CONTAINER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ContainerRuntime {
    pub fn new(engine: String) -> Self {
        let seq = CONTAINER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Podman under SELinux mounts host paths read-only for the
        // container user unless they are relabeled; `:Z` is a no-op where
        // SELinux is off.
        let version = Command::new(&engine).arg("--version").output().ok();
        let version_text = version
            .as_ref()
            .map(|output| {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                text.to_ascii_lowercase()
            })
            .unwrap_or_default();
        let recognized = version_text.contains("docker") || version_text.contains("podman");
        let available = version
            .as_ref()
            .is_some_and(|output| output.status.success())
            && recognized;
        let relabels = available && version_text.contains("podman");
        let use_cdi = std::env::var("ARCHON_CONTAINER_USE_CDI")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes"
                )
            })
            .unwrap_or(false);
        let cdi_spec_dir = (use_cdi && relabels)
            .then(|| std::env::var("ARCHON_CDI_SPEC_DIR").ok())
            .flatten();
        Self {
            engine,
            namespace: format!("{}-{}-{seq}", std::process::id(), lease_namespace_salt()),
            available,
            containers: BTreeMap::new(),
            relabels,
            use_cdi,
            cdi_spec_dir,
        }
    }

    pub fn capabilities(&self) -> crate::protocol::RuntimeCapabilities {
        crate::protocol::RuntimeCapabilities {
            available: self.available,
            cpu_limit: self.available,
            memory_limit: self.available,
            device_isolation: self.available,
            physical_cpu_placement: false,
            numa_memory_placement: false,
        }
    }

    fn name(&self, lease: LeaseId) -> String {
        format!("archon-{}-lease-{}", self.namespace, lease.as_u64())
    }

    pub fn activate(&mut self, lease: LeaseId, cfg: ContainerConfig) -> Result<(), String> {
        if self.containers.contains_key(&lease) {
            return Ok(());
        }
        let name = self.name(lease);
        let count = cfg.limits.cpu_count;
        let mut cmd = Command::new(&self.engine);
        if let Some(dir) = &self.cdi_spec_dir {
            cmd.arg("--cdi-spec-dir").arg(dir);
        }
        cmd.arg("run")
            // No --rm: the adapter removes containers on teardown, and a
            // removed container's exit code is unreadable, which would
            // misreport natural completion as failure.
            .arg("-d")
            .arg("--name")
            .arg(&name)
            .arg("--pull")
            .arg("missing");
        if count > 0 {
            cmd.arg(format!("--cpus={count}"));
        }
        if cfg.limits.memory_bytes > 0 {
            cmd.arg(format!("--memory={}b", cfg.limits.memory_bytes));
        }
        for device in cfg.devices {
            if self.use_cdi
                && let Some(cdi) = &device.cdi
            {
                cmd.arg(format!("--device={cdi}"));
            } else {
                for path in std::iter::once(&device.dev).chain(device.paths.iter()) {
                    cmd.arg(format!("--device={path}:{path}"));
                }
            }
        }
        for mount in cfg.storage {
            let relabel = if self.relabels { ":Z" } else { "" };
            let suffix = format!("{}{}", mount.mount_path, relabel);
            cmd.arg(format!("--volume={}:{}", mount.host_path, suffix));
        }
        for port in cfg.ports {
            match port.host_port {
                Some(host) => cmd.arg(format!("-p={host}:{}", port.container_port)),
                None => cmd.arg(format!("-p={}", port.container_port)),
            };
        }
        cmd.arg(cfg.image).args(cfg.command);
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
        matches!(self.status(lease), super::runtime::WorkStatus::Running)
    }

    /// Observe the container lifecycle via the engine's inspect state.
    pub fn status(&mut self, lease: LeaseId) -> super::runtime::WorkStatus {
        let Some(name) = self.containers.get(&lease) else {
            return super::runtime::WorkStatus::Gone;
        };
        let inspect = |format: &str| {
            Command::new(&self.engine)
                .arg("inspect")
                .arg("-f")
                .arg(format)
                .arg(name)
                .output()
        };
        match inspect("{{.State.Running}}") {
            Ok(output)
                if output.status.success()
                    && String::from_utf8_lossy(&output.stdout).trim() == "true" =>
            {
                super::runtime::WorkStatus::Running
            }
            Ok(_) => {
                let code = inspect("{{.State.ExitCode}}")
                    .ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
                    .unwrap_or(-1);
                super::runtime::WorkStatus::Exited(code)
            }
            Err(_) => super::runtime::WorkStatus::Gone,
        }
    }

    /// The container's captured output via the engine's logs command;
    /// works for exited containers too since the adapter owns removal.
    pub fn logs(&self, lease: LeaseId) -> String {
        let Some(name) = self.containers.get(&lease) else {
            return String::new();
        };
        Command::new(&self.engine)
            .arg("logs")
            .arg(name)
            .output()
            .map(|output| {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                text
            })
            .unwrap_or_default()
    }

    /// Stop with a SIGTERM grace budget before removing the container.
    /// Stop with a SIGTERM grace budget, then remove. The container stays
    /// tracked until cleanup is positively confirmed: a failed engine
    /// command returns an error (an uncertain failure the controller
    /// treats as lease failure) instead of acknowledging teardown while
    /// the workload may still run.
    pub fn terminate_with_grace(
        &mut self,
        lease: LeaseId,
        grace_secs: u32,
    ) -> Result<bool, String> {
        let Some(name) = self.containers.get(&lease) else {
            return Ok(false);
        };
        let name = name.clone();
        let stop = Command::new(&self.engine)
            .arg("stop")
            .arg("-t")
            .arg(grace_secs.to_string())
            .arg(&name)
            .output()
            .map_err(|err| format!("{} stop: {err}", self.engine))?;
        if !stop.status.success() {
            return Err(format!(
                "{} stop {}: {}",
                self.engine,
                name,
                String::from_utf8_lossy(&stop.stderr).trim()
            ));
        }
        let rm = Command::new(&self.engine)
            .arg("rm")
            .arg("-f")
            .arg(&name)
            .output()
            .map_err(|err| format!("{} rm: {err}", self.engine))?;
        if !rm.status.success() {
            return Err(format!(
                "{} rm {}: {}",
                self.engine,
                name,
                String::from_utf8_lossy(&rm.stderr).trim()
            ));
        }
        self.containers.remove(&lease);
        Ok(true)
    }
}

fn lease_namespace_salt() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
}
