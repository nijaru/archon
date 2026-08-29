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
text = text.replace(old, new, 1)
old_import = "    AgentRequest, AgentResponse, ExecutionCapabilities, LeaseLimits, RuntimeCapabilities, read_frame,\n"
new_import = "    AgentRequest, AgentResponse, ExecutionCapabilities, LeaseLimits, read_frame, write_frame,\n"
if old_import not in text:
    raise SystemExit("capability import line not found")
text = text.replace(old_import, new_import, 1)
# The original import spans a second line containing write_frame; remove it
# after folding write_frame onto the first line above.
text = text.replace("    write_frame,\n", "", 1)
service.write_text(text)
