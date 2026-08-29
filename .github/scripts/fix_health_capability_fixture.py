from pathlib import Path

path = Path("crates/node/tests/health_ops.rs")
text = path.read_text()
text = text.replace(
    "use archon_node::protocol::{read_request, write_response};\n",
    "use archon_node::protocol::{\n    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_request,\n    write_response,\n};\n",
    1,
)
text = text.replace(
    "use archon_node::service::NodeService;\n",
    "use archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};\n",
    1,
)

anchor = '''/// A keep-alive agent that dies when the returned guard drops.
'''
helper = r'''fn lifecycle_capabilities() -> ExecutionCapabilities {
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

struct HealthProcessExecutor {
    inner: LocalExecutor,
}

impl LeaseExecutor for HealthProcessExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: lifecycle_capabilities(),
            });
        }
        self.inner.execute(request)
    }
}

fn register_local_health_agent(service: &mut NodeService) {
    let description = archon_node::discover::try_describe().expect("local discovery");
    service
        .register_agent(
            description,
            Box::new(HealthProcessExecutor {
                inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
            }),
        )
        .expect("register health test machine");
}

'''
if anchor not in text:
    raise SystemExit("health agent anchor not found")
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
                        capabilities: lifecycle_capabilities(),
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
    raise SystemExit("remote health agent loop not found")
text = text.replace(old_loop, new_loop, 1)

old_local = '''    service
        .register_local(None)
        .expect("register local machine");
'''
count = text.count(old_local)
if count != 2:
    raise SystemExit(f"expected two local health registrations, found {count}")
text = text.replace(old_local, "    register_local_health_agent(&mut service);\n")
path.write_text(text)
