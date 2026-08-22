//! Single-node Archon controller: the Cluster transition function plus an
//! agent link that executes leases. The link is either the in-process
//! [`LeaseAgent`] or a TCP connection to a remote `archon agent` —
//! both speak the same protocol, so decision and enforcement stay on
//! opposite sides of the seam whether the machine is local or not.

use std::collections::{BTreeMap, VecDeque};
use std::net::TcpStream;

use archon_kernel::{
    BindingId, Cluster, Command, Dimension, Effect, Error, LeaseId, NodeId, NodeKind, OwnerId,
    ProviderId, Queued, Request, RequestId, quantity_get,
};

type CommandSink = Box<dyn FnMut(&Command) + Send>;

use crate::agent::LeaseAgent;
use crate::protocol::{AgentRequest, AgentResponse, LeaseLimits, read_frame, write_frame};
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

/// Remote execution over TCP; one connection per agent.
pub struct RemoteExecutor {
    stream: TcpStream,
}

impl RemoteExecutor {
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        Ok(Self {
            stream: TcpStream::connect(addr)?,
        })
    }

    /// Wrap an already-connected socket (dial-in agents).
    pub fn from_stream(stream: TcpStream) -> Self {
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
    /// One executor per registered machine, keyed by the machine NodeId.
    agents: BTreeMap<NodeId, Box<dyn LeaseExecutor>>,
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
    next_request_id: u64,
    pending: VecDeque<Effect>,
    next_session: u64,
    next_binding: u64,
    /// Observes every command applied to the cluster; the control plane
    /// persists them here.
    command_sink: Option<CommandSink>,
    /// Every applied command since construction; tests and debugging use
    /// this, the durable log remains the control plane's.
    history: Vec<Command>,
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
        let description = crate::discover::describe();
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
        let mut executor = RemoteExecutor::connect(addr).map_err(|err| Error::Refused {
            explanation: format!("connect {addr}: {err}"),
        })?;
        let description = Self::hello(&mut executor)?;
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
                name,
                cpus,
                memory_bytes,
            } => Ok(crate::discover::MachineDescription {
                instance_id: String::new(),
                name,
                cpus,
                memory_bytes,
            }),
            other => Err(Error::Refused {
                explanation: format!("expected Welcome, got {other:?}"),
            }),
        }
    }

    /// Register one machine's agent: apply its graph fragment (or match an
    /// existing machine by name on re-registration), assign a fresh session,
    /// and let the kernel's Reconcile re-drive live work onto the agent.
    pub fn register_agent(
        &mut self,
        description: crate::discover::MachineDescription,
        executor: Box<dyn LeaseExecutor>,
    ) -> Result<NodeId, Error> {
        // Identity is the agent's instance id, stored in the machine's
        // attrs so it survives control-plane restarts via replay.
        let named = |cluster: &Cluster, instance: &str| {
            cluster
                .graph
                .nodes_of_kind(NodeKind::Machine)
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
            Some(machine) => (machine, false),
            None => {
                let base = self
                    .cluster
                    .graph
                    .nodes()
                    .map(|node| node.id.as_u64())
                    .max()
                    .unwrap_or(0);
                let (_local, nodes, edges) = crate::discover::build_graph(&description, base);
                self.commit(Command::ApplyGraph { nodes, edges })?;
                let machine =
                    named(&self.cluster, &description.instance_id).ok_or(Error::Refused {
                        explanation: "applied graph fragment but machine node is missing".into(),
                    })?;
                (machine, true)
            }
        };
        self.agents.insert(machine, executor);
        let session = self.next_session;
        self.next_session += 1;
        if !new_machine {
            // A known machine came back: clear any quarantine and health
            // marks before reconciliation re-drives its live work.
            self.commit(Command::UnquarantineNode { node: machine })
                .ok();
            self.commit(Command::SetNodeHealth {
                node: machine,
                health: "healthy".into(),
            })?;
        }
        self.commit(Command::SetAgentSession { machine, session })?;
        self.deliver_all()?;
        let _ = new_machine;
        Ok(machine)
    }

    fn with_agents(agents: BTreeMap<NodeId, Box<dyn LeaseExecutor>>) -> Self {
        Self {
            cluster: Cluster::new(),
            agents,
            queue: Vec::new(),
            commands: BTreeMap::new(),
            lease_commands: BTreeMap::new(),
            lease_images: BTreeMap::new(),
            requests: BTreeMap::new(),
            restart_handled: std::collections::BTreeSet::new(),
            restart_counts: BTreeMap::new(),
            next_request_id: 1,
            pending: VecDeque::new(),
            next_session: 1,
            next_binding: 1,
            command_sink: None,
            history: Vec::new(),
        }
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
            if let Command::SetAgentSession { session, .. } = &command {
                self.next_session = self.next_session.max(*session + 1);
            }
            self.commit(command)?;
        }
        Ok(())
    }

    /// Apply one command and deliver the resulting effects. The control
    /// plane uses this for recovery actions and administrative commands.
    pub fn apply(&mut self, command: Command) -> Result<(), Error> {
        self.commit(command)?;
        self.deliver_all()
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

    /// Admit one request and drive its lease to Active. Returns the admitted
    /// request id, or None when nothing fits.
    pub fn admit_one(&mut self) -> Result<Option<RequestId>, Error> {
        let Some(admission) = self
            .cluster
            .admit_backfill(&self.queue, &Default::default())
        else {
            return Ok(None);
        };
        let request_id = admission.request.id;
        let lease = LeaseId::from_u64(self.cluster.leases.len() as u64 + 1);
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
        self.deliver_all()?;
        self.commit(Command::ActivateLease { lease })?;
        let bindings: Vec<BindingId> = self.cluster.bindings_for(lease);
        for binding in bindings {
            self.commit(Command::ActivateBinding { binding })?;
        }
        self.deliver_all()?;
        Ok(Some(request_id))
    }

    /// Every command applied since construction, in order.
    pub fn command_history(&self) -> Vec<Command> {
        self.history.clone()
    }

    /// Queue depth, for status reporting.
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Revoke every live lease regardless of deadline. Recovery uses this:
    /// a fresh agent holds no processes, so in-flight work cannot survive a
    /// restart and must be revoked, not re-executed.
    pub fn revoke_live_leases(&mut self) -> Result<usize, Error> {
        let live: Vec<LeaseId> = self
            .cluster
            .leases
            .values()
            .filter(|lease| {
                matches!(
                    lease.state,
                    archon_kernel::LeaseState::Preparing
                        | archon_kernel::LeaseState::Active
                        | archon_kernel::LeaseState::Reserved
                )
            })
            .map(|lease| lease.id)
            .collect();
        let count = live.len();
        for lease in &live {
            self.commit(Command::RevokeLease { lease: *lease })?;
        }
        self.deliver_all()?;
        Ok(count)
    }

    /// Probe every registered agent's liveness. Returns the machines whose
    /// agent could not be reached; the caller decides policy.
    pub fn probe_agents(&mut self) -> Vec<NodeId> {
        let mut unreachable = Vec::new();
        for (machine, executor) in self.agents.iter_mut() {
            if executor.execute(AgentRequest::Status { lease: 0 }).is_err() {
                unreachable.push(*machine);
            }
        }
        unreachable
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
        self.deliver_all()
    }

    /// Clear a machine's quarantine and mark it healthy again; called when
    /// its agent re-registers.
    pub fn mark_machine_healthy(&mut self, machine: NodeId) -> Result<(), Error> {
        self.commit(Command::UnquarantineNode { node: machine })
            .ok();
        self.commit(Command::SetNodeHealth {
            node: machine,
            health: "healthy".into(),
        })?;
        self.deliver_all()
    }

    /// Collect failed/expired keep-alive leases as fresh re-submissions,
    /// capped per original request so a permanently-broken workload cannot
    /// spin. Run-once workloads are never restarted.
    pub fn take_restarts(&mut self) -> Vec<(Request, OwnerId)> {
        const MAX_RESTARTS: u32 = 5;
        let mut out = Vec::new();
        for (id, lease) in self.cluster.leases.iter() {
            let dead = matches!(
                lease.state,
                archon_kernel::LeaseState::Failed | archon_kernel::LeaseState::Expired
            );
            if !dead || !self.restart_handled.insert(*id) {
                continue;
            }
            let Some((request, owner)) = self.requests.get(id) else {
                continue;
            };
            if !request.keep_alive {
                continue;
            }
            let count = self.restart_counts.entry(request.id).or_insert(0);
            if *count >= MAX_RESTARTS {
                eprintln!("archon: request {} exceeded restart cap", request.id);
                continue;
            }
            *count += 1;
            let mut fresh = request.clone();
            fresh.id = RequestId::from_u64(self.next_request_id);
            self.next_request_id += 1;
            out.push((fresh, *owner));
        }
        out
    }

    pub fn revoke(&mut self, lease: LeaseId) -> Result<(), Error> {
        self.commit(Command::RevokeLease { lease })?;
        self.deliver_all()
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
        self.deliver_all()?;
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
    pub fn is_running(&mut self, lease: LeaseId) -> bool {
        let Some(lease_record) = self.cluster.leases.get(&lease) else {
            return false;
        };
        let machines: std::collections::BTreeSet<NodeId> = lease_record
            .allocation
            .claims
            .iter()
            .filter_map(|claim| self.cluster.graph.machine_of(claim.node))
            .collect();
        machines
            .into_iter()
            .any(|machine| match self.agents.get_mut(&machine) {
                Some(executor) => matches!(
                    executor.execute(AgentRequest::Status {
                        lease: lease.as_u64()
                    }),
                    Ok(AgentResponse::Running { running: true, .. })
                ),
                None => false,
            })
    }

    fn dequeue(&mut self, request_id: &RequestId) {
        self.queue.retain(|queued| queued.request.id != *request_id);
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
            let enforced = self
                .cluster
                .graph
                .node(claim.node)
                .is_some_and(|node| node.kind.is_enforced());
            if !enforced {
                continue;
            }
            let binding = BindingId::from_u64(self.next_binding);
            self.next_binding += 1;
            self.commit(Command::OpenBinding {
                binding,
                lease,
                node: claim.node,
                provider: ProviderId::ENFORCE,
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

    fn deliver_all(&mut self) -> Result<(), Error> {
        while let Some(effect) = self.pending.pop_front() {
            for command in self.execute(effect)? {
                self.commit(command)?;
            }
        }
        Ok(())
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
                Some(NodeKind::Cpu) => {
                    limits.cpu_count += quantity_get(&claim.quantity, Dimension::Count);
                }
                Some(NodeKind::Memory) => {
                    limits.memory_bytes += quantity_get(&claim.quantity, Dimension::Bytes);
                }
                _ => {}
            }
        }
        Ok(limits)
    }

    /// The controller side of the seam: route one kernel Effect to the
    /// agent that owns its node, and turn the answer into Record commands.
    fn execute(&mut self, effect: Effect) -> Result<Vec<Command>, Error> {
        // Reconcile re-drives a machine's live bindings onto its (fresh)
        // agent: rebind each still-Active binding to the machine's new
        // session, then ActivateBinding — idempotent for Active bindings,
        // forward-moving for Preparing ones. The resulting Activate effects
        // spawn the work again on the new agent process. Revoked or expired
        // leases stay dead.
        if let Effect::Reconcile {
            bindings, session, ..
        } = &effect
        {
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
        let record = self
            .cluster
            .bindings
            .get(&binding_id)
            .ok_or(Error::UnknownBinding(binding_id))?;
        let (lease, session, fence) = (record.lease, record.agent_session, record.fence);
        let machine = self
            .cluster
            .graph
            .machine_of(record.node)
            .ok_or(Error::Refused {
                explanation: format!("binding {binding_id} node has no machine ancestor"),
            })?;

        let request = match &effect {
            Effect::Prepare { .. } => AgentRequest::Prepare {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                session,
                fence,
            },
            Effect::Activate { .. } => AgentRequest::Activate {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                session,
                fence,
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
            },
            Effect::Release { .. } => AgentRequest::Release {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                session,
                fence,
            },
            Effect::Fence { .. } => AgentRequest::Fence {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                session,
                fence,
            },
            Effect::Reconcile { .. } => unreachable!(),
        };

        let Some(executor) = self.agents.get_mut(&machine) else {
            // No agent for this machine (restart before re-registration, or
            // the agent died). Kernel state proceeds; Reconcile re-drives
            // live work when the agent registers again. A dead agent holds
            // no processes, so dropping the effect is honest.
            eprintln!("archon: no agent for machine {machine}; dropping effect");
            return Ok(Vec::new());
        };
        match executor.execute(request).map_err(|reason| Error::Refused {
            explanation: reason,
        })? {
            AgentResponse::Prepared { handle, .. } => Ok(vec![Command::RecordBindingPrepared {
                binding: binding_id,
                session,
                provider_handle: handle,
                fence,
            }]),
            AgentResponse::Activated { .. } => Ok(vec![Command::RecordBindingActive {
                binding: binding_id,
                session,
                fence,
            }]),
            AgentResponse::Released { .. } => Ok(vec![Command::RecordBindingReleased {
                binding: binding_id,
                session,
                fence,
            }]),
            AgentResponse::Fenced { .. } => Ok(vec![Command::RecordBindingFenced {
                binding: binding_id,
                session,
                fence,
            }]),
            AgentResponse::Failed { reason, .. } => Ok(vec![Command::RecordBindingFailed {
                binding: binding_id,
                session,
                reason,
                fence,
            }]),
            other => Err(Error::Refused {
                explanation: format!("unexpected agent response {other:?}"),
            }),
        }
    }
}
