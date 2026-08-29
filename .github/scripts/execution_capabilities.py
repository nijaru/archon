from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"expected snippet not found in {path}: {old[:160]!r}")
    p.write_text(text.replace(old, new, count))

# Protocol: execution capabilities are read-only agent facts, deliberately
# separate from MachineDescription and therefore from the resource Graph.
replace(
    "crates/node/src/protocol.rs",
    '''impl DeviceAccess {
    /// Build from a device node's graph attributes.
''',
    '''/// What one execution adapter can make true for a workload. These are
/// runtime facts, not schedulable resource facts: the controller uses them to
/// reject placements that an agent cannot enforce, but never writes them into
/// the resource Graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    pub available: bool,
    pub cpu_limit: bool,
    pub memory_limit: bool,
    pub device_isolation: bool,
    pub physical_cpu_placement: bool,
    pub numa_memory_placement: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCapabilities {
    pub process: RuntimeCapabilities,
    pub container: RuntimeCapabilities,
}

impl DeviceAccess {
    /// Build from a device node's graph attributes.
'''
)
replace(
    "crates/node/src/protocol.rs",
    '''    /// Ask the agent to describe its machine; the controller builds the
    /// graph from the answer.
    Hello,
''',
    '''    /// Ask the agent to describe its machine; the controller builds the
    /// graph from the answer.
    Hello,
    /// Ask which execution guarantees this agent can actually enforce. This
    /// is queried during registration before any Graph or Lease mutation.
    Capabilities,
'''
)
replace(
    "crates/node/src/protocol.rs",
    '''pub enum AgentResponse {
    Welcome {
''',
    '''pub enum AgentResponse {
    Capabilities {
        capabilities: ExecutionCapabilities,
    },
    Welcome {
'''
)

# Process adapter: cgroup-backed limits/device BPF are enforceable only when a
# Linux cgroup root was configured. Physical cpuset/NUMA placement is not yet
# implemented and remains false even with a cgroup root.
replace(
    "crates/node/src/runtime.rs",
    '''    #[cfg(target_os = "linux")]
    pub fn with_cgroup_root(mut self, root: String) -> Self {
        self.cgroup_root = Some(root);
        self
    }
''',
    '''    #[cfg(target_os = "linux")]
    pub fn with_cgroup_root(mut self, root: String) -> Self {
        self.cgroup_root = Some(root);
        self
    }

    pub fn capabilities(&self) -> crate::protocol::RuntimeCapabilities {
        #[cfg(target_os = "linux")]
        {
            let cgroup = self.cgroup_root.is_some();
            return crate::protocol::RuntimeCapabilities {
                available: true,
                cpu_limit: cgroup,
                memory_limit: cgroup,
                device_isolation: cgroup,
                physical_cpu_placement: false,
                numa_memory_placement: false,
            };
        }
        #[cfg(not(target_os = "linux"))]
        {
            crate::protocol::RuntimeCapabilities {
                available: true,
                ..Default::default()
            }
        }
    }
'''
)

# Container adapter: advertise CPU/memory/device enforcement only for a
# recognized Docker/Podman engine that successfully answers --version.
replace(
    "crates/node/src/container.rs",
    '''    /// Per-runtime prefix so concurrent agents never fight over names.
    namespace: String,
''',
    '''    /// Per-runtime prefix so concurrent agents never fight over names.
    namespace: String,
    /// A recognized Docker/Podman CLI answered --version successfully.
    available: bool,
'''
)
old = '''        let relabels = Command::new(&engine)
            .arg("--version")
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .to_lowercase()
                    .contains("podman")
            })
            .unwrap_or(false);
'''
new = '''        let version = Command::new(&engine).arg("--version").output().ok();
        let version_text = version
            .as_ref()
            .map(|output| {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                text.to_ascii_lowercase()
            })
            .unwrap_or_default();
        let recognized = version_text.contains("docker") || version_text.contains("podman");
        let available = version
            .as_ref()
            .is_some_and(|output| output.status.success())
            && recognized;
        let relabels = available && version_text.contains("podman");
'''
replace("crates/node/src/container.rs", old, new)
replace(
    "crates/node/src/container.rs",
    '''        Self {
            engine,
            namespace: format!("{}-{}-{seq}", std::process::id(), lease_namespace_salt()),
            containers: BTreeMap::new(),
''',
    '''        Self {
            engine,
            namespace: format!("{}-{}-{seq}", std::process::id(), lease_namespace_salt()),
            available,
            containers: BTreeMap::new(),
'''
)
replace(
    "crates/node/src/container.rs",
    '''    fn name(&self, lease: LeaseId) -> String {
        format!("archon-{}-lease-{}", self.namespace, lease.as_u64())
    }
''',
    '''    pub fn capabilities(&self) -> crate::protocol::RuntimeCapabilities {
        crate::protocol::RuntimeCapabilities {
            available: self.available,
            cpu_limit: self.available,
            memory_limit: self.available,
            device_isolation: self.available,
            physical_cpu_placement: false,
            numa_memory_placement: false,
        }
    }

    fn name(&self, lease: LeaseId) -> String {
        format!("archon-{}-lease-{}", self.namespace, lease.as_u64())
    }
'''
)

# Agent answers the capability query from its concrete adapters.
replace(
    "crates/node/src/agent.rs",
    '''        match request {
            AgentRequest::Hello => self.hello(),
''',
    '''        match request {
            AgentRequest::Hello => self.hello(),
            AgentRequest::Capabilities => AgentResponse::Capabilities {
                capabilities: crate::protocol::ExecutionCapabilities {
                    process: self.process.capabilities(),
                    container: self.containers.capabilities(),
                },
            },
'''
)

# Controller queries capabilities synchronously before it mutates the Graph,
# stores them only for the live registered machine, and folds them into the
# request-specific hard placement exclusions.
replace(
    "crates/node/src/service.rs",
    '''use crate::protocol::{AgentRequest, AgentResponse, LeaseLimits, read_frame, write_frame};
''',
    '''use crate::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, LeaseLimits, RuntimeCapabilities, read_frame,
    write_frame,
};
'''
)
replace(
    "crates/node/src/service.rs",
    '''    agents: BTreeMap<NodeId, AgentHandle>,
    /// Completed agent answers awaiting absorption.
''',
    '''    agents: BTreeMap<NodeId, AgentHandle>,
    /// Live execution guarantees reported by each registered agent. These are
    /// deliberately not durable Graph facts; a fresh controller/agent session
    /// must prove them again before constrained work can place there.
    execution_capabilities: BTreeMap<NodeId, ExecutionCapabilities>,
    /// Completed agent answers awaiting absorption.
'''
)
replace(
    "crates/node/src/service.rs",
    '''            agents,
            inbox: Arc::new(Inbox::default()),
''',
    '''            agents,
            execution_capabilities: BTreeMap::new(),
            inbox: Arc::new(Inbox::default()),
'''
)
replace(
    "crates/node/src/service.rs",
    '''    pub fn register_agent(
        &mut self,
        description: crate::discover::MachineDescription,
        executor: Box<dyn LeaseExecutor>,
    ) -> Result<NodeId, Error> {
        crate::discover::validate_machine_description(&description)
''',
    '''    pub fn register_agent(
        &mut self,
        description: crate::discover::MachineDescription,
        mut executor: Box<dyn LeaseExecutor>,
    ) -> Result<NodeId, Error> {
        let capabilities = match executor
            .execute(AgentRequest::Capabilities)
            .map_err(|reason| Error::Refused {
                explanation: format!("agent capability query failed: {reason}"),
            })? {
            AgentResponse::Capabilities { capabilities } => capabilities,
            other => {
                return Err(Error::Refused {
                    explanation: format!("agent did not report execution capabilities: {other:?}"),
                });
            }
        };
        crate::discover::validate_machine_description(&description)
'''
)
replace(
    "crates/node/src/service.rs",
    '''        self.agents
            .insert(machine, spawn_worker(executor, self.inbox.clone()));
''',
    '''        self.execution_capabilities.insert(machine, capabilities);
        self.agents
            .insert(machine, spawn_worker(executor, self.inbox.clone()));
'''
)
old = '''    /// Hard placement exclusions imposed by the current execution adapters.
    /// Normalized CPU and Memory Nodes carry provider-authored physical
    /// locality (`archon.host-id`), while today's process/container adapters
    /// enforce only aggregate CPU/memory limits. Until cpuset/NUMA translation
    /// lands, executable work must not claim those physical placements.
    fn execution_exclusions(&self) -> archon_kernel::RequestExclusions {
        let mut exclusions = archon_kernel::RequestExclusions::new();
        let machines = self.cluster.graph.nodes_of_class(ResourceClass::Machine);
        for queued in &self.queue {
            let request = &queued.request;
            if request.command.is_empty() && request.image.is_none() {
                continue;
            }
            let exact_kinds: BTreeSet<ResourceClass> = request
                .needs
                .iter()
                .filter_map(|need| match need.kind {
                    ResourceClass::Cpu | ResourceClass::Memory => Some(need.kind),
                    _ => None,
                })
                .collect();
            if exact_kinds.is_empty() {
                continue;
            }
            for machine in machines {
                let has_normalized_claim_kind = self
                    .cluster
                    .graph
                    .descendants(*machine)
                    .into_iter()
                    .filter_map(|node| self.cluster.graph.node(node))
                    .any(|node| {
                        exact_kinds.contains(&node.kind)
                            && node.attrs.contains_key(crate::discover::HOST_ID_ATTR)
                    });
                if has_normalized_claim_kind {
                    exclusions.entry(request.id).or_default().insert(*machine);
                }
            }
        }
        exclusions
    }
'''
new = '''    /// Hard placement exclusions imposed by each live execution adapter.
    /// Runtime guarantees are session-local controller state, not Graph facts:
    /// placement may use them to refuse a machine, but they never grant
    /// resource authority or rewrite provider topology/capacity.
    fn execution_exclusions(&self) -> archon_kernel::RequestExclusions {
        let mut exclusions = archon_kernel::RequestExclusions::new();
        let machines = self.cluster.graph.nodes_of_class(ResourceClass::Machine);
        for queued in &self.queue {
            let request = &queued.request;
            if request.command.is_empty() && request.image.is_none() {
                continue;
            }
            let needs_cpu = request
                .needs
                .iter()
                .any(|need| need.kind == ResourceClass::Cpu);
            let needs_memory = request
                .needs
                .iter()
                .any(|need| need.kind == ResourceClass::Memory);
            let needs_devices = request.needs.iter().any(|need| {
                matches!(
                    need.kind,
                    ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
                )
            });
            let container = request.image.as_deref().is_some_and(|image| !image.is_empty());

            for machine in machines {
                let mode = self
                    .execution_capabilities
                    .get(machine)
                    .map(|caps| if container { caps.container } else { caps.process })
                    .unwrap_or_else(RuntimeCapabilities::default);
                let descendants = self.cluster.graph.descendants(*machine);
                let normalized_cpu = needs_cpu
                    && descendants.iter().any(|node| {
                        self.cluster.graph.node(*node).is_some_and(|node| {
                            node.kind == ResourceClass::Cpu
                                && node.attrs.contains_key(crate::discover::HOST_ID_ATTR)
                        })
                    });
                let normalized_memory = needs_memory
                    && descendants.iter().any(|node| {
                        self.cluster.graph.node(*node).is_some_and(|node| {
                            node.kind == ResourceClass::Memory
                                && node.attrs.contains_key(crate::discover::HOST_ID_ATTR)
                        })
                    });
                let incompatible = !mode.available
                    || (needs_cpu && !mode.cpu_limit)
                    || (needs_memory && !mode.memory_limit)
                    || (needs_devices && !mode.device_isolation)
                    || (normalized_cpu && !mode.physical_cpu_placement)
                    || (normalized_memory && !mode.numa_memory_placement);
                if incompatible {
                    exclusions.entry(request.id).or_default().insert(*machine);
                }
            }
        }
        exclusions
    }
'''
replace("crates/node/src/service.rs", old, new)

# Focused regressions for the capability seam and fail-closed admission.
Path("crates/node/tests/execution_capabilities.rs").write_text(r'''use archon_kernel::{
    CapacityDimension, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::MachineDescription;
use archon_node::protocol::{AgentRequest, AgentResponse};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};

fn machine(instance: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: instance.into(),
        cpus: 1,
        memory_bytes: 1 << 30,
        host_nodes: Vec::new(),
        devices: Vec::new(),
    }
}

fn cpu_request(id: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: Vec::new(),
        }],
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        command: vec!["true".into()],
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime: 60,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
    }
}

#[test]
fn lifecycle_only_process_reports_no_resource_enforcement() {
    let mut agent = LeaseAgent::new(ProcessRuntime::new());
    let AgentResponse::Capabilities { capabilities } = agent.handle(AgentRequest::Capabilities)
    else {
        panic!("capability response");
    };
    assert!(capabilities.process.available);
    assert!(!capabilities.process.cpu_limit);
    assert!(!capabilities.process.memory_limit);
    assert!(!capabilities.process.device_isolation);
    assert!(!capabilities.process.physical_cpu_placement);
    assert!(!capabilities.process.numa_memory_placement);
}

#[test]
fn lifecycle_only_process_is_excluded_before_cpu_lease_authority() {
    let mut service = NodeService::new();
    service
        .register_agent(
            machine("lifecycle-only"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("registration");
    service.submit(cpu_request(1), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), None);
    assert!(service.cluster.leases.is_empty());
}

struct LegacyExecutor;

impl LeaseExecutor for LegacyExecutor {
    fn execute(&mut self, _request: AgentRequest) -> Result<AgentResponse, String> {
        Ok(AgentResponse::Welcome {
            instance_id: "legacy".into(),
            name: "legacy".into(),
            cpus: 1,
            memory_bytes: 1 << 30,
            host_nodes: Vec::new(),
            devices: Vec::new(),
        })
    }
}

#[test]
fn registration_refuses_an_agent_that_cannot_prove_capabilities() {
    let mut service = NodeService::new();
    let error = service
        .register_agent(machine("legacy"), Box::new(LegacyExecutor))
        .expect_err("legacy capability ambiguity must fail closed");
    assert!(error.to_string().contains("did not report execution capabilities"));
    assert!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Machine)
            .is_empty(),
        "capability proof happens before Graph mutation"
    );
}
''')
