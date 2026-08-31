//! Agent-side workload execution supervision.
//!
//! Executors create, observe, and terminate workload members using resources
//! already authorized by the current Lease/Bindings. Resource endpoint
//! generations and fencing live in the Provider component, not here.

use std::collections::BTreeMap;

use archon_kernel::LeaseId;

use crate::container::{ContainerConfig, ContainerRuntime};
use crate::protocol::{DeviceAccess, ExecutionCapabilities, LeaseLimits};
use crate::runtime::{ProcessRuntime, WorkStatus};
use crate::workload::{PortPublish, StorageMount};

/// Execution payload selected for one workload member activation.
pub(crate) struct ExecutionStart<'a> {
    pub command: &'a [String],
    pub limits: &'a LeaseLimits,
    pub image: &'a str,
    pub storage: &'a [StorageMount],
    pub ports: &'a [PortPublish],
    pub grace_secs: u32,
    pub devices: &'a [DeviceAccess],
}

/// Supervises the installed execution classes on one Agent.
///
/// This proof-stage set contains native processes and OCI containers. The
/// boundary is intentionally above either concrete runtime so additional
/// execution classes do not become resource-kernel concepts.
pub(crate) struct ExecutionSupervisor {
    process: ProcessRuntime,
    containers: ContainerRuntime,
    /// Drain budget per execution, captured at successful activation.
    grace: BTreeMap<LeaseId, u32>,
}

impl ExecutionSupervisor {
    pub(crate) fn new(process: ProcessRuntime) -> Self {
        Self {
            process,
            containers: ContainerRuntime::new(
                std::env::var("ARCHON_CONTAINER_ENGINE").unwrap_or_else(|_| "docker".to_string()),
            ),
            grace: BTreeMap::new(),
        }
    }

    pub(crate) fn capabilities(&self) -> ExecutionCapabilities {
        ExecutionCapabilities {
            process: self.process.capabilities(),
            container: self.containers.capabilities(),
        }
    }

    /// Start one execution idempotently by Lease identity. Resource-provider
    /// activation is deliberately outside this method.
    pub(crate) fn start(
        &mut self,
        lease: LeaseId,
        start: ExecutionStart<'_>,
    ) -> Result<(), String> {
        let result = if start.image.is_empty() {
            // Processes share the host filesystem and network; mounts and
            // ports are container-only concerns. Device claims are enforced
            // by the process executor's cgroup-device filter.
            self.process
                .activate(lease, start.command, start.limits, start.devices)
        } else {
            self.containers.activate(
                lease,
                ContainerConfig {
                    image: start.image,
                    command: start.command,
                    limits: start.limits,
                    storage: start.storage,
                    ports: start.ports,
                    devices: start.devices,
                },
            )
        };
        if result.is_ok() {
            self.grace.insert(lease, start.grace_secs);
        }
        result
    }

    pub(crate) fn status(&mut self, lease: LeaseId) -> WorkStatus {
        if self.containers.is_tracked(lease) {
            self.containers.status(lease)
        } else {
            self.process.status(lease)
        }
    }

    pub(crate) fn logs(&self, lease: LeaseId) -> String {
        if self.containers.is_tracked(lease) {
            self.containers.logs(lease)
        } else {
            ProcessRuntime::read_log(lease)
        }
    }

    /// Drain and terminate one execution with the grace budget captured at
    /// activation. Repeated calls are idempotent against the concrete
    /// runtimes' Lease tracking.
    pub(crate) fn stop(&mut self, lease: LeaseId) -> Result<bool, String> {
        let grace = self.grace.remove(&lease).unwrap_or(0);
        if self.containers.is_tracked(lease) {
            self.containers.terminate_with_grace(lease, grace)
        } else {
            self.process.terminate_with_grace(lease, grace)
        }
    }
}
