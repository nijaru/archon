//! Single-node Archon controller: the Cluster transition function plus an
//! agent link that executes leases. The link is either the in-process
//! [`LeaseAgent`] or a TCP connection to a remote `archon agent` —
//! both speak the same protocol, so decision and enforcement stay on
//! opposite sides of the seam whether the machine is local or not.

use std::collections::{BTreeMap, VecDeque};
use std::net::TcpStream;

use archon_kernel::{
    BindingId, Cluster, Command, Dimension, Effect, Error, LeaseId, NodeKind, OwnerId, ProviderId,
    Queued, Request, RequestId, quantity_get,
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
}

impl LeaseExecutor for RemoteExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        write_frame(&mut self.stream, &request).map_err(|err| err.to_string())?;
        read_frame(&mut self.stream).map_err(|err| err.to_string())
    }
}

pub struct NodeService {
    pub cluster: Cluster,
    executor: Box<dyn LeaseExecutor>,
    queue: Vec<Queued>,
    /// Workload payload per queued request, kept outside the kernel log:
    /// resource decisions never need it, only execution does.
    commands: BTreeMap<RequestId, Vec<String>>,
    /// Command per active lease, recorded when its request is admitted.
    lease_commands: BTreeMap<LeaseId, Vec<String>>,
    pending: VecDeque<Effect>,
    next_session: u64,
    next_binding: u64,
    /// Observes every command applied to the cluster; the control plane
    /// persists them here.
    command_sink: Option<CommandSink>,
}

impl Default for NodeService {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeService {
    /// A controller executing on this machine, lifecycle-only (no cgroups).
    pub fn new() -> Self {
        let executor = LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()));
        Self::with_executor(Box::new(executor))
    }

    /// A controller executing on this machine; cgroup enforcement when a
    /// root is given (Linux only). Unbooted: the caller applies the graph.
    pub fn local(cgroup_root: Option<String>) -> Self {
        #[cfg(target_os = "linux")]
        if let Some(root) = cgroup_root {
            return Self::local_with_cgroups(root);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = cgroup_root;
        Self::new()
    }

    /// A controller executing on this machine with cgroup v2 enforcement.
    #[cfg(target_os = "linux")]
    pub fn local_with_cgroups(root: String) -> Self {
        let runtime = ProcessRuntime::new().with_cgroup_root(root);
        let executor = LocalExecutor::new(LeaseAgent::new(runtime));
        Self::with_executor(Box::new(executor))
    }

    /// Connect to a remote agent, learn its machine, and boot the cluster
    /// over its discovered graph.
    pub fn connect(addr: &str) -> Result<Self, Error> {
        let mut executor = RemoteExecutor::connect(addr).map_err(|err| Error::Refused {
            explanation: format!("connect {addr}: {err}"),
        })?;
        let welcome = executor
            .execute(AgentRequest::Hello)
            .map_err(|reason| Error::Refused {
                explanation: reason,
            })?;
        let (name, cpus, memory_bytes) = match welcome {
            AgentResponse::Welcome {
                name,
                cpus,
                memory_bytes,
            } => (name, cpus, memory_bytes),
            other => {
                return Err(Error::Refused {
                    explanation: format!("expected Welcome, got {other:?}"),
                });
            }
        };
        let description = crate::discover::MachineDescription {
            name,
            cpus,
            memory_bytes,
        };
        let (_local, nodes, edges) = crate::discover::build_graph(&description);
        let executor = Box::new(executor);
        let mut service = Self::with_executor(executor);
        service.boot(nodes, edges)?;
        Ok(service)
    }

    fn with_executor(executor: Box<dyn LeaseExecutor>) -> Self {
        Self {
            cluster: Cluster::new(),
            executor,
            queue: Vec::new(),
            commands: BTreeMap::new(),
            lease_commands: BTreeMap::new(),
            pending: VecDeque::new(),
            next_session: 1,
            next_binding: 1,
            command_sink: None,
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
    pub fn replay(&mut self, commands: impl IntoIterator<Item = Command>) -> Result<(), Error> {
        for command in commands {
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

    pub fn boot(
        &mut self,
        nodes: Vec<archon_kernel::Node>,
        edges: Vec<archon_kernel::Edge>,
    ) -> Result<(), Error> {
        self.commit(Command::ApplyGraph { nodes, edges })?;
        let machine = self
            .cluster
            .graph
            .nodes_of_kind(NodeKind::Machine)
            .first()
            .copied()
            .expect("discovered machine");
        let session = self.next_session;
        self.next_session += 1;
        self.commit(Command::SetAgentSession { machine, session })?;
        self.deliver_all()
    }

    /// Submit a workload: queued for admission; its command runs when the
    /// lease activates.
    pub fn submit(&mut self, request: Request, owner: OwnerId, command: Vec<String>) {
        self.commands.insert(request.id, command);
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
        let command = self.commands.remove(&request_id).unwrap_or_default();
        self.lease_commands.insert(lease, command.clone());
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
        let session = self.next_session.saturating_sub(1);
        matches!(
            self.executor.execute(AgentRequest::Status {
                lease: lease.as_u64(),
                session,
            }),
            Ok(AgentResponse::Running { running: true, .. })
        )
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

    /// The controller side of the seam: turn one kernel Effect into an
    /// agent request and the acknowledgement commands the Cluster expects.
    fn execute(&mut self, effect: Effect) -> Result<Vec<Command>, Error> {
        let binding_id = match &effect {
            Effect::Prepare { binding, .. }
            | Effect::Activate { binding, .. }
            | Effect::Release { binding, .. }
            | Effect::Fence { binding, .. } => *binding,
            Effect::Reconcile { .. } => return Ok(Vec::new()),
        };
        let record = self
            .cluster
            .bindings
            .get(&binding_id)
            .ok_or(Error::UnknownBinding(binding_id))?;
        let (lease, session, fence) = (record.lease, record.agent_session, record.fence);

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
            Effect::Reconcile { .. } => return Ok(Vec::new()),
        };

        match self
            .executor
            .execute(request)
            .map_err(|reason| Error::Refused {
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
