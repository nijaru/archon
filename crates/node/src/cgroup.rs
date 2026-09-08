//! Linux cgroup v2 enforcement: a lease's claims become real kernel limits.
//! One cgroup per lease under an Archon root; the lease's CPU and memory
//! claims map to `cpu.max` and `memory.max`, the spawned process joins the
//! group, and termination kills the whole group via `cgroup.kill`.
//! Requires root or a delegated cgroup v2 subtree.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;

use archon_kernel::LeaseId;

use crate::protocol::LeaseLimits;

pub struct CgroupGroup {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);

impl CgroupGroup {
    /// Create `archon/lease-<id>` under the cgroup v2 root and enable only
    /// the controllers required by this lease's CPU/memory limits. A
    /// device-only group needs no resource controller; it is still a valid
    /// cgroup-device BPF attach point.
    pub fn create(root: &str, lease: LeaseId, limits: &LeaseLimits) -> Result<Self, String> {
        let root = PathBuf::from(root);
        prepare_root(&root, limits)?;
        let path = root.join(format!("lease-{}", lease.as_u64()));
        if let Err(err) = fs::create_dir(&path) {
            if err.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(format!("create {}: {err}", path.display()));
            }
            // A previous controller run left the group behind (lease ids
            // restart at 1); kill its stragglers and reuse it.
            Self { path: path.clone() }.kill()?;
        }
        let group = Self { path };
        if let Err(err) = group.configure_limits(limits) {
            let _ = group.kill();
            let _ = fs::remove_dir(&group.path);
            return Err(err);
        }
        Ok(group)
    }

    #[cfg(target_os = "linux")]
    /// Create one empty, uniquely-named probe group. Device capability proof
    /// attaches its BPF program here before deleting the group again.
    pub(crate) fn create_probe(root: &str, limits: &LeaseLimits) -> Result<Self, String> {
        let root = PathBuf::from(root);
        prepare_root(&root, limits)?;
        let seq = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(".archon-probe-{}-{seq}", std::process::id()));
        fs::create_dir(&path).map_err(|err| format!("create {}: {err}", path.display()))?;
        let group = Self { path };
        if let Err(err) = group.configure_limits(limits) {
            let _ = fs::remove_dir(&group.path);
            return Err(err);
        }
        Ok(group)
    }

    fn configure_limits(&self, limits: &LeaseLimits) -> Result<(), String> {
        if limits.cpu_count > 0 {
            // quota/period: cpu_count cores worth of every 100 ms window.
            let quota = limits
                .cpu_count
                .checked_mul(100_000)
                .ok_or("cpu quota overflow")?;
            self.write("cpu.max", &format!("{quota} 100000"))?;
        }
        if limits.memory_bytes > 0 {
            self.write("memory.max", &limits.memory_bytes.to_string())?;
            // A memory Claim is a hard resident-memory boundary. Do not let
            // an unbounded host swap configuration turn it into an
            // effectively larger, unaccounted allocation.
            self.write("memory.swap.max", "0")?;
        }
        // Normalized placement: pin the member to exactly the CPUs and NUMA
        // nodes its claims landed on. Both files exist only when the cpuset
        // controller is enabled, which enable_controllers did from the same
        // limits; a missing file is a configuration bug, not a skip.
        if !limits.placement.cpus.is_empty() {
            self.write("cpuset.cpus", &limits.placement.cpus.render())?;
        }
        if !limits.placement.mems.is_empty() {
            self.write("cpuset.mems", &limits.placement.mems.render())?;
        }
        Ok(())
    }

    /// Open the cgroup directory for `clone3(CLONE_INTO_CGROUP)`.
    #[cfg(target_os = "linux")]
    pub fn open_fd(&self) -> Result<File, String> {
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(&self.path)
            .map_err(|err| format!("open {}: {err}", self.path.display()))
    }

    /// The lease group's filesystem path (the BPF attach point).
    pub fn path(&self) -> &std::path::Path {
        &self.path
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

fn prepare_root(root: &Path, limits: &LeaseLimits) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|err| format!("create {}: {err}", root.display()))?;
    // Controllers must be enabled in the parent's subtree_control before
    // children can use their limit files.
    enable_controllers(root, limits)
}

fn enable_controllers(root: &Path, limits: &LeaseLimits) -> Result<(), String> {
    let mut required = Vec::new();
    if limits.cpu_count > 0 {
        required.push("+cpu");
    }
    if limits.memory_bytes > 0 {
        required.push("+memory");
    }
    if !limits.placement.cpus.is_empty() || !limits.placement.mems.is_empty() {
        required.push("+cpuset");
    }
    if required.is_empty() {
        return Ok(());
    }

    let control = root.join("cgroup.subtree_control");
    let current =
        fs::read_to_string(&control).map_err(|err| format!("read {}: {err}", control.display()))?;
    let missing: Vec<&str> = required
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
