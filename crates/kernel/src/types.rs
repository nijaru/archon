use std::collections::BTreeMap;

use crate::ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum NodeKind {
    Machine,
    Rack,
    PowerDomain,
    Socket,
    Numa,
    Cpu,
    Memory,
    PcieRoot,
    Gpu,
    Nic,
    Nvme,
    Region,
    Datacenter,
    DataObject,
}

impl NodeKind {
    pub const fn default_dimension(self) -> Option<Dimension> {
        match self {
            Self::Cpu | Self::Gpu | Self::Nic | Self::Nvme => Some(Dimension::Count),
            Self::Memory => Some(Dimension::Bytes),
            _ => None,
        }
    }

    pub const fn is_enforced(self) -> bool {
        matches!(
            self,
            Self::Cpu | Self::Memory | Self::Gpu | Self::Nic | Self::Nvme
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum EdgeKind {
    Contains,
    SameNuma,
    SamePcie,
    Connected,
    CachedOn,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Dimension {
    Count,
    Bytes,
}

pub type Quantity = BTreeMap<Dimension, u64>;
pub type Attrs = BTreeMap<String, String>;

pub fn qty(dimension: Dimension, amount: u64) -> Quantity {
    let mut quantity = Quantity::new();
    quantity.insert(dimension, amount);
    quantity
}

pub fn quantity_get(quantity: &Quantity, dimension: Dimension) -> u64 {
    quantity.get(&dimension).copied().unwrap_or(0)
}

pub fn quantity_le(left: &Quantity, right: &Quantity) -> bool {
    left.iter()
        .all(|(dimension, amount)| quantity_get(right, *dimension) >= *amount)
}

pub fn quantity_saturating_sub(left: &Quantity, right: &Quantity) -> Quantity {
    let mut out = Quantity::new();
    for (dimension, amount) in left {
        let remain = amount.saturating_sub(quantity_get(right, *dimension));
        if remain > 0 {
            out.insert(*dimension, remain);
        }
    }
    out
}

pub fn quantity_max(left: &Quantity, right: &Quantity) -> Quantity {
    let mut out = left.clone();
    for (dimension, amount) in right {
        let entry = out.entry(*dimension).or_insert(0);
        *entry = (*entry).max(*amount);
    }
    out
}

pub fn quantity_add_assign(left: &mut Quantity, right: &Quantity) {
    for (dimension, amount) in right {
        *left.entry(*dimension).or_insert(0) += *amount;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub attrs: Attrs,
    pub capacity: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub attrs: Attrs,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RequestClass {
    Service,
    Batch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    pub kind: NodeKind,
    pub quantity: Quantity,
    pub filters: Vec<Filter>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyConstraint {
    pub left: usize,
    pub right: usize,
    pub kind: EdgeKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preference {
    Pack,
    Spread,
    PreferAttr { key: String, value: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: crate::ids::RequestId,
    pub class: RequestClass,
    pub needs: Vec<Need>,
    pub topology: Vec<TopologyConstraint>,
    pub preferences: Vec<Preference>,
    /// Data objects the workload reads. Locality is a scoring preference:
    /// candidates whose ancestry caches these objects rank higher, but the
    /// request is never refused for missing locality.
    pub data: Vec<NodeId>,
    /// Process to execute when the lease activates, e.g. ["sleep", "30"].
    /// Empty means a pure resource claim with no executable payload. The
    /// node's execution adapter consumes this; it is workload intent, not
    /// resource state.
    pub command: Vec<String>,
    pub lifetime: u64,
    pub priority: u32,
}

/// A queued request carries the owner that submitted it and the submit time,
/// so fair-share admission can attribute consumption and break ties.
pub struct Queued {
    pub request: Request,
    pub owner: crate::ids::OwnerId,
    pub submitted_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub node: NodeId,
    pub quantity: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    pub claims: Vec<Claim>,
    pub graph_revision: u64,
    pub explanation: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LeaseState {
    Reserved,
    Preparing,
    Active,
    Released,
    Expired,
    Revoked,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub id: LeaseId,
    pub owner: OwnerId,
    pub allocation: Allocation,
    pub parent: Option<LeaseId>,
    pub expires_at: u64,
    pub prepare_deadline: u64,
    pub priority: u32,
    pub state: LeaseState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindingState {
    Preparing,
    Active,
    Released,
    Fenced,
    Failed,
}

impl BindingState {
    pub const fn is_closed(self) -> bool {
        matches!(self, Self::Released | Self::Fenced)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub id: BindingId,
    pub lease: LeaseId,
    pub node: NodeId,
    pub provider: ProviderId,
    pub fence: u64,
    pub agent_session: u64,
    pub state: BindingState,
    pub provider_handle: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EndpointPhase {
    Idle,
    Prepared,
    Active,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub provider: ProviderId,
    pub node: NodeId,
    pub accepted_fence: u64,
    pub open: bool,
    pub binding: Option<BindingId>,
    pub phase: EndpointPhase,
    pub session: u64,
}

impl Endpoint {
    pub fn new(provider: ProviderId, node: NodeId, session: u64) -> Self {
        Self {
            provider,
            node,
            accepted_fence: 0,
            open: false,
            binding: None,
            phase: EndpointPhase::Idle,
            session,
        }
    }
}
