//! Container execution adapter: runs a lease's command inside an OCI
//! container via an engine CLI (Docker or Podman share the interface).
//! Lease limits become container limits; termination force-removes the
//! container, which kills it regardless of what it is doing.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use archon_kernel::LeaseId;

use crate::protocol::LeaseLimits;

pub struct ContainerRuntime {
    /// Engine binary; "docker" and "podman" share the CLI surface.
    engine: String,
    containers: BTreeMap<LeaseId, String>,
}

impl ContainerRuntime {
    pub fn new(engine: String) -> Self {
        Self {
            engine,
            containers: BTreeMap::new(),
        }
    }

    fn name(lease: LeaseId) -> String {
        format!("archon-lease-{}", lease.as_u64())
    }

    pub fn activate(
        &mut self,
        lease: LeaseId,
        image: &str,
        command: &[String],
        limits: &LeaseLimits,
    ) -> Result<(), String> {
        if self.containers.contains_key(&lease) {
            return Ok(());
        }
        let name = Self::name(lease);
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
