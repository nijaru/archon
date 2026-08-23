//! Real process execution adapter. A lease's claims become a spawned OS
//! process; lease termination kills it. On Linux the process also runs
//! inside a per-lease cgroup v2 group, so CPU and memory claims are real
//! kernel limits. On macOS only lifecycle enforcement exists.

use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use archon_kernel::LeaseId;

use crate::protocol::LeaseLimits;

#[cfg(target_os = "linux")]
use crate::cgroup::CgroupGroup;

/// Lifecycle observation of a lease's workload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkStatus {
    /// Nothing tracked under this lease.
    Gone,
    /// The workload is still running.
    Running,
    /// The workload exited on its own.
    Exited(i32),
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

    /// Directory holding per-lease output files; ARCHON_LOG_DIR overrides
    /// the default temporary location.
    pub fn log_dir() -> std::path::PathBuf {
        std::env::var_os("ARCHON_LOG_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("archon-logs"))
    }

    /// A lease's captured output; the tail when the file grew large.
    pub fn read_log(lease: LeaseId) -> String {
        let path = Self::log_dir().join(format!("lease-{}.log", lease.as_u64()));
        match std::fs::read(&path) {
            Ok(bytes) if bytes.len() > 64 * 1024 => {
                String::from_utf8_lossy(&bytes[bytes.len() - 64 * 1024..]).into_owned()
            }
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        }
    }

    /// Enable cgroup v2 enforcement under `root` (e.g.
    /// `/sys/fs/cgroup/archon`). Requires root or a delegated subtree.
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
        // Output goes to a per-lease file so results survive the process.
        let log_path = Self::log_dir().join(format!("lease-{}.log", lease.as_u64()));
        if let Some(parent) = log_path.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            return Err(format!("create {}: {err}", parent.display()));
        }
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|err| format!("open {}: {err}", log_path.display()))?;
        child_command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log_file.try_clone().map_err(|err| err.to_string())?,
            ))
            .stderr(Stdio::from(log_file));

        #[cfg(target_os = "linux")]
        let group = match (&self.cgroup_root, limits.is_empty()) {
            (Some(root), false) => {
                let group = CgroupGroup::create(root, lease, limits)?;
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
        matches!(self.status(lease), WorkStatus::Running)
    }

    /// Observe the lease's workload lifecycle, reaping finished children.
    pub fn status(&mut self, lease: LeaseId) -> WorkStatus {
        let Some(child) = self.children.get_mut(&lease) else {
            return WorkStatus::Gone;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.children.remove(&lease);
                WorkStatus::Exited(status.code().unwrap_or(-1))
            }
            Ok(None) => WorkStatus::Running,
            Err(_) => WorkStatus::Gone,
        }
    }

    /// Terminate the lease's workload, asking politely first: SIGTERM with
    /// a grace budget before the existing kill path. Zero grace goes
    /// straight to terminate.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    pub fn terminate_with_grace(
        &mut self,
        lease: LeaseId,
        grace_secs: u32,
    ) -> Result<bool, String> {
        if grace_secs == 0 {
            return self.terminate(lease);
        }
        #[cfg(target_os = "linux")]
        if let Some(group) = self.groups.get(&lease) {
            for pid in group.pids() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
            wait_until(Duration::from_secs(grace_secs as u64), || {
                group.pids().is_empty()
            });
        }
        #[cfg(not(target_os = "linux"))]
        if let Some(child) = self.children.get_mut(&lease)
            && matches!(child.try_wait(), Ok(None))
        {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            let mut done = false;
            wait_until(Duration::from_secs(grace_secs as u64), || {
                if done {
                    return true;
                }
                done = !matches!(child.try_wait(), Ok(None));
                done
            });
        }
        self.terminate(lease)
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

/// Poll `check` every 100 ms until it returns true or `budget` elapses.
fn wait_until(budget: std::time::Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + budget;
    while !check() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    true
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
