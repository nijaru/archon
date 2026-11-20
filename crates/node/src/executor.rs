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
    state: ExecutionState,
}

#[derive(Clone, Copy)]
enum ExecutionState {
    Native(u32),
    Container { grace: u32, started: bool },
    Closed,
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
            return match existing.state {
                ExecutionState::Native(_) | ExecutionState::Container { started: true, .. } => {
                    Ok(())
                }
                ExecutionState::Container { started: false, .. } => {
                    Err(format!("container start for lease {lease} is unconfirmed"))
                }
                ExecutionState::Closed => Err(format!("execution for lease {lease} is closed")),
            };
        }
        if start.image.is_empty() {
            if !start.storage.is_empty() || !start.ports.is_empty() {
                return Err("volumes and published ports need an OCI image".into());
            }
            self.process.activate_assigned(
                lease,
                id,
                start.command,
                start.limits,
                start.devices,
            )?;
            self.executions.insert(
                lease,
                TrackedExecution {
                    id,
                    state: ExecutionState::Native(start.grace_secs),
                },
            );
            return Ok(());
        }

        // Keep the OCI assignment even when the engine's reply is uncertain.
        // It cannot be retried as a fresh start or stopped as a native process.
        let result = self.containers.activate(
            lease,
            ContainerConfig {
                image: start.image,
                command: start.command,
                limits: start.limits,
                storage: start.storage,
                ports: start.ports,
                devices: start.devices,
            },
        );
        self.executions.insert(
            lease,
            TrackedExecution {
                id,
                state: ExecutionState::Container {
                    grace: start.grace_secs,
                    started: result.is_ok(),
                },
            },
        );
        result
    }

    pub(crate) fn status(&mut self, lease: LeaseId, id: Option<ExecutionId>) -> WorkStatus {
        match self
            .executions
            .get(&lease)
            .filter(|record| record.id == id)
            .map(|record| record.state)
        {
            Some(ExecutionState::Native(_)) => self.process.status(lease),
            Some(ExecutionState::Container { started: true, .. }) => self.containers.status(lease),
            _ => WorkStatus::Gone,
        }
    }

    pub(crate) fn logs(&self, lease: LeaseId) -> String {
        if matches!(
            self.executions.get(&lease).map(|record| record.state),
            Some(ExecutionState::Container { .. })
        ) {
            self.containers.logs(lease)
        } else {
            ProcessRuntime::read_log(lease)
        }
    }

    /// A missing handle in a replacement supervisor is not closure proof.
    pub(crate) fn stop(&mut self, lease: LeaseId, id: Option<ExecutionId>) -> Result<(), String> {
        let Some(record) = self.executions.get_mut(&lease) else {
            return if self.process.stop_assignment(lease, id, 0)? {
                Ok(())
            } else {
                Err(format!("execution for lease {lease} is untracked"))
            };
        };
        if record.id != id {
            return Err(format!("execution identity mismatch for lease {lease}"));
        }
        match record.state {
            ExecutionState::Closed => return Ok(()),
            ExecutionState::Container { grace, .. } => {
                self.containers.terminate_with_grace(lease, grace)?;
            }
            ExecutionState::Native(grace) => {
                if !self.process.stop_assignment(lease, id, grace)? {
                    self.process.terminate_with_grace(lease, grace)?;
                }
            }
        }
        record.state = ExecutionState::Closed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn uncertain_container_start_keeps_the_exact_cleanup_target() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let suffix = serde_json::to_string(&ExecutionId::generate().unwrap()).unwrap();
        let dir = Scratch(std::env::temp_dir().join(format!("archon-engine-{suffix}")));
        fs::create_dir(&dir.0).unwrap();
        let engine = dir.0.join("engine");
        let container = "a".repeat(64);
        fs::write(
            &engine,
            format!(
                r#"#!/bin/sh
set -eu
cd "$(dirname "$0")"
case "$1" in
    --version) echo 'Docker version test';;
    create) echo create >> calls; [ ! -e fail-create ]; echo {container};;
    start) [ "$2" = {container} ]; echo start >> calls; [ -e confirm-start ];;
    inspect) [ "$4" = {container} ]; exit 1;;
    stop) [ "$2 $3 $4" = '-t 7 {container}' ]; echo stop >> calls;;
    rm) [ "$2 $3" = '-f {container}' ]; echo rm >> calls; [ -e confirm-removal ];;
    container)
        [ "$*" = 'container ls --all --quiet --no-trunc --filter id={container}' ]
        [ ! -e fail-list ]
        if [ ! -e absent ]; then echo {container}; fi;;
    *) exit 2;;
esac
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o700)).unwrap();
        let mut supervisor = ExecutionSupervisor::new(ProcessRuntime::new());
        supervisor.containers = ContainerRuntime::new(engine.to_str().unwrap().into());
        let lease = LeaseId::from_u64(1);
        let id = Some(ExecutionId::generate().unwrap());
        let other = Some(ExecutionId::generate().unwrap());
        let limits = LeaseLimits::default();
        let start = || ExecutionStart {
            command: &[],
            limits: &limits,
            image: "fixture",
            storage: &[],
            ports: &[],
            grace_secs: 7,
            devices: &[],
        };
        assert!(
            supervisor
                .start(lease, id, start())
                .unwrap_err()
                .contains(" start ")
        );
        assert!(supervisor.start(lease, id, start()).is_err());
        assert!(supervisor.stop(lease, other).is_err());
        assert!(
            supervisor.stop(lease, id).is_err(),
            "failed removal is not closure"
        );
        fs::write(dir.0.join("confirm-removal"), "").unwrap();
        supervisor.stop(lease, id).unwrap();
        supervisor.stop(lease, id).unwrap();
        assert!(supervisor.start(lease, id, start()).is_err());
        assert_eq!(
            fs::read_to_string(dir.0.join("calls")).unwrap(),
            "create\nstart\nstop\nrm\nstop\nrm\n"
        );

        let lease = LeaseId::from_u64(2);
        fs::write(dir.0.join("confirm-start"), "").unwrap();
        supervisor.start(lease, id, start()).unwrap();
        assert_eq!(
            supervisor.status(lease, id),
            WorkStatus::Gone,
            "inspect error is not an exit"
        );
        // An empty but unsuccessful list cannot prove removal. Once the
        // same query succeeds, the exact ID's absence is sufficient.
        fs::remove_file(dir.0.join("confirm-removal")).unwrap();
        fs::write(dir.0.join("absent"), "").unwrap();
        fs::write(dir.0.join("fail-list"), "").unwrap();
        assert!(supervisor.stop(lease, id).is_err());
        fs::remove_file(dir.0.join("fail-list")).unwrap();
        supervisor.stop(lease, id).unwrap();
        let lease = LeaseId::from_u64(3);
        fs::write(dir.0.join("fail-create"), "").unwrap();
        assert!(supervisor.start(lease, id, start()).is_err());
        assert!(
            supervisor
                .stop(lease, id)
                .unwrap_err()
                .contains("no confirmed engine ID")
        );
    }

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
