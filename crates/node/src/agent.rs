//! The agent core: executes leases as real processes and answers protocol
//! requests. Shared by the in-process path and the TCP daemon, so local and
//! remote enforcement are literally the same code.

use archon_kernel::LeaseId;

use crate::protocol::{AgentRequest, AgentResponse};
use crate::runtime::ProcessRuntime;

pub struct LeaseAgent {
    runtime: ProcessRuntime,
    /// Highest session seen; older generations are rejected. Sessions are
    /// process generations — an older hello never reinstates.
    session: u64,
    next_handle: u64,
}

impl LeaseAgent {
    pub fn new(runtime: ProcessRuntime) -> Self {
        Self {
            runtime,
            session: 0,
            next_handle: 1,
        }
    }

    pub fn handle(&mut self, request: AgentRequest) -> AgentResponse {
        match request {
            AgentRequest::Hello => self.hello(),
            AgentRequest::Status { lease, session } => {
                if self.check_session(session) {
                    return self.failed_none(&format!("stale session {session}"));
                }
                AgentResponse::Running {
                    lease,
                    running: self.runtime.is_running(LeaseId::from_u64(lease)),
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
                command,
                limits,
                ..
            } => {
                if self.check_session(session) {
                    return self.failed(binding, &format!("stale session {session}"));
                }
                match self
                    .runtime
                    .activate(LeaseId::from_u64(lease), &command, &limits)
                {
                    Ok(()) => AgentResponse::Activated { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
            AgentRequest::Release { binding, lease, .. } => {
                match self.runtime.terminate(LeaseId::from_u64(lease)) {
                    Ok(_) => AgentResponse::Released { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
            AgentRequest::Fence { binding, lease, .. } => {
                match self.runtime.terminate(LeaseId::from_u64(lease)) {
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
