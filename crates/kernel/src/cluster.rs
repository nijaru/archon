use std::collections::{BTreeMap, BTreeSet};

use crate::command::{Command, Effect};
use crate::error::Error;
use crate::graph::Graph;
use crate::ids::{BindingId, LeaseId, NodeId};
use crate::occupancy::{claim_fits, covers, occupancy_from_leases, resolve_claim, subtree_used};
use crate::types::{Binding, BindingState, Lease, LeaseState, NodeState, Quantity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseDigest {
    pub state: LeaseState,
    pub exit_code: Option<i32>,
    pub owner: crate::ids::OwnerId,
    pub parent: Option<LeaseId>,
    pub priority: u32,
    pub expires_at: u64,
    pub prepare_deadline: u64,
    pub graph_revision: u64,
    pub claims: Vec<(NodeId, Quantity)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingDigest {
    pub state: BindingState,
    pub lease: LeaseId,
    pub node: NodeId,
    pub provider: crate::ids::ProviderId,
    pub fence: u64,
    pub session: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digest {
    pub epoch: u64,
    pub now: u64,
    pub agreed: bool,
    pub graph_revision: u64,
    pub graph_nodes: BTreeMap<NodeId, (crate::types::ResourceClass, Quantity, crate::types::Attrs)>,
    pub graph_edges: Vec<(NodeId, NodeId, crate::types::EdgeKind)>,
    pub leases: BTreeMap<LeaseId, LeaseDigest>,
    pub bindings: BTreeMap<BindingId, BindingDigest>,
    pub sessions: BTreeMap<NodeId, u64>,
    pub last_fence: BTreeMap<(crate::ids::ProviderId, NodeId), u64>,
    pub node_states: BTreeMap<NodeId, NodeState>,
}

/// (De)hydrate a tuple-keyed map as a sequence of pairs.
#[cfg(feature = "serde")]
mod tuple_key_map {
    use crate::ids::NodeId;
    use crate::ids::ProviderId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(
        map: &BTreeMap<(ProviderId, NodeId), u64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let pairs: Vec<((ProviderId, NodeId), u64)> =
            map.iter().map(|(key, value)| (*key, *value)).collect();
        pairs.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<(ProviderId, NodeId), u64>, D::Error> {
        let pairs: Vec<((ProviderId, NodeId), u64)> = Vec::deserialize(deserializer)?;
        Ok(pairs.into_iter().collect())
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cluster {
    pub epoch: u64,
    pub now: u64,
    pub agreed: bool,
    pub graph: Graph,
    /// In-process history of applied commands. Recovery replays the
    /// durable JSONL log instead, so this never enters snapshots.
    #[cfg_attr(feature = "serde", serde(skip_serializing, default))]
    pub log: Vec<Command>,
    pub leases: BTreeMap<LeaseId, Lease>,
    pub bindings: BTreeMap<BindingId, Binding>,
    pub sessions: BTreeMap<NodeId, u64>,
    /// Serde JSON cannot use tuple keys in objects, so the map travels as
    /// a vector of pairs.
    #[cfg_attr(
        feature = "serde",
        serde(
            with = "tuple_key_map",
            default,
            skip_serializing_if = "BTreeMap::is_empty"
        )
    )]
    pub last_fence: BTreeMap<(crate::ids::ProviderId, NodeId), u64>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "BTreeMap::is_empty")
    )]
    pub node_states: BTreeMap<NodeId, NodeState>,
}

impl Default for Cluster {
    fn default() -> Self {
        Self {
            epoch: 1,
            now: 0,
            agreed: true,
            graph: Graph::new(),
            log: Vec::new(),
            leases: BTreeMap::new(),
            bindings: BTreeMap::new(),
            sessions: BTreeMap::new(),
            last_fence: BTreeMap::new(),
            node_states: BTreeMap::new(),
        }
    }
}

impl Cluster {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_now(&mut self, now: u64) {
        self.now = now;
    }

    pub fn set_agreement(&mut self, agreed: bool) {
        self.agreed = agreed;
    }

    /// Returns the authoritative control state for a known node. Graph nodes
    /// without an explicit entry use the proof-era default: schedulable.
    pub fn node_state(&self, node: NodeId) -> Option<NodeState> {
        self.graph.node(node).map(|_| {
            self.node_states
                .get(&node)
                .copied()
                .unwrap_or(NodeState::Schedulable)
        })
    }

    pub(crate) fn placement_blocked(&self) -> BTreeSet<NodeId> {
        self.graph
            .nodes()
            .filter(|node| {
                !self
                    .node_state(node.id)
                    .is_some_and(NodeState::is_schedulable)
            })
            .map(|node| node.id)
            .collect()
    }

    pub fn advance_epoch(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
    }

    pub fn apply(&mut self, command: Command) -> Result<Vec<Effect>, Error> {
        let effects = self.dispatch(&command)?;
        self.log.push(command);
        Ok(effects)
    }

    /// Replay the command log into a fresh Cluster. Clock, agreement, and
    /// epoch changes live outside the command log; replaying a trace that
    /// used them needs the simulator's `World::replay_trace`, which carries
    /// those events.
    pub fn replay(commands: &[Command]) -> Result<Self, Error> {
        let mut cluster = Self::new();
        for command in commands {
            cluster.apply(command.clone())?;
        }
        Ok(cluster)
    }

    pub fn digest(&self) -> Digest {
        Digest {
            epoch: self.epoch,
            now: self.now,
            agreed: self.agreed,
            graph_revision: self.graph.revision,
            graph_nodes: self
                .graph
                .nodes()
                .map(|node| {
                    (
                        node.id,
                        (node.kind, node.capacity.clone(), node.attrs.clone()),
                    )
                })
                .collect(),
            graph_edges: self
                .graph
                .edges()
                .iter()
                .map(|edge| (edge.from, edge.to, edge.kind))
                .collect(),
            leases: self
                .leases
                .iter()
                .map(|(id, lease)| {
                    (
                        *id,
                        LeaseDigest {
                            state: lease.state,
                            exit_code: lease.exit_code,
                            owner: lease.owner,
                            parent: lease.parent,
                            priority: lease.priority,
                            expires_at: lease.expires_at,
                            prepare_deadline: lease.prepare_deadline,
                            graph_revision: lease.allocation.graph_revision,
                            claims: lease
                                .allocation
                                .claims
                                .iter()
                                .map(|claim| (claim.node, claim.quantity.clone()))
                                .collect(),
                        },
                    )
                })
                .collect(),
            bindings: self
                .bindings
                .iter()
                .map(|(id, binding)| {
                    (
                        *id,
                        BindingDigest {
                            state: binding.state,
                            lease: binding.lease,
                            node: binding.node,
                            provider: binding.provider,
                            fence: binding.fence,
                            session: binding.agent_session,
                        },
                    )
                })
                .collect(),
            sessions: self.sessions.clone(),
            last_fence: self.last_fence.clone(),
            node_states: self.node_states.clone(),
        }
    }

    pub fn occupancy_except(&self, except: &BTreeSet<LeaseId>) -> crate::occupancy::Occupancy {
        occupancy_from_leases(self.leases.values(), &self.open_binding_leases(), except)
    }

    pub fn occupies(&self, lease: LeaseId) -> bool {
        self.leases.get(&lease).is_some_and(|lease| {
            matches!(
                lease.state,
                LeaseState::Reserved | LeaseState::Preparing | LeaseState::Active
            ) || self.open_bindings(lease.id)
        })
    }

    pub fn children(&self, parent: LeaseId) -> Vec<LeaseId> {
        self.leases
            .values()
            .filter(|lease| lease.parent == Some(parent))
            .map(|lease| lease.id)
            .collect()
    }

    pub fn descendants_postorder(&self, parent: LeaseId) -> Vec<LeaseId> {
        let mut out = Vec::new();
        self.walk_descendants(parent, &mut out);
        out
    }

    pub fn bindings_for(&self, lease: LeaseId) -> Vec<BindingId> {
        self.bindings
            .values()
            .filter(|binding| binding.lease == lease)
            .map(|binding| binding.id)
            .collect()
    }

    pub fn machine_bindings(&self, machine: NodeId) -> Vec<BindingId> {
        self.bindings
            .values()
            .filter(|binding| self.graph.machine_of(binding.node) == Some(machine))
            .map(|binding| binding.id)
            .collect()
    }

    pub fn live_machine_bindings(&self, machine: NodeId) -> Vec<BindingId> {
        self.machine_bindings(machine)
            .into_iter()
            .filter(|id| {
                self.bindings.get(id).is_some_and(|binding| {
                    matches!(
                        binding.state,
                        BindingState::Preparing | BindingState::Active
                    )
                })
            })
            .collect()
    }

    fn walk_descendants(&self, parent: LeaseId, out: &mut Vec<LeaseId>) {
        let mut children = self.children(parent);
        children.sort();
        for child in children {
            self.walk_descendants(child, out);
            out.push(child);
        }
    }

    fn open_bindings(&self, lease: LeaseId) -> bool {
        self.bindings
            .values()
            .any(|binding| binding.lease == lease && !binding.state.is_closed())
    }

    pub(crate) fn open_binding_leases(&self) -> BTreeSet<LeaseId> {
        self.bindings
            .values()
            .filter(|binding| !binding.state.is_closed())
            .map(|binding| binding.lease)
            .collect()
    }

    fn node_quarantined(&self, node: NodeId) -> bool {
        let blocked = |candidate: NodeId| {
            self.node_states
                .get(&candidate)
                .is_some_and(|state| !state.is_schedulable())
        };
        blocked(node) || self.graph.ancestors(node).into_iter().any(blocked)
    }

    fn require_session(&self, node: NodeId, session: u64) -> Result<NodeId, Error> {
        let machine = self
            .graph
            .machine_of(node)
            .ok_or(Error::UnknownNode(node))?;
        let expected = self
            .sessions
            .get(&machine)
            .copied()
            .ok_or(Error::NoAgent { machine })?;
        if expected != session {
            return Err(Error::StaleSession {
                expected,
                got: session,
            });
        }
        Ok(machine)
    }

    fn binding_effect(
        &self,
        binding: &Binding,
        kind: fn(BindingId, NodeId, crate::ids::ProviderId, u64, u64, u64) -> Effect,
    ) -> Effect {
        let session = self
            .graph
            .machine_of(binding.node)
            .and_then(|machine| self.sessions.get(&machine).copied())
            .unwrap_or(binding.agent_session);
        kind(
            binding.id,
            binding.node,
            binding.provider,
            binding.fence,
            session,
            self.epoch,
        )
    }

    fn dispatch(&mut self, command: &Command) -> Result<Vec<Effect>, Error> {
        match command {
            Command::ApplyGraph { nodes, edges } => self.apply_graph(nodes.clone(), edges.clone()),
            command @ Command::ReserveLease { .. } => self.reserve_lease(command),
            Command::PromoteLease {
                lease,
                prepare_deadline,
            } => self.promote_lease(*lease, *prepare_deadline),
            command @ Command::OpenLease { .. } => self.open_lease(command),
            Command::ActivateLease { lease } => self.activate_lease(*lease),
            Command::FailLease { lease, reason } => self.fail_lease(*lease, reason),
            Command::ReleaseLease { lease } => self.release_lease(*lease),
            Command::RevokeLease { lease } => self.revoke_lease(*lease),
            Command::ExpireLease { lease } => self.expire_lease(*lease),
            Command::CompleteLease { lease, exit_code } => self.complete_lease(*lease, *exit_code),
            Command::RenewLease {
                lease,
                new_expires_at,
            } => self.renew_lease(*lease, *new_expires_at),
            Command::OpenBinding {
                binding,
                lease,
                node,
                provider,
            } => self.open_binding(*binding, *lease, *node, *provider),
            Command::ActivateBinding { binding } => self.activate_binding(*binding),
            Command::FenceBinding { binding } => self.fence_binding(*binding),
            Command::FailBinding { binding, reason } => self.fail_binding(*binding, reason),
            Command::ReleaseBinding { binding } => self.release_binding(*binding),
            Command::RecordBindingPrepared {
                binding,
                session,
                provider_handle,
                fence,
            } => self.record_prepared(*binding, *session, *provider_handle, *fence),
            Command::RecordBindingActive {
                binding,
                session,
                fence,
            } => self.record_active(*binding, *session, *fence),
            Command::RecordBindingReleased {
                binding,
                session,
                fence,
            } => self.record_released(*binding, *session, *fence),
            Command::RecordBindingFenced {
                binding,
                session,
                fence,
            } => self.record_fenced(*binding, *session, *fence),
            Command::RecordBindingFailed {
                binding,
                session,
                reason,
                fence,
            } => self.record_failed(*binding, *session, reason, *fence),
            Command::SetAgentSession { machine, session } => {
                self.set_agent_session(*machine, *session)
            }
            Command::SetNodeHealth { node, health } => self.set_node_health(*node, health.clone()),
            Command::SetNodeState { node, state } => self.set_node_state(*node, *state),
            Command::RebindSession { binding, session } => self.rebind_session(*binding, *session),
            Command::QuarantineNode { node } => self.quarantine_node(*node),
            Command::UnquarantineNode { node } => self.unquarantine_node(*node),
        }
    }

    fn apply_graph(
        &mut self,
        nodes: Vec<crate::types::Node>,
        edges: Vec<crate::types::Edge>,
    ) -> Result<Vec<Effect>, Error> {
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        let mut staged = self.graph.clone();
        staged.apply(nodes, edges)?;
        // Capacity may grow freely but never drop below what live leases
        // already hold: saturating arithmetic would hide the overcommit.
        let occupancy = self.occupancy();
        if let Some(node) = occupancy.exceeds_capacity(&staged)? {
            return Err(Error::CapacityBelowOccupancy { node });
        }
        self.graph = staged;
        Ok(Vec::new())
    }

    fn reserve_lease(&mut self, command: &Command) -> Result<Vec<Effect>, Error> {
        let Command::ReserveLease {
            lease: id,
            owner,
            allocation,
            expires_at,
            priority,
        } = command
        else {
            return Err(Error::Invalid("reserve_lease requires ReserveLease"));
        };
        let id = *id;
        let owner = *owner;
        let expires_at = *expires_at;
        let priority = *priority;
        let mut allocation = allocation.clone();
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        if let Some(existing) = self.leases.get(&id) {
            if existing.state == LeaseState::Reserved
                && existing.owner == owner
                && existing.allocation.claims == allocation.claims
                && existing.expires_at == expires_at
            {
                return Ok(Vec::new());
            }
            return Err(Error::DuplicateLease(id));
        }
        if allocation.graph_revision != self.graph.revision {
            return Err(Error::StaleGraphRevision {
                current: self.graph.revision,
                got: allocation.graph_revision,
            });
        }
        let mut claims = Vec::new();
        let mut seen_nodes = BTreeSet::new();
        for claim in &allocation.claims {
            if !seen_nodes.insert(claim.node) {
                return Err(Error::DuplicateClaim { node: claim.node });
            }
            claims.push(resolve_claim(&self.graph, claim)?);
        }
        for claim in &claims {
            if self.node_quarantined(claim.node) {
                return Err(Error::Quarantined(claim.node));
            }
        }
        let except = BTreeSet::from([id]);
        let occupancy =
            occupancy_from_leases(self.leases.values(), &self.open_binding_leases(), &except);
        for claim in &claims {
            if !occupancy.can_cover(&self.graph, claim)? {
                return Err(Error::Overlap { node: claim.node });
            }
        }
        allocation.claims = claims;
        self.leases.insert(
            id,
            Lease {
                id,
                owner,
                allocation,
                parent: None,
                expires_at,
                prepare_deadline: 0,
                priority,
                state: LeaseState::Reserved,
                exit_code: None,
            },
        );
        Ok(Vec::new())
    }

    fn promote_lease(&mut self, id: LeaseId, prepare_deadline: u64) -> Result<Vec<Effect>, Error> {
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        let lease = self.leases.get(&id).ok_or(Error::UnknownLease(id))?;
        if lease.state == LeaseState::Preparing {
            return Ok(Vec::new());
        }
        if lease.state != LeaseState::Reserved {
            return Err(Error::LeaseState {
                lease: id,
                state: lease.state,
            });
        }
        // Revalidate the committed claims against the CURRENT graph: any
        // ApplyGraph advances the revision, and an unrelated update must not
        // strand a reservation that still fits.
        let claims: Vec<crate::types::Claim> = lease
            .allocation
            .claims
            .iter()
            .map(|claim| resolve_claim(&self.graph, claim))
            .collect::<Result<_, _>>()?;
        for claim in &claims {
            if self.node_quarantined(claim.node) {
                return Err(Error::Quarantined(claim.node));
            }
        }
        if self.now >= lease.expires_at {
            return Err(Error::LeaseExpired { lease: id });
        }
        let except = BTreeSet::from([id]);
        let occupancy =
            occupancy_from_leases(self.leases.values(), &self.open_binding_leases(), &except);
        for claim in &claims {
            if !occupancy.can_cover(&self.graph, claim)? {
                return Err(Error::Overlap { node: claim.node });
            }
        }
        let revision = self.graph.revision;
        if let Some(lease) = self.leases.get_mut(&id) {
            lease.allocation.claims = claims;
            lease.allocation.graph_revision = revision;
            lease.state = LeaseState::Preparing;
            lease.prepare_deadline = prepare_deadline;
        }
        Ok(Vec::new())
    }

    fn open_lease(&mut self, command: &Command) -> Result<Vec<Effect>, Error> {
        let Command::OpenLease {
            lease: id,
            owner,
            allocation,
            parent,
            expires_at,
            prepare_deadline,
            priority,
        } = command
        else {
            return Err(Error::Invalid("open_lease requires OpenLease"));
        };
        let id = *id;
        let owner = *owner;
        let parent = *parent;
        let expires_at = *expires_at;
        let prepare_deadline = *prepare_deadline;
        let priority = *priority;
        let mut allocation = allocation.clone();
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        if let Some(existing) = self.leases.get(&id) {
            if existing.owner == owner
                && existing.parent == parent
                && existing.allocation.claims == allocation.claims
            {
                return Ok(Vec::new());
            }
            return Err(Error::DuplicateLease(id));
        }
        if allocation.graph_revision != self.graph.revision {
            return Err(Error::StaleGraphRevision {
                current: self.graph.revision,
                got: allocation.graph_revision,
            });
        }
        let mut claims = Vec::new();
        let mut seen_nodes = BTreeSet::new();
        for claim in &allocation.claims {
            if !seen_nodes.insert(claim.node) {
                return Err(Error::DuplicateClaim { node: claim.node });
            }
            claims.push(resolve_claim(&self.graph, claim)?);
        }
        for claim in &claims {
            if self.node_quarantined(claim.node) {
                return Err(Error::Quarantined(claim.node));
            }
        }
        if let Some(parent_id) = parent {
            let parent_lease = self
                .leases
                .get(&parent_id)
                .ok_or(Error::UnknownLease(parent_id))?;
            if parent_lease.state != LeaseState::Active {
                return Err(Error::ParentNotActive { parent: parent_id });
            }
            if !covers(&parent_lease.allocation.claims, &claims) {
                return Err(Error::ChildEscapesParent {
                    child: id,
                    parent: parent_id,
                });
            }
            for claim in &claims {
                let parent_qty = parent_lease
                    .allocation
                    .claims
                    .iter()
                    .find(|parent_claim| parent_claim.node == claim.node)
                    .map(|parent_claim| &parent_claim.quantity)
                    .ok_or(Error::ChildEscapesParent {
                        child: id,
                        parent: parent_id,
                    })?;
                let sibling_qty =
                    subtree_used(&self.leases, &self.open_binding_leases(), parent_id, id)
                        .used_on(claim.node);
                if !claim_fits(parent_qty, &sibling_qty, &claim.quantity) {
                    return Err(Error::Overlap { node: claim.node });
                }
            }
        } else {
            let except = BTreeSet::from([id]);
            let occupancy =
                occupancy_from_leases(self.leases.values(), &self.open_binding_leases(), &except);
            for claim in &claims {
                if !occupancy.can_cover(&self.graph, claim)? {
                    return Err(Error::Overlap { node: claim.node });
                }
            }
        }
        allocation.claims = claims;
        self.leases.insert(
            id,
            Lease {
                id,
                owner,
                allocation,
                parent,
                expires_at,
                prepare_deadline,
                priority,
                state: LeaseState::Preparing,
                exit_code: None,
            },
        );
        Ok(Vec::new())
    }

    fn activate_lease(&mut self, id: LeaseId) -> Result<Vec<Effect>, Error> {
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        let lease = self.leases.get(&id).ok_or(Error::UnknownLease(id))?;
        if lease.state == LeaseState::Active {
            return Ok(Vec::new());
        }
        if lease.state != LeaseState::Preparing {
            return Err(Error::LeaseState {
                lease: id,
                state: lease.state,
            });
        }
        if lease.allocation.graph_revision != self.graph.revision {
            return Err(Error::StaleGraphRevision {
                current: self.graph.revision,
                got: lease.allocation.graph_revision,
            });
        }
        let claims = lease.allocation.claims.clone();
        let parent = lease.parent;
        for claim in &claims {
            if self.node_quarantined(claim.node) {
                return Err(Error::Quarantined(claim.node));
            }
        }
        let now = self.now;
        let (expires_at, prepare_deadline) = (lease.expires_at, lease.prepare_deadline);
        if now >= expires_at {
            return Err(Error::LeaseExpired { lease: id });
        }
        if now > prepare_deadline {
            return Err(Error::PrepareDeadlinePassed { lease: id });
        }
        if let Some(parent_id) = parent {
            let parent_lease = self
                .leases
                .get(&parent_id)
                .ok_or(Error::UnknownLease(parent_id))?;
            if parent_lease.state != LeaseState::Active {
                return Err(Error::ParentNotActive { parent: parent_id });
            }
        } else {
            let except = BTreeSet::from([id]);
            let occupancy =
                occupancy_from_leases(self.leases.values(), &self.open_binding_leases(), &except);
            for claim in &claims {
                if !occupancy.can_cover(&self.graph, claim)? {
                    return Err(Error::Overlap { node: claim.node });
                }
            }
        }
        let required: Vec<BindingId> = self
            .bindings
            .values()
            .filter(|binding| binding.lease == id)
            .map(|binding| binding.id)
            .collect();
        if required.iter().any(|binding_id| {
            self.bindings.get(binding_id).is_none_or(|binding| {
                binding.state != BindingState::Preparing || binding.provider_handle.is_none()
            })
        }) {
            return Err(Error::BindingsNotPrepared { lease: id });
        }
        // A root lease must enforce every enforced claim through a prepared
        // Binding before it becomes Active; accounting-only children may
        // activate without Bindings.
        if parent.is_none() {
            for claim in &claims {
                let enforced = self
                    .graph
                    .node(claim.node)
                    .is_some_and(|node| node.kind.is_enforced());
                if !enforced {
                    continue;
                }
                let covered = self.bindings.values().any(|binding| {
                    binding.lease == id
                        && binding.node == claim.node
                        && binding.state == BindingState::Preparing
                        && binding.provider_handle.is_some()
                });
                if !covered {
                    return Err(Error::BindingsNotPrepared { lease: id });
                }
            }
        }
        if let Some(lease) = self.leases.get_mut(&id) {
            lease.state = LeaseState::Active;
        }
        Ok(Vec::new())
    }

    fn fail_lease(&mut self, id: LeaseId, _reason: &str) -> Result<Vec<Effect>, Error> {
        let mut effects = Vec::new();
        for descendant in self.descendants_postorder(id) {
            if let Some(lease) = self.leases.get_mut(&descendant)
                && !matches!(
                    lease.state,
                    LeaseState::Failed
                        | LeaseState::Released
                        | LeaseState::Revoked
                        | LeaseState::Expired
                )
            {
                lease.state = LeaseState::Failed;
            }
            effects.extend(self.fence_effects(descendant));
        }
        let lease = self.leases.get_mut(&id).ok_or(Error::UnknownLease(id))?;
        if matches!(
            lease.state,
            LeaseState::Failed | LeaseState::Released | LeaseState::Revoked | LeaseState::Expired
        ) {
            return Ok(effects);
        }
        lease.state = LeaseState::Failed;
        effects.extend(self.fence_effects(id));
        Ok(effects)
    }

    fn live_descendants(&self, id: LeaseId) -> Vec<LeaseId> {
        self.descendants_postorder(id)
            .into_iter()
            .filter(|descendant| {
                self.leases
                    .get(descendant)
                    .is_some_and(|lease| self.occupies(lease.id))
            })
            .collect()
    }

    fn release_lease(&mut self, id: LeaseId) -> Result<Vec<Effect>, Error> {
        // Releasing a lease ends the authority its descendants borrow, so the
        // owner must terminate them first.
        let live = self.live_descendants(id);
        if !live.is_empty() {
            return Err(Error::HasLiveDescendants { lease: id });
        }
        let lease = self.leases.get_mut(&id).ok_or(Error::UnknownLease(id))?;
        if lease.state == LeaseState::Released {
            return Ok(self.release_effects(id));
        }
        if !matches!(lease.state, LeaseState::Active | LeaseState::Reserved) {
            return Err(Error::LeaseState {
                lease: id,
                state: lease.state,
            });
        }
        lease.state = LeaseState::Released;
        Ok(self.release_effects(id))
    }

    fn revoke_lease(&mut self, id: LeaseId) -> Result<Vec<Effect>, Error> {
        let mut effects = Vec::new();
        let descendants = self.descendants_postorder(id);
        for child in descendants {
            if let Some(lease) = self.leases.get_mut(&child) {
                if !matches!(
                    lease.state,
                    LeaseState::Revoked
                        | LeaseState::Released
                        | LeaseState::Expired
                        | LeaseState::Failed
                ) {
                    lease.state = LeaseState::Revoked;
                }
                effects.extend(self.fence_effects(child));
            }
        }
        let lease = self.leases.get_mut(&id).ok_or(Error::UnknownLease(id))?;
        if !matches!(
            lease.state,
            LeaseState::Revoked | LeaseState::Released | LeaseState::Expired | LeaseState::Failed
        ) {
            lease.state = LeaseState::Revoked;
        }
        effects.extend(self.fence_effects(id));
        Ok(effects)
    }

    fn expire_lease(&mut self, id: LeaseId) -> Result<Vec<Effect>, Error> {
        let now = self.now;
        // Validate the parent completely before touching descendants: a
        // rejected expiry must mutate nothing and stay replay-consistent.
        let (state, expires_at) = {
            let lease = self.leases.get(&id).ok_or(Error::UnknownLease(id))?;
            (lease.state, lease.expires_at)
        };
        if state == LeaseState::Expired {
            return Ok(self.fence_effects(id));
        }
        if !matches!(
            state,
            LeaseState::Preparing | LeaseState::Active | LeaseState::Reserved
        ) {
            return Err(Error::LeaseState { lease: id, state });
        }
        if now < expires_at {
            return Err(Error::ExpireNotDue);
        }
        let mut effects = Vec::new();
        // Expiry terminates the parent's authority, so live descendants expire
        // with it and their Bindings fence, mirroring revocation.
        for descendant in self.descendants_postorder(id) {
            if let Some(lease) = self.leases.get_mut(&descendant)
                && !matches!(
                    lease.state,
                    LeaseState::Revoked
                        | LeaseState::Released
                        | LeaseState::Expired
                        | LeaseState::Failed
                )
            {
                lease.state = LeaseState::Expired;
            }
            effects.extend(self.fence_effects(descendant));
        }
        let lease = self.leases.get_mut(&id).ok_or(Error::UnknownLease(id))?;
        lease.state = LeaseState::Expired;
        effects.extend(self.fence_effects(id));
        Ok(effects)
    }

    /// A workload exited on its own. Success completes the lease; failure
    /// fails it. Either way authority ends and bindings fence.
    fn complete_lease(&mut self, id: LeaseId, exit_code: i32) -> Result<Vec<Effect>, Error> {
        let final_state = if exit_code == 0 {
            LeaseState::Completed
        } else {
            LeaseState::Failed
        };
        let state = {
            let lease = self.leases.get(&id).ok_or(Error::UnknownLease(id))?;
            lease.state
        };
        if matches!(state, LeaseState::Completed | LeaseState::Failed) {
            return Ok(self.fence_effects(id));
        }
        if !matches!(
            state,
            LeaseState::Preparing | LeaseState::Active | LeaseState::Reserved
        ) {
            return Err(Error::LeaseState { lease: id, state });
        }
        let mut effects = Vec::new();
        for descendant in self.descendants_postorder(id) {
            if let Some(lease) = self.leases.get_mut(&descendant)
                && !matches!(
                    lease.state,
                    LeaseState::Revoked
                        | LeaseState::Released
                        | LeaseState::Expired
                        | LeaseState::Failed
                        | LeaseState::Completed
                )
            {
                lease.state = final_state;
            }
            effects.extend(self.fence_effects(descendant));
        }
        let lease = self.leases.get_mut(&id).ok_or(Error::UnknownLease(id))?;
        lease.state = final_state;
        lease.exit_code = Some(exit_code);
        effects.extend(self.fence_effects(id));
        Ok(effects)
    }

    fn renew_lease(&mut self, id: LeaseId, new_expires_at: u64) -> Result<Vec<Effect>, Error> {
        if !self.agreed {
            return Err(Error::NotAgreed);
        }
        let now = self.now;
        let lease = self.leases.get_mut(&id).ok_or(Error::UnknownLease(id))?;
        if !matches!(lease.state, LeaseState::Active | LeaseState::Reserved) {
            return Err(Error::LeaseState {
                lease: id,
                state: lease.state,
            });
        }
        if new_expires_at <= lease.expires_at || new_expires_at <= now {
            return Err(Error::RenewNotLater);
        }
        lease.expires_at = new_expires_at;
        Ok(Vec::new())
    }

    fn open_binding(
        &mut self,
        id: BindingId,
        lease_id: LeaseId,
        node: NodeId,
        provider: crate::ids::ProviderId,
    ) -> Result<Vec<Effect>, Error> {
        if let Some(existing) = self.bindings.get(&id) {
            if existing.lease == lease_id && existing.node == node && existing.provider == provider
            {
                // Retry of a committed OpenBinding: re-emit Prepare only while
                // still preparing; never disturb Active or closed Bindings.
                if existing.state == BindingState::Preparing {
                    return Ok(vec![self.binding_effect(
                        existing,
                        |binding, node, provider, fence, session, epoch| Effect::Prepare {
                            binding,
                            node,
                            provider,
                            fence,
                            session,
                            epoch,
                        },
                    )]);
                }
                return Ok(Vec::new());
            }
            return Err(Error::DuplicateBinding(id));
        }
        let lease = self
            .leases
            .get(&lease_id)
            .ok_or(Error::UnknownLease(lease_id))?;
        if lease.state != LeaseState::Preparing {
            return Err(Error::LeaseState {
                lease: lease_id,
                state: lease.state,
            });
        }
        // Children are accounting records enforced through their parent's
        // Bindings; a second Binding on the same node would take over the
        // shared endpoint and orphan the parent's enforcement.
        if lease.parent.is_some() {
            return Err(Error::ChildBindingRefused { lease: lease_id });
        }
        if !lease
            .allocation
            .claims
            .iter()
            .any(|claim| claim.node == node)
        {
            return Err(Error::Invalid("binding node is not in lease claims"));
        }
        if self.node_quarantined(node) {
            return Err(Error::Quarantined(node));
        }
        let machine = self
            .graph
            .machine_of(node)
            .ok_or(Error::UnknownNode(node))?;
        let session = self
            .sessions
            .get(&machine)
            .copied()
            .ok_or(Error::NoAgent { machine })?;
        let fence = self.last_fence.get(&(provider, node)).copied().unwrap_or(0) + 1;
        self.last_fence.insert((provider, node), fence);
        let binding = Binding {
            id,
            lease: lease_id,
            node,
            provider,
            fence,
            agent_session: session,
            state: BindingState::Preparing,
            provider_handle: None,
        };
        let effect = self.binding_effect(
            &binding,
            |binding, node, provider, fence, session, epoch| Effect::Prepare {
                binding,
                node,
                provider,
                fence,
                session,
                epoch,
            },
        );
        self.bindings.insert(id, binding);
        Ok(vec![effect])
    }

    fn activate_binding(&mut self, id: BindingId) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        let lease = self
            .leases
            .get(&binding.lease)
            .ok_or(Error::UnknownLease(binding.lease))?;
        if lease.state != LeaseState::Active {
            return Err(Error::LeaseState {
                lease: lease.id,
                state: lease.state,
            });
        }
        if binding.state == BindingState::Active {
            return Ok(vec![self.binding_effect(
                binding,
                |binding, node, provider, fence, session, epoch| Effect::Activate {
                    binding,
                    node,
                    provider,
                    fence,
                    session,
                    epoch,
                },
            )]);
        }
        if binding.state != BindingState::Preparing {
            return Err(Error::BindingState {
                binding: id,
                state: binding.state,
            });
        }
        let effect =
            self.binding_effect(binding, |binding, node, provider, fence, session, epoch| {
                Effect::Activate {
                    binding,
                    node,
                    provider,
                    fence,
                    session,
                    epoch,
                }
            });
        if let Some(binding) = self.bindings.get_mut(&id) {
            binding.state = BindingState::Active;
        }
        Ok(vec![effect])
    }

    fn fence_binding(&mut self, id: BindingId) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        if binding.state == BindingState::Fenced {
            return Ok(vec![self.binding_effect(
                binding,
                |binding, node, provider, fence, session, epoch| Effect::Fence {
                    binding,
                    node,
                    provider,
                    fence,
                    session,
                    epoch,
                },
            )]);
        }
        if binding.state == BindingState::Released {
            return Ok(Vec::new());
        }
        let effect =
            self.binding_effect(binding, |binding, node, provider, fence, session, epoch| {
                Effect::Fence {
                    binding,
                    node,
                    provider,
                    fence,
                    session,
                    epoch,
                }
            });
        Ok(vec![effect])
    }

    fn fail_binding(&mut self, id: BindingId, _reason: &str) -> Result<Vec<Effect>, Error> {
        let binding = self
            .bindings
            .get_mut(&id)
            .ok_or(Error::UnknownBinding(id))?;
        if binding.state.is_closed() {
            return Ok(Vec::new());
        }
        binding.state = BindingState::Failed;
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        Ok(vec![self.binding_effect(
            binding,
            |binding, node, provider, fence, session, epoch| Effect::Fence {
                binding,
                node,
                provider,
                fence,
                session,
                epoch,
            },
        )])
    }

    fn release_binding(&mut self, id: BindingId) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        if binding.state == BindingState::Released {
            return Ok(vec![self.binding_effect(
                binding,
                |binding, node, provider, fence, session, epoch| Effect::Release {
                    binding,
                    node,
                    provider,
                    fence,
                    session,
                    epoch,
                },
            )]);
        }
        if binding.state.is_closed() {
            return Ok(Vec::new());
        }
        let effect =
            self.binding_effect(binding, |binding, node, provider, fence, session, epoch| {
                Effect::Release {
                    binding,
                    node,
                    provider,
                    fence,
                    session,
                    epoch,
                }
            });
        Ok(vec![effect])
    }

    fn record_prepared(
        &mut self,
        id: BindingId,
        session: u64,
        handle: u64,
        fence: u64,
    ) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        self.require_session(binding.node, session)?;
        self.require_fence(id, binding.fence, fence)?;
        if binding.state == BindingState::Preparing || binding.state == BindingState::Active {
            if let Some(binding) = self.bindings.get_mut(&id) {
                binding.provider_handle = Some(handle);
                binding.agent_session = session;
            }
            return Ok(Vec::new());
        }
        Err(Error::BindingState {
            binding: id,
            state: binding.state,
        })
    }

    fn record_active(
        &mut self,
        id: BindingId,
        session: u64,
        fence: u64,
    ) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        self.require_session(binding.node, session)?;
        self.require_fence(id, binding.fence, fence)?;
        if binding.state == BindingState::Active {
            if let Some(binding) = self.bindings.get_mut(&id) {
                binding.agent_session = session;
            }
            return Ok(Vec::new());
        }
        Err(Error::BindingState {
            binding: id,
            state: binding.state,
        })
    }

    fn record_released(
        &mut self,
        id: BindingId,
        session: u64,
        fence: u64,
    ) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        self.require_session(binding.node, session)?;
        self.require_fence(id, binding.fence, fence)?;
        if let Some(binding) = self.bindings.get_mut(&id) {
            binding.agent_session = session;
            binding.state = BindingState::Released;
        }
        Ok(Vec::new())
    }

    fn record_fenced(
        &mut self,
        id: BindingId,
        session: u64,
        fence: u64,
    ) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        self.require_session(binding.node, session)?;
        self.require_fence(id, binding.fence, fence)?;
        if let Some(binding) = self.bindings.get_mut(&id) {
            binding.agent_session = session;
            binding.state = BindingState::Fenced;
        }
        Ok(Vec::new())
    }

    fn record_failed(
        &mut self,
        id: BindingId,
        session: u64,
        _reason: &str,
        fence: u64,
    ) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        self.require_session(binding.node, session)?;
        self.require_fence(id, binding.fence, fence)?;
        let lease_id = binding.lease;
        if let Some(binding) = self.bindings.get_mut(&id) {
            binding.agent_session = session;
            if !binding.state.is_closed() {
                binding.state = BindingState::Failed;
            }
        }
        // A failed Binding means enforcement was lost or never established;
        // the Lease cannot stay Active or continue preparing past it.
        let lease = self
            .leases
            .get_mut(&lease_id)
            .ok_or(Error::UnknownLease(lease_id))?;
        if !matches!(
            lease.state,
            LeaseState::Failed | LeaseState::Released | LeaseState::Revoked | LeaseState::Expired
        ) {
            lease.state = LeaseState::Failed;
        }
        Ok(self.fence_effects(lease_id))
    }

    fn require_fence(&self, binding: BindingId, expected: u64, got: u64) -> Result<(), Error> {
        if expected != got {
            return Err(Error::FenceMismatch {
                binding,
                expected,
                got,
            });
        }
        Ok(())
    }

    fn node_has_live_authority(&self, node: NodeId) -> bool {
        let in_scope = |candidate: NodeId| {
            candidate == node || self.graph.ancestors(candidate).contains(&node)
        };
        self.leases.values().any(|lease| {
            self.occupies(lease.id)
                && lease
                    .allocation
                    .claims
                    .iter()
                    .any(|claim| in_scope(claim.node))
        }) || self
            .bindings
            .values()
            .any(|binding| !binding.state.is_closed() && in_scope(binding.node))
    }

    fn set_node_state(&mut self, node: NodeId, state: NodeState) -> Result<Vec<Effect>, Error> {
        if self.graph.node(node).is_none() {
            return Err(Error::UnknownNode(node));
        }
        let current = self.node_state(node).expect("checked graph node");
        if current == NodeState::Retired && state != NodeState::Retired {
            return Err(Error::Invalid("retired resources require a new identity"));
        }
        if state == NodeState::Schedulable
            && current != NodeState::Schedulable
            && current != NodeState::Draining
            && self.bindings.values().any(|binding| {
                !binding.state.is_closed() && {
                    binding.node == node || self.graph.ancestors(binding.node).contains(&node)
                }
            })
        {
            return Err(Error::UnquarantineBlocked { node });
        }
        if state == NodeState::Retired && self.node_has_live_authority(node) {
            return Err(Error::ResourceBusy { node });
        }
        if state == NodeState::Schedulable {
            self.node_states.remove(&node);
        } else {
            self.node_states.insert(node, state);
        }
        Ok(Vec::new())
    }

    /// Health is scoring input, not authoritative ownership state: it changes
    /// node attrs without advancing Graph.revision, so in-flight Allocations
    /// are never stranded by a health update.
    fn set_node_health(&mut self, node: NodeId, health: String) -> Result<Vec<Effect>, Error> {
        self.graph
            .set_attr(node, "health", health)
            .then_some(Vec::new())
            .ok_or(Error::UnknownNode(node))
    }

    fn set_agent_session(&mut self, machine: NodeId, session: u64) -> Result<Vec<Effect>, Error> {
        let node = self
            .graph
            .node(machine)
            .ok_or(Error::UnknownNode(machine))?;
        if node.kind != crate::types::ResourceClass::Machine {
            return Err(Error::Invalid("agent session requires a machine node"));
        }
        // Sessions are process generations: a delayed hello from an older
        // Agent process must never reinstate it as current. An equal session
        // is a retransmission: idempotent, but re-emit reconciliation so a
        // lost response can be recovered.
        if let Some(&current) = self.sessions.get(&machine) {
            if session < current {
                return Err(Error::StaleSession {
                    expected: current,
                    got: session,
                });
            }
            self.sessions.insert(machine, session);
            return Ok(vec![Effect::Reconcile {
                machine,
                session,
                epoch: self.epoch,
                bindings: self.live_machine_bindings(machine),
            }]);
        }
        self.sessions.insert(machine, session);
        Ok(vec![Effect::Reconcile {
            machine,
            session,
            epoch: self.epoch,
            bindings: self.live_machine_bindings(machine),
        }])
    }

    fn rebind_session(&mut self, id: BindingId, session: u64) -> Result<Vec<Effect>, Error> {
        let binding = self.bindings.get(&id).ok_or(Error::UnknownBinding(id))?;
        self.require_session(binding.node, session)?;
        if let Some(binding) = self.bindings.get_mut(&id) {
            binding.agent_session = session;
        }
        Ok(Vec::new())
    }

    fn quarantine_node(&mut self, node: NodeId) -> Result<Vec<Effect>, Error> {
        self.set_node_state(node, NodeState::Quarantined)
    }

    fn unquarantine_node(&mut self, node: NodeId) -> Result<Vec<Effect>, Error> {
        if self.graph.node(node).is_none() {
            return Err(Error::UnknownNode(node));
        }
        if self.node_state(node) != Some(NodeState::Quarantined) {
            return Ok(Vec::new());
        }
        self.set_node_state(node, NodeState::Schedulable)
    }

    fn fence_effects(&self, lease: LeaseId) -> Vec<Effect> {
        self.bindings
            .values()
            .filter(|binding| binding.lease == lease && !binding.state.is_closed())
            .map(|binding| {
                self.binding_effect(binding, |binding, node, provider, fence, session, epoch| {
                    Effect::Fence {
                        binding,
                        node,
                        provider,
                        fence,
                        session,
                        epoch,
                    }
                })
            })
            .collect()
    }

    fn release_effects(&self, lease: LeaseId) -> Vec<Effect> {
        self.bindings
            .values()
            .filter(|binding| binding.lease == lease && !binding.state.is_closed())
            .map(|binding| {
                self.binding_effect(binding, |binding, node, provider, fence, session, epoch| {
                    Effect::Release {
                        binding,
                        node,
                        provider,
                        fence,
                        session,
                        epoch,
                    }
                })
            })
            .collect()
    }
}
