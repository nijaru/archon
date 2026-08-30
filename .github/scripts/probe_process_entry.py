from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


cgroup = Path("crates/node/src/cgroup.rs")
text = cgroup.read_text()
text = replace_once(
    text,
    '''    /// Prove that this cgroup root can create one temporary group and apply
    /// the requested limits. Capability advertisement uses this before any
    /// Lease authority is admitted.
    pub(crate) fn probe_limits(root: &str, limits: &LeaseLimits) -> Result<(), String> {
        Self::create_probe(root, limits)?.destroy()
    }

''',
    "",
    "remove filesystem-only capability probe",
)
cgroup.write_text(text)

linux_process = Path("crates/node/src/linux_process.rs")
text = linux_process.read_text()
anchor = '''fn redirect_fd_or_exit(source: RawFd, target: RawFd, error_fd: RawFd) {'''
probe = r'''/// Prove the exact process-entry primitive used by lease activation. The
/// caller owns a temporary cgroup and removes it after this child exits.
pub(crate) fn probe(group: &CgroupGroup) -> Result<(), String> {
    let log = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .map_err(|err| format!("open /dev/null for process probe: {err}"))?;
    let mut child = spawn(group, "/bin/true", &[], &log)
        .map_err(|err| format!("process cgroup probe: {err}"))?;
    let status = child
        .wait()
        .map_err(|err| format!("wait for process cgroup probe: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("process cgroup probe exited with {status}"))
    }
}

'''
if anchor not in text:
    raise SystemExit("linux process probe anchor changed")
text = text.replace(anchor, probe + anchor, 1)
linux_process.write_text(text)

runtime = Path("crates/node/src/runtime.rs")
text = runtime.read_text()
text = replace_once(
    text,
    '''            let cpu_limit = CgroupGroup::probe_limits(
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
''',
    '''            let cpu_limit = probe_process_limit(
                root,
                &LeaseLimits {
                    cpu_count: 1,
                    memory_bytes: 0,
                },
            );
            let memory_limit = probe_process_limit(
                root,
                &LeaseLimits {
                    cpu_count: 0,
                    memory_bytes: 1 << 20,
                },
            );
''',
    "runtime process-entry capability probes",
)
anchor = '''#[cfg(target_os = "linux")]
fn probe_device_isolation(root: &str) -> bool {'''
helper = r'''#[cfg(target_os = "linux")]
fn probe_process_limit(root: &str, limits: &LeaseLimits) -> bool {
    let Ok(group) = CgroupGroup::create_probe(root, limits) else {
        return false;
    };
    let proof = linux_process::probe(&group);
    let cleanup = group.destroy();
    proof.is_ok() && cleanup.is_ok()
}

'''
if anchor not in text:
    raise SystemExit("runtime device probe anchor changed")
text = text.replace(anchor, helper + anchor, 1)
text = replace_once(
    text,
    '''    let proof = crate::device_filter::probe(group.path());
    let cleanup = group.destroy();
    proof.is_ok() && cleanup.is_ok()
''',
    '''    let filter = crate::device_filter::probe(group.path());
    let process = filter
        .as_ref()
        .map(|_| linux_process::probe(&group))
        .unwrap_or_else(|_| Ok(()));
    let cleanup = group.destroy();
    filter.is_ok() && process.is_ok() && cleanup.is_ok()
''',
    "device capability process-entry proof",
)
runtime.write_text(text)
