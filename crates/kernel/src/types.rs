use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use crate::ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId};

const MAX_IDENTIFIER_LEN: usize = 63;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IdentifierError {
    Empty,
    TooLong,
    InvalidCharacter { index: usize },
    InvalidBoundary,
}

impl fmt::Debug for IdentifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for IdentifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "identifier must not be empty"),
            Self::TooLong => write!(f, "identifier exceeds {MAX_IDENTIFIER_LEN} bytes"),
            Self::InvalidCharacter { index } => {
                write!(
                    f,
                    "identifier contains an invalid character at byte {index}"
                )
            }
            Self::InvalidBoundary => {
                write!(f, "identifier must start and end with a letter or digit")
            }
        }
    }
}

impl std::error::Error for IdentifierError {}

fn validate_identifier(value: &str) -> Result<([u8; MAX_IDENTIFIER_LEN], u8), IdentifierError> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err(IdentifierError::Empty);
    }
    if bytes.len() > MAX_IDENTIFIER_LEN {
        return Err(IdentifierError::TooLong);
    }
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return Err(IdentifierError::InvalidBoundary);
    }
    for (index, byte) in bytes.iter().enumerate() {
        if !byte.is_ascii_lowercase()
            && !byte.is_ascii_digit()
            && !matches!(byte, b'-' | b'_' | b'.' | b'/')
        {
            return Err(IdentifierError::InvalidCharacter { index });
        }
    }
    let mut out = [0; MAX_IDENTIFIER_LEN];
    out[..bytes.len()].copy_from_slice(bytes);
    Ok((out, bytes.len() as u8))
}

macro_rules! validated_identifier {
    ($name:ident { $( $constant:ident = $value:literal ),+ $(,)? }) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name {
            bytes: [u8; MAX_IDENTIFIER_LEN],
            len: u8,
        }

        #[allow(non_upper_case_globals)]
        impl $name {
            const fn from_static(value: &'static str) -> Self {
                let source = value.as_bytes();
                let mut bytes = [0; MAX_IDENTIFIER_LEN];
                let mut index = 0;
                while index < source.len() {
                    bytes[index] = source[index];
                    index += 1;
                }
                Self {
                    bytes,
                    len: source.len() as u8,
                }
            }

            $(pub const $constant: Self = Self::from_static($value);)+

            pub fn new(value: &str) -> Result<Self, IdentifierError> {
                let (bytes, len) = validate_identifier(value)?;
                Ok(Self { bytes, len })
            }

            pub fn as_str(&self) -> &str {
                // The inline representation is validated at construction and
                // all constants are ASCII literals.
                std::str::from_utf8(&self.bytes[..self.len as usize])
                    .expect("validated identifier is UTF-8")
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.as_str()).finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdentifierError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdentifierError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(&value)
            }
        }

        #[cfg(feature = "serde")]
        impl serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        #[cfg(feature = "serde")]
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = <String as serde::Deserialize>::deserialize(deserializer)?;
                Self::new(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

validated_identifier!(ResourceClass {
    Machine = "machine",
    Rack = "rack",
    PowerDomain = "power-domain",
    Socket = "socket",
    Numa = "numa",
    Cpu = "cpu",
    Memory = "memory",
    PcieRoot = "pcie-root",
    Gpu = "gpu",
    Nic = "nic",
    Nvme = "nvme",
    Region = "region",
    Datacenter = "datacenter",
    DataObject = "data-object",
});

impl ResourceClass {
    pub const fn default_dimension(self) -> Option<CapacityDimension> {
        match self {
            Self::Cpu | Self::Gpu | Self::Nic | Self::Nvme => Some(CapacityDimension::Count),
            Self::Memory => Some(CapacityDimension::Bytes),
            _ => None,
        }
    }

    pub const fn is_enforced(self) -> bool {
        !matches!(
            self,
            Self::Machine
                | Self::Rack
                | Self::PowerDomain
                | Self::Socket
                | Self::Numa
                | Self::PcieRoot
                | Self::Region
                | Self::Datacenter
                | Self::DataObject
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NodeState {
    Joining,
    Schedulable,
    Draining,
    Unavailable,
    Quarantined,
    Retired,
}

impl NodeState {
    pub const fn is_schedulable(self) -> bool {
        matches!(self, Self::Schedulable)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EdgeKind {
    Contains,
    Connected,
    CachedOn,
}

/// A placement relationship evaluated against the graph's containment and
/// sparse connectivity facts. This stays separate from `EdgeKind` so a new
/// ancestor class does not require a new stored-edge variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TopologyRelation {
    SameAncestor { class: ResourceClass },
    DifferentAncestor { class: ResourceClass },
    Contains,
    Connected,
    CachedOn,
}

validated_identifier!(CapacityDimension {
    Count = "count",
    Bytes = "bytes",
});

pub type Quantity = BTreeMap<CapacityDimension, u64>;
pub type Attrs = BTreeMap<String, String>;

pub fn qty(dimension: CapacityDimension, amount: u64) -> Quantity {
    let mut quantity = Quantity::new();
    quantity.insert(dimension, amount);
    quantity
}

pub fn quantity_get(quantity: &Quantity, dimension: CapacityDimension) -> u64 {
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
    pub kind: ResourceClass,
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
    pub kind: ResourceClass,
    pub quantity: Quantity,
    pub filters: Vec<Filter>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TopologyConstraint {
    pub left: usize,
    pub right: usize,
    pub relation: TopologyRelation,
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
    /// Outcome of a workload that finished on its own; None until then.
    pub exit_code: Option<i32>,
}

/// Enforcement namespace for one Binding. Exclusive bindings serialize
/// ownership at one (provider, Node) endpoint. Independent shares keep a
/// binding-local endpoint generation so one cgroup-backed share can close
/// without invalidating another share of the same accounting Node.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BindingScope {
    #[default]
    Exclusive,
    IndependentShare,
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
    #[cfg_attr(feature = "serde", serde(default))]
    pub scope: BindingScope,
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
