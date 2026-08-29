from pathlib import Path

path = Path("crates/node/tests/execution_placement.rs")
text = path.read_text()
text = text.replace(
    "use archon_node::runtime::ProcessRuntime;\nuse archon_node::service::{LocalExecutor, NodeService};\n",
    "use archon_node::agent::LeaseAgent;\nuse archon_node::protocol::{\n    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities,\n};\nuse archon_node::runtime::ProcessRuntime;\nuse archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};\n",
    1,
)
# The file already imported LeaseAgent before discover; remove that original import.
text = text.replace("use archon_node::agent::LeaseAgent;\nuse archon_node::discover", "use archon_node::discover", 1)
old = '''fn executor() -> Box<LocalExecutor> {
    Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())))
}
'''
new = '''struct AggregateExecutor {
    inner: LocalExecutor,
}

impl LeaseExecutor for AggregateExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: ExecutionCapabilities {
                    process: RuntimeCapabilities {
                        available: true,
                        cpu_limit: true,
                        memory_limit: true,
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
    Box::new(AggregateExecutor {
        inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
    })
}
'''
if old not in text:
    raise SystemExit("executor fixture not found")
path.write_text(text.replace(old, new, 1))
