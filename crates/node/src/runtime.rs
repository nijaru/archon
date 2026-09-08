//! Real process execution adapter. A lease's claims become a spawned OS
//! process; lease termination kills it. On Linux the process also runs
//! inside a per-lease cgroup v2 group, so CPU and memory claims are real
//! kernel limits. On macOS only lifecycle enforcement exists.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use archon_kernel::LeaseId;

use crate::protocol::LeaseLimits;
#[cfg(target_os = "linux")]
use crate::protocol::{CpuList, CpuPlacement};

#[cfg(target_os = "linux")]
use crate::cgroup::CgroupGroup;
#[cfg(target_os = "linux")]
use crate::linux_process::{self, LinuxChild};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

enum TrackedChild {
    Standard(Child),
    #[cfg(target_os = "linux")]
    Cgroup(LinuxChild),
}

impl TrackedChild {
    #[cfg(not(target_os = "linux"))]
    fn id(&self) -> u32 {
        match self {
            Self::Standard(child) => child.id(),
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Standard(child) => child.try_wait(),
            #[cfg(target_os = "linux")]
            Self::Cgroup(child) => child.try_wait(),
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        match self {
            Self::Standard(child) => child.wait(),
            #[cfg(target_os = "linux")]
            Self::Cgroup(child) => child.wait(),
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        match self {
            Self::Standard(child) => child.kill(),
            #[cfg(target_os = "linux")]
            Self::Cgroup(child) => child.kill(),
        }
    }
}

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
    children: BTreeMap<LeaseId, TrackedChild>,
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

    pub fn capabilities(&self) -> crate::protocol::RuntimeCapabilities {
        #[cfg(target_os = "linux")]
        {
            let Some(root) = self.cgroup_root.as_deref() else {
                return crate::protocol::RuntimeCapabilities {
                    available: true,
                    ..Default::default()
                };
            };
            let cpu_limit = probe_process_limit(
                root,
                &LeaseLimits {
                    cpu_count: 1,
                    ..Default::default()
                },
            );
            let memory_limit = probe_process_limit(
                root,
                &LeaseLimits {
                    memory_bytes: 1 << 20,
                    ..Default::default()
                },
            );
            let device_isolation = probe_device_isolation(root);
            // Placement proof: write a real (host-valid) cpuset/mems pair
            // into a probe group. Only a host that actually honors these
            // writes may claim placement enforcement; the values come from
            // the process's own affinity/mempolicy so the write cannot
            // fail merely for naming offline CPUs.
            let (placement_cpus, placement_mems) = host_affinity();
            let physical_cpu_placement = probe_placement(root, placement_cpus, placement_mems);
            let numa_memory_placement = physical_cpu_placement;
            crate::protocol::RuntimeCapabilities {
                available: true,
                cpu_limit,
                memory_limit,
                device_isolation,
                physical_cpu_placement,
                numa_memory_placement,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            crate::protocol::RuntimeCapabilities {
                available: true,
                ..Default::default()
            }
        }
    }

    /// Spawn the lease's command. On Linux with a cgroup root, the child
    /// runs inside `lease-<id>` under the configured limits and device
    /// grants. Device claims fail closed when this runtime cannot create an
    /// enforceable cgroup.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    pub fn activate(
        &mut self,
        lease: LeaseId,
        command: &[String],
        limits: &LeaseLimits,
        devices: &[crate::protocol::DeviceAccess],
    ) -> Result<(), String> {
        if self.children.contains_key(&lease) {
            return Ok(());
        }
        let [program, args @ ..] = command else {
            return Err(format!("lease {lease} has no command to execute"));
        };
        #[cfg(target_os = "linux")]
        if !devices.is_empty() && self.cgroup_root.is_none() {
            return Err(format!(
                "lease {lease} has device claims but process device enforcement requires a cgroup v2 root"
            ));
        }
        #[cfg(not(target_os = "linux"))]
        if !devices.is_empty() {
            return Err(format!(
                "lease {lease} has device claims but this process runtime cannot enforce device access"
            ));
        }
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

        #[cfg(target_os = "linux")]
        let mut group = match (&self.cgroup_root, limits.is_empty() && devices.is_empty()) {
            (Some(root), false) => Some(CgroupGroup::create(root, lease, limits)?),
            _ => None,
        };

        #[cfg(target_os = "linux")]
        if group.is_some() {
            // Device claims must hold inside the cgroup before the workload
            // can start; an unattachable filter fails activation loudly.
            if !devices.is_empty() {
                let created = group.as_ref().expect("group checked above");
                if let Err(err) = crate::device_filter::enforce_devices(created.path(), devices) {
                    if let Some(created) = group.take() {
                        let _ = created.kill();
                        let _ = created.destroy();
                    }
                    return Err(format!("device enforcement for lease {lease}: {err}"));
                }
            }
            let child = {
                let created = group.as_ref().expect("group checked above");
                linux_process::spawn(created, program, args, &log_file)
            };
            match child {
                Ok(child) => {
                    let created = group.take().expect("group remains after launch");
                    self.groups.insert(lease, created);
                    self.children.insert(lease, TrackedChild::Cgroup(child));
                    return Ok(());
                }
                Err(err) => {
                    if let Some(created) = group.take() {
                        let _ = created.kill();
                        let _ = created.destroy();
                    }
                    return Err(format!("spawn {program}: {err}"));
                }
            }
        }

        let mut child_command = Command::new(program);
        child_command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log_file.try_clone().map_err(|err| err.to_string())?,
            ))
            .stderr(Stdio::from(log_file));
        #[cfg(target_os = "linux")]
        {
            let parent_pid = unsafe { libc::getpid() };
            // SAFETY: `pre_exec` runs in the freshly forked child before any
            // user code or allocator state is touched.
            unsafe {
                child_command.pre_exec(move || {
                    if libc::getppid() != parent_pid {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "agent exited before workload setup",
                        ));
                    }
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = child_command
            .spawn()
            .map_err(|err| format!("spawn {program}: {err}"))?;
        self.children.insert(lease, TrackedChild::Standard(child));
        Ok(())
    }

    /// Kill the lease's process if it is still running. On Linux with a
    /// cgroup, the whole group dies atomically and the group is removed.
    pub fn terminate(&mut self, lease: LeaseId) -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        if let Some(group) = self.groups.get(&lease) {
            // Kill before dropping the handle: an error keeps the group
            // tracked for a later retry.
            group.kill()?;
        }
        #[cfg(target_os = "linux")]
        if self.groups.contains_key(&lease) {
            let killed = self.reap(lease)?;
            // Destroy may race a still-exiting process; tolerate failure.
            if let Some(group) = self.groups.remove(&lease) {
                let _ = group.destroy();
            }
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

#[cfg(target_os = "linux")]
fn probe_process_limit(root: &str, limits: &LeaseLimits) -> bool {
    let Ok(group) = CgroupGroup::create_probe(root, limits) else {
        return false;
    };
    let proof = linux_process::probe(&group);
    let cleanup = group.destroy();
    proof.is_ok() && cleanup.is_ok()
}

#[cfg(target_os = "linux")]
fn probe_device_isolation(root: &str) -> bool {
    let Ok(group) = CgroupGroup::create_probe(root, &LeaseLimits::default()) else {
        return false;
    };
    let filter = crate::device_filter::probe(group.path());
    let process = filter
        .as_ref()
        .map(|_| linux_process::probe(&group))
        .unwrap_or_else(|_| Ok(()));
    let cleanup = group.destroy();
    filter.is_ok() && process.is_ok() && cleanup.is_ok()
}

/// The probe's own affinity and memory policy, as sysfs lists: what the
/// kernel currently lets this process run on. Probing placement with
/// these values can only fail when the cpuset controller is unusable,
/// never because the probe named a CPU the host does not have.
#[cfg(target_os = "linux")]
fn host_affinity() -> (CpuList, CpuList) {
    (sched_affinity_list(), numa_mems_list())
}

#[cfg(target_os = "linux")]
fn sched_affinity_list() -> CpuList {
    let set = unsafe {
        let mut set = std::mem::zeroed::<libc::cpu_set_t>();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return CpuList::default();
        }
        set
    };
    let mut numbers = Vec::new();
    for number in 0..libc::CPU_SETSIZE as u32 {
        if unsafe { libc::CPU_ISSET(number as usize, &set) } {
            numbers.push(number);
        }
    }
    CpuList { numbers }
}

#[cfg(target_os = "linux")]
fn numa_mems_list() -> CpuList {
    // The process's current NUMA memory nodes. get_mempolicy is the
    // authoritative query; fall back to every node the kernel exposes.
    let mut mask = 0usize;
    unsafe {
        let mut mode = 0i32;
        if libc::syscall(
            libc::SYS_get_mempolicy,
            &mut mode as *mut i32,
            &mut mask as *mut usize,
            std::mem::size_of::<usize>() * 8,
            0 as *mut libc::c_void,
            2u64, // MPOL_F_MEMS_ALLOWED
        ) == 0
        {
            let mut numbers = Vec::new();
            for node in 0..(std::mem::size_of::<usize>() * 8) as u32 {
                if mask & (1 << node) != 0 {
                    numbers.push(node);
                }
            }
            return CpuList { numbers };
        }
    }
    // Fallback: all online NUMA nodes.
    let numbers = std::fs::read_to_string("/sys/devices/system/node/online")
        .ok()
        .and_then(|list| crate::discover::parse_cpu_list(list.trim()).ok())
        .unwrap_or_default();
    CpuList { numbers }
}

#[cfg(target_os = "linux")]
fn probe_placement(root: &str, cpus: CpuList, mems: CpuList) -> bool {
    if cpus.is_empty() || mems.is_empty() {
        return false;
    }
    let limits = LeaseLimits {
        placement: CpuPlacement { cpus, mems },
        ..Default::default()
    };
    let Ok(group) = CgroupGroup::create_probe(root, &limits) else {
        return false;
    };
    let cleanup = group.destroy();
    cleanup.is_ok()
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

impl Drop for ProcessRuntime {
    fn drop(&mut self) {
        // A dropped agent must not leak a workload while the controller
        // reconciles the lease against a new agent generation.
        #[cfg(target_os = "linux")]
        let leases = {
            let mut leases: BTreeSet<LeaseId> = self.children.keys().copied().collect();
            leases.extend(self.groups.keys().copied());
            leases
        };
        #[cfg(not(target_os = "linux"))]
        let leases: BTreeSet<LeaseId> = self.children.keys().copied().collect();
        for lease in leases {
            let _ = self.terminate(lease);
        }
    }
}
