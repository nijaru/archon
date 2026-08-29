from pathlib import Path

path = Path("crates/node/tests/multi.rs")
text = path.read_text()
text = text.replace(
    "use archon_node::protocol::{read_request, write_response};\n",
    "use archon_node::protocol::{\n    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_request,\n    write_response,\n};\n",
    1,
)
anchor = '''/// A named in-process agent with a stable instance id: serves one\n'''
helper = r'''fn multi_capabilities() -> ExecutionCapabilities {
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
    raise SystemExit("multi agent anchor not found")
text = text.replace(anchor, helper + anchor, 1)
text = text.replace(
    "            use archon_node::protocol::{AgentRequest, AgentResponse};\n",
    "",
    1,
)
old_loop = '''            let mut lease_agent = LeaseAgent::new(ProcessRuntime::new());
            while let Ok(request) = read_request(&mut stream) {
                let response = lease_agent.handle(request);
                if write_response(&mut stream, &response).is_err() {
                    break;
                }
            }
'''
new_loop = '''            let mut lease_agent = LeaseAgent::new(ProcessRuntime::new());
            while let Ok(request) = read_request(&mut stream) {
                let response = if matches!(request, AgentRequest::Capabilities) {
                    AgentResponse::Capabilities {
                        capabilities: multi_capabilities(),
                    }
                } else {
                    lease_agent.handle(request)
                };
                if write_response(&mut stream, &response).is_err() {
                    break;
                }
            }
'''
if old_loop not in text:
    raise SystemExit("multi remote agent loop not found")
text = text.replace(old_loop, new_loop, 1)
path.write_text(text)
