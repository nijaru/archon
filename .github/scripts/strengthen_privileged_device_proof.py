from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


filter_path = Path("crates/node/src/device_filter.rs")
filter_text = filter_path.read_text()
filter_text = replace_once(
    filter_text,
    '''// bpf_cgroup_dev_ctx access bits (linux/bpf.h).
const DEVCG_ACC_READ: u32 = 1;
const DEVCG_ACC_WRITE: u32 = 2;
const DEVCG_ACC_MKNOD: u32 = 4;
// Device type lives in the upper 16 bits of ctx->access_type.
const DEVCG_DEV_BLOCK: u32 = 1;
const DEVCG_DEV_CHAR: u32 = 2;
''',
    '''// bpf_cgroup_dev_ctx access/type encoding (linux/bpf.h).
const DEVCG_ACC_MKNOD: u32 = 1;
const DEVCG_ACC_READ: u32 = 2;
const DEVCG_ACC_WRITE: u32 = 4;
// Access bits live in the upper 16 bits and device type in the lower 16.
const DEVCG_DEV_BLOCK: u32 = 1;
const DEVCG_DEV_CHAR: u32 = 2;
''',
    "cgroup device ABI constants",
)
filter_text = replace_once(
    filter_text,
    '''/// r2 = ctx->access_type; r3 = ctx->major; r4 = ctx->minor
/// for each rule:
///     if (access_type & 0xffff) & ~rule.access == 0        // request ⊆ grant
///     && (access_type >> 16) == rule.dev_type
''',
    '''/// r2 = ctx->access_type; r3 = ctx->major; r4 = ctx->minor
/// for each rule:
///     if (access_type >> 16) & ~rule.access == 0          // request ⊆ grant
///     && (access_type & 0xffff) == rule.dev_type
''',
    "cgroup device ABI documentation",
)
filter_text = replace_once(
    filter_text,
    '''        // s+0..1: r6 = requested access flags.
        prog.push(mov_reg(6, 2)); // r6 = access_type
        prog.push(and_imm(6, 0xffff));
        // s+2..3: any requested bit outside the grant means "not this rule".
        prog.push(and_imm(6, disallowed));
        prog.push(Insn {
            code: BPF_JMP32_JNE_IMM,
            dst_reg: 6,
            src_reg: 0,
            off: jump_to(s + 3, next_rule),
            imm: 0,
        });
        // s+4..6: device type from the upper half-word must match exactly.
        prog.push(mov_reg(6, 2));
        prog.push(rsh_imm(6, 16));
''',
    '''        // s+0..1: r6 = requested access flags from the upper half-word.
        prog.push(mov_reg(6, 2)); // r6 = access_type
        prog.push(rsh_imm(6, 16));
        // s+2..3: any requested bit outside the grant means "not this rule".
        prog.push(and_imm(6, disallowed));
        prog.push(Insn {
            code: BPF_JMP32_JNE_IMM,
            dst_reg: 6,
            src_reg: 0,
            off: jump_to(s + 3, next_rule),
            imm: 0,
        });
        // s+4..6: device type from the lower half-word must match exactly.
        prog.push(mov_reg(6, 2));
        prog.push(and_imm(6, 0xffff));
''',
    "compiled cgroup device ABI decoding",
)
filter_text = replace_once(
    filter_text,
    '''    fn ctx_of(flags: u32, dev_type: u32, major: u32, minor: u32) -> (u32, u32, u32) {
        ((dev_type << 16) | flags, major, minor)
    }
''',
    '''    fn ctx_of(flags: u32, dev_type: u32, major: u32, minor: u32) -> (u32, u32, u32) {
        ((flags << 16) | dev_type, major, minor)
    }

    #[test]
    fn cgroup_device_constants_match_linux_abi() {
        assert_eq!(DEVCG_ACC_MKNOD, 1);
        assert_eq!(DEVCG_ACC_READ, 2);
        assert_eq!(DEVCG_ACC_WRITE, 4);
        assert_eq!(DEVCG_DEV_BLOCK, 1);
        assert_eq!(DEVCG_DEV_CHAR, 2);
        assert_eq!(
            ctx_of(DEVCG_ACC_WRITE, DEVCG_DEV_CHAR, 195, 0).0,
            (4 << 16) | 2
        );
    }
''',
    "unit-test cgroup device ABI encoding",
)
filter_path.write_text(filter_text)

path = Path("crates/node/tests/cgroup_linux.rs")
text = path.read_text()
text = replace_once(
    text,
    "use std::fs;\nuse std::time::{Duration, Instant};",
    "use std::ffi::CStr;\nuse std::fs;\nuse std::os::fd::FromRawFd;\nuse std::time::{Duration, Instant};",
    "linux test imports",
)
text = replace_once(
    text,
    '''        Err(err) => {
            eprintln!("skipping: cannot create cgroups at {root}: {err}");
            false
        }
''',
    '''        Err(err) => {
            if std::env::var_os("ARCHON_REQUIRE_PRIVILEGED_CGROUP").is_some() {
                panic!("privileged cgroup proof required but {root} is not writable: {err}");
            }
            eprintln!("skipping: cannot create cgroups at {root}: {err}");
            false
        }
''',
    "required privileged cgroup guard",
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

    // Prove both PTY slaves are usable before the lease filter is attached;
    // otherwise a parent cgroup/device policy could masquerade as Archon's
    // deny decision and make the test environment unsuitable.
    fs::OpenOptions::new()
        .write(true)
        .open(&claimed)
        .expect("claimed PTY must be host-accessible before filtering");
    fs::OpenOptions::new()
        .write(true)
        .open(&unclaimed)
        .expect("unclaimed PTY must be host-accessible before filtering");

    let mut runtime = ProcessRuntime::new().with_cgroup_root(root.clone());
    let lease = LeaseId::from_u64(2);
    let log = ProcessRuntime::log_dir().join(format!("lease-{}.log", lease.as_u64()));
    let _ = fs::remove_file(log);
    let devices = vec![archon_node::protocol::DeviceAccess {
        id: "test-device".into(),
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
path.write_text(text[:start] + new)

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
        env:
          ARCHON_REQUIRE_PRIVILEGED_CGROUP: "1"
        run: |
          test_bin="$(find target/debug/deps -maxdepth 1 -type f -perm -111 -name 'cgroup_linux-*' | head -n 1)"
          test -n "$test_bin"
          sudo --preserve-env=ARCHON_REQUIRE_PRIVILEGED_CGROUP env ARCHON_LOG_DIR="$RUNNER_TEMP/archon-privileged-logs" "$test_bin" --nocapture
'''
if "privileged-enforcement:" in ci_text:
    raise SystemExit("CI already has privileged-enforcement job")
ci.write_text(ci_text.rstrip() + addition + "\n")
