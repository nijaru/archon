//! The agent core: executes leases as real processes and answers protocol
//! requests. Shared by the in-process path and the TCP daemon, so local and
//! remote enforcement are literally the same code.

use std::collections::BTreeMap;

use archon_kernel::LeaseId;

use crate::protocol::{AgentRequest, AgentResponse};
use crate::runtime::{ProcessRuntime, WorkStatus};

pub struct LeaseAgent {
    process: ProcessRuntime,
    containers: crate::container::ContainerRuntime,
    /// Drain budget per lease, captured at activation for teardown.
    grace: BTreeMap<LeaseId, u32>,
    /// Highest session seen; older generations are rejected. Sessions are
    /// process generations — an older hello never reinstates.
    session: u64,
    next_handle: u64,
}

impl LeaseAgent {
    pub fn new(runtime: ProcessRuntime) -> Self {
        Self {
            process: runtime,
            containers: crate::container::ContainerRuntime::new(
                std::env::var("ARCHON_CONTAINER_ENGINE").unwrap_or_else(|_| "docker".to_string()),
            ),
            grace: BTreeMap::new(),
            session: 0,
            next_handle: 1,
        }
    }

    pub fn handle(&mut self, request: AgentRequest) -> AgentResponse {
        match request {
            AgentRequest::Hello => self.hello(),
            // Registration is consumed by the control plane before effects
            // ever reach a LeaseAgent.
            AgentRequest::Register { .. } => {
                self.failed_none("Register is not handled by an agent")
            }
            AgentRequest::Status { lease } => {
                let lease_id = LeaseId::from_u64(lease);
                let status = if self.containers.is_tracked(lease_id) {
                    self.containers.status(lease_id)
                } else {
                    self.process.status(lease_id)
                };
                AgentResponse::Running {
                    lease,
                    running: status == WorkStatus::Running,
                    exit_code: match status {
                        WorkStatus::Exited(code) => Some(code),
                        _ => None,
                    },
                }
            }
            AgentRequest::Prepare {
                binding,
                lease,
                session,
                ..
            } => {
                if self.check_session(session) {
                    return self.failed(binding, &format!("stale session {session}"));
                }
                let handle = self.next_handle;
                self.next_handle += 1;
                let _ = lease;
                AgentResponse::Prepared { binding, handle }
            }
            AgentRequest::Activate {
                binding,
                lease,
                session,
                fence: _,
                command,
                limits,
                image,
                storage,
                ports,
                grace_secs,
                devices,
            } => {
                self.grace.insert(LeaseId::from_u64(lease), grace_secs);
                if self.check_session(session) {
                    return self.failed(binding, &format!("stale session {session}"));
                }
                let lease_id = LeaseId::from_u64(lease);
                let result = if image.is_empty() {
                    // Processes share the host filesystem and network;
                    // mounts and ports are container-only concerns.
                    self.process.activate(lease_id, &command, &limits)
                } else {
                    self.containers.activate(
                        lease_id, &image, &command, &limits, &storage, &ports, &devices,
                    )
                };
                match result {
                    Ok(()) => AgentResponse::Activated { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
            AgentRequest::Release { binding, lease, .. } => {
                let lease_id = LeaseId::from_u64(lease);
                let grace = self.grace.remove(&lease_id).unwrap_or(0);
                let terminated = if self.containers.is_tracked(lease_id) {
                    self.containers.terminate_with_grace(lease_id, grace)
                } else {
                    self.process.terminate_with_grace(lease_id, grace)
                };
                match terminated {
                    Ok(_) => AgentResponse::Released { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
            AgentRequest::Fence { binding, lease, .. } => {
                let lease_id = LeaseId::from_u64(lease);
                // Authoritative teardown of the current generation drains
                // like release does; stale generations are rejected by the
                // session check before effects ever reach here.
                let grace = self.grace.remove(&lease_id).unwrap_or(0);
                let terminated = if self.containers.is_tracked(lease_id) {
                    self.containers.terminate_with_grace(lease_id, grace)
                } else {
                    self.process.terminate_with_grace(lease_id, grace)
                };
                match terminated {
                    Ok(_) => AgentResponse::Fenced { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
        }
    }

    fn hello(&self) -> AgentResponse {
        let description = crate::discover::describe();
        AgentResponse::Welcome {
            name: description.name,
            cpus: description.cpus,
            memory_bytes: description.memory_bytes,
            devices: description.devices,
        }
    }

    fn check_session(&mut self, session: u64) -> bool {
        if session < self.session {
            return true;
        }
        self.session = session;
        false
    }

    fn failed(&self, binding: u64, reason: &str) -> AgentResponse {
        AgentResponse::Failed {
            binding,
            reason: reason.to_string(),
        }
    }

    fn failed_none(&self, reason: &str) -> AgentResponse {
        AgentResponse::Failed {
            binding: 0,
            reason: reason.to_string(),
        }
    }
}
