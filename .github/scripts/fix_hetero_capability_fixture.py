from pathlib import Path

path = Path("crates/node/tests/hetero.rs")
text = path.read_text()
text = text.replace(
    "use archon_node::protocol::{read_request, write_response};\n",
    "use archon_node::protocol::{\n    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_request,\n    write_response,\n};\n",
    1,
)
text = text.replace(
    "use archon_node::service::LocalExecutor;\nuse archon_node::service::NodeService;\n",
    "use archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};\n",
    1,
)
anchor = '''/// An agent reporting a specific machine shape.\n'''
helper = r'''fn hetero_capabilities(device_isolation: bool) -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: false,
            device_isolation,
            physical_cpu_placement: false,
            numa_memory_placement: false,
        },
        container: RuntimeCapabilities::default(),
    }
}

struct HeteroExecutor {
    inner: LocalExecutor,
    device_isolation: bool,
}

impl LeaseExecutor for HeteroExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: hetero_capabilities(self.device_isolation),
            });
        }
        self.inner.execute(request)
    }
}

'''
if anchor not in text:
    raise SystemExit("hetero agent anchor not found")
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
                        capabilities: hetero_capabilities(false),
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
    raise SystemExit("hetero remote agent loop not found")
text = text.replace(old_loop, new_loop, 1)
old_local = "Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())))"
if old_local not in text:
    raise SystemExit("hetero local executor fixture not found")
text = text.replace(
    old_local,
    '''Box::new(HeteroExecutor {
                inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
                device_isolation: true,
            })''',
    1,
)
path.write_text(text)
