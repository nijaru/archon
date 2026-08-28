from pathlib import Path
import re

# --- kernel Binding scope -------------------------------------------------
path = Path("crates/kernel/src/types.rs")
text = path.read_text()
anchor = '''#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BindingState {
'''
scope = '''/// Enforcement namespace for one Binding. Exclusive bindings serialize
/// ownership at one (provider, Node) endpoint. Independent shares keep a
/// binding-local endpoint generation so one cgroup-backed share can close
/// without invalidating another share of the same accounting Node.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BindingScope {
    #[default]
    Exclusive,
    IndependentShare,
}

'''
if text.count(anchor) != 1:
    raise RuntimeError("BindingState anchor changed")
text = text.replace(anchor, scope + anchor, 1)
old = '''    pub node: NodeId,
    pub provider: ProviderId,
    pub fence: u64,
'''
new = '''    pub node: NodeId,
    pub provider: ProviderId,
    #[cfg_attr(feature = "serde", serde(default))]
    pub scope: BindingScope,
    pub fence: u64,
'''
if text.count(old) != 1:
    raise RuntimeError("Binding fields changed")
text = text.replace(old, new, 1)
path.write_text(text)

path = Path("crates/kernel/src/lib.rs")
text = path.read_text()
old = '''    Allocation, Attrs, Binding, BindingState, CapacityDimension, Claim, Edge, EdgeKind, Endpoint,
'''
new = '''    Allocation, Attrs, Binding, BindingScope, BindingState, CapacityDimension, Claim, Edge,
    EdgeKind, Endpoint,
'''
if text.count(old) != 1:
    raise RuntimeError("kernel export anchor changed")
path.write_text(text.replace(old, new, 1))

path = Path("crates/kernel/src/command.rs")
text = path.read_text()
text = text.replace(
    "use crate::types::{Allocation, Edge, Node, NodeState};",
    "use crate::types::{Allocation, BindingScope, Edge, Node, NodeState};",
    1,
)
old = '''    OpenBinding {
        binding: BindingId,
        lease: LeaseId,
        node: NodeId,
        provider: ProviderId,
    },
'''
new = '''    OpenBinding {
        binding: BindingId,
        lease: LeaseId,
        node: NodeId,
        provider: ProviderId,
        #[cfg_attr(feature = "serde", serde(default))]
        scope: BindingScope,
    },
'''
if text.count(old) != 1:
    raise RuntimeError("OpenBinding command shape changed")
path.write_text(text.replace(old, new, 1))

# --- Cluster persists and validates the scope -----------------------------
path = Path("crates/kernel/src/cluster.rs")
text = path.read_text()
text = text.replace(
    "use crate::types::{Binding, BindingState, Lease, LeaseState, NodeState, Quantity};",
    "use crate::types::{Binding, BindingScope, BindingState, Lease, LeaseState, NodeState, Quantity};",
    1,
)
old = '''    pub node: NodeId,
    pub provider: crate::ids::ProviderId,
    pub fence: u64,
'''
new = '''    pub node: NodeId,
    pub provider: crate::ids::ProviderId,
    pub scope: BindingScope,
    pub fence: u64,
'''
if text.count(old) != 1:
    raise RuntimeError("BindingDigest fields changed")
text = text.replace(old, new, 1)
old = '''                            node: binding.node,
                            provider: binding.provider,
                            fence: binding.fence,
'''
new = '''                            node: binding.node,
                            provider: binding.provider,
                            scope: binding.scope,
                            fence: binding.fence,
'''
if text.count(old) != 1:
    raise RuntimeError("BindingDigest construction changed")
text = text.replace(old, new, 1)
old = '''            Command::OpenBinding {
                binding,
                lease,
                node,
                provider,
            } => self.open_binding(*binding, *lease, *node, *provider),
'''
new = '''            Command::OpenBinding {
                binding,
                lease,
                node,
                provider,
                scope,
            } => self.open_binding(*binding, *lease, *node, *provider, *scope),
'''
if text.count(old) != 1:
    raise RuntimeError("OpenBinding dispatch changed")
text = text.replace(old, new, 1)
old = '''    fn open_binding(
        &mut self,
        id: BindingId,
        lease_id: LeaseId,
        node: NodeId,
        provider: crate::ids::ProviderId,
    ) -> Result<Vec<Effect>, Error> {
'''
new = '''    fn open_binding(
        &mut self,
        id: BindingId,
        lease_id: LeaseId,
        node: NodeId,
        provider: crate::ids::ProviderId,
        scope: BindingScope,
    ) -> Result<Vec<Effect>, Error> {
'''
if text.count(old) != 1:
    raise RuntimeError("open_binding signature changed")
text = text.replace(old, new, 1)
old = '''            if existing.lease == lease_id && existing.node == node && existing.provider == provider
            {
'''
new = '''            if existing.lease == lease_id
                && existing.node == node
                && existing.provider == provider
                && existing.scope == scope
            {
'''
if text.count(old) != 1:
    raise RuntimeError("OpenBinding idempotency changed")
text = text.replace(old, new, 1)
old = '''        let fence = self.last_fence.get(&(provider, node)).copied().unwrap_or(0) + 1;
        self.last_fence.insert((provider, node), fence);
        let binding = Binding {
            id,
            lease: lease_id,
            node,
            provider,
            fence,
'''
new = '''        // An exclusive endpoint has one monotonic fence namespace per
        // (provider, Node). Independent shares have disjoint provider-side
        // enforcement objects keyed by Binding, so their stale-operation
        // generation is binding-local and does not supersede sibling shares.
        let conflicting_endpoint = self.bindings.values().any(|existing| {
            existing.provider == provider
                && existing.node == node
                && !existing.state.is_closed()
                && (scope == BindingScope::Exclusive
                    || existing.scope == BindingScope::Exclusive)
        });
        if conflicting_endpoint {
            return Err(Error::ResourceBusy { node });
        }
        let fence = match scope {
            BindingScope::Exclusive => {
                let fence = self.last_fence.get(&(provider, node)).copied().unwrap_or(0) + 1;
                self.last_fence.insert((provider, node), fence);
                fence
            }
            BindingScope::IndependentShare => id.as_u64().max(1),
        };
        let binding = Binding {
            id,
            lease: lease_id,
            node,
            provider,
            scope,
            fence,
'''
if text.count(old) != 1:
    raise RuntimeError("binding fence construction changed")
text = text.replace(old, new, 1)
path.write_text(text)

# Existing explicit OpenBinding commands are exclusive unless a focused test
# says otherwise. The service construction is replaced separately below.
pattern = re.compile(
    r"(Command::OpenBinding\s*\{.*?\n(?P<indent>\s*)provider:\s*[^\n]+,\n)(?P=indent)(\})",
    re.S,
)
for rust in Path("crates").rglob("*.rs"):
    source = rust.read_text()
    updated = pattern.sub(
        lambda m: m.group(1)
        + m.group("indent")
        + "scope: archon_kernel::BindingScope::Exclusive,\n"
        + m.group("indent")
        + m.group(3),
        source,
    )
    rust.write_text(updated)

# --- wire carries the authority dimensions the Agent must reject ----------
path = Path("crates/node/src/protocol.rs")
text = path.read_text()
text = text.replace(
    "use archon_kernel::{PortPublish, StorageMount};",
    "use archon_kernel::{BindingScope, PortPublish, StorageMount};",
    1,
)
old = '''    Prepare {
        binding: u64,
        lease: u64,
        session: u64,
        fence: u64,
    },
'''
new = '''    Prepare {
        binding: u64,
        lease: u64,
        node: u64,
        provider: u64,
        scope: BindingScope,
        session: u64,
        fence: u64,
        epoch: u64,
    },
'''
if text.count(old) != 1:
    raise RuntimeError("Prepare wire shape changed")
text = text.replace(old, new, 1)
old = '''    Activate {
        binding: u64,
        lease: u64,
        session: u64,
        fence: u64,
        command: Vec<String>,
'''
new = '''    Activate {
        binding: u64,
        lease: u64,
        node: u64,
        provider: u64,
        scope: BindingScope,
        session: u64,
        fence: u64,
        epoch: u64,
        command: Vec<String>,
'''
if text.count(old) != 1:
    raise RuntimeError("Activate wire shape changed")
text = text.replace(old, new, 1)
for variant in ["Release", "Fence"]:
    old = f'''    {variant} {{\n        binding: u64,\n        lease: u64,\n        session: u64,\n        fence: u64,\n    }},\n'''
    new = f'''    {variant} {{\n        binding: u64,\n        lease: u64,\n        node: u64,\n        provider: u64,\n        scope: BindingScope,\n        session: u64,\n        fence: u64,\n        epoch: u64,\n    }},\n'''
    if text.count(old) != 1:
        raise RuntimeError(f"{variant} wire shape changed")
    text = text.replace(old, new, 1)
path.write_text(text)

# --- NodeService chooses the provider enforcement scope and preserves epoch -
path = Path("crates/node/src/service.rs")
text = path.read_text()
text = text.replace(
    '''    BindingId, CapacityDimension, Cluster, Command, Effect, Error, LeaseId, NodeId, OwnerId,
    ProviderId, Queued, Request, RequestId, ResourceClass, quantity_get,
''',
    '''    BindingId, BindingScope, CapacityDimension, Cluster, Command, Effect, Error, LeaseId,
    NodeId, OwnerId, ProviderId, Queued, Request, RequestId, ResourceClass, quantity_get,
''',
    1,
)
old = '''        for claim in claims {
            let enforced = self
                .cluster
                .graph
                .node(claim.node)
                .is_some_and(|node| node.kind.is_enforced());
            if !enforced {
                continue;
            }
            let binding = BindingId::from_u64(self.next_binding);
            self.next_binding += 1;
            self.commit(Command::OpenBinding {
                binding,
                lease,
                node: claim.node,
                provider: ProviderId::ENFORCE,
                scope: archon_kernel::BindingScope::Exclusive,
            })?;
        }
'''
new = '''        for claim in claims {
            let node = self
                .cluster
                .graph
                .node(claim.node)
                .ok_or(Error::UnknownNode(claim.node))?;
            let scope = match node.kind {
                ResourceClass::Cpu
                | ResourceClass::Gpu
                | ResourceClass::Nic
                | ResourceClass::Nvme => BindingScope::Exclusive,
                // Memory limits are independently enforced by each lease's
                // cgroup/container object. They therefore use the explicit
                // shared-capacity Binding scope rather than pretending the
                // whole Memory accounting Node is one exclusive endpoint.
                ResourceClass::Memory => BindingScope::IndependentShare,
                kind if !kind.is_enforced() => continue,
                kind => {
                    return Err(Error::Refused {
                        explanation: format!(
                            "no execution provider is registered for enforced resource class {kind}"
                        ),
                    });
                }
            };
            let binding = BindingId::from_u64(self.next_binding);
            self.next_binding += 1;
            self.commit(Command::OpenBinding {
                binding,
                lease,
                node: claim.node,
                provider: ProviderId::ENFORCE,
                scope,
            })?;
        }
'''
if text.count(old) != 1:
    raise RuntimeError("open_enforced_bindings changed")
text = text.replace(old, new, 1)
old = '''        let binding_id = match &effect {
            Effect::Prepare { binding, .. }
            | Effect::Activate { binding, .. }
            | Effect::Release { binding, .. }
            | Effect::Fence { binding, .. } => *binding,
            Effect::Reconcile { .. } => unreachable!(),
        };
        let (lease, mut session, mut fence, record_node) = {
'''
new = '''        let binding_id = match &effect {
            Effect::Prepare { binding, .. }
            | Effect::Activate { binding, .. }
            | Effect::Release { binding, .. }
            | Effect::Fence { binding, .. } => *binding,
            Effect::Reconcile { .. } => unreachable!(),
        };
        let epoch = match &effect {
            Effect::Prepare { epoch, .. }
            | Effect::Activate { epoch, .. }
            | Effect::Release { epoch, .. }
            | Effect::Fence { epoch, .. } => *epoch,
            Effect::Reconcile { .. } => unreachable!(),
        };
        let (lease, mut session, mut fence, record_node, record_provider, record_scope) = {
'''
if text.count(old) != 1:
    raise RuntimeError("route binding tuple anchor changed")
text = text.replace(old, new, 1)
old = '''            (
                record.lease,
                record.agent_session,
                record.fence,
                record.node,
            )
'''
new = '''            (
                record.lease,
                record.agent_session,
                record.fence,
                record.node,
                record.provider,
                record.scope,
            )
'''
if text.count(old) != 1:
    raise RuntimeError("route binding tuple body changed")
text = text.replace(old, new, 1)
old = '''            Effect::Prepare { .. } => AgentRequest::Prepare {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                session,
                fence,
            },
'''
new = '''            Effect::Prepare { .. } => AgentRequest::Prepare {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                node: record_node.as_u64(),
                provider: record_provider.as_u64(),
                scope: record_scope,
                session,
                fence,
                epoch,
            },
'''
if text.count(old) != 1:
    raise RuntimeError("Prepare routing changed")
text = text.replace(old, new, 1)
old = '''            Effect::Activate { .. } => AgentRequest::Activate {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                session,
                fence,
                command: self.lease_commands.get(&lease).cloned().unwrap_or_default(),
'''
new = '''            Effect::Activate { .. } => AgentRequest::Activate {
                binding: binding_id.as_u64(),
                lease: lease.as_u64(),
                node: record_node.as_u64(),
                provider: record_provider.as_u64(),
                scope: record_scope,
                session,
                fence,
                epoch,
                command: self.lease_commands.get(&lease).cloned().unwrap_or_default(),
'''
if text.count(old) != 1:
    raise RuntimeError("Activate routing changed")
text = text.replace(old, new, 1)
for variant in ["Release", "Fence"]:
    old = f'''            Effect::{variant} {{ .. }} => AgentRequest::{variant} {{\n                binding: binding_id.as_u64(),\n                lease: lease.as_u64(),\n                session,\n                fence,\n            }},\n'''
    new = f'''            Effect::{variant} {{ .. }} => AgentRequest::{variant} {{\n                binding: binding_id.as_u64(),\n                lease: lease.as_u64(),\n                node: record_node.as_u64(),\n                provider: record_provider.as_u64(),\n                scope: record_scope,\n                session,\n                fence,\n                epoch,\n            }},\n'''
    if text.count(old) != 1:
        raise RuntimeError(f"{variant} routing changed")
    text = text.replace(old, new, 1)
path.write_text(text)

# --- production Agent uses the same endpoint state machine as the kernel ---
path = Path("crates/node/src/agent.rs")
text = path.read_text()
text = text.replace(
    "use archon_kernel::LeaseId;",
    "use archon_kernel::{BindingId, BindingScope, Endpoint, EndpointOp, LeaseId, NodeId, ProviderId};",
    1,
)
anchor = '''pub struct LeaseAgent {
'''
key = '''#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EndpointKey {
    Exclusive { provider: u64, node: u64 },
    IndependentShare { binding: u64 },
}

'''
if text.count(anchor) != 1:
    raise RuntimeError("LeaseAgent anchor changed")
text = text.replace(anchor, key + anchor, 1)
old = '''    /// Highest session seen; older generations are rejected. Sessions are
    /// process generations — an older hello never reinstates.
    session: u64,
    next_handle: u64,
    /// Highest fence seen per binding; operations carrying an older
    /// generation are rejected as stale.
    bindings: BTreeMap<u64, u64>,
'''
new = '''    /// Highest Agent session accepted by this process.
    session: u64,
    /// Highest Cluster authority epoch accepted by this Agent.
    epoch: u64,
    next_handle: u64,
    /// Provider endpoint state. Exclusive resources are keyed by
    /// (provider, Node); independent shares are keyed by Binding so one
    /// share cannot fence a sibling's cgroup-backed enforcement object.
    endpoints: BTreeMap<EndpointKey, Endpoint>,
'''
if text.count(old) != 1:
    raise RuntimeError("LeaseAgent generation fields changed")
text = text.replace(old, new, 1)
old = '''            grace: BTreeMap::new(),
            session: 0,
            next_handle: 1,
            bindings: BTreeMap::new(),
'''
new = '''            grace: BTreeMap::new(),
            session: 0,
            epoch: 0,
            next_handle: 1,
            endpoints: BTreeMap::new(),
'''
if text.count(old) != 1:
    raise RuntimeError("LeaseAgent constructor changed")
text = text.replace(old, new, 1)

prepare_start = text.index("            AgentRequest::Prepare {")
status_after = text.index("            AgentRequest::Status", prepare_start) if False else -1
# Replace each mutation arm independently.
old = '''            AgentRequest::Prepare {
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
'''
new = '''            AgentRequest::Prepare {
                binding,
                node,
                provider,
                scope,
                session,
                fence,
                epoch,
                ..
            } => {
                if let Err(reason) = self.apply_endpoint(
                    EndpointOp::Prepare,
                    binding,
                    node,
                    provider,
                    scope,
                    session,
                    fence,
                    epoch,
                ) {
                    return self.failed(binding, &reason);
                }
                let handle = self.next_handle;
                self.next_handle += 1;
                AgentResponse::Prepared { binding, handle }
            }
'''
if text.count(old) != 1:
    raise RuntimeError("Agent Prepare arm changed")
text = text.replace(old, new, 1)
old_head = '''            AgentRequest::Activate {
                binding,
                lease,
                session,
                fence,
                command,
'''
new_head = '''            AgentRequest::Activate {
                binding,
                lease,
                node,
                provider,
                scope,
                session,
                fence,
                epoch,
                command,
'''
if text.count(old_head) != 1:
    raise RuntimeError("Agent Activate fields changed")
text = text.replace(old_head, new_head, 1)
old = '''                if !self.generation_valid(binding, session, fence) {
                    return self.failed(binding, &format!("stale generation {session}/{fence}"));
                }
                self.grace.insert(LeaseId::from_u64(lease), grace_secs);
'''
new = '''                if let Err(reason) = self.apply_endpoint(
                    EndpointOp::Activate,
                    binding,
                    node,
                    provider,
                    scope,
                    session,
                    fence,
                    epoch,
                ) {
                    return self.failed(binding, &reason);
                }
                self.grace.insert(LeaseId::from_u64(lease), grace_secs);
'''
if text.count(old) != 1:
    raise RuntimeError("Agent Activate generation check changed")
text = text.replace(old, new, 1)
old = '''            AgentRequest::Release { binding, lease, .. } => {
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
'''
new = '''            AgentRequest::Release {
                binding,
                lease,
                node,
                provider,
                scope,
                session,
                fence,
                epoch,
            } => {
                if let Err(reason) = self.apply_endpoint(
                    EndpointOp::Release,
                    binding,
                    node,
                    provider,
                    scope,
                    session,
                    fence,
                    epoch,
                ) {
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
                if let Err(reason) = self.apply_endpoint(
                    EndpointOp::Fence,
                    binding,
                    node,
                    provider,
                    scope,
                    session,
                    fence,
                    epoch,
                ) {
                    return self.failed(binding, &reason);
                }
                let lease_id = LeaseId::from_u64(lease);
                let grace = self.grace.remove(&lease_id).unwrap_or(0);
                match self.terminate_lease(lease_id, grace) {
                    Ok(_) => AgentResponse::Fenced { binding },
                    Err(reason) => self.failed(binding, &reason),
                }
            }
'''
if text.count(old) != 1:
    raise RuntimeError("Agent close arms changed")
text = text.replace(old, new, 1)

old_start = text.index("    fn check_session(&mut self, session: u64) -> bool {")
old_end = text.index("    fn failed(&self, binding: u64, reason: &str) -> AgentResponse {", old_start)
helpers = r'''    fn endpoint_key(
        scope: BindingScope,
        binding: u64,
        provider: u64,
        node: u64,
    ) -> EndpointKey {
        match scope {
            BindingScope::Exclusive => EndpointKey::Exclusive { provider, node },
            BindingScope::IndependentShare => EndpointKey::IndependentShare { binding },
        }
    }

    fn check_control_generation(&mut self, session: u64, epoch: u64) -> Result<(), String> {
        if epoch < self.epoch {
            return Err(format!("stale cluster epoch {epoch}; current {}", self.epoch));
        }
        if session < self.session {
            return Err(format!("stale agent session {session}; current {}", self.session));
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

    #[allow(clippy::too_many_arguments)]
    fn apply_endpoint(
        &mut self,
        op: EndpointOp,
        binding: u64,
        node: u64,
        provider: u64,
        scope: BindingScope,
        session: u64,
        fence: u64,
        epoch: u64,
    ) -> Result<(), String> {
        self.check_control_generation(session, epoch)?;
        let key = Self::endpoint_key(scope, binding, provider, node);
        let create = matches!(op, EndpointOp::Prepare | EndpointOp::Release | EndpointOp::Fence);
        if create {
            self.endpoints.entry(key).or_insert_with(|| {
                Endpoint::new(
                    ProviderId::from_u64(provider),
                    NodeId::from_u64(node),
                    session,
                )
            });
        }
        let endpoint = self
            .endpoints
            .get_mut(&key)
            .ok_or_else(|| "binding endpoint has not been prepared".to_string())?;
        endpoint.handshake(session);
        endpoint
            .apply(op, BindingId::from_u64(binding), fence, session)
            .map_err(|err| format!("provider endpoint rejected generation: {err:?}"))
    }

'''
text = text[:old_start] + helpers + text[old_end:]

# Production endpoint regression tests: no runtime subprocess is needed.
text += r'''

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
        agent.apply_endpoint(op, binding, 7, 1, scope, 3, fence, epoch)
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
'''
path.write_text(text)

# Focused kernel proof for shared-vs-exclusive endpoint coexistence.
path = Path("crates/kernel/tests/harden.rs")
text = path.read_text()
text += r'''

#[test]
fn independent_share_bindings_can_coexist_but_exclusive_scope_cannot_mix() {
    let mut cluster = Cluster::new();
    let machine = NodeId::from_u64(1);
    let memory = NodeId::from_u64(2);
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, ResourceClass::Machine, Quantity::new()),
                node(2, ResourceClass::Memory, qty(CapacityDimension::Bytes, 1024)),
            ],
            edges: vec![archon_kernel::Edge {
                from: machine,
                to: memory,
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap();
    cluster
        .apply(Command::SetAgentSession {
            machine,
            session: 1,
        })
        .unwrap();
    for (lease, binding) in [(1, 1), (2, 2)] {
        let allocation = archon_kernel::Allocation {
            claims: vec![archon_kernel::Claim {
                node: memory,
                quantity: qty(CapacityDimension::Bytes, 256),
            }],
            graph_revision: cluster.graph.revision,
            explanation: "share".into(),
        };
        cluster
            .apply(Command::OpenLease {
                lease: LeaseId::from_u64(lease),
                owner: OwnerId::from_u64(lease),
                allocation,
                parent: None,
                expires_at: 1_000,
                prepare_deadline: 1_000,
                priority: 1,
            })
            .unwrap();
        cluster
            .apply(Command::OpenBinding {
                binding: BindingId::from_u64(binding),
                lease: LeaseId::from_u64(lease),
                node: memory,
                provider: ProviderId::ENFORCE,
                scope: archon_kernel::BindingScope::IndependentShare,
            })
            .expect("independent shares may coexist");
    }
    let allocation = archon_kernel::Allocation {
        claims: vec![archon_kernel::Claim {
            node: memory,
            quantity: qty(CapacityDimension::Bytes, 256),
        }],
        graph_revision: cluster.graph.revision,
        explanation: "exclusive".into(),
    };
    cluster
        .apply(Command::OpenLease {
            lease: LeaseId::from_u64(3),
            owner: OwnerId::from_u64(3),
            allocation,
            parent: None,
            expires_at: 1_000,
            prepare_deadline: 1_000,
            priority: 1,
        })
        .unwrap();
    let err = cluster
        .apply(Command::OpenBinding {
            binding: BindingId::from_u64(3),
            lease: LeaseId::from_u64(3),
            node: memory,
            provider: ProviderId::ENFORCE,
            scope: archon_kernel::BindingScope::Exclusive,
        })
        .unwrap_err();
    assert!(matches!(err, Error::ResourceBusy { node } if node == memory));
}
'''
path.write_text(text)
