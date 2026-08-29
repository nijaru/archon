from pathlib import Path

path = Path("crates/node/tests/devices.rs")
text = path.read_text()
text = text.replace(
    "use archon_node::runtime::ProcessRuntime;\nuse archon_node::service::{LocalExecutor, NodeService};\n",
    "use archon_node::protocol::{\n    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities,\n};\nuse archon_node::runtime::ProcessRuntime;\nuse archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};\n",
    1,
)
anchor = '''fn description(instance: &str, gpu_dev: &str) -> MachineDescription {
'''
helper = r'''struct DeviceClaimExecutor {
    inner: LocalExecutor,
}

impl LeaseExecutor for DeviceClaimExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: ExecutionCapabilities {
                    process: RuntimeCapabilities {
                        available: true,
                        cpu_limit: false,
                        memory_limit: false,
                        device_isolation: true,
                        physical_cpu_placement: false,
                        numa_memory_placement: false,
                    },
                    container: RuntimeCapabilities::default(),
                },
            });
        }
        self.inner.execute(request)
    }
}

fn executor() -> Box<dyn LeaseExecutor> {
    Box::new(DeviceClaimExecutor {
        inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
    })
}

'''
if anchor not in text:
    raise SystemExit("device description anchor not found")
text = text.replace(anchor, helper + anchor, 1)
old = "Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())))"
count = text.count(old)
if count == 0:
    raise SystemExit("device LocalExecutor fixtures not found")
text = text.replace(old, "executor()")
path.write_text(text)
