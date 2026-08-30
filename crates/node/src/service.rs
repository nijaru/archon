//! Single-node Archon controller: the Cluster transition function plus an
//! agent link that executes leases. The link is either the in-process
//! [`LeaseAgent`] or a TCP connection to a remote `archon agent` —
//! both speak the same protocol, so decision and enforcement stay on
//! opposite sides of the seam whether the machine is local or not.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use archon_kernel::{
    Allocation, BindingId, CapacityDimension, Cluster, Command, Effect, Error,
    FactWriterAssignment, LeaseId, NodeId, OwnerId, ProviderFactBatch, ProviderId, Queued, Request,
    RequestId, ResourceClass, quantity_get,
};

type CommandSink = Box<dyn FnMut(&Command) + Send>;

use crate::agent::LeaseAgent;
use crate::dispatch::{
    AgentHandle, AgentReply, Delivery, EffectPhase, Inbox, Job, Tag, direct_job, spawn_worker,
};
use crate::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, LeaseLimits, read_frame, write_frame,
};
use crate::runtime::ProcessRuntime;

/// The controller side of the enforcement seam.
pub trait LeaseExecutor: Send {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String>;
}

/// In-process execution: the agent runs in this same process.
pub struct LocalExecutor {
    agent: LeaseAgent,
}

impl LocalExecutor {
    pub fn new(agent: LeaseAgent) -> Self {
        Self { agent }
    }
}

impl LeaseExecutor for LocalExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        Ok(self.agent.handle(request))
    }
}

/// Remote execution over TCP; one encrypted connection per agent.
pub struct RemoteExecutor {
    stream: crate::transport::SecureStream,
}

impl RemoteExecutor {
    pub fn connect(addr: &str, token: Option<&str>) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(60)))?;
        let mut stream = crate::transport::establish_initiator(stream, token)?;
        let greeting = crate::protocol::Greeting::Agent {
            instance_id: String::new(),
            name: String::new(),
            cpus: 0,
            memory_bytes: 0,
            host_nodes: Vec::new(),
            devices: Vec::new(),
        };
        write_frame(&mut stream, &greeting).map_err(std::io::Error::other)?;
        Ok(Self { stream })
    }

    /// Wrap an already-established secure stream (dial-in agents).
    pub fn from_secure(stream: crate::transport::SecureStream) -> Self {
        Self { stream }
    }
}

impl LeaseExecutor for RemoteExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        write_frame(&mut self.stream, &request).map_err(|err| err.to_string())?;
        read_frame(&mut self.stream).map_err(|err| err.to_string())
    }
}

pub struct NodeService {
    pub cluster: Cluster,
    /// Shared secret for outbound agent connections; None runs open mode.
    link_token: Option<String>,
    /// One worker handle per registered machine, keyed by the machine
    /// NodeId. Workers own the executors and do all network I/O off the
    /// controller's critical section.
    agents: BTreeMap<NodeId, AgentHandle>,
    /// Live execution guarantees reported by each registered agent. These are
    /// deliberately not durable Graph facts; a fresh controller/agent session
    /// must prove them again before constrained work can place there.
    execution_capabilities: BTreeMap<NodeId, ExecutionCapabilities>,
    /// Completed agent answers awaiting absorption.
    inbox: Arc<Inbox>,
    /// Outstanding asynchronous calls, keyed by their completion tag.
    inflight: BTreeSet<Tag>,
    /// Last observed running state per lease, from status polls.
    running: BTreeMap<LeaseId, bool>,
    /// Machines whose last probe failed, consumed by health policy.
    unreachable: Vec<NodeId>,
    queue: Vec<Queued>,
    /// Workload payload per queued request, kept outside the kernel log:
    /// resource decisions never need it, only execution does.
    commands: BTreeMap<RequestId, Vec<String>>,
    /// Command per active lease, recorded when its request is admitted.
    lease_commands: BTreeMap<LeaseId, Vec<String>>,
    /// Container image per active lease; None runs a bare process.
    lease_images: BTreeMap<LeaseId, Option<String>>,
    /// Original request per admitted lease, for keep-alive restarts.
    requests: BTreeMap<LeaseId, (Request, OwnerId)>,
    /// Dead leases whose failure has been processed for restarts.
    restart_handled: std::collections::BTreeSet<LeaseId>,
    /// Restart attempts per original request id.
    restart_counts: BTreeMap<RequestId, u32>,
    /// Cluster time of each request's last restart (backoff anchor).
    restart_last_at: BTreeMap<RequestId, u64>,
    /// Restarted request id -> original request id.
    restart_root: BTreeMap<RequestId, RequestId>,
    next_request_id: u64,
    /// Next lease id; recovered from the log on replay.
    next_lease: u64,
    pending: VecDeque<Effect>,
    next_session: u64,
    next_binding: u64,
    /// First session value this controller process may issue. Recorded
    /// sessions below this floor belong to a previous controller process:
    /// a machine returning with such a session reconciles against real
    /// endpoint state instead of blindly re-driving work.
    session_floor: u64,
    /// Machines re-registered after a controller restart whose live
    /// Bindings are still being proven against actual endpoint state.
    /// Each entry lists the Active Leases awaiting their proof query.
    recovering_machines: BTreeMap<NodeId, BTreeSet<LeaseId>>,
    /// Active Leases whose recovery Status query is outstanding.
    recovering_leases: BTreeSet<LeaseId>,
    /// Observes every command applied to the cluster; the control plane
    /// persists them here.
    command_sink: Option<CommandSink>,
    /// Every applied command since construction; tests and debugging use
    /// this, the durable log remains the control plane's.
    history: Vec<Command>,
}

/// Restorable controller-side state that lives outside the kernel log.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ServiceState {
    pub lease_commands: BTreeMap<LeaseId, Vec<String>>,
    pub lease_images: BTreeMap<LeaseId, Option<String>>,
    pub requests: BTreeMap<LeaseId, (Request, OwnerId)>,
    pub restart_handled: std::collections::BTreeSet<LeaseId>,
    pub restart_counts: BTreeMap<RequestId, u32>,
    /// Cluster time of each request's last restart, backing exponential
    /// backoff between attempts.
    pub restart_last_at: BTreeMap<RequestId, u64>,
    /// Restart lineage: each restarted request id points at the original,
    /// so caps and backoff survive id churn across generations.
    pub restart_root: BTreeMap<RequestId, RequestId>,
    pub next_lease: u64,
    pub next_request_id: u64,
    pub next_session: u64,
    pub next_binding: u64,
}

/// One registered machine's identity in the controller.
#[derive(Clone, Debug)]
pub struct MachineRegistration {
    pub machine: NodeId,
    pub name: String,
    /// False when an agent re-registered over an existing machine.
    pub new_machine: bool,
}

impl Default for NodeService {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeService {
    /// A controller with no agents yet; register machines with
    /// [`NodeService::register_local`] or [`NodeService::register_remote`].
    pub fn new() -> Self {
        Self::with_agents(BTreeMap::new())
    }

    /// Set the shared secret used to secure outbound agent links.
    pub fn set_link_token(&mut self, token: String) {
        self.link_token = Some(token);
    }

    /// A controller executing on this machine; cgroup enforcement when a
    /// root is given (Linux only). Unregistered: call `register_local`.
    pub fn local(cgroup_root: Option<String>) -> Self {
        let mut service = Self::new();
        service
            .register_local(cgroup_root)
            .expect("register local machine");
        service
    }

    /// Register this machine as an agent of the controller; cgroup
    /// enforcement when a root is given (Linux only).
    pub fn register_local(&mut self, cgroup_root: Option<String>) -> Result<NodeId, Error> {
        let description = crate::discover::try_describe().map_err(|reason| Error::Refused {
            explanation: format!("local device discovery incomplete: {reason}"),
        })?;
        #[cfg(target_os = "linux")]
        let runtime = match cgroup_root {
            Some(root) => ProcessRuntime::new().with_cgroup_root(root),
            None => ProcessRuntime::new(),
        };
        #[cfg(not(target_os = "linux"))]
        let runtime = {
            let _ = cgroup_root;
            ProcessRuntime::new()
        };
        let executor = LocalExecutor::new(LeaseAgent::new(runtime));
        self.register_agent(description, Box::new(executor))
    }

    /// Connect to a remote agent, learn its machine, and register it.
    pub fn register_remote(&mut self, addr: &str) -> Result<NodeId, Error> {
        let token = self.link_token.as_deref();
        let mut executor = RemoteExecutor::connect(addr, token).map_err(|err| Error::Refused {
            explanation: format!("connect {addr}: {err}"),
        })?;
        let description = Self::hello(&mut executor)?;
        if description.instance_id.is_empty() {
            return Err(Error::Refused {
                explanation: "remote agent returned an empty stable instance id".into(),
            });
        }
        self.register_agent(description, Box::new(executor))
    }

    pub fn hello(
        executor: &mut dyn LeaseExecutor,
    ) -> Result<crate::discover::MachineDescription, Error> {
        match executor
            .execute(AgentRequest::Hello)
            .map_err(|reason| Error::Refused {
                explanation: reason,
            })? {
            AgentResponse::Welcome {
                instance_id,
                name,
                cpus,
                memory_bytes,
                host_nodes,
                devices,
            } => Ok(crate::discover::MachineDescription {
                instance_id,
                name,
                cpus,
                memory_bytes,
                host_nodes,
                devices,
            }),
            other => Err(Error::Refused {
                explanation: format!("expected Welcome, got {other:?}"),
            }),
        }
    }

    /// Register one machine's agent: apply its graph fragment (or match an
    /// existing machine by name on re-registration), assign a fresh session,
    /// and let the kernel's Reconcile re-drive live work onto the agent.
    /// The executor moves onto a dedicated worker thread; registration and
    /// all later effects enqueue without blocking on the agent.
    pub fn query_execution_capabilities(
        executor: &mut dyn LeaseExecutor,
    ) -> Result<ExecutionCapabilities, Error> {
        match executor
            .execute(AgentRequest::Capabilities)
            .map_err(|reason| Error::Refused {
                explanation: format!("agent capability query failed: {reason}"),
            })? {
            AgentResponse::Capabilities { capabilities } => Ok(capabilities),
            other => Err(Error::Refused {
                explanation: format!("agent did not report execution capabilities: {other:?}"),
            }),
        }
    }

    pub fn register_agent(
        &mut self,
        description: crate::discover::MachineDescription,
        mut executor: Box<dyn LeaseExecutor>,
    ) -> Result<NodeId, Error> {
        let capabilities = Self::query_execution_capabilities(executor.as_mut())?;
        self.register_agent_with_capabilities(description, executor, capabilities)
    }

    /// Register an agent whose execution guarantees were already proven on
    /// its connection thread. Capability proof still precedes all Graph
    /// mutation, but a silent network peer never holds a shared controller
    /// lock while the proof waits or times out.
    pub fn register_agent_with_capabilities(
        &mut self,
        description: crate::discover::MachineDescription,
        executor: Box<dyn LeaseExecutor>,
        capabilities: ExecutionCapabilities,
    ) -> Result<NodeId, Error> {
        crate::discover::validate_machine_description(&description)
            .map_err(|explanation| Error::Refused { explanation })?;

        // Identity is the agent's instance id, stored in the machine's
        // attrs so it survives control-plane restarts via replay.
        let named = |cluster: &Cluster, instance: &str| {
            cluster
                .graph
                .nodes_of_class(ResourceClass::Machine)
                .iter()
                .copied()
                .find(|id| {
                    cluster
                        .graph
                        .node(*id)
                        .and_then(|node| node.attrs.get("agent_id"))
                        .is_some_and(|attr| attr == instance)
                })
        };
        let (machine, new_machine) = match named(&self.cluster, &description.instance_id) {
            Some(machine) => {
                if let Err(explanation) = crate::discover::verify_registered_host(
                    &self.cluster.graph,
                    machine,
                    &description,
                ) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(Error::Refused {
                        explanation: format!(
                            "returning agent host inventory changed; explicit topology reconciliation is required: {explanation}"
                        ),
                    });
                }
                if let Err(err) = self.preflight_fact_writers(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.preflight_claim_contracts(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_fact_writers(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_device_subtree(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_claim_contracts(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                (machine, false)
            }
            None => {
                let base = self
                    .cluster
                    .graph
                    .nodes()
                    .map(|node| node.id.as_u64())
                    .max()
                    .unwrap_or(0);
                let (_local, nodes, edges) = crate::discover::build_graph(&description, base);
                let claim_bindings = crate::discover::claim_bindings(&nodes);
                let batches = crate::discover::provider_fact_batches(nodes, edges);
                self.commit(Command::ApplyProviderFacts {
                    batches,
                    claim_bindings,
                })?;
                let machine =
                    named(&self.cluster, &description.instance_id).ok_or(Error::Refused {
                        explanation: "applied graph fragment but machine node is missing".into(),
                    })?;
                (machine, true)
            }
        };
        self.execution_capabilities.insert(machine, capabilities);
        self.agents
            .insert(machine, spawn_worker(executor, self.inbox.clone()));
        // A recorded session older than this controller process means the
        // machine is returning after a controller restart: its live work
        // must be adopted-or-fenced from real endpoint state, never blindly
        // re-executed. Mid-flight re-registrations (fresh agent process)
        // keep the re-drive path: the new agent holds nothing to adopt.
        let floor = self.session_floor();
        let prior_session = self.cluster.sessions.get(&machine).copied();
        let recovering = !new_machine && prior_session.is_some_and(|prior| prior < floor);
        if recovering {
            self.recovering_machines.insert(machine, BTreeSet::new());
            eprintln!(
                "archon: machine {machine} returned after restart; reconciling live bindings"
            );
        } else if !new_machine {
            // A known machine came back mid-flight: clear any quarantine
            // and health marks before reconciliation re-drives its live
            // work. Recovery defers these marks until its bindings resolve,
            // because quarantine cannot lift while open bindings exist.
            self.commit(Command::UnquarantineNode { node: machine })
                .ok();
            self.commit(Command::SetNodeHealth {
                node: machine,
                health: "healthy".into(),
            })?;
        }
        let session = self.next_session;
        self.next_session += 1;
        self.commit(Command::SetAgentSession { machine, session })?;
        self.pump()?;
        if recovering
            && self
                .recovering_machines
                .get(&machine)
                .is_some_and(BTreeSet::is_empty)
        {
            // No live bindings needed proof: restore health marks now.
            self.finish_machine_recovery(machine);
        }
        Ok(machine)
    }

    fn missing_fact_writer_assignments(
        &self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<Vec<FactWriterAssignment>, Error> {
        let expected =
            crate::discover::current_fact_writer_assignments(&self.cluster.graph, machine, devices);
        let mut missing = Vec::new();
        for assignment in expected {
            let mut nodes = Vec::new();
            let mut edges = Vec::new();
            for node in assignment.nodes {
                match self.cluster.graph.node_fact_writer(node) {
                    None => nodes.push(node),
                    Some(writer) if writer == assignment.writer => {}
                    Some(writer) => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "resource node {node} already belongs to discovery writer {writer}; returning agent expects {}",
                                assignment.writer
                            ),
                        });
                    }
                }
            }
            for edge in assignment.edges {
                match self.cluster.graph.edge_fact_writer(edge) {
                    None => edges.push(edge),
                    Some(writer) if writer == assignment.writer => {}
                    Some(writer) => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "resource edge {} -> {} {:?} already belongs to discovery writer {writer}; returning agent expects {}",
                                edge.from, edge.to, edge.kind, assignment.writer
                            ),
                        });
                    }
                }
            }
            if !nodes.is_empty() || !edges.is_empty() {
                missing.push(FactWriterAssignment {
                    writer: assignment.writer,
                    nodes,
                    edges,
                });
            }
        }
        Ok(missing)
    }

    fn preflight_fact_writers(
        &self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        self.missing_fact_writer_assignments(machine, devices)
            .map(|_| ())
    }

    fn reconcile_fact_writers(
        &mut self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        let assignments = self.missing_fact_writer_assignments(machine, devices)?;
        if !assignments.is_empty() {
            self.commit(Command::AdoptFactWriters { assignments })?;
        }
        Ok(())
    }

    fn missing_claim_contracts(
        &self,
        nodes: &[archon_kernel::Node],
    ) -> Result<Vec<archon_kernel::ClaimBindingUpdate>, Error> {
        let mut missing = Vec::new();
        for update in crate::discover::claim_bindings(nodes) {
            let expected = update
                .binding
                .expect("provider normalization emits additions");
            match self
                .cluster
                .graph
                .claim_binding(update.node, update.dimension)
            {
                None => missing.push(update),
                Some(actual) if actual == expected => {}
                Some(actual) => {
                    return Err(Error::Refused {
                        explanation: format!(
                            "resource {} dimension {} already belongs to provider {} with {:?} scope; returning agent reports provider {} with {:?} scope",
                            update.node,
                            update.dimension,
                            actual.provider,
                            actual.scope,
                            expected.provider,
                            expected.scope,
                        ),
                    });
                }
            }
        }
        Ok(missing)
    }

    fn current_claim_contract_nodes(
        &self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Vec<archon_kernel::Node> {
        let current_devices: BTreeSet<String> =
            devices.iter().map(|device| device.id.clone()).collect();
        let mut ids = self.cluster.graph.descendants(machine);
        ids.push(machine);
        ids.into_iter()
            .filter_map(|id| self.cluster.graph.node(id))
            .filter(|node| {
                !matches!(
                    node.kind,
                    ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
                ) || node
                    .attrs
                    .get("id")
                    .is_some_and(|id| current_devices.contains(id))
            })
            .cloned()
            .collect()
    }

    /// Reject conflicting current provider contracts before returning-device
    /// reconciliation can mutate facts, availability, or Lease state.
    fn preflight_claim_contracts(
        &self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        let nodes = self.current_claim_contract_nodes(machine, devices);
        self.missing_claim_contracts(&nodes).map(|_| ())
    }

    /// Reconcile proof-stage provider contracts only for resources present in
    /// the returning Agent's validated authoritative inventory. Older Graphs
    /// may have no `ClaimBinding` metadata, but a provider-omitted tombstone
    /// must never regain ownership merely because its stale Node still exists.
    fn reconcile_claim_contracts(
        &mut self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        let nodes = self.current_claim_contract_nodes(machine, devices);
        let missing = self.missing_claim_contracts(&nodes)?;
        if !missing.is_empty() {
            self.commit(Command::ApplyProviderFacts {
                batches: Vec::new(),
                claim_bindings: missing,
            })?;
        }
        Ok(())
    }

    /// A returning Agent whose authoritative inventory cannot be reconciled
    /// is not accepted as an execution endpoint. Stop new placement without
    /// weakening an existing drain/quarantine/retired state; outstanding
    /// authority remains occupied until its ordinary reconciliation closes.
    fn mark_inventory_mismatch(&mut self, machine: NodeId) -> Result<(), Error> {
        if matches!(
            self.cluster.node_state(machine),
            Some(archon_kernel::NodeState::Joining | archon_kernel::NodeState::Schedulable)
        ) {
            self.commit(Command::SetNodeState {
                node: machine,
                state: archon_kernel::NodeState::Unavailable,
            })?;
        }
        Ok(())
    }

    /// Restore a reconciled machine's health marks, deferred during
    /// recovery because quarantine cannot lift while open bindings exist.
    /// A rejected un-quarantine leaves the machine unavailable — the safe
    /// direction when some Binding could not be resolved.
    fn finish_machine_recovery(&mut self, machine: NodeId) {
        self.recovering_machines.remove(&machine);
        if let Err(err) = self.commit(Command::UnquarantineNode { node: machine }) {
            eprintln!("archon: machine {machine} stays quarantined after recovery: {err}");
        }
        if let Err(err) = self.commit(Command::SetNodeHealth {
            node: machine,
            health: "healthy".into(),
        }) {
            eprintln!("archon: failed to restore health of machine {machine}: {err}");
        }
    }

    /// Re-align a known machine's provider-owned device inventory with its
    /// current declaration. Stable ids retain their NodeId across path/fact
    /// refreshes. A previously known id omitted by the provider remains as a
    /// tombstone-like Graph node but becomes unavailable; any live lease that
    /// claims it fails and fences before the resource can be reused.
    fn reconcile_device_subtree(
        &mut self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        use archon_kernel::{
            Attrs, CapacityDimension, Edge, EdgeKind, LeaseState, Node, NodeState, qty,
        };

        let mut existing: BTreeMap<String, NodeId> = self
            .cluster
            .graph
            .descendants(machine)
            .into_iter()
            .filter_map(|child| {
                let node = self.cluster.graph.node(child)?;
                if !matches!(
                    node.kind,
                    ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
                ) {
                    return None;
                }
                node.attrs.get("id").map(|id| (id.clone(), child))
            })
            .collect();

        let mut base = self
            .cluster
            .graph
            .nodes()
            .map(|node| node.id.as_u64())
            .max()
            .unwrap_or(0);
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut returning = BTreeSet::new();
        let mut reported_existing = BTreeSet::new();

        for spec in devices {
            let requested_parent = spec
                .host_parent
                .as_deref()
                .map(|parent| {
                    crate::discover::host_parent_node(&self.cluster.graph, machine, parent)
                        .map_err(|explanation| Error::Refused { explanation })
                })
                .transpose()?;
            let id = if let Some(id) = existing.remove(&spec.id) {
                let node = self.cluster.graph.node(id).ok_or(Error::UnknownNode(id))?;
                let recorded_parent = node
                    .attrs
                    .get(crate::discover::DEVICE_HOST_PARENT_ATTR)
                    .map(String::as_str);
                match (recorded_parent, spec.host_parent.as_deref()) {
                    (Some(previous), Some(current)) if previous != current => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "device {:?} changed host containment from {previous:?} to {current:?}; explicit topology reconciliation is required",
                                spec.id
                            ),
                        });
                    }
                    (Some(previous), None) => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "device {:?} withdrew authoritative host containment {previous:?}; explicit topology reconciliation is required",
                                spec.id
                            ),
                        });
                    }
                    _ => {}
                }
                if let Some(parent) = requested_parent
                    && self.cluster.graph.parent(id) != Some(parent)
                {
                    return Err(Error::Refused {
                        explanation: format!(
                            "device {:?} changed physical containment; explicit topology reconciliation is required",
                            spec.id
                        ),
                    });
                }
                if node.kind != spec.kind {
                    return Err(Error::Refused {
                        explanation: format!(
                            "device {:?} changed resource class; replacement requires a new stable id",
                            spec.id
                        ),
                    });
                }
                reported_existing.insert(id);
                match self.cluster.node_state(id) {
                    Some(NodeState::Retired) => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "device {:?} is retired; replacement requires a new stable id",
                                spec.id
                            ),
                        });
                    }
                    Some(NodeState::Unavailable | NodeState::Joining) => {
                        returning.insert(id);
                    }
                    _ => {}
                }
                id
            } else {
                base += 1;
                let fresh = NodeId::from_u64(base);
                edges.push(Edge {
                    from: requested_parent.unwrap_or(machine),
                    to: fresh,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                });
                fresh
            };

            let mut attrs = spec.attrs.clone();
            attrs.insert("id".into(), spec.id.clone());
            attrs.insert("dev".into(), spec.dev.clone());
            if let Some(parent) = &spec.host_parent {
                attrs.insert(
                    crate::discover::DEVICE_HOST_PARENT_ATTR.into(),
                    parent.clone(),
                );
            }
            if !spec.access.is_empty() {
                attrs.insert(
                    "access".into(),
                    serde_json::to_string(&spec.access).expect("device access is serializable"),
                );
            } else {
                attrs.remove("access");
            }
            let node = self.cluster.graph.node(id);
            if node.is_some_and(|node| node.kind == spec.kind && node.attrs == attrs) {
                continue;
            }
            nodes.push(Node {
                id,
                kind: spec.kind,
                attrs,
                capacity: qty(CapacityDimension::Count, 1),
            });
        }

        let existing_nodes: Vec<_> = reported_existing
            .iter()
            .filter_map(|id| self.cluster.graph.node(*id).cloned())
            .collect();
        let mut claim_bindings = self.missing_claim_contracts(&existing_nodes)?;
        let fresh_nodes: Vec<_> = nodes
            .iter()
            .filter(|node| !reported_existing.contains(&node.id))
            .cloned()
            .collect();
        claim_bindings.extend(crate::discover::claim_bindings(&fresh_nodes));
        if !nodes.is_empty() || !edges.is_empty() || !claim_bindings.is_empty() {
            self.commit(Command::ApplyProviderFacts {
                batches: vec![ProviderFactBatch {
                    writer: crate::discover::DEVICE_FACT_WRITER,
                    nodes,
                    edges,
                }],
                claim_bindings,
            })?;
        }

        let disappeared: BTreeSet<NodeId> = existing.into_values().collect();
        for node in &disappeared {
            if matches!(
                self.cluster.node_state(*node),
                Some(NodeState::Schedulable | NodeState::Joining)
            ) {
                self.commit(Command::SetNodeState {
                    node: *node,
                    state: NodeState::Unavailable,
                })?;
            }
        }

        let affected: BTreeSet<LeaseId> = self
            .cluster
            .leases
            .values()
            .filter(|lease| {
                matches!(
                    lease.state,
                    LeaseState::Reserved | LeaseState::Preparing | LeaseState::Active
                ) && lease
                    .allocation
                    .claims
                    .iter()
                    .any(|claim| disappeared.contains(&claim.node))
            })
            .map(|lease| lease.id)
            .collect();
        for lease in affected {
            self.commit(Command::FailLease {
                lease,
                reason: "provider resource disappeared".into(),
            })?;
        }

        for node in returning {
            if self.cluster.node_state(node) == Some(NodeState::Unavailable) {
                self.commit(Command::SetNodeState {
                    node,
                    state: NodeState::Joining,
                })?;
            }
            let open_binding = self
                .cluster
                .bindings
                .values()
                .any(|binding| binding.node == node && !binding.state.is_closed());
            if !open_binding && self.cluster.node_state(node) == Some(NodeState::Joining) {
                self.commit(Command::SetNodeState {
                    node,
                    state: NodeState::Schedulable,
                })?;
            }
        }

        Ok(())
    }

    fn with_agents(agents: BTreeMap<NodeId, AgentHandle>) -> Self {
        Self {
            cluster: Cluster::new(),
            link_token: None,
            agents,
            execution_capabilities: BTreeMap::new(),
            inbox: Arc::new(Inbox::default()),
            inflight: BTreeSet::new(),
            running: BTreeMap::new(),
            unreachable: Vec::new(),
            queue: Vec::new(),
            commands: BTreeMap::new(),
            lease_commands: BTreeMap::new(),
            lease_images: BTreeMap::new(),
            requests: BTreeMap::new(),
            restart_handled: std::collections::BTreeSet::new(),
            restart_counts: BTreeMap::new(),
            restart_last_at: BTreeMap::new(),
            restart_root: BTreeMap::new(),
            next_request_id: 1,
            next_lease: 1,
            pending: VecDeque::new(),
            next_session: 1,
            next_binding: 1,
            session_floor: 0,
            recovering_machines: BTreeMap::new(),
            recovering_leases: BTreeSet::new(),
            command_sink: None,
            history: Vec::new(),
        }
    }

    /// Lazily capture the first session this process can hand out: any
    /// machine still carrying a lower session was registered by a previous
    /// controller generation.
    fn session_floor(&mut self) -> u64 {
        if self.session_floor == 0 {
            self.session_floor = self.next_session;
        }
        self.session_floor
    }

    /// Persist every command applied to the cluster (kernel commands and
    /// agent records alike). Detach while replaying a log: replayed
    /// commands are already persisted.
    pub fn set_command_sink(&mut self, sink: Option<CommandSink>) {
        self.command_sink = sink;
    }

    /// Rebuild cluster state from a persisted command log without delivering
    /// effects to the agent: the log already contains the agent's records.
    /// Session numbering resumes above the highest replayed session — a
    /// restarted controller must never hand out an old generation.
    pub fn replay(&mut self, commands: impl IntoIterator<Item = Command>) -> Result<(), Error> {
        for command in commands {
            // Recover every id high-water mark from the log before new
            // work allocates colliding ids.
            match &command {
                Command::SetAgentSession { session, .. } => {
                    self.next_session = self.next_session.max(*session + 1);
                }
                Command::OpenBinding { binding, .. } => {
                    self.next_binding = self.next_binding.max(binding.as_u64() + 1);
                }
                Command::ReserveLease { lease, .. }
                | Command::PromoteLease { lease, .. }
                | Command::OpenLease { lease, .. } => {
                    self.next_lease = self.next_lease.max(lease.as_u64() + 1);
                }
                _ => {}
            }
            self.commit(command)?;
        }
        // Replayed commands re-emit their historical effects (stale
        // Prepares, Activates, Reconciles). They were delivered once,
        // before the restart; delivering them again would re-execute or
        // duplicate work. Recovery regenerates exactly what is live.
        self.pending.clear();
        Ok(())
    }

    /// Apply one command and deliver the resulting effects. The control
    /// plane uses this for recovery actions and administrative commands.
    pub fn apply(&mut self, command: Command) -> Result<(), Error> {
        self.commit(command)?;
        self.pump()
    }

    /// Submit a workload: queued for admission; its command runs when the
    /// lease activates.
    pub fn submit(&mut self, request: Request, owner: OwnerId) {
        self.commands.remove(&request.id);
        self.next_request_id = self.next_request_id.max(request.id.as_u64() + 1);
        self.queue.push(Queued {
            request,
            owner,
            submitted_at: self.cluster.now,
        });
    }

    /// Hard placement exclusions imposed by each live execution adapter.
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
            let container = request
                .image
                .as_deref()
                .is_some_and(|image| !image.is_empty());

            for machine in machines {
                let mode = self
                    .execution_capabilities
                    .get(machine)
                    .map(|caps| {
                        if container {
                            caps.container
                        } else {
                            caps.process
                        }
                    })
                    .unwrap_or_default();
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

    /// Admit one request and drive its lease to Active. Returns the admitted
    /// request id, or None when nothing fits.
    pub fn admit_one(&mut self) -> Result<Option<RequestId>, Error> {
        let exclusions = self.execution_exclusions();
        let Some(admission) = self.cluster.admit_backfill_with_exclusions(
            &self.queue,
            &Default::default(),
            &exclusions,
        ) else {
            return Ok(None);
        };
        self.validate_allocation_bindings(&admission.allocation)?;
        let request_id = admission.request.id;
        let lease = LeaseId::from_u64(self.next_lease);
        self.next_lease += 1;
        self.commands.remove(&request_id);
        let command = admission.request.command.clone();
        self.lease_commands.insert(lease, command);
        self.lease_images
            .insert(lease, admission.request.image.clone());
        self.requests
            .insert(lease, (admission.request.clone(), admission.owner));
        let expires_at = self.cluster.now.saturating_add(admission.request.lifetime);
        self.commit(Command::OpenLease {
            lease,
            owner: admission.owner,
            allocation: admission.allocation,
            parent: None,
            expires_at,
            prepare_deadline: self.cluster.now.saturating_add(20),
            priority: admission.request.priority,
        })?;
        self.dequeue(&request_id);
        self.open_enforced_bindings(lease)?;
        self.pump()?;
        // Activation waits for every binding's Prepare ack; a lease without
        // enforced bindings activates right away.
        self.maybe_activate(lease);
        Ok(Some(request_id))
    }

    /// Every command applied since construction, in order.
    pub fn command_history(&self) -> Vec<Command> {
        self.history.clone()
    }

    /// Capture the controller-side state that outlives restarts alongside
    /// the cluster snapshot.
    pub fn state_snapshot(&self) -> ServiceState {
        ServiceState {
            lease_commands: self.lease_commands.clone(),
            lease_images: self.lease_images.clone(),
            requests: self.requests.clone(),
            restart_handled: self.restart_handled.clone(),
            restart_counts: self.restart_counts.clone(),
            restart_last_at: self.restart_last_at.clone(),
            restart_root: self.restart_root.clone(),
            next_lease: self.next_lease,
            next_request_id: self.next_request_id,
            next_session: self.next_session,
            next_binding: self.next_binding,
        }
    }

    /// Restore a full controller from a snapshot: the cluster's decisions
    /// plus the controller-side state that outlives restarts.
    pub fn restore(&mut self, mut cluster: archon_kernel::Cluster, state: ServiceState) {
        cluster.migrate_legacy_observations();
        self.cluster = cluster;
        self.pending.clear();
        self.restore_state(state);
    }

    /// Restore controller-side state captured by [`NodeService::state_snapshot`].
    pub fn restore_state(&mut self, state: ServiceState) {
        self.next_request_id = state.next_request_id;
        self.next_session = state.next_session;
        self.next_binding = state.next_binding;
        self.lease_commands = state.lease_commands;
        self.lease_images = state.lease_images;
        self.requests = state.requests;
        self.restart_handled = state.restart_handled;
        self.restart_counts = state.restart_counts;
        self.restart_last_at = state.restart_last_at;
        self.restart_root = state.restart_root;
        self.next_lease = state.next_lease;
    }

    /// Queue depth, for status reporting.
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Live leases restored by replay/snapshot that await agent
    /// reconciliation after this controller's boot.
    pub fn live_lease_count(&self) -> usize {
        self.cluster
            .leases
            .values()
            .filter(|lease| self.cluster.occupies(lease.id))
            .count()
    }

    /// Probe every registered agent's liveness without blocking: each
    /// machine's worker answers through the inbox, and failures land in
    /// [`NodeService::take_unreachable`].
    pub fn probe_agents(&mut self) {
        for machine in self.agents.keys().copied().collect::<Vec<_>>() {
            let tag = Tag::Probe { machine };
            if self.inflight.contains(&tag) {
                continue;
            }
            self.dispatch(machine, tag, AgentRequest::Status { lease: 0 });
        }
    }

    /// Machines whose most recent probe failed; the caller decides policy.
    pub fn take_unreachable(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.unreachable)
    }

    /// Declare a machine unhealthy: stop placements there (quarantine) and
    /// fail its live leases. Effects toward the dead agent are dropped by
    /// the routing rules; surviving machines can pick up keep-alive work.
    pub fn mark_machine_unhealthy(&mut self, machine: NodeId) -> Result<(), Error> {
        eprintln!("archon: machine {machine} unhealthy; quarantining");
        self.commit(Command::SetNodeHealth {
            node: machine,
            health: "unhealthy".into(),
        })?;
        self.commit(Command::QuarantineNode { node: machine })?;
        let live: Vec<LeaseId> = self
            .cluster
            .leases
            .values()
            .filter(|lease| {
                !matches!(
                    lease.state,
                    archon_kernel::LeaseState::Failed
                        | archon_kernel::LeaseState::Released
                        | archon_kernel::LeaseState::Revoked
                        | archon_kernel::LeaseState::Expired
                ) && lease
                    .allocation
                    .claims
                    .iter()
                    .any(|claim| self.cluster.graph.machine_of(claim.node) == Some(machine))
            })
            .map(|lease| lease.id)
            .collect();
        for lease in live {
            self.commit(Command::FailLease {
                lease,
                reason: "machine unhealthy".into(),
            })?;
        }
        self.pump()
    }

    /// Collect failed/expired keep-alive leases as fresh re-submissions,
    /// capped per original request so a permanently-broken workload cannot
    /// spin, with exponential backoff between attempts (1s doubling to a
    /// 60s ceiling). Run-once workloads are never restarted. Leases whose
    /// backoff has not elapsed stay pending for a later tick.
    pub fn take_restarts(&mut self) -> Vec<(Request, OwnerId)> {
        const MAX_RESTARTS: u32 = 5;
        const BACKOFF_CAP_SECS: u64 = 60;
        let now = self.cluster.now;
        let mut out = Vec::new();
        for (id, lease) in self.cluster.leases.iter() {
            let dead = matches!(
                lease.state,
                archon_kernel::LeaseState::Failed | archon_kernel::LeaseState::Expired
            );
            if !dead || self.restart_handled.contains(id) {
                continue;
            }
            let Some((request, owner)) = self.requests.get(id) else {
                continue;
            };
            if !request.keep_alive {
                continue;
            }
            // Caps and backoff attach to the original workload, not the
            // per-attempt ids.
            let root = self
                .restart_root
                .get(&request.id)
                .copied()
                .unwrap_or(request.id);
            let count = self.restart_counts.entry(root).or_insert(0);
            if *count >= MAX_RESTARTS {
                eprintln!("archon: request {root} exceeded restart cap");
                self.restart_handled.insert(*id);
                continue;
            }
            let last_at = self.restart_last_at.get(&root).copied().unwrap_or(0);
            let delay = (1u64 << (*count).min(6)).min(BACKOFF_CAP_SECS);
            if now < last_at.saturating_add(delay) {
                continue; // backoff: retry on a later tick
            }
            *count += 1;
            self.restart_last_at.insert(root, now);
            self.restart_handled.insert(*id);
            let mut fresh = request.clone();
            fresh.id = RequestId::from_u64(self.next_request_id);
            self.next_request_id += 1;
            self.restart_root.insert(fresh.id, root);
            out.push((fresh, *owner));
        }
        out
    }

    /// Poll every executing workload and record natural exits: zero exit
    /// codes complete the lease (claims released), failures fail it.
    /// Status queries go out to the agents; their answers complete leases
    /// on a later absorb. Returns the leases whose workload finished this
    /// call.
    pub fn collect_completions(&mut self) -> Result<Vec<LeaseId>, Error> {
        let finished = self.absorb()?;
        for lease in self.executing_leases() {
            let tag = Tag::Status { lease };
            if self.inflight.contains(&tag) {
                continue;
            }
            self.request_status(lease);
        }
        self.pump()?;
        Ok(finished)
    }

    /// Active leases with an executable payload (pure claims never finish).
    fn executing_leases(&self) -> Vec<LeaseId> {
        self.cluster
            .leases
            .values()
            .filter(|lease| {
                matches!(lease.state, archon_kernel::LeaseState::Active)
                    && self.lease_commands.contains_key(&lease.id)
            })
            .map(|lease| lease.id)
            .collect()
    }

    /// The machine executing a lease's first claim, if any.
    fn lease_machine(&self, lease: LeaseId) -> Option<NodeId> {
        let allocation_lease = self.cluster.leases.get(&lease)?;
        let claim = allocation_lease.allocation.claims.first()?;
        self.cluster.graph.machine_of(claim.node)
    }

    /// The command executing under a lease, for status reporting.
    pub fn lease_command_of(&self, lease: LeaseId) -> Option<Vec<String>> {
        self.lease_commands.get(&lease).cloned()
    }

    /// A lease's captured output, fetched from its executing agent. Blocks
    /// briefly on the agent's reply; callers holding a shared lock should
    /// use [`NodeService::request_logs`] and wait outside it instead.
    pub fn lease_logs(&mut self, lease: LeaseId) -> Result<String, Error> {
        match self.request_logs(lease) {
            None => Ok(ProcessRuntime::read_log(lease)),
            Some(receiver) => match receiver.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(crate::protocol::AgentResponse::Logs { output, .. })) => Ok(output),
                Ok(Ok(other)) => Err(Error::Refused {
                    explanation: format!("expected Logs, got {other:?}"),
                }),
                Ok(Err(reason)) => Err(Error::Refused {
                    explanation: reason,
                }),
                Err(_) => Err(Error::Refused {
                    explanation: "timed out waiting for agent logs".into(),
                }),
            },
        }
    }

    /// Send one Logs query and return the direct reply channel; resolving
    /// the agent round trip happens off-lock.
    pub fn request_logs(&mut self, lease: LeaseId) -> Option<Receiver<AgentReply>> {
        let machine = self.lease_machine(lease)?;
        let handle = self.agents.get(&machine)?;
        let (job, receiver) = direct_job(AgentRequest::Logs {
            lease: lease.as_u64(),
        });
        handle.send(job).then_some(receiver)
    }

    /// Host device paths bound by a lease's device-kind claims, resolved
    /// through the graph's `dev` attributes.
    pub fn lease_devices(&self, lease: LeaseId) -> Vec<crate::protocol::DeviceAccess> {
        use archon_kernel::ResourceClass;
        let Some(allocation_lease) = self.cluster.leases.get(&lease) else {
            return Vec::new();
        };
        allocation_lease
            .allocation
            .claims
            .iter()
            .filter(|claim| {
                self.cluster.graph.node(claim.node).is_some_and(|node| {
                    matches!(
                        node.kind,
                        ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
                    )
                })
            })
            .filter_map(|claim| {
                self.cluster
                    .graph
                    .node(claim.node)
                    .and_then(|node| crate::protocol::DeviceAccess::from_attrs(&node.attrs))
            })
            .collect()
    }

    pub fn revoke(&mut self, lease: LeaseId) -> Result<(), Error> {
        self.commit(Command::RevokeLease { lease })?;
        self.pump()
    }

    pub fn expire_due(&mut self) -> Result<Vec<LeaseId>, Error> {
        let due: Vec<LeaseId> = self
            .cluster
            .leases
            .values()
            .filter(|lease| {
                matches!(
                    lease.state,
                    archon_kernel::LeaseState::Preparing
                        | archon_kernel::LeaseState::Active
                        | archon_kernel::LeaseState::Reserved
                ) && self.cluster.now >= lease.expires_at
            })
            .map(|lease| lease.id)
            .collect();
        for lease in &due {
            self.commit(Command::ExpireLease { lease: *lease })?;
        }
        self.pump()?;
        Ok(due)
    }

    /// Advance the clock and expire anything due. Real wall-clock time.
    pub fn tick(&mut self) -> Result<Vec<LeaseId>, Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        self.cluster.set_now(now);
        self.expire_due()
    }

    /// Whether the lease's process is running, per the agent.
    /// Whether the lease's process is running, per its agent. Waits for
    /// activation to settle, sends one status query if none is outstanding,
    /// then answers from the latest poll.
    pub fn is_running(&mut self, lease: LeaseId) -> bool {
        if !self.cluster.leases.contains_key(&lease) {
            return false;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while self
            .cluster
            .leases
            .get(&lease)
            .is_some_and(|l| l.state == archon_kernel::LeaseState::Preparing)
            && Instant::now() < deadline
        {
            let _ = self.drive(Duration::from_millis(50));
        }
        let active = self
            .cluster
            .leases
            .get(&lease)
            .is_some_and(|l| l.state == archon_kernel::LeaseState::Active);
        if !active {
            return false;
        }
        self.request_status(lease);
        let _ = self.drive(Duration::from_secs(1));
        self.running.get(&lease).copied().unwrap_or(false)
    }

    fn dequeue(&mut self, request_id: &RequestId) {
        self.queue.retain(|queued| queued.request.id != *request_id);
    }

    fn validate_allocation_bindings(&self, allocation: &Allocation) -> Result<(), Error> {
        for claim in &allocation.claims {
            let node = self
                .cluster
                .graph
                .node(claim.node)
                .ok_or(Error::UnknownNode(claim.node))?;
            let binding = self
                .cluster
                .graph
                .claim_binding_for_quantity(claim.node, &claim.quantity)?;
            if binding.provider != ProviderId::ENFORCE
                || !matches!(
                    node.kind,
                    ResourceClass::Cpu
                        | ResourceClass::Memory
                        | ResourceClass::Gpu
                        | ResourceClass::Nic
                        | ResourceClass::Nvme
                )
            {
                return Err(Error::Refused {
                    explanation: format!(
                        "no registered resource provider can enforce claims on {} ({})",
                        claim.node, node.kind
                    ),
                });
            }
        }
        Ok(())
    }

    fn open_enforced_bindings(&mut self, lease: LeaseId) -> Result<(), Error> {
        let claims = self
            .cluster
            .leases
            .get(&lease)
            .ok_or(Error::UnknownLease(lease))?
            .allocation
            .claims
            .clone();
        for claim in claims {
            let binding_spec = self
                .cluster
                .graph
                .claim_binding_for_quantity(claim.node, &claim.quantity)?;
            let binding = BindingId::from_u64(self.next_binding);
            self.next_binding += 1;
            self.commit(Command::OpenBinding {
                binding,
                lease,
                node: claim.node,
                provider: binding_spec.provider,
                scope: binding_spec.scope,
            })?;
        }
        Ok(())
    }

    fn commit(&mut self, command: Command) -> Result<(), Error> {
        // Log only applied commands: a rejected command must not enter the
        // log, or replay would diverge from the live cluster.
        let effects = self.cluster.apply(command.clone())?;
        self.history.push(command.clone());
        if let Some(sink) = &mut self.command_sink {
            sink(&command);
        }
        self.pending.extend(effects);
        Ok(())
    }

    /// Commit inside the asynchronous completion path: kernel rejections
    /// there are legitimate races (expiry vs. late agent answers), so they
    /// are logged instead of failing the whole drain.
    fn commit_lenient(&mut self, command: Command) {
        if let Err(err) = self.commit(command) {
            eprintln!("archon: rejected stale completion: {err}");
        }
    }

    /// Send every pending effect to its machine's worker. Pure routing and
    /// session rebinding happen here; agent round trips happen on workers.
    fn pump(&mut self) -> Result<(), Error> {
        while let Some(effect) = self.pending.pop_front() {
            for command in self.route(effect)? {
                self.commit(command)?;
            }
        }
        Ok(())
    }

    /// Absorb completed agent answers: record binding acks (the kernel
    /// validates each against the binding's session and fence), advance
    /// leases whose bindings are all prepared, note status polls, and
    /// collect failed probes. Returns the leases completed this pass.
    fn absorb(&mut self) -> Result<Vec<LeaseId>, Error> {
        let mut finished = Vec::new();
        for (tag, reply) in self.inbox.take() {
            self.inflight.remove(&tag);
            match (&tag, reply) {
                (
                    Tag::Binding {
                        binding,
                        session,
                        fence,
                        phase: _,
                    },
                    reply,
                ) => {
                    let binding = *binding;
                    match reply {
                        Ok(AgentResponse::Prepared { handle, .. }) => {
                            let lease = self.binding_lease(binding);
                            self.commit_lenient(Command::RecordBindingPrepared {
                                binding,
                                session: *session,
                                provider_handle: handle,
                                fence: *fence,
                            });
                            if let Some(lease) = lease {
                                self.maybe_activate(lease);
                            }
                        }
                        Ok(AgentResponse::Activated { .. }) => {
                            self.commit_lenient(Command::RecordBindingActive {
                                binding,
                                session: *session,
                                fence: *fence,
                            })
                        }
                        Ok(AgentResponse::Released { .. }) => {
                            self.commit_lenient(Command::RecordBindingReleased {
                                binding,
                                session: *session,
                                fence: *fence,
                            })
                        }
                        Ok(AgentResponse::Fenced { .. }) => {
                            self.commit_lenient(Command::RecordBindingFenced {
                                binding,
                                session: *session,
                                fence: *fence,
                            })
                        }
                        Ok(AgentResponse::Failed { reason, .. }) => {
                            self.commit_lenient(Command::RecordBindingFailed {
                                binding,
                                session: *session,
                                reason,
                                fence: *fence,
                            })
                        }
                        Ok(other) => {
                            eprintln!(
                                "archon: unexpected agent response for binding {binding}: {other:?}"
                            )
                        }
                        Err(reason) => {
                            // Transport failure: leave the binding as-is;
                            // prepare deadlines, probes, and reconciliation
                            // recover from here.
                            eprintln!(
                                "archon: agent call for binding {binding} failed; dropping: {reason}"
                            );
                        }
                    }
                }
                (
                    Tag::Status { lease },
                    Ok(AgentResponse::Running {
                        running, exit_code, ..
                    }),
                ) => {
                    let lease = *lease;
                    if self.recovering_leases.remove(&lease) {
                        self.resolve_recovered_lease(lease, running, exit_code, &mut finished);
                    } else {
                        self.running.insert(lease, running);
                        if let Some(code) = exit_code {
                            self.commit_lenient(Command::CompleteLease {
                                lease,
                                exit_code: code,
                            });
                            finished.push(lease);
                        }
                    }
                }
                (Tag::Status { .. }, Ok(other)) => {
                    eprintln!("archon: unexpected status response: {other:?}")
                }
                (Tag::Status { .. }, Err(_)) => {}
                (Tag::Probe { machine }, Err(_)) => self.unreachable.push(*machine),
                (Tag::Probe { .. }, Ok(_)) => {}
            }
        }
        if !finished.is_empty() {
            self.pump()?;
        }
        Ok(finished)
    }

    /// Advance pending work: absorb completions, send queued effects, and
    /// wait for up to `timeout` while calls are still outstanding. Never
    /// blocks when the service is quiescent.
    pub fn drive(&mut self, timeout: Duration) -> Result<(), Error> {
        let start = Instant::now();
        loop {
            self.absorb()?;
            self.pump()?;
            if self.pending.is_empty() && self.inflight.is_empty() || start.elapsed() >= timeout {
                return Ok(());
            }
            self.inbox.wait_timeout(Duration::from_millis(25));
        }
    }

    /// Shared completion inbox, for waiters outside the controller lock.
    pub fn inbox_handle(&self) -> Arc<Inbox> {
        self.inbox.clone()
    }

    /// True when no effects are queued and no agent call is outstanding.
    pub fn is_quiescent(&self) -> bool {
        self.pending.is_empty() && self.inflight.is_empty()
    }

    fn binding_lease(&self, binding: BindingId) -> Option<LeaseId> {
        self.cluster.bindings.get(&binding).map(|r| r.lease)
    }

    /// Activate a preparing lease once every enforced binding is prepared:
    /// commit ActivateLease and ActivateBinding so their effects spawn the
    /// workload on the agent.
    fn maybe_activate(&mut self, lease: LeaseId) {
        let Some(record) = self.cluster.leases.get(&lease) else {
            return;
        };
        if record.state != archon_kernel::LeaseState::Preparing {
            return;
        }
        let bindings = self.cluster.bindings_for(lease);
        // A prepared binding carries its provider handle from the agent's
        // ack; the kernel keeps the state Preparing until activation.
        let all_prepared = !bindings.is_empty()
            && bindings.iter().all(|binding| {
                self.cluster
                    .bindings
                    .get(binding)
                    .is_some_and(|r| r.provider_handle.is_some())
            });
        if !all_prepared {
            return;
        }
        self.commit_lenient(Command::ActivateLease { lease });
        for binding in bindings {
            self.commit_lenient(Command::ActivateBinding { binding });
        }
    }

    fn request_status(&mut self, lease: LeaseId) {
        let tag = Tag::Status { lease };
        if self.inflight.contains(&tag) {
            return;
        }
        let Some(machine) = self.lease_machine(lease) else {
            return;
        };
        self.dispatch(
            machine,
            tag,
            AgentRequest::Status {
                lease: lease.as_u64(),
            },
        );
    }

    /// CPU and memory claims of a lease, as enforceable limits.
    fn lease_limits(&self, lease: LeaseId) -> Result<LeaseLimits, Error> {
        let mut limits = LeaseLimits::default();
        let claims = &self
            .cluster
            .leases
            .get(&lease)
            .ok_or(Error::UnknownLease(lease))?
            .allocation
            .claims;
        for claim in claims {
            match self.cluster.graph.node(claim.node).map(|node| node.kind) {
                Some(ResourceClass::Cpu) => {
                    limits.cpu_count += quantity_get(&claim.quantity, CapacityDimension::Count);
                }
                Some(ResourceClass::Memory) => {
                    limits.memory_bytes += quantity_get(&claim.quantity, CapacityDimension::Bytes);
                }
                _ => {}
            }
        }
        Ok(limits)
    }

    /// Enqueue one asynchronous agent call toward `machine`, tagged for
    /// completion routing.
    fn dispatch(&mut self, machine: NodeId, tag: Tag, request: AgentRequest) {
        self.inflight.insert(tag.clone());
        let sent = match self.agents.get(&machine) {
            Some(handle) => handle.send(Job {
                request,
                delivery: Delivery::Inbox(tag.clone()),
            }),
            None => false,
        };
        if !sent {
            // No agent for this machine (restart before re-registration,
            // or the agent died). Kernel state proceeds; Reconcile re-drives
            // live work when the agent registers again. A dead agent holds
            // no processes, so dropping the effect is honest.
            self.inflight.remove(&tag);
            eprintln!("archon: no agent for machine {machine}; dropping effect");
        }
    }

    /// Controller-restart reconciliation for one re-registered machine:
    /// decide each live Binding from actual endpoint state rather than
    /// assuming it. Active Leases get a Status proof query (answered in
    /// [`NodeService::absorb`]); Preparing Leases fail — their prepare
    /// deadline passed and partial preparation is unprovable; terminal
    /// Leases' leftover open Bindings fence immediately so occupied claims
    /// from before the restart become reusable only after fencing lands.
    fn route_recovery(
        &mut self,
        machine: NodeId,
        bindings: Vec<BindingId>,
    ) -> Result<Vec<Command>, Error> {
        let mut commands = Vec::new();
        for binding in bindings {
            let Some(record) = self.cluster.bindings.get(&binding) else {
                continue;
            };
            let Some(lease_record) = self.cluster.leases.get(&record.lease) else {
                continue;
            };
            let (lease_id, state) = (record.lease, lease_record.state);
            match state {
                archon_kernel::LeaseState::Active => {
                    if self.recovering_leases.insert(lease_id)
                        && let Some(pending) = self.recovering_machines.get_mut(&machine)
                    {
                        pending.insert(lease_id);
                    }
                    self.dispatch(
                        machine,
                        Tag::Status { lease: lease_id },
                        AgentRequest::Status {
                            lease: lease_id.as_u64(),
                        },
                    );
                }
                archon_kernel::LeaseState::Preparing => commands.push(Command::FailLease {
                    lease: lease_id,
                    reason: "restart left preparation unproven".into(),
                }),
                _ => commands.push(Command::FenceBinding { binding }),
            }
        }
        Ok(commands)
    }

    /// Resolve one lease whose ownership was unproven at controller restart:
    /// running work is adopted onto the agent's new session (never
    /// re-executed), a natural exit completes the lease normally, and
    /// anything else is revoked so its bindings fence before reuse.
    fn resolve_recovered_lease(
        &mut self,
        lease: LeaseId,
        running: bool,
        exit_code: Option<i32>,
        finished: &mut Vec<LeaseId>,
    ) {
        if running {
            let session = self
                .lease_machine(lease)
                .and_then(|machine| self.cluster.sessions.get(&machine).copied());
            if let Some(session) = session {
                for binding in self.cluster.bindings_for(lease) {
                    if let Err(err) = self.commit(Command::RebindSession { binding, session }) {
                        eprintln!(
                            "archon: adopting lease {lease} failed on binding {binding}: {err}"
                        );
                        return;
                    }
                }
                self.running.insert(lease, true);
                eprintln!("archon: recovered lease {lease}: adopted from its agent");
            }
        } else if let Some(code) = exit_code {
            self.commit_lenient(Command::CompleteLease {
                lease,
                exit_code: code,
            });
            finished.push(lease);
            eprintln!("archon: recovered lease {lease}: workload exited while detached");
        } else {
            self.commit_lenient(Command::RevokeLease { lease });
            finished.push(lease);
            eprintln!("archon: recovered lease {lease}: ownership unprovable, revoking");
        }
        self.settle_machine_recovery(lease);
    }

    /// Drop `lease` from its machine's recovery set; when every recovering
    /// lease on that machine resolved, restore its deferred health marks.
    fn settle_machine_recovery(&mut self, lease: LeaseId) {
        let Some(machine) = self.lease_machine(lease) else {
            return;
        };
        let mut done = false;
        if let Some(pending) = self.recovering_machines.get_mut(&machine) {
            pending.remove(&lease);
            done = pending.is_empty();
        }
        if done {
            self.finish_machine_recovery(machine);
        }
    }

    /// The controller side of the seam: route one kernel Effect toward the
    /// agent that owns its node without waiting for the answer. Agent acks
    /// come back through [`NodeService::absorb`] as Record commands.
    fn route(&mut self, effect: Effect) -> Result<Vec<Command>, Error> {
        // Reconcile re-drives a machine's live bindings onto its (fresh)
        // agent: rebind each still-Active binding to the machine's new
        // session, then ActivateBinding — idempotent for Active bindings,
        // forward-moving for Preparing ones. The resulting Activate effects
        // spawn the work again on the new agent process. Revoked or expired
        // leases stay dead.
        if let Effect::Reconcile {
            machine,
            bindings,
            session,
            ..
        } = &effect
        {
            let machine = *machine;
            if self.recovering_machines.contains_key(&machine) {
                return self.route_recovery(machine, bindings.clone());
            }
            let mut commands: Vec<Command> = Vec::new();
            for binding in bindings {
                let active = self
                    .cluster
                    .bindings
                    .get(binding)
                    .and_then(|record| self.cluster.leases.get(&record.lease))
                    .is_some_and(|lease| lease.state == archon_kernel::LeaseState::Active);
                if !active {
                    continue;
                }
                commands.push(Command::RebindSession {
                    binding: *binding,
                    session: *session,
                });
                commands.push(Command::ActivateBinding { binding: *binding });
            }
            return Ok(commands);
        }
        let binding_id = match &effect {
            Effect::Prepare { binding, .. }
            | Effect::Activate { binding, .. }
            | Effect::Release { binding, .. }
            | Effect::Fence { binding, .. } => *binding,
            Effect::Reconcile { .. } => unreachable!(),
        };
        let epoch = match &effect {
            Effect::Prepare { epoch, .. }
            | Effect::Activate { epoch, .. }
            | Effect::Release { epoch, .. }
            | Effect::Fence { epoch, .. } => *epoch,
            Effect::Reconcile { .. } => unreachable!(),
        };
        let (lease, mut session, mut fence, record_node, record_provider, record_scope) = {
            let record = self
                .cluster
                .bindings
                .get(&binding_id)
                .ok_or(Error::UnknownBinding(binding_id))?;
            (
                record.lease,
                record.agent_session,
                record.fence,
                record.node,
                record.provider,
                record.scope,
            )
        };

        // A restart leaves bindings attached to a dead generation. Move a
        // stale binding onto the machine's live session before driving its
        // effect — including teardown effects of terminal leases, whose
        // open bindings would otherwise strand capacity forever.
        let machine_for_rebind = self.cluster.graph.machine_of(record_node);
        if let Some(machine) = machine_for_rebind
            && let Some(&current) = self.cluster.sessions.get(&machine)
            && current != session
        {
            self.commit(Command::RebindSession {
                binding: binding_id,
                session: current,
            })?;
            let refreshed = self
                .cluster
                .bindings
                .get(&binding_id)
                .ok_or(Error::UnknownBinding(binding_id))?;
            session = refreshed.agent_session;
            fence = refreshed.fence;
        }
        let machine = self
            .cluster
            .graph
            .machine_of(record_node)
            .ok_or(Error::Refused {
                explanation: format!("binding {binding_id} node has no machine ancestor"),
            })?;

        let request = match &effect {
            Effect::Prepare { .. } => AgentRequest::Prepare {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                node: record_node.as_u64(),
                provider: record_provider.as_u64(),
                scope: record_scope,
                session,
                fence,
                epoch,
            },
            Effect::Activate { .. } => AgentRequest::Activate {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                node: record_node.as_u64(),
                provider: record_provider.as_u64(),
                scope: record_scope,
                session,
                fence,
                epoch,
                command: self.lease_commands.get(&lease).cloned().unwrap_or_default(),
                limits: self.lease_limits(lease)?,
                image: self
                    .lease_images
                    .get(&lease)
                    .cloned()
                    .flatten()
                    .unwrap_or_default(),
                storage: self
                    .requests
                    .get(&lease)
                    .map(|(request, _)| request.storage.clone())
                    .unwrap_or_default(),
                ports: self
                    .requests
                    .get(&lease)
                    .map(|(request, _)| request.ports.clone())
                    .unwrap_or_default(),
                grace_secs: self
                    .requests
                    .get(&lease)
                    .map(|(request, _)| request.grace_secs)
                    .unwrap_or(0),
                devices: self.lease_devices(lease),
            },
            Effect::Release { .. } => AgentRequest::Release {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                node: record_node.as_u64(),
                provider: record_provider.as_u64(),
                scope: record_scope,
                session,
                fence,
                epoch,
            },
            Effect::Fence { .. } => AgentRequest::Fence {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                node: record_node.as_u64(),
                provider: record_provider.as_u64(),
                scope: record_scope,
                session,
                fence,
                epoch,
            },
            Effect::Reconcile { .. } => unreachable!(),
        };

        let phase = match &effect {
            Effect::Prepare { .. } => EffectPhase::Prepare,
            Effect::Activate { .. } => EffectPhase::Activate,
            Effect::Release { .. } => EffectPhase::Release,
            Effect::Fence { .. } => EffectPhase::Fence,
            Effect::Reconcile { .. } => unreachable!(),
        };
        self.dispatch(
            machine,
            Tag::Binding {
                binding: binding_id,
                session,
                fence,
                phase,
            },
            request,
        );
        Ok(Vec::new())
    }
}
