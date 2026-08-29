//! The agent core: executes leases as real processes and answers protocol
//! requests. Shared by the in-process path and the TCP daemon, so local and
//! remote enforcement are literally the same code.

use std::collections::BTreeMap;

use archon_kernel::{BindingId, BindingScope, Endpoint, EndpointOp, LeaseId, NodeId, ProviderId};

use crate::protocol::{AgentRequest, AgentResponse};
use crate::runtime::{ProcessRuntime, WorkStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EndpointKey {
    Exclusive { provider: u64, node: u64 },
    IndependentShare { binding: u64 },
}

#[derive(Clone, Copy, Debug)]
struct EndpointGeneration {
    binding: u64,
    node: u64,
    provider: u64,
    scope: BindingScope,
    session: u64,
    fence: u64,
    epoch: u64,
}

pub struct LeaseAgent {
    process: ProcessRuntime,
    containers: crate::container::ContainerRuntime,
    /// Drain budget per lease, captured at activation for teardown.
    grace: BTreeMap<LeaseId, u32>,
    /// Highest Agent session accepted by this process.
    session: u64,
    /// Highest Cluster authority epoch accepted by this Agent.
    epoch: u64,
    next_handle: u64,
    /// Provider endpoint state. Exclusive resources are keyed by
    /// (provider, Node); independent shares are keyed by Binding so one
    /// share cannot fence a sibling's cgroup-backed enforcement object.
    endpoints: BTreeMap<EndpointKey, Endpoint>,
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
            epoch: 0,
            next_handle: 1,
            endpoints: BTreeMap::new(),
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
                if let Err(reason) = self.apply_endpoint(EndpointOp::Prepare, generation) {
                    return self.failed(binding, &reason);
                }
                let handle = self.next_handle;
                self.next_handle += 1;
                AgentResponse::Prepared { binding, handle }
            }
            AgentRequest::Activate {
                binding,
                lease,
                node,
                provider,
                scope,
                session,
                fence,
                epoch,
                command,
                limits,
                image,
                storage,
                ports,
                grace_secs,
                devices,
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
                if let Err(reason) = self.apply_endpoint(EndpointOp::Activate, generation) {
                    return self.failed(binding, &reason);
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
                if let Err(reason) = self.apply_endpoint(EndpointOp::Release, generation) {
                    return self.failed(binding, &reason);
                }
                let lease_id = LeaseId::from_u64(lease);
                let grace = self.grace.remove(&lease_id).unwrap_or(0);
                match self.terminate_lease(lease_id, grace) {
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
                if let Err(reason) = self.apply_endpoint(EndpointOp::Fence, generation) {
                    return self.failed(binding, &reason);
                }
                let lease_id = LeaseId::from_u64(lease);
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

    fn endpoint_key(generation: EndpointGeneration) -> EndpointKey {
        match generation.scope {
            BindingScope::Exclusive => EndpointKey::Exclusive {
                provider: generation.provider,
                node: generation.node,
            },
            BindingScope::IndependentShare => EndpointKey::IndependentShare {
                binding: generation.binding,
            },
        }
    }

    fn check_control_generation(&mut self, session: u64, epoch: u64) -> Result<(), String> {
        if epoch < self.epoch {
            return Err(format!(
                "stale cluster epoch {epoch}; current {}",
                self.epoch
            ));
        }
        if session < self.session {
            return Err(format!(
                "stale agent session {session}; current {}",
                self.session
            ));
        }
        if epoch > self.epoch {
            self.epoch = epoch;
        }
        if session > self.session {
            self.session = session;
            for endpoint in self.endpoints.values_mut() {
                endpoint.handshake(session);
            }
        }
        Ok(())
    }

    fn apply_endpoint(
        &mut self,
        op: EndpointOp,
        generation: EndpointGeneration,
    ) -> Result<(), String> {
        self.check_control_generation(generation.session, generation.epoch)?;
        let key = Self::endpoint_key(generation);
        let adopt_on_activate =
            matches!(op, EndpointOp::Activate) && !self.endpoints.contains_key(&key);
        let create = adopt_on_activate
            || matches!(
                op,
                EndpointOp::Prepare | EndpointOp::Release | EndpointOp::Fence
            );
        if create {
            self.endpoints.entry(key).or_insert_with(|| {
                Endpoint::new(
                    ProviderId::from_u64(generation.provider),
                    NodeId::from_u64(generation.node),
                    generation.session,
                )
            });
        }
        let endpoint = self
            .endpoints
            .get_mut(&key)
            .ok_or_else(|| "binding endpoint has not been prepared".to_string())?;
        endpoint.handshake(generation.session);
        if adopt_on_activate {
            endpoint
                .apply(
                    EndpointOp::Prepare,
                    BindingId::from_u64(generation.binding),
                    generation.fence,
                    generation.session,
                )
                .map_err(|err| format!("provider endpoint rejected adoption: {err:?}"))?;
        }
        endpoint
            .apply(
                op,
                BindingId::from_u64(generation.binding),
                generation.fence,
                generation.session,
            )
            .map_err(|err| format!("provider endpoint rejected generation: {err:?}"))
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

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    fn apply(
        agent: &mut LeaseAgent,
        op: EndpointOp,
        binding: u64,
        scope: BindingScope,
        fence: u64,
        epoch: u64,
    ) -> Result<(), String> {
        agent.apply_endpoint(
            op,
            EndpointGeneration {
                binding,
                node: 7,
                provider: 1,
                scope,
                session: 3,
                fence,
                epoch,
            },
        )
    }

    #[test]
    fn exclusive_endpoint_rejects_superseded_and_closed_activation() {
        let mut agent = LeaseAgent::new(ProcessRuntime::new());
        apply(
            &mut agent,
            EndpointOp::Prepare,
            10,
            BindingScope::Exclusive,
            1,
            1,
        )
        .unwrap();
        apply(
            &mut agent,
            EndpointOp::Prepare,
            11,
            BindingScope::Exclusive,
            2,
            1,
        )
        .unwrap();
        assert!(
            apply(
                &mut agent,
                EndpointOp::Activate,
                10,
                BindingScope::Exclusive,
                1,
                1,
            )
            .is_err(),
            "a higher resource fence must supersede the old Binding"
        );
        apply(
            &mut agent,
            EndpointOp::Fence,
            11,
            BindingScope::Exclusive,
            2,
            1,
        )
        .unwrap();
        assert!(
            apply(
                &mut agent,
                EndpointOp::Activate,
                11,
                BindingScope::Exclusive,
                2,
                1,
            )
            .is_err(),
            "a closed generation must never reactivate"
        );
    }

    #[test]
    fn independent_share_generations_do_not_supersede_siblings() {
        let mut agent = LeaseAgent::new(ProcessRuntime::new());
        for (binding, fence) in [(10, 10), (11, 11)] {
            apply(
                &mut agent,
                EndpointOp::Prepare,
                binding,
                BindingScope::IndependentShare,
                fence,
                1,
            )
            .unwrap();
        }
        for (binding, fence) in [(10, 10), (11, 11)] {
            apply(
                &mut agent,
                EndpointOp::Activate,
                binding,
                BindingScope::IndependentShare,
                fence,
                1,
            )
            .unwrap();
        }
        apply(
            &mut agent,
            EndpointOp::Fence,
            10,
            BindingScope::IndependentShare,
            10,
            1,
        )
        .unwrap();
        assert!(
            apply(
                &mut agent,
                EndpointOp::Activate,
                11,
                BindingScope::IndependentShare,
                11,
                1,
            )
            .is_ok(),
            "closing one independent share must leave its sibling valid"
        );
    }

    #[test]
    fn fresh_agent_session_can_adopt_current_active_generation() {
        let mut agent = LeaseAgent::new(ProcessRuntime::new());
        assert!(
            apply(
                &mut agent,
                EndpointOp::Activate,
                10,
                BindingScope::Exclusive,
                4,
                7,
            )
            .is_ok(),
            "a fresh agent process must adopt the current generation during reconcile"
        );
        apply(
            &mut agent,
            EndpointOp::Fence,
            10,
            BindingScope::Exclusive,
            4,
            7,
        )
        .unwrap();
        assert!(
            apply(
                &mut agent,
                EndpointOp::Activate,
                10,
                BindingScope::Exclusive,
                4,
                7,
            )
            .is_err(),
            "adoption must not reopen a generation already closed in this process"
        );
    }

    #[test]
    fn stale_cluster_epoch_is_rejected_by_the_real_agent_path() {
        let mut agent = LeaseAgent::new(ProcessRuntime::new());
        apply(
            &mut agent,
            EndpointOp::Prepare,
            10,
            BindingScope::Exclusive,
            1,
            9,
        )
        .unwrap();
        assert!(
            apply(
                &mut agent,
                EndpointOp::Activate,
                10,
                BindingScope::Exclusive,
                1,
                8,
            )
            .is_err()
        );
    }
}
