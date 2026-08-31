//! Agent-side resource-provider lifecycle.
//!
//! This component owns Binding endpoint generations and fencing state. It
//! does not launch, observe, or terminate workload execution; those concerns
//! belong to the execution supervisor.

use std::collections::BTreeMap;

use archon_kernel::{BindingId, BindingScope, Endpoint, EndpointOp, NodeId, ProviderId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EndpointKey {
    Exclusive { provider: u64, node: u64 },
    IndependentShare { binding: u64 },
}

/// One controller-issued generation for a resource Binding endpoint.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EndpointGeneration {
    pub binding: u64,
    pub node: u64,
    pub provider: u64,
    pub scope: BindingScope,
    pub session: u64,
    pub fence: u64,
    pub epoch: u64,
}

/// Provider state for resources enforced by this Agent.
///
/// The proof-stage implementation uses the kernel's generic [`Endpoint`]
/// state machine for every enforceable resource. Process/container execution
/// is intentionally absent from this type.
#[derive(Default)]
pub(crate) struct ResourceProvider {
    /// Highest Agent session accepted by this process.
    session: u64,
    /// Highest Cluster authority epoch accepted by this Agent.
    epoch: u64,
    next_handle: u64,
    /// Exclusive resources are keyed by (provider, Node); independent shares
    /// are keyed by Binding so one share cannot fence a sibling.
    endpoints: BTreeMap<EndpointKey, Endpoint>,
}

impl ResourceProvider {
    pub(crate) fn new() -> Self {
        Self {
            next_handle: 1,
            ..Self::default()
        }
    }

    pub(crate) fn prepare(&mut self, generation: EndpointGeneration) -> Result<u64, String> {
        self.apply(EndpointOp::Prepare, generation)?;
        let handle = self.next_handle;
        self.next_handle += 1;
        Ok(handle)
    }

    pub(crate) fn activate(&mut self, generation: EndpointGeneration) -> Result<(), String> {
        self.apply(EndpointOp::Activate, generation)
    }

    pub(crate) fn release(&mut self, generation: EndpointGeneration) -> Result<(), String> {
        self.apply(EndpointOp::Release, generation)
    }

    pub(crate) fn fence(&mut self, generation: EndpointGeneration) -> Result<(), String> {
        self.apply(EndpointOp::Fence, generation)
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

    fn apply(&mut self, op: EndpointOp, generation: EndpointGeneration) -> Result<(), String> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(
        provider: &mut ResourceProvider,
        op: EndpointOp,
        binding: u64,
        scope: BindingScope,
        fence: u64,
        epoch: u64,
    ) -> Result<(), String> {
        provider.apply(
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
        let mut provider = ResourceProvider::new();
        apply(
            &mut provider,
            EndpointOp::Prepare,
            10,
            BindingScope::Exclusive,
            1,
            1,
        )
        .unwrap();
        apply(
            &mut provider,
            EndpointOp::Prepare,
            11,
            BindingScope::Exclusive,
            2,
            1,
        )
        .unwrap();
        assert!(
            apply(
                &mut provider,
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
            &mut provider,
            EndpointOp::Fence,
            11,
            BindingScope::Exclusive,
            2,
            1,
        )
        .unwrap();
        assert!(
            apply(
                &mut provider,
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
        let mut provider = ResourceProvider::new();
        for (binding, fence) in [(10, 10), (11, 11)] {
            apply(
                &mut provider,
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
                &mut provider,
                EndpointOp::Activate,
                binding,
                BindingScope::IndependentShare,
                fence,
                1,
            )
            .unwrap();
        }
        apply(
            &mut provider,
            EndpointOp::Fence,
            10,
            BindingScope::IndependentShare,
            10,
            1,
        )
        .unwrap();
        assert!(
            apply(
                &mut provider,
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
        let mut provider = ResourceProvider::new();
        assert!(
            apply(
                &mut provider,
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
            &mut provider,
            EndpointOp::Fence,
            10,
            BindingScope::Exclusive,
            4,
            7,
        )
        .unwrap();
        assert!(
            apply(
                &mut provider,
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
    fn stale_cluster_epoch_is_rejected() {
        let mut provider = ResourceProvider::new();
        apply(
            &mut provider,
            EndpointOp::Prepare,
            10,
            BindingScope::Exclusive,
            1,
            9,
        )
        .unwrap();
        assert!(
            apply(
                &mut provider,
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
