//! Single-node Fleet service: the Cluster transition function plus a real
//! agent that executes leases as OS processes. The agent consumes the same
//! kernel `Effect`s the simulator's endpoints consume — the seam between
//! decision and enforcement is unchanged; only the enforcement is real.

use std::collections::{BTreeMap, VecDeque};

use fleet_kernel::{
    BindingId, Cluster, Command, Effect, Error, LeaseId, NodeKind, OwnerId, ProviderId, Queued,
    Request, RequestId,
};

use crate::runtime::ProcessRuntime;

pub struct NodeService {
    pub cluster: Cluster,
    runtime: ProcessRuntime,
    queue: Vec<Queued>,
    /// Workload payload per queued request, kept outside the kernel log:
    /// resource decisions never need it, only execution does.
    commands: BTreeMap<RequestId, Vec<String>>,
    /// Command per active lease, recorded when its request is admitted.
    lease_commands: BTreeMap<LeaseId, Vec<String>>,
    pending: VecDeque<Effect>,
    next_session: u64,
    next_handle: u64,
    next_binding: u64,
}

impl Default for NodeService {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeService {
    pub fn new() -> Self {
        Self {
            cluster: Cluster::new(),
            runtime: ProcessRuntime::new(),
            queue: Vec::new(),
            commands: BTreeMap::new(),
            lease_commands: BTreeMap::new(),
            pending: VecDeque::new(),
            next_session: 1,
            next_handle: 1,
            next_binding: 1,
        }
    }

    pub fn boot(
        &mut self,
        nodes: Vec<fleet_kernel::Node>,
        edges: Vec<fleet_kernel::Edge>,
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
                    fleet_kernel::LeaseState::Preparing
                        | fleet_kernel::LeaseState::Active
                        | fleet_kernel::LeaseState::Reserved
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

    pub fn is_running(&mut self, lease: LeaseId) -> bool {
        self.runtime.is_running(lease)
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
        let effects = self.cluster.apply(command)?;
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

    /// The agent side of the seam: turn one kernel Effect into real process
    /// operations and the acknowledgement commands the Cluster expects.
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
        match effect {
            Effect::Prepare { .. } => {
                let handle = self.next_handle;
                self.next_handle += 1;
                Ok(vec![Command::RecordBindingPrepared {
                    binding: binding_id,
                    session,
                    provider_handle: handle,
                    fence,
                }])
            }
            Effect::Activate { .. } => {
                let command = self.lease_commands.get(&lease).cloned().unwrap_or_default();
                self.runtime
                    .activate(lease, &command)
                    .map_err(|reason| Error::Refused {
                        explanation: reason,
                    })?;
                Ok(vec![Command::RecordBindingActive {
                    binding: binding_id,
                    session,
                    fence,
                }])
            }
            Effect::Release { .. } => {
                self.runtime
                    .terminate(lease)
                    .map_err(|reason| Error::Refused {
                        explanation: reason,
                    })?;
                Ok(vec![Command::RecordBindingReleased {
                    binding: binding_id,
                    session,
                    fence,
                }])
            }
            Effect::Fence { .. } => {
                self.runtime
                    .terminate(lease)
                    .map_err(|reason| Error::Refused {
                        explanation: reason,
                    })?;
                Ok(vec![Command::RecordBindingFenced {
                    binding: binding_id,
                    session,
                    fence,
                }])
            }
            Effect::Reconcile { .. } => Ok(Vec::new()),
        }
    }
}
