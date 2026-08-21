//! Real process execution adapter. A lease's claims become a spawned OS
//! process; lease termination kills it. On Linux the process also runs
//! inside a per-lease cgroup v2 group, so CPU and memory claims are real
//! kernel limits. On macOS only lifecycle enforcement exists.

use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};

use fleet_kernel::LeaseId;

#[cfg(target_os = "linux")]
use crate::cgroup::{CgroupGroup, LeaseLimits};

#[cfg(not(target_os = "linux"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeaseLimits {
    pub cpu_count: u64,
    pub memory_bytes: u64,
}

#[derive(Default)]
pub struct ProcessRuntime {
    children: BTreeMap<LeaseId, Child>,
    /// Per-lease cgroup groups when enforcement is enabled.
    #[cfg(target_os = "linux")]
    groups: BTreeMap<LeaseId, CgroupGroup>,
    /// Cgroup root for lease groups; empty means lifecycle-only enforcement.
    #[cfg(target_os = "linux")]
    cgroup_root: Option<String>,
}

impl ProcessRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable cgroup v2 enforcement under `root` (e.g.
    /// `/sys/fs/cgroup/fleet`). Requires root or a delegated subtree.
    #[cfg(target_os = "linux")]
    pub fn with_cgroup_root(mut self, root: String) -> Self {
        self.cgroup_root = Some(root);
        self
    }

    /// Spawn the lease's command. On Linux with a cgroup root, the child
    /// runs inside `lease-<id>` under the configured limits.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    pub fn activate(
        &mut self,
        lease: LeaseId,
        command: &[String],
        limits: &LeaseLimits,
    ) -> Result<(), String> {
        if self.children.contains_key(&lease) {
            return Ok(());
        }
        let [program, args @ ..] = command else {
            return Err(format!("lease {lease} has no command to execute"));
        };
        let mut child_command = Command::new(program);
        child_command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(target_os = "linux")]
        let mut group = match (&self.cgroup_root, limits.is_empty()) {
            (Some(root), false) => {
                let group = CgroupGroup::create(root, lease, limits)?;
                child_command.stdout(Stdio::null()).stderr(Stdio::null());
                Some(group)
            }
            _ => None,
        };

        let child = child_command
            .spawn()
            .map_err(|err| format!("spawn {program}: {err}"))?;

        #[cfg(target_os = "linux")]
        if let Some(created) = group.take() {
            created.attach(child.id() as i32)?;
            self.groups.insert(lease, created);
        }

        self.children.insert(lease, child);
        Ok(())
    }

    /// Kill the lease's process if it is still running. On Linux with a
    /// cgroup, the whole group dies atomically and the group is removed.
    pub fn terminate(&mut self, lease: LeaseId) -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        if let Some(group) = self.groups.remove(&lease) {
            group.kill()?;
            let killed = self.reap(lease)?;
            group.destroy()?;
            return Ok(killed);
        }
        let Some(mut child) = self.children.remove(&lease) else {
            return Ok(false);
        };
        match child.try_wait() {
            Ok(Some(_)) => Ok(false),
            Ok(None) => {
                child
                    .kill()
                    .map_err(|err| format!("kill lease {lease}: {err}"))?;
                child
                    .wait()
                    .map_err(|err| format!("reap lease {lease}: {err}"))?;
                Ok(true)
            }
            Err(err) => Err(format!("poll lease {lease}: {err}")),
        }
    }

    /// Whether the lease has a running child.
    pub fn is_running(&mut self, lease: LeaseId) -> bool {
        match self.children.get_mut(&lease) {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    #[cfg(target_os = "linux")]
    fn reap(&mut self, lease: LeaseId) -> Result<bool, String> {
        let Some(mut child) = self.children.remove(&lease) else {
            return Ok(false);
        };
        let was_live = matches!(child.try_wait(), Ok(None));
        child
            .wait()
            .map_err(|err| format!("reap lease {lease}: {err}"))?;
        Ok(was_live)
    }
}

#[cfg(target_os = "linux")]
impl Drop for ProcessRuntime {
    fn drop(&mut self) {
        // Kill any surviving lease groups so a crashed agent cannot leak
        // processes or cgroups.
        let leases: Vec<LeaseId> = self.groups.keys().copied().collect();
        for lease in leases {
            let _ = self.terminate(lease);
        }
    }
}
