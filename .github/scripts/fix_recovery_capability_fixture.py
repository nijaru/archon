from pathlib import Path

path = Path("crates/node/tests/recovery.rs")
text = path.read_text()
text = text.replace(
    "    AgentRequest, AgentResponse, read_greeting, read_request, write_response,\n",
    "    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_greeting,\n    read_request, write_response,\n",
    1,
)
anchor = '''#[derive(Default)]\nstruct FakeState {\n'''
helper = r'''fn recovery_capabilities() -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: false,
            device_isolation: false,
            physical_cpu_placement: false,
            numa_memory_placement: false,
        },
        container: RuntimeCapabilities::default(),
    }
}

'''
if anchor not in text:
    raise SystemExit("recovery state anchor not found")
text = text.replace(anchor, helper + anchor, 1)
hello = '''                        AgentRequest::Hello => AgentResponse::Welcome {
                            instance_id: "recovery-agent".into(),
                            name: "fake".into(),
                            cpus: 8,
                            memory_bytes: 16 << 30,
                            host_nodes: Vec::new(),
                            devices: Vec::new(),
                        },
'''
replacement = hello + '''                        AgentRequest::Capabilities => AgentResponse::Capabilities {
                            capabilities: recovery_capabilities(),
                        },
'''
if hello not in text:
    raise SystemExit("recovery hello arm not found")
text = text.replace(hello, replacement, 1)
path.write_text(text)
