from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


Path("crates/node/src/cgroup.rs").write_text(r'''//! Linux cgroup v2 enforcement: a lease's claims become real kernel limits.
//! One cgroup per lease under an Archon root; the lease's CPU and memory
//! claims map to `cpu.max` and `memory.max`, the spawned process joins the
//! group, and termination kills the whole group via `cgroup.kill`.
//! Requires root or a delegated cgroup v2 subtree.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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

    /// Prove that this cgroup root can create one temporary group and apply
    /// the requested limits. Capability advertisement uses this before any
    /// Lease authority is admitted.
    pub(crate) fn probe_limits(root: &str, limits: &LeaseLimits) -> Result<(), String> {
        Self::create_probe(root, limits)?.destroy()
    }

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
''')

path = Path("crates/node/src/device_filter.rs")
text = path.read_text()
text = replace_once(
    text,
    "use std::os::unix::io::AsRawFd;",
    "use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};",
    "owned BPF fd imports",
)
text = replace_once(
    text,
    "fn load_program(prog: &[Insn]) -> io::Result<i64> {",
    "fn load_program(prog: &[Insn]) -> io::Result<OwnedFd> {",
    "load_program return type",
)
text = replace_once(
    text,
    "        Ok(fd) => Ok(fd),",
    '''        Ok(fd) => {
            // SAFETY: a successful BPF_PROG_LOAD returns one fresh owned fd.
            Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
        },''',
    "owned BPF fd construction",
)
text = replace_once(
    text,
    "fn attach_to_cgroup(prog_fd: i64, cgroup_path: &Path) -> io::Result<()> {",
    "fn attach_to_cgroup(prog_fd: &OwnedFd, cgroup_path: &Path) -> io::Result<()> {",
    "attach BPF fd signature",
)
text = replace_once(
    text,
    "    attr.write_u32(4, prog_fd as u32); // attach_bpf_fd",
    "    attr.write_u32(4, prog_fd.as_raw_fd() as u32); // attach_bpf_fd",
    "attach BPF raw fd",
)
text = replace_once(
    text,
    '''    let prog = compile(&rules);
    let prog_fd = load_program(&prog).map_err(|err| err.to_string())?;
    attach_to_cgroup(prog_fd, cgroup_path).map_err(|err| err.to_string())
}
''',
    '''    let prog = compile(&rules);
    let prog_fd = load_program(&prog).map_err(|err| err.to_string())?;
    attach_to_cgroup(&prog_fd, cgroup_path).map_err(|err| err.to_string())
}

/// Prove that this process may load and attach a cgroup-device program to an
/// empty cgroup. The temporary cgroup itself is owned by the runtime probe.
pub(crate) fn probe(cgroup_path: &Path) -> Result<(), String> {
    let prog_fd = load_program(&compile(&[])).map_err(|err| err.to_string())?;
    attach_to_cgroup(&prog_fd, cgroup_path).map_err(|err| err.to_string())
}
''',
    "device filter probe",
)
path.write_text(text)

runtime = Path("crates/node/src/runtime.rs")
text = runtime.read_text()
text = replace_once(
    text,
    '''    pub fn capabilities(&self) -> crate::protocol::RuntimeCapabilities {
        #[cfg(target_os = "linux")]
        {
            let cgroup = self.cgroup_root.is_some();
            crate::protocol::RuntimeCapabilities {
                available: true,
                cpu_limit: cgroup,
                memory_limit: cgroup,
                device_isolation: cgroup,
                physical_cpu_placement: false,
                numa_memory_placement: false,
            }
        }
''',
    '''    pub fn capabilities(&self) -> crate::protocol::RuntimeCapabilities {
        #[cfg(target_os = "linux")]
        {
            let Some(root) = self.cgroup_root.as_deref() else {
                return crate::protocol::RuntimeCapabilities {
                    available: true,
                    ..Default::default()
                };
            };
            let cpu_limit = CgroupGroup::probe_limits(
                root,
                &LeaseLimits {
                    cpu_count: 1,
                    memory_bytes: 0,
                },
            )
            .is_ok();
            let memory_limit = CgroupGroup::probe_limits(
                root,
                &LeaseLimits {
                    cpu_count: 0,
                    memory_bytes: 1 << 20,
                },
            )
            .is_ok();
            let device_isolation = probe_device_isolation(root);
            crate::protocol::RuntimeCapabilities {
                available: true,
                cpu_limit,
                memory_limit,
                device_isolation,
                physical_cpu_placement: false,
                numa_memory_placement: false,
            }
        }
''',
    "process capability proof",
)
insert = '''
#[cfg(target_os = "linux")]
fn probe_device_isolation(root: &str) -> bool {
    let Ok(group) = CgroupGroup::create_probe(root, &LeaseLimits::default()) else {
        return false;
    };
    let proof = crate::device_filter::probe(group.path());
    let cleanup = group.destroy();
    proof.is_ok() && cleanup.is_ok()
}

'''
anchor = '''/// Poll `check` every 100 ms until it returns true or `budget` elapses.
fn wait_until'''
if anchor not in text:
    raise SystemExit("runtime probe helper anchor changed")
text = text.replace(anchor, insert + anchor, 1)
runtime.write_text(text)

caps = Path("crates/node/tests/execution_capabilities.rs")
text = caps.read_text()
anchor = '''#[test]
fn lifecycle_only_process_is_excluded_before_cpu_lease_authority() {'''
addition = r'''#[cfg(target_os = "linux")]
#[test]
fn configured_but_unusable_cgroup_root_reports_no_resource_enforcement() {
    let runtime = ProcessRuntime::new().with_cgroup_root(
        "/proc/archon-capability-proof-must-not-exist".into(),
    );
    let capabilities = runtime.capabilities();
    assert!(capabilities.available);
    assert!(!capabilities.cpu_limit);
    assert!(!capabilities.memory_limit);
    assert!(!capabilities.device_isolation);
}

#[cfg(target_os = "linux")]
#[test]
fn unusable_cgroup_configuration_is_excluded_before_cpu_lease_authority() {
    let mut service = NodeService::new();
    let runtime = ProcessRuntime::new().with_cgroup_root(
        "/proc/archon-capability-authority-proof-must-not-exist".into(),
    );
    service
        .register_agent(
            machine("unusable-cgroup"),
            Box::new(LocalExecutor::new(LeaseAgent::new(runtime))),
        )
        .expect("registration");
    service.submit(cpu_request(1), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), None);
    assert!(
        service.cluster.leases.is_empty(),
        "unproven enforcement must exclude the machine before Lease authority"
    );
}

'''
if anchor not in text:
    raise SystemExit("execution capabilities test anchor changed")
text = text.replace(anchor, addition + anchor, 1)
caps.write_text(text)

cgroup_test = Path("crates/node/tests/cgroup_linux.rs")
text = cgroup_test.read_text()
anchor = '''#[test]
fn device_claims_without_cgroup_fail_closed() {'''
addition = r'''#[test]
fn configured_cgroup_capabilities_are_proven_against_the_kernel() {
    let root = root("capabilities");
    if !require_cgroup_writable("capabilities") {
        return;
    }
    cleanup_root(&root);
    let runtime = ProcessRuntime::new().with_cgroup_root(root.clone());
    let capabilities = runtime.capabilities();
    assert!(capabilities.available);
    assert!(capabilities.cpu_limit, "cpu controller probe must succeed");
    assert!(
        capabilities.memory_limit,
        "memory controller probe must succeed"
    );
    assert!(
        capabilities.device_isolation,
        "cgroup-device BPF load/attach probe must succeed"
    );
    cleanup_root(&root);
}

'''
if anchor not in text:
    raise SystemExit("cgroup capability test anchor changed")
text = text.replace(anchor, addition + anchor, 1)
cgroup_test.write_text(text)
