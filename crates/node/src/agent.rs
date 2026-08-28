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
    /// Highest fence seen per binding; operations carrying an older
    /// generation are rejected as stale.
    bindings: BTreeMap<u64, u64>,
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
            bindings: BTreeMap::new(),
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
            AgentRequest::Logs { lease } => {
                let lease_id = LeaseId::from_u64(lease);
                let output = if self.containers.is_tracked(lease_id) {
                    self.containers.logs(lease_id)
                } else {
                    ProcessRuntime::read_log(lease_id)
                };
                AgentResponse::Logs { lease, output }
            }
            AgentRequest::Prepare {
                binding,
                session,
                fence,
                ..
            } => {
                if self.check_session(session) {
                    return self.failed(binding, &format!("stale session {session}"));
                }
                // Record this binding's generation; only non-stale
                // operations are honored from here on.
                self.bindings.insert(binding, fence);
                let handle = self.next_handle;
                self.next_handle += 1;
                AgentResponse::Prepared { binding, handle }
            }
            AgentRequest::Activate {
                binding,
                lease,
                session,
                fence,
                command,
                limits,
                image,
                storage,
                ports,
                grace_secs,
                devices,
            } => {
                if !self.generation_valid(binding, session, fence) {
                    return self.failed(binding, &format!("stale generation {session}/{fence}"));
                }
                self.grace.insert(LeaseId::from_u64(lease), grace_secs);
                let lease_id = LeaseId::from_u64(lease);
                let result = if image.is_empty() {
                    // Processes share the host filesystem and network;
                    // mounts and ports are container-only concerns. Device
                    // claims are enforced by the cgroup-device filter.
                    self.process.activate(lease_id, &command, &limits, &devices)
                } else {
                    self.containers.activate(
                        lease_id,
                        crate::container::ContainerConfig {
                            image: &image,
                            command: &command,
                            limits: &limits,
                            storage: &storage,
                            ports: &ports,
                            devices: &devices,
                        },
                    )
                };
                match result {
                    Ok(()) => AgentResponse::Activated { binding },
                    Err(reason) => {
                        eprintln!("archon: activation for binding {binding} failed: {reason}");
                        self.failed(binding, &reason)
                    }
                }
            }
            AgentRequest::Release { binding, lease, .. } => {
                let lease_id = LeaseId::from_u64(lease);
                let grace = self.grace.remove(&lease_id).unwrap_or(0);
                match self.terminate_lease(lease_id, grace) {
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
                match self.terminate_lease(lease_id, grace) {
                    Ok(_) => AgentResponse::Fenced { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
        }
    }

    /// Drain a lease's workload with its recorded grace period, whichever
    /// runtime currently tracks it.
    fn terminate_lease(&mut self, lease_id: LeaseId, grace: u32) -> Result<bool, String> {
        if self.containers.is_tracked(lease_id) {
            self.containers.terminate_with_grace(lease_id, grace)
        } else {
            self.process.terminate_with_grace(lease_id, grace)
        }
    }

    fn hello(&self) -> AgentResponse {
        let description = match crate::discover::try_describe() {
            Ok(description) => description,
            Err(reason) => {
                return self.failed_none(&format!("device discovery incomplete: {reason}"));
            }
        };
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

    /// Whether this operation's generation is current: the session must
    /// not be stale, and the fence must not regress behind one already
    /// seen for the binding. An unseen binding at the current session is
    /// accepted — reconciliation legitimately activates bindings prepared
    /// under a previous agent process.
    fn generation_valid(&mut self, binding: u64, session: u64, fence: u64) -> bool {
        if self.check_session(session) {
            return false;
        }
        match self.bindings.get(&binding) {
            Some(seen) if *seen > fence => false,
            _ => {
                self.bindings.insert(binding, fence);
                true
            }
        }
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
