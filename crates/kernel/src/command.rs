use crate::ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId};
use crate::types::{Allocation, Edge, Node};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    ApplyGraph {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
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
