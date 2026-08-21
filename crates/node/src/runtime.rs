//! Real process execution adapter. A lease's claims become a spawned OS
//! process; lease termination kills it. This is lifecycle enforcement only —
//! resource isolation (cgroups on Linux) is the next adapter, and macOS has
//! no isolation story at all.

use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};

use fleet_kernel::LeaseId;

#[derive(Default)]
pub struct ProcessRuntime {
    children: BTreeMap<LeaseId, Child>,
}

impl ProcessRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn the lease's command. The child inherits this terminal so demo
    /// output is visible.
    pub fn activate(&mut self, lease: LeaseId, command: &[String]) -> Result<(), String> {
        if self.children.contains_key(&lease) {
            return Ok(());
        }
        let [program, args @ ..] = command else {
            return Err(format!("lease {lease} has no command to execute"));
        };
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|err| format!("spawn {program}: {err}"))?;
        self.children.insert(lease, child);
        Ok(())
    }

    /// Kill the lease's process if it is still running. Returns whether a
    /// live child was terminated.
    pub fn terminate(&mut self, lease: LeaseId) -> Result<bool, String> {
        let Some(mut child) = self.children.remove(&lease) else {
            return Ok(false);
        };
        match child.try_wait() {
            Ok(Some(_)) => Ok(false),
            Ok(None) => {
                child
                    .kill()
                    .map_err(|err| format!("kill lease {lease}: {err}"))?;
                child
                    .wait()
                    .map_err(|err| format!("reap lease {lease}: {err}"))?;
                Ok(true)
            }
            Err(err) => Err(format!("poll lease {lease}: {err}")),
        }
    }

    /// Whether the lease has a running child.
    pub fn is_running(&mut self, lease: LeaseId) -> bool {
        match self.children.get_mut(&lease) {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}
