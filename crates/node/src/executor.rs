//! Agent-side workload execution supervision.
//!
//! Executors create, observe, and terminate workload members using resources
//! already authorized by the current Lease/Bindings. Resource endpoint
//! generations and fencing live in the Provider component, not here.

use std::collections::BTreeMap;

use archon_kernel::LeaseId;

use crate::container::{ContainerConfig, ContainerRuntime};
use crate::protocol::{DeviceAccess, ExecutionCapabilities, ExecutionId, LeaseLimits};
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
    executions: BTreeMap<LeaseId, TrackedExecution>,
}

struct TrackedExecution {
    id: Option<ExecutionId>,
    /// None records confirmed closure for retries within this supervisor.
    grace: Option<u32>,
}

impl ExecutionSupervisor {
    pub(crate) fn new(process: ProcessRuntime) -> Self {
        Self {
            process,
            containers: ContainerRuntime::new(
                std::env::var("ARCHON_CONTAINER_ENGINE").unwrap_or_else(|_| "docker".to_string()),
            ),
            executions: BTreeMap::new(),
        }
    }

    pub(crate) fn capabilities(&self) -> ExecutionCapabilities {
        ExecutionCapabilities {
            process: self.process.capabilities(),
            container: self.containers.capabilities(),
        }
    }

    /// Start one execution idempotently by attempt identity. Resource-provider
    /// activation is deliberately outside this method.
    pub(crate) fn start(
        &mut self,
        lease: LeaseId,
        id: Option<ExecutionId>,
        start: ExecutionStart<'_>,
    ) -> Result<(), String> {
        if let Some(existing) = self.executions.get(&lease) {
            if existing.id != id {
                return Err(format!("execution identity mismatch for lease {lease}"));
            }
            return if existing.grace.is_some() {
                Ok(())
            } else {
                Err(format!("execution for lease {lease} is closed"))
            };
        }
        let result = if start.image.is_empty() {
            // Processes share the host filesystem and network; mounts and
            // ports are container-only concerns. Refuse rather than run a
            // workload that silently drops its requested spec.
            if !start.storage.is_empty() || !start.ports.is_empty() {
                return Err("volumes and published ports need an OCI image".into());
            }
            // Device claims are enforced by the process executor's
            // cgroup-device filter.
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
            self.executions.insert(
                lease,
                TrackedExecution {
                    id,
                    grace: Some(start.grace_secs),
                },
            );
        }
        result
    }

    pub(crate) fn status(&mut self, lease: LeaseId, id: Option<ExecutionId>) -> WorkStatus {
        if !self
            .executions
            .get(&lease)
            .is_some_and(|record| record.id == id && record.grace.is_some())
        {
            return WorkStatus::Gone;
        }
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

    /// A missing handle in a replacement supervisor is not closure proof.
    pub(crate) fn stop(&mut self, lease: LeaseId, id: Option<ExecutionId>) -> Result<(), String> {
        let Some(record) = self.executions.get_mut(&lease) else {
            return Err(format!("execution for lease {lease} is untracked"));
        };
        if record.id != id {
            return Err(format!("execution identity mismatch for lease {lease}"));
        }
        let Some(grace) = record.grace else {
            return Ok(());
        };
        if self.containers.is_tracked(lease) {
            self.containers.terminate_with_grace(lease, grace)?;
        } else {
            self.process.terminate_with_grace(lease, grace)?;
        }
        record.grace = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tracked_execution_can_report_stop() {
        let lease = LeaseId::from_u64(193);
        let mut supervisor = ExecutionSupervisor::new(ProcessRuntime::new());
        let command = ["sleep".into(), "15".into()];
        let limits = LeaseLimits::default();
        let start = || ExecutionStart {
            command: &command,
            limits: &limits,
            image: "",
            storage: &[],
            ports: &[],
            grace_secs: 0,
            devices: &[],
        };
        let id = Some(ExecutionId::generate().unwrap());
        let other = Some(ExecutionId::generate().unwrap());
        supervisor.start(lease, id, start()).unwrap();
        let mut replacement = ExecutionSupervisor::new(ProcessRuntime::new());
        assert!(replacement.stop(lease, id).is_err());
        assert!(supervisor.start(lease, other, start()).is_err());
        assert!(supervisor.stop(lease, other).is_err());
        assert!(supervisor.stop(lease, None).is_err());
        assert_eq!(supervisor.status(lease, other), WorkStatus::Gone);
        assert_eq!(supervisor.status(lease, id), WorkStatus::Running);
        supervisor.stop(lease, id).unwrap();
        supervisor.stop(lease, id).unwrap();
        assert!(supervisor.start(lease, id, start()).is_err());
    }
}
