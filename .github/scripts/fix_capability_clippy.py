from pathlib import Path

runtime = Path("crates/node/src/runtime.rs")
text = runtime.read_text()
old = '''            return crate::protocol::RuntimeCapabilities {
                available: true,
                cpu_limit: cgroup,
                memory_limit: cgroup,
                device_isolation: cgroup,
                physical_cpu_placement: false,
                numa_memory_placement: false,
            };
'''
new = '''            crate::protocol::RuntimeCapabilities {
                available: true,
                cpu_limit: cgroup,
                memory_limit: cgroup,
                device_isolation: cgroup,
                physical_cpu_placement: false,
                numa_memory_placement: false,
            }
'''
if old not in text:
    raise SystemExit("runtime capability return not found")
runtime.write_text(text.replace(old, new, 1))

service = Path("crates/node/src/service.rs")
text = service.read_text()
old = ".unwrap_or_else(RuntimeCapabilities::default);"
new = ".unwrap_or_default();"
if old not in text:
    raise SystemExit("capability default expression not found")
service.write_text(text.replace(old, new, 1))
