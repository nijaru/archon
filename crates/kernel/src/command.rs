use crate::ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId};
use crate::types::{
    Allocation, BindingScope, ClaimBindingUpdate, Edge, FactWriterAssignment, Node, NodeState,
    ProviderFactBatch,
};

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Command {
    ApplyGraph {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
    },
    /// Apply provider-authored resource facts and their claim contracts in one
    /// Graph revision. Capacity omitted from `claim_bindings` is placement-only.
    ApplyResourceFacts {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        claim_bindings: Vec<ClaimBindingUpdate>,
    },
    /// Apply explicitly-owned provider facts and claim contracts in one Graph
    /// revision. `ProviderFactBatch.writer` is discovery/fact provenance and
    /// is intentionally independent from `ClaimBinding.provider` enforcement.
    ApplyProviderFacts {
        batches: Vec<ProviderFactBatch>,
        claim_bindings: Vec<ClaimBindingUpdate>,
    },
    /// Adopt writer provenance for legacy facts after the caller has verified
    /// them against a current authoritative provider inventory.
    AdoptFactWriters {
        assignments: Vec<FactWriterAssignment>,
    },
    ReserveLease {
        lease: LeaseId,
        owner: OwnerId,
        allocation: Allocation,
        expires_at: u64,
        priority: u32,
    },
    PromoteLease {
        lease: LeaseId,
        prepare_deadline: u64,
    },
    OpenLease {
        lease: LeaseId,
        owner: OwnerId,
        allocation: Allocation,
        parent: Option<LeaseId>,
        expires_at: u64,
        prepare_deadline: u64,
        priority: u32,
    },
    ActivateLease {
        lease: LeaseId,
    },
    FailLease {
        lease: LeaseId,
        reason: String,
    },
    ReleaseLease {
        lease: LeaseId,
    },
    /// A workload exited on its own; zero means success (Completed),
    /// anything else fails the lease.
    CompleteLease {
        lease: LeaseId,
        exit_code: i32,
    },
    RevokeLease {
        lease: LeaseId,
    },
    ExpireLease {
        lease: LeaseId,
    },
    RenewLease {
        lease: LeaseId,
        new_expires_at: u64,
    },
    OpenBinding {
        binding: BindingId,
        lease: LeaseId,
        node: NodeId,
        provider: ProviderId,
        #[cfg_attr(feature = "serde", serde(default))]
        scope: BindingScope,
    },
    ActivateBinding {
        binding: BindingId,
    },
    FenceBinding {
        binding: BindingId,
    },
    FailBinding {
        binding: BindingId,
        reason: String,
    },
    ReleaseBinding {
        binding: BindingId,
    },
    RecordBindingPrepared {
        binding: BindingId,
        session: u64,
        provider_handle: u64,
        fence: u64,
    },
    RecordBindingActive {
        binding: BindingId,
        session: u64,
        fence: u64,
    },
    RecordBindingReleased {
        binding: BindingId,
        session: u64,
        fence: u64,
    },
    RecordBindingFenced {
        binding: BindingId,
        session: u64,
        fence: u64,
    },
    RecordBindingFailed {
        binding: BindingId,
        session: u64,
        reason: String,
        fence: u64,
    },
    SetAgentSession {
        machine: NodeId,
        session: u64,
    },
    SetNodeHealth {
        node: NodeId,
        health: String,
    },
    SetNodeState {
        node: NodeId,
        state: NodeState,
    },
    RebindSession {
        binding: BindingId,
        session: u64,
    },
    QuarantineNode {
        node: NodeId,
    },
    UnquarantineNode {
        node: NodeId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Effect {
    Prepare {
        binding: BindingId,
        node: NodeId,
        provider: ProviderId,
        fence: u64,
        session: u64,
        epoch: u64,
    },
    Activate {
        binding: BindingId,
        node: NodeId,
        provider: ProviderId,
        fence: u64,
        session: u64,
        epoch: u64,
    },
    Release {
        binding: BindingId,
        node: NodeId,
        provider: ProviderId,
        fence: u64,
        session: u64,
        epoch: u64,
    },
    Fence {
        binding: BindingId,
        node: NodeId,
        provider: ProviderId,
        fence: u64,
        session: u64,
        epoch: u64,
    },
    Reconcile {
        machine: NodeId,
        session: u64,
        epoch: u64,
        bindings: Vec<BindingId>,
    },
}
