use std::collections::{BTreeMap, BTreeSet, VecDeque};

use archon_kernel::{
    Allocation, BindingId, Cluster, Command, Digest, Effect, Endpoint, EndpointOp, Error, LeaseId,
    NodeId, OwnerId, ProviderId, Queued, Request, RequestId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    Command(Command),
    SetNow(u64),
    SetAgreement(bool),
    AdvanceEpoch,
    Deliver,
    Drop,
    RestartAgent { machine: NodeId, session: u64 },
}

#[derive(Clone, Debug)]
struct Agent {
    session: u64,
    epoch: u64,
}

pub struct World {
    pub cluster: Cluster,
    agents: BTreeMap<NodeId, Agent>,
    pub endpoints: BTreeMap<(ProviderId, NodeId), Endpoint>,
    pending: VecDeque<Effect>,
    pub trace: Vec<TraceEvent>,
    next_session: u64,
    next_handle: u64,
    pub queue: Vec<Queued>,
    next_binding: u64,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        Self {
            cluster: Cluster::new(),
            agents: BTreeMap::new(),
            endpoints: BTreeMap::new(),
            pending: VecDeque::new(),
            trace: Vec::new(),
            next_session: 1,
            next_handle: 1,
            queue: Vec::new(),
            next_binding: 1,
        }
    }

    pub fn apply_graph(
        &mut self,
        nodes: Vec<archon_kernel::Node>,
        edges: Vec<archon_kernel::Edge>,
    ) -> Result<(), Error> {
        self.apply(Command::ApplyGraph { nodes, edges })?;
        Ok(())
    }

    pub fn register_agent(&mut self, machine: NodeId) -> Result<u64, Error> {
        let session = self.next_session;
        self.next_session += 1;
        self.agents.insert(
            machine,
            Agent {
                session,
                epoch: self.cluster.epoch,
            },
        );
        for node in enforced_under(&self.cluster, machine) {
            self.endpoints
                .entry((ProviderId::ENFORCE, node))
                .and_modify(|endpoint| endpoint.handshake(session))
                .or_insert_with(|| Endpoint::new(ProviderId::ENFORCE, node, session));
        }
        self.apply(Command::SetAgentSession { machine, session })?;
        Ok(session)
    }

    pub fn apply(&mut self, command: Command) -> Result<Vec<Effect>, Error> {
        let effects = self.commit(command)?;
        for effect in &effects {
            self.pending.push_back(effect.clone());
        }
        Ok(effects)
    }

    fn commit(&mut self, command: Command) -> Result<Vec<Effect>, Error> {
        let effects = self.cluster.apply(command.clone())?;
        self.trace.push(TraceEvent::Command(command));
        Ok(effects)
    }

    pub fn set_now(&mut self, now: u64) {
        self.cluster.set_now(now);
        self.trace.push(TraceEvent::SetNow(now));
    }

    pub fn set_agreement(&mut self, agreed: bool) {
        self.cluster.set_agreement(agreed);
        self.trace.push(TraceEvent::SetAgreement(agreed));
    }

    pub fn advance_epoch(&mut self) {
        self.cluster.advance_epoch();
        self.trace.push(TraceEvent::AdvanceEpoch);
    }

    pub fn deliver_all(&mut self) -> Result<(), Error> {
        while self.deliver_one()? {}
        Ok(())
    }

    pub fn drop_one(&mut self) -> bool {
        if self.pending.pop_front().is_some() {
            self.trace.push(TraceEvent::Drop);
            true
        } else {
            false
        }
    }

    pub fn deliver_one(&mut self) -> Result<bool, Error> {
        let Some(effect) = self.pending.pop_front() else {
            return Ok(false);
        };
        self.trace.push(TraceEvent::Deliver);
        self.dispatch(effect)?;
        Ok(true)
    }

    pub fn inject(&mut self, effect: Effect) -> Result<(), Error> {
        self.dispatch(effect)
    }

    pub fn restart_agent(&mut self, machine: NodeId) -> Result<u64, Error> {
        let session = self.next_session;
        self.next_session += 1;
        self.agents.insert(
            machine,
            Agent {
                session,
                epoch: self.cluster.epoch,
            },
        );
        for endpoint in self.endpoints.values_mut() {
            if self.cluster.graph.machine_of(endpoint.node) == Some(machine) {
                endpoint.handshake(session);
            }
        }
        self.trace
            .push(TraceEvent::RestartAgent { machine, session });
        self.apply(Command::SetAgentSession { machine, session })?;
        Ok(session)
    }

    /// Controller crash and recovery: a fresh authority rebuilds the identical
    /// Cluster state machine from the committed trace while Agents and provider
    /// endpoints keep enforcing their last accepted generations.
    /// Re-registration then drives [`World::reconcile`], which adopts work whose
    /// Binding generation is provably still enforced and fences the rest.
    pub fn restart_controller(&mut self) -> Result<(), Error> {
        let cluster = self.replay_trace()?;
        self.cluster = cluster;
        Ok(())
    }

    /// Remove an admitted request from the queue only after its lease opens
    /// successfully, so a failed open never loses the request.
    fn dequeue(&mut self, request_id: &RequestId) {
        self.queue.retain(|queued| queued.request.id != *request_id);
    }

    pub fn enqueue(&mut self, request: Request, owner: OwnerId) {
        self.queue.push(Queued {
            request,
            owner,
            submitted_at: self.cluster.now,
        });
    }

    /// Admit the head of the queue under an optional per-owner fair-share
    /// ceiling. An empty ceiling disables the budget.
    pub fn admit_next(
        &mut self,
        lease: LeaseId,
        owner: OwnerId,
    ) -> Result<Option<RequestId>, Error> {
        let Some(admission) = self.cluster.admit(&self.queue) else {
            return Ok(None);
        };
        let expires_at = self.cluster.now.saturating_add(admission.request.lifetime);
        let prepare_deadline = self.cluster.now.saturating_add(20);
        self.apply(Command::OpenLease {
            lease,
            owner,
            allocation: admission.allocation,
            parent: None,
            expires_at,
            prepare_deadline,
            priority: admission.request.priority,
        })?;
        self.dequeue(&admission.request.id);

        self.bind_enforced(lease, self.next_binding)?;
        self.next_binding = self.next_binding.saturating_add(32);
        Ok(Some(admission.request.id))
    }

    /// Admit with EASY-style backfill: later requests may start when they do
    /// not delay a blocked higher-priority request.
    pub fn admit_next_backfill(
        &mut self,
        lease: LeaseId,
        owner: OwnerId,
    ) -> Result<Option<RequestId>, Error> {
        let Some(admission) = self
            .cluster
            .admit_backfill(&self.queue, &archon_kernel::KindUsage::new())
        else {
            return Ok(None);
        };
        let expires_at = self.cluster.now.saturating_add(admission.request.lifetime);
        let prepare_deadline = self.cluster.now.saturating_add(20);
        self.apply(Command::OpenLease {
            lease,
            owner,
            allocation: admission.allocation,
            parent: None,
            expires_at,
            prepare_deadline,
            priority: admission.request.priority,
        })?;
        self.dequeue(&admission.request.id);

        self.bind_enforced(lease, self.next_binding)?;
        self.next_binding = self.next_binding.saturating_add(32);
        Ok(Some(admission.request.id))
    }

    /// Admit with per-owner, per-kind fair-share ceilings.
    pub fn admit_next_fair(
        &mut self,
        lease: LeaseId,
        owner: OwnerId,
        fair_share: &archon_kernel::KindUsage,
    ) -> Result<Option<RequestId>, Error> {
        let Some(admission) = self.cluster.admit_fair(&self.queue, fair_share) else {
            return Ok(None);
        };
        self.queue
            .retain(|queued| queued.request.id != admission.request.id);
        let expires_at = self.cluster.now.saturating_add(admission.request.lifetime);
        let prepare_deadline = self.cluster.now.saturating_add(20);
        self.apply(Command::OpenLease {
            lease,
            owner,
            allocation: admission.allocation,
            parent: None,
            expires_at,
            prepare_deadline,
            priority: admission.request.priority,
        })?;
        self.bind_enforced(lease, self.next_binding)?;
        self.next_binding = self.next_binding.saturating_add(32);
        Ok(Some(admission.request.id))
    }

    /// Update a node's health without advancing Graph.revision: scoring
    /// input only, never authority, never stranding in-flight allocations.
    pub fn set_health(&mut self, node: NodeId, health: &str) -> Result<(), Error> {
        self.apply(Command::SetNodeHealth {
            node,
            health: health.into(),
        })?;
        Ok(())
    }

    /// Commit capacity for a future request as a Reserved lease. Reserved
    /// capacity occupies the graph but has no bindings and enforces nothing.
    pub fn reserve(
        &mut self,
        request: &Request,
        lease: LeaseId,
        owner: OwnerId,
        expires_at: u64,
    ) -> Result<Allocation, Error> {
        let allocation = self.cluster.allocate(request)?;
        self.apply(Command::ReserveLease {
            lease,
            owner,
            allocation: allocation.clone(),
            expires_at,
            priority: request.priority,
        })?;
        Ok(allocation)
    }

    /// Move a Reserved lease into the ordinary prepare/bind/activate path.
    pub fn promote(&mut self, lease: LeaseId) -> Result<(), Error> {
        let prepare_deadline = self.cluster.now.saturating_add(20);
        self.apply(Command::PromoteLease {
            lease,
            prepare_deadline,
        })?;
        self.bind_enforced(lease, self.next_binding)?;
        self.next_binding = self.next_binding.saturating_add(32);
        Ok(())
    }

    pub fn place(
        &mut self,
        request: &Request,
        lease: LeaseId,
        owner: OwnerId,
        parent: Option<LeaseId>,
        expires_at: u64,
        prepare_deadline: u64,
    ) -> Result<Allocation, Error> {
        let allocation = self.cluster.allocate(request)?;
        self.apply(Command::OpenLease {
            lease,
            owner,
            allocation: allocation.clone(),
            parent,
            expires_at,
            prepare_deadline,
            priority: request.priority,
        })?;
        Ok(allocation)
    }

    pub fn bind_enforced(&mut self, lease: LeaseId, start: u64) -> Result<Vec<BindingId>, Error> {
        let claims = self
            .cluster
            .leases
            .get(&lease)
            .ok_or(Error::UnknownLease(lease))?
            .allocation
            .claims
            .clone();
        let mut bindings = Vec::new();
        let mut next = start;
        for claim in claims {
            let kind = self
                .cluster
                .graph
                .node(claim.node)
                .ok_or(Error::UnknownNode(claim.node))?
                .kind;
            if !kind.is_enforced() {
                continue;
            }
            let binding = BindingId::from_u64(next);
            next += 1;
            self.apply(Command::OpenBinding {
                binding,
                lease,
                node: claim.node,
                provider: ProviderId::ENFORCE,
            })?;
            bindings.push(binding);
        }
        Ok(bindings)
    }

    pub fn activate_lease(&mut self, lease: LeaseId) -> Result<(), Error> {
        self.apply(Command::ActivateLease { lease })?;
        let bindings = self.cluster.bindings_for(lease);
        for binding in bindings {
            self.apply(Command::ActivateBinding { binding })?;
        }
        Ok(())
    }

    pub fn release_lease(&mut self, lease: LeaseId) -> Result<(), Error> {
        self.apply(Command::ReleaseLease { lease })?;
        Ok(())
    }

    pub fn revoke_lease(&mut self, lease: LeaseId) -> Result<(), Error> {
        self.apply(Command::RevokeLease { lease })?;
        Ok(())
    }

    pub fn preempt_for(&mut self, request: &Request) -> Result<Option<Vec<LeaseId>>, Error> {
        let Some(victims) = archon_kernel::preempt_victims(&self.cluster, request) else {
            return Ok(None);
        };
        for victim in &victims {
            self.revoke_lease(*victim)?;
        }
        Ok(Some(victims))
    }

    /// Fail a machine: quarantine it, revoke the leases it enforces, and take
    /// its Agent unreachable. Fence effects stay undelivered — occupancy
    /// persists until a restarted Agent's session reconciles and acknowledges
    /// the fences.
    pub fn fail_machine(&mut self, machine: NodeId) -> Result<Vec<LeaseId>, Error> {
        let node = self
            .cluster
            .graph
            .node(machine)
            .ok_or(Error::UnknownNode(machine))?;
        if node.kind != archon_kernel::NodeKind::Machine {
            return Err(Error::Invalid("fail_machine requires a machine node"));
        }
        self.apply(Command::QuarantineNode { node: machine })?;
        let victims: Vec<LeaseId> = self
            .cluster
            .leases
            .values()
            .filter(|lease| lease.parent.is_none())
            .filter(|lease| {
                lease
                    .allocation
                    .claims
                    .iter()
                    .any(|claim| self.cluster.graph.machine_of(claim.node) == Some(machine))
            })
            .filter(|lease| self.cluster.occupies(lease.id))
            .map(|lease| lease.id)
            .collect();
        for lease in &victims {
            self.revoke_lease(*lease)?;
        }
        self.agents.remove(&machine);
        Ok(victims)
    }

    pub fn unquarantine_machine(&mut self, machine: NodeId) -> Result<(), Error> {
        let _ = self.apply(Command::UnquarantineNode { node: machine })?;
        Ok(())
    }

    pub fn expire_due(&mut self) -> Result<(), Error> {
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
        for lease in due {
            self.apply(Command::ExpireLease { lease })?;
        }
        Ok(())
    }

    pub fn fail_late_prepares(&mut self) -> Result<(), Error> {
        let due: Vec<LeaseId> = self
            .cluster
            .leases
            .values()
            .filter(|lease| {
                lease.state == archon_kernel::LeaseState::Preparing
                    && self.cluster.now >= lease.prepare_deadline
            })
            .map(|lease| lease.id)
            .collect();
        for lease in due {
            self.apply(Command::FailLease {
                lease,
                reason: "prepare_deadline".into(),
            })?;
        }
        Ok(())
    }

    pub fn digest(&self) -> Digest {
        self.cluster.digest()
    }

    pub fn replay_trace(&self) -> Result<Cluster, Error> {
        let mut cluster = Cluster::new();
        for event in &self.trace {
            match event {
                TraceEvent::Command(command) => {
                    cluster.apply(command.clone())?;
                }
                TraceEvent::SetNow(now) => cluster.set_now(*now),
                TraceEvent::SetAgreement(agreed) => cluster.set_agreement(*agreed),
                TraceEvent::AdvanceEpoch => cluster.advance_epoch(),
                TraceEvent::Deliver | TraceEvent::Drop | TraceEvent::RestartAgent { .. } => {}
            }
        }
        Ok(cluster)
    }

    fn dispatch(&mut self, effect: Effect) -> Result<(), Error> {
        match effect {
            Effect::Reconcile {
                machine,
                session,
                epoch,
                bindings,
            } => self.reconcile(machine, session, epoch, bindings),
            other => {
                if let Some(command) = apply_effect(
                    &self.cluster,
                    &mut self.agents,
                    &mut self.endpoints,
                    other,
                    &mut self.next_handle,
                )? {
                    let effects = self.commit(command)?;
                    self.pending.extend(effects);
                }
                Ok(())
            }
        }
    }

    fn reconcile(
        &mut self,
        machine: NodeId,
        session: u64,
        epoch: u64,
        bindings: Vec<BindingId>,
    ) -> Result<(), Error> {
        let Some(agent) = self.agents.get_mut(&machine) else {
            return Ok(());
        };
        if epoch < agent.epoch {
            return Ok(());
        }
        agent.epoch = epoch;
        agent.session = session;
        // Leases already failed during this reconciliation pass; a second
        // unprovable sibling binding only needs its Fence, not a re-fail.
        let mut failed_leases = BTreeSet::new();
        for binding_id in bindings {
            let Some(binding) = self.cluster.bindings.get(&binding_id).cloned() else {
                continue;
            };
            let key = (binding.provider, binding.node);
            let endpoint = self.endpoints.get(&key);
            // A binding whose lease is terminal must fence on reconcile even
            // if the endpoint still looks consistent: the authority is gone.
            let lease_live = self
                .cluster
                .leases
                .get(&binding.lease)
                .is_some_and(|lease| {
                    matches!(
                        lease.state,
                        archon_kernel::LeaseState::Reserved
                            | archon_kernel::LeaseState::Preparing
                            | archon_kernel::LeaseState::Active
                    )
                });
            let same = lease_live
                && endpoint.is_some_and(|endpoint| {
                    endpoint.open
                        && endpoint.binding == Some(binding_id)
                        && endpoint.accepted_fence == binding.fence
                });
            if same {
                self.commit(Command::RebindSession {
                    binding: binding_id,
                    session,
                })?;
                if binding.state == archon_kernel::BindingState::Preparing {
                    self.pending.push_back(Effect::Prepare {
                        binding: binding_id,
                        node: binding.node,
                        provider: binding.provider,
                        fence: binding.fence,
                        session,
                        epoch,
                    });
                }
                if binding.state == archon_kernel::BindingState::Active {
                    self.pending.push_back(Effect::Activate {
                        binding: binding_id,
                        node: binding.node,
                        provider: binding.provider,
                        fence: binding.fence,
                        session,
                        epoch,
                    });
                }
            } else if matches!(
                binding.state,
                archon_kernel::BindingState::Preparing | archon_kernel::BindingState::Active
            ) {
                if lease_live && failed_leases.insert(binding.lease) {
                    // The generation could not be proven at the endpoint;
                    // authority ends before anything may reuse the claims.
                    // FailLease emits Fence effects for every open Binding of
                    // the lease, including this one.
                    self.apply(Command::FailLease {
                        lease: binding.lease,
                        reason: "reconciliation could not prove the binding".into(),
                    })?;
                    continue;
                }
                self.pending.push_back(Effect::Fence {
                    binding: binding_id,
                    node: binding.node,
                    provider: binding.provider,
                    fence: binding.fence,
                    session,
                    epoch,
                });
            }
        }
        for ((_, node), endpoint) in &self.endpoints {
            if self.cluster.graph.machine_of(*node) != Some(machine) || !endpoint.open {
                continue;
            }
            let expected = endpoint
                .binding
                .and_then(|id| self.cluster.bindings.get(&id));
            if expected.is_none_or(|binding| {
                !matches!(
                    binding.state,
                    archon_kernel::BindingState::Preparing | archon_kernel::BindingState::Active
                )
            }) && let Some(binding) = endpoint.binding
            {
                self.pending.push_back(Effect::Fence {
                    binding,
                    node: *node,
                    provider: endpoint.provider,
                    fence: endpoint.accepted_fence,
                    session,
                    epoch,
                });
            }
        }
        Ok(())
    }
}

fn enforced_under(cluster: &Cluster, machine: NodeId) -> Vec<NodeId> {
    let mut nodes = cluster.graph.descendants(machine);
    nodes.push(machine);
    nodes
        .into_iter()
        .filter(|id| {
            cluster
                .graph
                .node(*id)
                .is_some_and(|node| node.kind.is_enforced())
        })
        .collect()
}

fn apply_effect(
    cluster: &Cluster,
    agents: &mut BTreeMap<NodeId, Agent>,
    endpoints: &mut BTreeMap<(ProviderId, NodeId), Endpoint>,
    effect: Effect,
    next_handle: &mut u64,
) -> Result<Option<Command>, Error> {
    let (op, binding, node, provider, fence, session, epoch) = match effect {
        Effect::Prepare {
            binding,
            node,
            provider,
            fence,
            session,
            epoch,
        } => (
            EndpointOp::Prepare,
            binding,
            node,
            provider,
            fence,
            session,
            epoch,
        ),
        Effect::Activate {
            binding,
            node,
            provider,
            fence,
            session,
            epoch,
        } => (
            EndpointOp::Activate,
            binding,
            node,
            provider,
            fence,
            session,
            epoch,
        ),
        Effect::Release {
            binding,
            node,
            provider,
            fence,
            session,
            epoch,
        } => (
            EndpointOp::Release,
            binding,
            node,
            provider,
            fence,
            session,
            epoch,
        ),
        Effect::Fence {
            binding,
            node,
            provider,
            fence,
            session,
            epoch,
        } => (
            EndpointOp::Fence,
            binding,
            node,
            provider,
            fence,
            session,
            epoch,
        ),
        Effect::Reconcile { .. } => return Ok(None),
    };
    let Some(machine) = cluster.graph.machine_of(node) else {
        return Ok(None);
    };
    let Some(agent) = agents.get_mut(&machine) else {
        return Ok(None);
    };
    if epoch < agent.epoch || session != agent.session {
        return Ok(None);
    }
    if epoch > agent.epoch {
        agent.epoch = epoch;
    }
    let endpoint = endpoints
        .entry((provider, node))
        .or_insert_with(|| Endpoint::new(provider, node, agent.session));
    match endpoint.apply(op, binding, fence, session) {
        Ok(()) => {
            let command = match op {
                EndpointOp::Prepare => {
                    let handle = *next_handle;
                    *next_handle += 1;
                    Command::RecordBindingPrepared {
                        binding,
                        session,
                        provider_handle: handle,
                        fence,
                    }
                }
                EndpointOp::Activate => Command::RecordBindingActive {
                    binding,
                    session,
                    fence,
                },
                EndpointOp::Release => Command::RecordBindingReleased {
                    binding,
                    session,
                    fence,
                },
                EndpointOp::Fence => Command::RecordBindingFenced {
                    binding,
                    session,
                    fence,
                },
            };
            return Ok(Some(command));
        }
        Err(_) => {
            if matches!(op, EndpointOp::Prepare) {
                return Ok(Some(Command::RecordBindingFailed {
                    binding,
                    session,
                    reason: "endpoint rejected prepare".into(),
                    fence,
                }));
            }
        }
    }
    Ok(None)
}
