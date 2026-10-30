//! Controller-local evidence for one returning agent session. Status and
//! provider enumeration are independent observations; neither substitutes for
//! the other, and none of this evidence survives a new registration.

use std::collections::{BTreeMap, BTreeSet};

use archon_kernel::{Binding, BindingScope, BindingState, EndpointPhase, LeaseId};

use crate::dispatch::Tag;
use crate::protocol::EnumeratedEndpoint;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemberStatus {
    pub running: bool,
    pub exit_code: Option<i32>,
}

pub(crate) struct MachineRecovery {
    pub session: u64,
    /// None until this member's current-session Status query answers.
    pub members: BTreeMap<LeaseId, Option<MemberStatus>>,
    /// None after failed or invalid enumeration, not an empty provider picture.
    pub endpoints: Option<Vec<EnumeratedEndpoint>>,
    /// Every exact stray close must be acknowledged before recovery completes.
    pub stray_fences: BTreeSet<Tag>,
}

impl MachineRecovery {
    pub fn new(session: u64) -> Self {
        Self {
            session,
            members: BTreeMap::new(),
            endpoints: None,
            stray_fences: BTreeSet::new(),
        }
    }

    pub fn endpoints_settled(&self) -> bool {
        self.endpoints.is_some() && self.stray_fences.is_empty()
    }

    pub fn complete(&self) -> bool {
        self.members.is_empty() && self.endpoints_settled()
    }

    pub fn proves(&self, binding: &Binding) -> bool {
        let Some(endpoints) = &self.endpoints else {
            return false;
        };
        // A duplicate/conflicting report is not exact proof. Exclusive
        // endpoints share a provider/node namespace; shares are binding-local.
        let mut candidates = endpoints.iter().filter(|endpoint| {
            endpoint.binding == Some(binding.id.as_u64())
                || (binding.scope == BindingScope::Exclusive
                    && endpoint.scope == BindingScope::Exclusive
                    && endpoint.node == binding.node.as_u64()
                    && endpoint.provider == binding.provider.as_u64())
        });
        let Some(endpoint) = candidates.next() else {
            return false;
        };
        candidates.next().is_none()
            && binding.state == BindingState::Active
            && same_generation(endpoint, binding)
            && endpoint.open
            && endpoint.phase == EndpointPhase::Active
    }
}

pub(crate) fn same_generation(endpoint: &EnumeratedEndpoint, binding: &Binding) -> bool {
    endpoint.binding == Some(binding.id.as_u64())
        && endpoint.node == binding.node.as_u64()
        && endpoint.provider == binding.provider.as_u64()
        && endpoint.scope == binding.scope
        && endpoint.accepted_fence == binding.fence
}
