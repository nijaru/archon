use std::fmt;

use crate::ids::{BindingId, LeaseId, NodeId};
use crate::types::{BindingState, LeaseState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownNode(NodeId),
    UnknownLease(LeaseId),
    UnknownBinding(BindingId),
    DuplicateLease(LeaseId),
    DuplicateBinding(BindingId),
    NotAgreed,
    Quarantined(NodeId),
    StaleSession {
        expected: u64,
        got: u64,
    },
    StaleEpoch {
        current: u64,
        got: u64,
    },
    StaleGraphRevision {
        current: u64,
        got: u64,
    },
    Overlap {
        node: NodeId,
    },
    ChildEscapesParent {
        child: LeaseId,
        parent: LeaseId,
    },
    ParentNotActive {
        parent: LeaseId,
    },
    LeaseState {
        lease: LeaseId,
        state: LeaseState,
    },
    BindingState {
        binding: BindingId,
        state: BindingState,
    },
    BindingsNotPrepared {
        lease: LeaseId,
    },
    NoAgent {
        machine: NodeId,
    },
    RenewNotLater,
    ExpireNotDue,
    PrepareDeadlinePassed {
        lease: LeaseId,
    },
    LeaseExpired {
        lease: LeaseId,
    },
    HasLiveDescendants {
        lease: LeaseId,
    },
    FenceMismatch {
        binding: BindingId,
        expected: u64,
        got: u64,
    },
    InvalidTopology {
        reason: String,
    },
    CapacityBelowOccupancy {
        node: NodeId,
    },
    DuplicateClaim {
        node: NodeId,
    },
    UnclaimableNode {
        node: NodeId,
    },
    ChildBindingRefused {
        lease: LeaseId,
    },
    UnquarantineBlocked {
        node: NodeId,
    },
    Refused {
        explanation: String,
    },
    Invalid(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "unknown node {id}"),
            Self::UnknownLease(id) => write!(f, "unknown lease {id}"),
            Self::UnknownBinding(id) => write!(f, "unknown binding {id}"),
            Self::DuplicateLease(id) => write!(f, "duplicate lease {id}"),
            Self::DuplicateBinding(id) => write!(f, "duplicate binding {id}"),
            Self::NotAgreed => write!(f, "cluster has no agreement"),
            Self::Quarantined(id) => write!(f, "node {id} is quarantined"),
            Self::StaleSession { expected, got } => {
                write!(f, "stale session: expected {expected}, got {got}")
            }
            Self::StaleEpoch { current, got } => {
                write!(f, "stale epoch: current {current}, got {got}")
            }
            Self::StaleGraphRevision { current, got } => {
                write!(f, "stale graph revision: current {current}, got {got}")
            }
            Self::Overlap { node } => write!(f, "exclusive overlap on {node}"),
            Self::ChildEscapesParent { child, parent } => {
                write!(f, "child {child} escapes parent {parent}")
            }
            Self::ParentNotActive { parent } => write!(f, "parent {parent} is not active"),
            Self::LeaseState { lease, state } => write!(f, "lease {lease} in state {state:?}"),
            Self::BindingState { binding, state } => {
                write!(f, "binding {binding} in state {state:?}")
            }
            Self::BindingsNotPrepared { lease } => {
                write!(f, "lease {lease} is missing prepared bindings")
            }
            Self::NoAgent { machine } => write!(f, "no agent session for {machine}"),
            Self::RenewNotLater => write!(f, "renewal must extend expires_at"),
            Self::ExpireNotDue => write!(f, "lease has not reached expires_at"),
            Self::PrepareDeadlinePassed { lease } => {
                write!(f, "lease {lease} passed its prepare deadline")
            }
            Self::LeaseExpired { lease } => {
                write!(f, "lease {lease} expired before activation")
            }
            Self::HasLiveDescendants { lease } => {
                write!(f, "lease {lease} still has live descendants")
            }
            Self::FenceMismatch {
                binding,
                expected,
                got,
            } => {
                write!(f, "binding {binding} fence {got} does not match {expected}")
            }
            Self::InvalidTopology { reason } => write!(f, "invalid topology: {reason}"),
            Self::CapacityBelowOccupancy { node } => {
                write!(
                    f,
                    "node {node} capacity would drop below occupied units"
                )
            }
            Self::DuplicateClaim { node } => {
                write!(f, "allocation claims node {node} more than once")
            }
            Self::UnclaimableNode { node } => {
                write!(f, "node {node} is not a claimable resource")
            }
            Self::ChildBindingRefused { lease } => {
                write!(
                    f,
                    "lease {lease} is a child; only root leases hold Bindings"
                )
            }
            Self::UnquarantineBlocked { node } => {
                write!(f, "cannot unquarantine {node} before fence ack")
            }
            Self::Refused { explanation } => write!(f, "refused: {explanation}"),
            Self::Invalid(reason) => write!(f, "invalid command: {reason}"),
        }
    }
}

impl std::error::Error for Error {}
