//! Linux cgroup v2 enforcement: a lease's claims become real kernel limits.
//! One cgroup per lease under an Archon root; the lease's CPU and memory
//! claims map to `cpu.max` and `memory.max`, the spawned process joins the
//! group, and termination kills the whole group via `cgroup.kill`.
//! Requires root or a delegated cgroup v2 subtree.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use archon_kernel::LeaseId;

use crate::protocol::LeaseLimits;

pub struct CgroupGroup {
    path: PathBuf,
}

impl CgroupGroup {
    /// Create `archon/lease-<id>` under the cgroup v2 root and enable the cpu
    /// and memory controllers on the Archon subtree.
    pub fn create(root: &str, lease: LeaseId, limits: &LeaseLimits) -> Result<Self, String> {
        let root = PathBuf::from(root);
        fs::create_dir_all(&root).map_err(|err| format!("create {}: {err}", root.display()))?;
        // Controllers must be enabled in the parent's subtree_control before
        // children can use them.
        enable_controllers(&root)?;
        let path = root.join(format!("lease-{}", lease.as_u64()));
        fs::create_dir(&path).map_err(|err| format!("create {}: {err}", path.display()))?;
        let group = Self { path };
        if limits.cpu_count > 0 {
            // quota/period: cpu_count cores worth of every 100 ms window.
            let quota = limits
                .cpu_count
                .checked_mul(100_000)
                .ok_or("cpu quota overflow")?;
            group.write("cpu.max", &format!("{quota} 100000"))?;
        }
        if limits.memory_bytes > 0 {
            group.write("memory.max", &limits.memory_bytes.to_string())?;
        }
        Ok(group)
    }

    pub fn attach(&self, pid: i32) -> Result<(), String> {
        self.write("cgroup.procs", &pid.to_string())
    }

    /// Kill every process in the group atomically (kernel 5.14+).
    pub fn kill(&self) -> Result<(), String> {
        self.write("cgroup.kill", "1")
    }

    /// Process ids currently in the group.
    pub fn pids(&self) -> Vec<i32> {
        fs::read_to_string(self.path.join("cgroup.procs"))
            .map(|text| {
                text.lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Remove the group once its process list is empty.
    pub fn destroy(self) -> Result<(), String> {
        fs::remove_dir(&self.path).map_err(|err| format!("remove {}: {err}", self.path.display()))
    }

    fn write(&self, file: &str, content: &str) -> Result<(), String> {
        let path = self.path.join(file);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|err| format!("open {}: {err}", path.display()))?;
        file.write_all(content.as_bytes())
            .map_err(|err| format!("write {}: {err}", path.display()))
    }
}

fn enable_controllers(root: &Path) -> Result<(), String> {
    let control = root.join("cgroup.subtree_control");
    let current =
        fs::read_to_string(&control).map_err(|err| format!("read {}: {err}", control.display()))?;
    let missing: Vec<&str> = ["+cpu", "+memory"]
        .into_iter()
        .filter(|token| !current.split_whitespace().any(|word| word == &token[1..]))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(&control)
        .map_err(|err| format!("open {}: {err}", control.display()))?;
    file.write_all(missing.join(" ").as_bytes())
        .map_err(|err| format!("write {}: {err}", control.display()))
}
