from pathlib import Path

path = Path("crates/node/src/device_filter.rs")
text = path.read_text()
old = '''            let err = bpf_syscall(BPF_PROG_LOAD, &attr).unwrap_err();
            let detail = String::from_utf8_lossy(&log);
            Err(io::Error::other(format!(
                "bpf(BPF_PROG_LOAD, cgroup_device): {err}: {}",
                detail.trim_end_matches('\\0').trim()
            )))
'''
new = '''            let err = bpf_syscall(BPF_PROG_LOAD, &attr).unwrap_err();
            let kind = err.kind();
            let detail = String::from_utf8_lossy(&log);
            Err(io::Error::new(
                kind,
                format!(
                    "bpf(BPF_PROG_LOAD, cgroup_device): {err}: {}",
                    detail.trim_end_matches('\\0').trim()
                ),
            ))
'''
if text.count(old) != 1:
    raise RuntimeError("BPF verifier error wrapper changed")
path.write_text(text.replace(old, new, 1))
