//! Agent protocol core.
//!
//! The Agent composes two distinct responsibilities: a resource Provider that
//! owns Binding endpoint generations/fencing, and an execution supervisor that
//! creates and observes workload members. Local and remote controller paths
//! both reach this same composition through the Agent protocol.

use archon_kernel::LeaseId;

use crate::executor::{ExecutionStart, ExecutionSupervisor};
use crate::protocol::{AgentRequest, AgentResponse};
use crate::provider::{EndpointGeneration, ResourceProvider};
use crate::runtime::{ProcessRuntime, WorkStatus};

pub struct LeaseAgent {
    provider: ResourceProvider,
    execution: ExecutionSupervisor,
    /// Stable identity advertised to controllers that connect to this agent.
    instance_id: String,
    /// Optional display-name override from the agent CLI.
    name: Option<String>,
}

impl LeaseAgent {
    pub fn new(runtime: ProcessRuntime) -> Self {
        Self {
            provider: ResourceProvider::new(),
            execution: ExecutionSupervisor::new(runtime),
            instance_id: String::new(),
            name: None,
        }
    }

    /// Set the stable registration identity used by controller-initiated
    /// connections. Local in-process agents may leave it empty because they
    /// register directly from `MachineDescription`.
    pub fn with_identity(mut self, instance_id: String, name: Option<String>) -> Self {
        self.instance_id = instance_id;
        self.name = name;
        self
    }

    pub fn handle(&mut self, request: AgentRequest) -> AgentResponse {
        match request {
            AgentRequest::Hello => self.hello(),
            AgentRequest::Capabilities => AgentResponse::Capabilities {
                capabilities: self.execution.capabilities(),
            },
            // Registration is consumed by the control plane before effects
            // ever reach a LeaseAgent.
            AgentRequest::Register { .. } => {
                self.failed_none("Register is not handled by an agent")
            }
            AgentRequest::Status { lease } => {
                let status = self.execution.status(LeaseId::from_u64(lease));
                AgentResponse::Running {
                    lease,
                    running: status == WorkStatus::Running,
                    exit_code: match status {
                        WorkStatus::Exited(code) => Some(code),
                        _ => None,
                    },
                }
            }
            AgentRequest::Logs { lease } => AgentResponse::Logs {
                lease,
                output: self.execution.logs(LeaseId::from_u64(lease)),
            },
            AgentRequest::Prepare {
                binding,
                node,
                provider,
                scope,
                session,
                fence,
                epoch,
                ..
            } => {
                let generation = EndpointGeneration {
                    binding,
                    node,
                    provider,
                    scope,
                    session,
                    fence,
                    epoch,
                };
                match self.provider.prepare(generation) {
                    Ok(handle) => AgentResponse::Prepared { binding, handle },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
            AgentRequest::Activate {
                binding,
                node,
                provider,
                scope,
                session,
                fence,
                epoch,
                ..
            } => {
                let generation = EndpointGeneration {
                    binding,
                    node,
                    provider,
                    scope,
                    session,
                    fence,
                    epoch,
                };
                match self.provider.activate(generation) {
                    Ok(()) => AgentResponse::Activated { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
            AgentRequest::StartExecution {
                lease,
                session,
                epoch,
                command,
                limits,
                image,
                storage,
                ports,
                grace_secs,
                devices,
            } => {
                if let Err(reason) = self.provider.authorize_execution(session, epoch) {
                    return AgentResponse::ExecutionFailed { lease, reason };
                }
                let result = self.execution.start(
                    LeaseId::from_u64(lease),
                    ExecutionStart {
                        command: &command,
                        limits: &limits,
                        image: &image,
                        storage: &storage,
                        ports: &ports,
                        grace_secs,
                        devices: &devices,
                    },
                );
                match result {
                    Ok(()) => AgentResponse::ExecutionStarted { lease },
                    Err(reason) => {
                        eprintln!("archon: execution for lease {lease} failed: {reason}");
                        AgentResponse::ExecutionFailed { lease, reason }
                    }
                }
            }
            AgentRequest::Release {
                binding,
                lease,
                node,
                provider,
                scope,
                session,
                fence,
                epoch,
            } => {
                let generation = EndpointGeneration {
                    binding,
                    node,
                    provider,
                    scope,
                    session,
                    fence,
                    epoch,
                };
                if let Err(reason) = self.provider.release(generation) {
                    return self.failed(binding, &reason);
                }
                match self.execution.stop(LeaseId::from_u64(lease)) {
                    Ok(_) => AgentResponse::Released { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
            AgentRequest::Fence {
                binding,
                lease,
                node,
                provider,
                scope,
                session,
                fence,
                epoch,
            } => {
                let generation = EndpointGeneration {
                    binding,
                    node,
                    provider,
                    scope,
                    session,
                    fence,
                    epoch,
                };
                if let Err(reason) = self.provider.fence(generation) {
                    return self.failed(binding, &reason);
                }
                match self.execution.stop(LeaseId::from_u64(lease)) {
                    Ok(_) => AgentResponse::Fenced { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
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
            instance_id: self.instance_id.clone(),
            name: self.name.clone().unwrap_or(description.name),
            cpus: description.cpus,
            memory_bytes: description.memory_bytes,
            host_nodes: description.host_nodes,
            devices: description.devices,
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
