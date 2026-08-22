use std::collections::BTreeMap;

use crate::ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EdgeKind {
    Contains,
    SameNuma,
    SamePcie,
    Connected,
    CachedOn,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub attrs: Attrs,
    pub capacity: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub attrs: Attrs,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RequestClass {
    Service,
    Batch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Filter {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Need {
    pub kind: NodeKind,
    pub quantity: Quantity,
    pub filters: Vec<Filter>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TopologyConstraint {
    pub left: usize,
    pub right: usize,
    pub kind: EdgeKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Preference {
    Pack,
    Spread,
    PreferAttr { key: String, value: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    /// resource state. When `image` is set the command runs inside a
    /// container instead of as a bare process.
    pub command: Vec<String>,
    /// OCI image reference for container execution; None runs the command
    /// as a plain process on the host.
    pub image: Option<String>,
    /// Host directories bound into the container. Ignored by the process
    /// adapter (it already shares the host filesystem).
    pub storage: Vec<StorageMount>,
    /// Container ports to publish to the host. Ignored by the process
    /// adapter (it already shares the host network namespace).
    pub ports: Vec<PortPublish>,
    pub lifetime: u64,
    pub priority: u32,
    /// Keep-alive workloads are re-queued and re-placed automatically when
    /// their lease fails or expires (services). False: run-once.
    pub keep_alive: bool,
    /// Whether one need's claims must share a single machine. True (the
    /// default) fits process/container workloads that cannot span hosts;
    /// false allows a need's claims to spread across machines for explicit
    /// multi-member groups.
    pub machine_local: bool,
    /// Seconds between SIGTERM and SIGKILL when this lease is torn down
    /// (drain-on-revoke). Zero tears down immediately.
    pub grace_secs: u32,
}

/// A queued request carries the owner that submitted it and the submit time,
/// so fair-share admission can attribute consumption and break ties.
pub struct Queued {
    pub request: Request,
    pub owner: crate::ids::OwnerId,
    pub submitted_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StorageMount {
    pub host_path: String,
    pub mount_path: String,
}

/// A container port published to the host; None lets the host choose.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortPublish {
    pub container_port: u16,
    pub host_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Claim {
    pub node: NodeId,
    pub quantity: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Allocation {
    pub claims: Vec<Claim>,
    pub graph_revision: u64,
    pub explanation: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LeaseState {
    Reserved,
    Preparing,
    Active,
    Released,
    Expired,
    Revoked,
    Failed,
    /// Batch workload finished on its own with a zero exit code.
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EndpointPhase {
    Idle,
    Prepared,
    Active,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
