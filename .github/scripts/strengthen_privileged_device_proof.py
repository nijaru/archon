from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


path = Path("crates/node/tests/cgroup_linux.rs")
text = path.read_text()
text = replace_once(
    text,
    "use std::fs;\nuse std::time::{Duration, Instant};",
    "use std::ffi::CStr;\nuse std::fs;\nuse std::os::fd::FromRawFd;\nuse std::time::{Duration, Instant};",
    "linux test imports",
)

marker = '''fn cleanup_root(root: &str) {
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let _ = fs::remove_dir(entry.path());
        }
    }
    let _ = fs::remove_dir(root);
}
'''
helper = marker + '''
/// Create one safe, disposable character device that is not part of the
/// runtime's fixed pseudo-device allowlist. Holding the PTY master keeps its
/// slave node alive for the duration of the enforcement test.
fn pty_slave() -> (fs::File, String) {
    // SAFETY: posix_openpt returns a new owned fd or -1. grantpt/unlockpt and
    // ptsname_r operate on that fd while it remains live.
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    assert!(
        master >= 0,
        "posix_openpt: {}",
        std::io::Error::last_os_error()
    );
    if unsafe { libc::grantpt(master) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(master) };
        panic!("grantpt: {error}");
    }
    if unsafe { libc::unlockpt(master) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(master) };
        panic!("unlockpt: {error}");
    }
    let mut name = [0 as libc::c_char; 128];
    if unsafe { libc::ptsname_r(master, name.as_mut_ptr(), name.len()) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(master) };
        panic!("ptsname_r: {error}");
    }
    // SAFETY: successful ptsname_r wrote a NUL-terminated path into `name`.
    let path = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .expect("PTY path is UTF-8")
        .to_owned();
    // SAFETY: `master` is a fresh owned fd and ownership transfers to File.
    let master = unsafe { fs::File::from_raw_fd(master) };
    (master, path)
}
'''
text = replace_once(text, marker, helper, "pty helper")

start = text.index("#[test]\nfn claimed_devices_are_enforced_by_cgroup_device_filter()")
old = text[start:]
new = r'''#[test]
fn claimed_devices_are_enforced_by_cgroup_device_filter() {
    let root = root("devices");
    if !require_cgroup_writable("devices") {
        return;
    }
    cleanup_root(&root);

    let (_claimed_master, claimed) = pty_slave();
    let (_unclaimed_master, unclaimed) = pty_slave();
    assert_ne!(claimed, unclaimed);

    let mut runtime = ProcessRuntime::new().with_cgroup_root(root.clone());
    let lease = LeaseId::from_u64(2);
    let log = ProcessRuntime::log_dir().join(format!("lease-{}.log", lease.as_u64()));
    let _ = fs::remove_file(log);
    let devices = vec![archon_node::protocol::DeviceAccess {
        id: "gpu0".into(),
        dev: claimed.clone(),
        paths: Vec::new(),
        cdi: None,
    }];
    runtime
        .activate(
            lease,
            &[
                "sh".into(),
                "-c".into(),
                // The first write must succeed only because this exact PTY
                // slave is claimed. A second same-class PTY slave is not in
                // the claim and must be denied by the attached cgroup-device
                // program. Distinct exit codes make either failure explicit.
                "printf claimed-ok > \"$1\" || exit 10; if printf unclaimed-opened > \"$2\"; then echo unclaimed-opened; exit 11; fi; echo device-proof-ok"
                    .into(),
                "archon-device-proof".into(),
                claimed,
                unclaimed,
            ],
            &LeaseLimits::default(),
            &devices,
        )
        .expect("device-only activation with device filter");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exit_code = None;
    while Instant::now() < deadline {
        match runtime.status(lease) {
            WorkStatus::Exited(code) => {
                exit_code = Some(code);
                break;
            }
            WorkStatus::Running => std::thread::sleep(Duration::from_millis(20)),
            WorkStatus::Gone => break,
        }
    }
    let output = ProcessRuntime::read_log(lease);
    assert_eq!(
        exit_code,
        Some(0),
        "claimed device must open while the unclaimed peer is denied; log: {output}"
    );
    assert!(
        output.contains("device-proof-ok"),
        "kernel device proof did not complete: {output}"
    );
    assert!(
        !output.contains("unclaimed-opened"),
        "unclaimed device unexpectedly opened: {output}"
    );

    runtime.terminate(lease).unwrap();
    cleanup_root(&root);
}
'''
text = text[:start] + new
path.write_text(text)

ci = Path(".github/workflows/ci.yml")
ci_text = ci.read_text()
addition = r'''

  privileged-enforcement:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build native cgroup enforcement test
        run: cargo test -p archon-node --test cgroup_linux --no-run
      - name: Prove native cgroup and device enforcement
        run: |
          test_bin="$(find target/debug/deps -maxdepth 1 -type f -perm -111 -name 'cgroup_linux-*' | head -n 1)"
          test -n "$test_bin"
          sudo env ARCHON_LOG_DIR="$RUNNER_TEMP/archon-privileged-logs" "$test_bin" --nocapture
'''
if "privileged-enforcement:" in ci_text:
    raise SystemExit("CI already has privileged-enforcement job")
ci.write_text(ci_text.rstrip() + addition + "\n")
