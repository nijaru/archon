from pathlib import Path

path = Path("crates/node/tests/exec.rs")
text = path.read_text()
text = text.replace(
    "use archon_node::protocol::LeaseLimits;\nuse archon_node::runtime::ProcessRuntime;\nuse archon_node::service::NodeService;\n",
    "use archon_node::agent::LeaseAgent;\nuse archon_node::protocol::{\n    AgentRequest, AgentResponse, ExecutionCapabilities, LeaseLimits, RuntimeCapabilities,\n};\nuse archon_node::runtime::ProcessRuntime;\nuse archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};\n",
    1,
)
anchor = '''fn boot() -> NodeService {
'''
helper = r'''/// Hosted CI has no delegated cgroup subtree, but this suite is about the
/// higher-level process lifecycle rather than proving kernel CPU isolation.
/// Advertise that guarantee through an explicit test double while forwarding
/// every lifecycle operation to the real ProcessRuntime. `cgroup_linux.rs`
/// remains the proof that production CPU limits are actually enforced.
struct LifecycleProcessExecutor {
    inner: LocalExecutor,
}

impl LeaseExecutor for LifecycleProcessExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        if matches!(request, AgentRequest::Capabilities) {
            return Ok(AgentResponse::Capabilities {
                capabilities: ExecutionCapabilities {
                    process: RuntimeCapabilities {
                        available: true,
                        cpu_limit: true,
                        memory_limit: false,
                        device_isolation: false,
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

'''
if anchor not in text:
    raise SystemExit("boot anchor not found")
text = text.replace(anchor, helper + anchor, 1)
old_boot = '''    let mut service = NodeService::new();
    service
        .register_local(None)
        .expect("register local machine");
    service
'''
new_boot = '''    let description = archon_node::discover::try_describe().expect("local discovery");
    let mut service = NodeService::new();
    service
        .register_agent(
            description,
            Box::new(LifecycleProcessExecutor {
                inner: LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())),
            }),
        )
        .expect("register lifecycle test machine");
    service
'''
if old_boot not in text:
    raise SystemExit("boot registration not found")
path.write_text(text.replace(old_boot, new_boot, 1))
