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
