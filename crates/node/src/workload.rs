//! Workload desired-state and execution intent above the resource kernel.
//!
//! `archon_kernel::Request` describes schedulable resource intent. Execution
//! payload and restart policy live here instead of participating in resource
//! authority, placement, or the kernel command log.

use std::ops::Deref;

use archon_kernel::Request;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageMount {
    pub host_path: String,
    pub mount_path: String,
}

/// A container port published to the host; None lets the host choose.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortPublish {
    pub container_port: u16,
    pub host_port: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionSpec {
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub storage: Vec<StorageMount>,
    #[serde(default)]
    pub ports: Vec<PortPublish>,
    #[serde(default)]
    pub grace_secs: u32,
}

impl ExecutionSpec {
    pub fn has_program(&self) -> bool {
        !self.command.is_empty() || self.image.is_some()
    }
}

/// One workload submission. The flattened representation intentionally matches
/// the legacy combined Request JSON shape, so old controller snapshots remain
/// readable while the kernel Request itself no longer carries execution data.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkloadSpec {
    #[serde(flatten)]
    pub resources: Request,
    #[serde(flatten)]
    pub execution: ExecutionSpec,
    #[serde(default)]
    pub keep_alive: bool,
}

impl WorkloadSpec {
    pub fn resource_only(resources: Request) -> Self {
        Self {
            resources,
            execution: ExecutionSpec::default(),
            keep_alive: false,
        }
    }
}

impl From<Request> for WorkloadSpec {
    fn from(resources: Request) -> Self {
        Self::resource_only(resources)
    }
}

/// Desired service-member group above the resource kernel: one member
/// template plus a cardinality policy. The group is product-level desired
/// state only — reconciliation compiles it into ordinary member
/// submissions, one independent Lease per member, so group semantics never
/// enter resource authority, placement, or the kernel command log.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceGroup {
    /// Group identity, stable across member replacement and restarts.
    pub id: String,
    pub owner: archon_kernel::OwnerId,
    /// Cardinality policy the controller maintains for the group.
    pub cardinality: Cardinality,
    /// Member template: resource request plus execution intent. Each
    /// compiled member gets its own request id and Lease.
    pub template: WorkloadSpec,
}

/// How many members a group wants, and what drives changes:
/// fixed counts stay put, elastic bounds are steered by an explicit target
/// within [min, max], and per-machine groups follow the eligible machine
/// set. Only death replacements consume the group's restart cap; deliberate
/// cardinality changes (scaling) do not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Cardinality {
    /// Run exactly this many members.
    Fixed(usize),
    /// Maintain `target` members, steerable within [min, max] by scaling.
    Elastic {
        min: usize,
        target: usize,
        max: usize,
    },
    /// One member on each machine able to host the template.
    PerMachine,
}

impl Cardinality {
    /// Current desired member count given the present machine set.
    /// Per-machine groups want as many members as machines; the scheduler
    /// decides per machine whether a pinned member can actually place.
    pub fn desired_count(&self, machines: usize) -> usize {
        match *self {
            Cardinality::Fixed(count) => count,
            Cardinality::Elastic { target, .. } => target,
            Cardinality::PerMachine => machines,
        }
    }

    /// Elastic bounds must admit at least one member and hold
    /// min <= target <= max; other modes are always well-formed.
    pub fn validate(&self) -> Result<(), String> {
        match *self {
            Cardinality::Fixed(0) => Err("fixed cardinality must be positive".into()),
            Cardinality::Elastic { min, target, max }
                if min == 0 || min > target || target > max =>
            {
                Err("elastic cardinality needs 1 <= min <= target <= max".into())
            }
            _ => Ok(()),
        }
    }
}

impl ServiceGroup {
    /// Compile one member submission with a fresh request id. The caller
    /// owns id allocation so restarts cannot mint colliding ids. The member
    /// carries no per-member keep-alive: the group is the sole desired-state
    /// owner, so per-member restart paths never double-act on it.
    pub fn member(&self, request_id: archon_kernel::RequestId) -> WorkloadSpec {
        let mut member = self.template.clone();
        member.resources.id = request_id;
        member.keep_alive = false;
        member
    }
}

impl Deref for WorkloadSpec {
    type Target = Request;

    fn deref(&self) -> &Self::Target {
        &self.resources
    }
}

impl AsRef<Request> for WorkloadSpec {
    fn as_ref(&self) -> &Request {
        &self.resources
    }
}
