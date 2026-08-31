from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    return text.replace(old, new, 1)


# Resource Provider: execution may be requested only by the current control
# generation, but Provider authority and execution launch remain separate.
provider_path = Path("crates/node/src/provider.rs")
provider = provider_path.read_text()
provider = replace_once(
    provider,
    """    pub(crate) fn activate(&mut self, generation: EndpointGeneration) -> Result<(), String> {\n        self.apply(EndpointOp::Activate, generation)\n    }\n\n    pub(crate) fn release(&mut self, generation: EndpointGeneration) -> Result<(), String> {\n""",
    """    pub(crate) fn activate(&mut self, generation: EndpointGeneration) -> Result<(), String> {\n        self.apply(EndpointOp::Activate, generation)\n    }\n\n    /// Validate that an execution operation came from the current controller\n    /// generation. This does not grant resource authority or launch work.\n    pub(crate) fn authorize_execution(\n        &mut self,\n        session: u64,\n        epoch: u64,\n    ) -> Result<(), String> {\n        self.check_control_generation(session, epoch)\n    }\n\n    pub(crate) fn release(&mut self, generation: EndpointGeneration) -> Result<(), String> {\n""",
    "provider execution authorization",
)
provider_path.write_text(provider)


service_path = Path("crates/node/src/service.rs")
service = service_path.read_text()

service = replace_once(
    service,
    """    /// Last observed workload state per (lease, machine) member. A rigid\n    /// multi-machine Lease is complete only after member states aggregate.\n    member_status: BTreeMap<(LeaseId, NodeId), MemberStatus>,\n    /// Machines whose last probe failed, consumed by health policy.\n""",
    """    /// Last observed workload state per (lease, machine) member. A rigid\n    /// multi-machine Lease is complete only after member states aggregate.\n    member_status: BTreeMap<(LeaseId, NodeId), MemberStatus>,\n    /// Provider activation acknowledgements for exact Binding generations.\n    /// Kernel BindingState becomes Active when activation is committed, before\n    /// the Agent ack, so execution barriers must use this stronger proof.\n    provider_activations: BTreeSet<(BindingId, u64, u64)>,\n    /// Workload members whose execution start was acknowledged for an exact\n    /// Agent session. A new Agent session naturally requires a new start.\n    execution_started: BTreeSet<(LeaseId, NodeId, u64)>,\n    /// Machines whose last probe failed, consumed by health policy.\n""",
    "service ephemeral activation state",
)

service = replace_once(
    service,
    """            inflight: BTreeSet::new(),\n            member_status: BTreeMap::new(),\n            unreachable: Vec::new(),\n""",
    """            inflight: BTreeSet::new(),\n            member_status: BTreeMap::new(),\n            provider_activations: BTreeSet::new(),\n            execution_started: BTreeSet::new(),\n            unreachable: Vec::new(),\n""",
    "constructor activation state",
)

service = replace_once(
    service,
    """        self.requests = state.requests;\n        self.restart_handled = state.restart_handled;\n""",
    """        self.requests = state.requests;\n        // Provider/execution acknowledgements are observations of the current\n        // Agent sessions, never durable controller state. Recovery proves them\n        // again from live Agents before adopting authority.\n        self.provider_activations.clear();\n        self.execution_started.clear();\n        self.restart_handled = state.restart_handled;\n""",
    "restore ephemeral activation state",
)

service = replace_once(
    service,
    """    fn commit_lenient(&mut self, command: Command) {\n        if let Err(err) = self.commit(command) {\n            eprintln!(\"archon: rejected stale completion: {err}\");\n        }\n    }\n""",
    """    fn commit_lenient(&mut self, command: Command) {\n        let _ = self.try_commit_lenient(command);\n    }\n\n    /// Like commit_lenient, but report whether the completion was accepted.\n    /// Controller-only activation barriers use this to avoid treating stale\n    /// Agent acknowledgements as proof of the current Binding generation.\n    fn try_commit_lenient(&mut self, command: Command) -> bool {\n        match self.commit(command) {\n            Ok(()) => true,\n            Err(err) => {\n                eprintln!(\"archon: rejected stale completion: {err}\");\n                false\n            }\n        }\n    }\n""",
    "accepted completion helper",
)

service = replace_once(
    service,
    """                        Ok(AgentResponse::Activated { .. }) => {\n                            self.commit_lenient(Command::RecordBindingActive {\n                                binding,\n                                session: *session,\n                                fence: *fence,\n                            })\n                        }\n""",
    """                        Ok(AgentResponse::Activated { .. }) => {\n                            let lease = self.binding_lease(binding);\n                            let accepted = self.try_commit_lenient(Command::RecordBindingActive {\n                                binding,\n                                session: *session,\n                                fence: *fence,\n                            });\n                            if accepted {\n                                self.provider_activations\n                                    .insert((binding, *session, *fence));\n                                if let Some(lease) = lease {\n                                    self.maybe_start_executions(lease)?;\n                                }\n                            }\n                        }\n""",
    "binding activation ack",
)

status_marker = """                (\n                    Tag::Status { lease, machine },\n                    Ok(AgentResponse::Running {\n"""
execution_arm = """                (\n                    Tag::ExecutionStart {\n                        lease,\n                        machine,\n                        session,\n                    },\n                    reply,\n                ) => {\n                    let (lease, machine, session) = (*lease, *machine, *session);\n                    let current = self\n                        .cluster\n                        .sessions\n                        .get(&machine)\n                        .is_some_and(|current| *current == session)\n                        && self\n                            .cluster\n                            .leases\n                            .get(&lease)\n                            .is_some_and(|record| record.state == archon_kernel::LeaseState::Active);\n                    if !current {\n                        eprintln!(\n                            \"archon: ignoring stale execution-start completion for lease {lease} on machine {machine} session {session}\"\n                        );\n                        continue;\n                    }\n                    match reply {\n                        Ok(AgentResponse::ExecutionStarted { lease: got })\n                            if got == lease.as_u64() =>\n                        {\n                            self.execution_started.insert((lease, machine, session));\n                        }\n                        Ok(AgentResponse::ExecutionFailed { lease: got, reason })\n                            if got == lease.as_u64() =>\n                        {\n                            self.commit_lenient(Command::FailLease {\n                                lease,\n                                reason: format!(\n                                    \"execution start failed on machine {machine}: {reason}\"\n                                ),\n                            });\n                        }\n                        Ok(other) => {\n                            self.commit_lenient(Command::FailLease {\n                                lease,\n                                reason: format!(\n                                    \"execution start on machine {machine} returned unexpected response: {other:?}\"\n                                ),\n                            });\n                        }\n                        Err(reason) => {\n                            self.commit_lenient(Command::FailLease {\n                                lease,\n                                reason: format!(\n                                    \"execution start on machine {machine} became unprovable: {reason}\"\n                                ),\n                            });\n                        }\n                    }\n                }\n"""
service = replace_once(
    service,
    status_marker,
    execution_arm + status_marker,
    "execution start completion arm",
)

service = replace_once(
    service,
    """    /// Activate a preparing lease once every enforced binding is prepared:\n    /// commit ActivateLease and ActivateBinding so their effects spawn the\n    /// workload on the agent.\n    fn maybe_activate(&mut self, lease: LeaseId) {\n""",
    """    /// Activate a preparing lease once every enforced binding is prepared.\n    /// This commits resource activation only; workload members start later,\n    /// after every current Binding generation has acknowledged activation.\n    fn maybe_activate(&mut self, lease: LeaseId) {\n""",
    "maybe activate comment",
)

maybe_activate_end = """        for binding in bindings {\n            self.commit_lenient(Command::ActivateBinding { binding });\n        }\n    }\n\n    fn request_status(&mut self, lease: LeaseId) {\n"""
maybe_start = """        for binding in bindings {\n            self.commit_lenient(Command::ActivateBinding { binding });\n        }\n    }\n\n    /// Start each workload member exactly once per Agent session, but only\n    /// after every resource Binding in the rigid root has acknowledged the\n    /// exact generation currently recorded by the kernel.\n    fn maybe_start_executions(&mut self, lease: LeaseId) -> Result<(), Error> {\n        let Some(record) = self.cluster.leases.get(&lease) else {\n            return Ok(());\n        };\n        if record.state != archon_kernel::LeaseState::Active {\n            return Ok(());\n        }\n        let Some((workload, _)) = self.requests.get(&lease) else {\n            return Ok(());\n        };\n        if !workload.execution.has_program() {\n            return Ok(());\n        }\n        let execution = workload.execution.clone();\n        let bindings = self.cluster.bindings_for(lease);\n        let all_provider_active = !bindings.is_empty()\n            && bindings.iter().all(|binding| {\n                self.cluster.bindings.get(binding).is_some_and(|record| {\n                    record.state == archon_kernel::BindingState::Active\n                        && self.provider_activations.contains(&(\n                            *binding,\n                            record.agent_session,\n                            record.fence,\n                        ))\n                })\n            });\n        if !all_provider_active {\n            return Ok(());\n        }\n\n        for machine in self.lease_machines(lease) {\n            // Controller-restart recovery adopts already-running execution\n            // through Status proof; it must never respawn it blindly.\n            if self.recovering_machines.contains_key(&machine) {\n                continue;\n            }\n            let Some(session) = self.cluster.sessions.get(&machine).copied() else {\n                continue;\n            };\n            let key = (lease, machine, session);\n            if self.execution_started.contains(&key)\n                || self.inflight.iter().any(|tag| {\n                    matches!(\n                        tag,\n                        Tag::ExecutionStart {\n                            lease: pending_lease,\n                            machine: pending_machine,\n                            session: pending_session,\n                        } if (*pending_lease, *pending_machine, *pending_session) == key\n                    )\n                })\n            {\n                continue;\n            }\n            let request = AgentRequest::StartExecution {\n                lease: lease.as_u64(),\n                session,\n                epoch: self.cluster.epoch,\n                command: execution.command.clone(),\n                limits: self.lease_limits_on_machine(lease, machine)?,\n                image: execution.image.clone().unwrap_or_default(),\n                storage: execution.storage.clone(),\n                ports: execution.ports.clone(),\n                grace_secs: execution.grace_secs,\n                devices: self.lease_devices_on_machine(lease, machine),\n            };\n            self.dispatch(\n                machine,\n                Tag::ExecutionStart {\n                    lease,\n                    machine,\n                    session,\n                },\n                request,\n            );\n        }\n        Ok(())\n    }\n\n    fn request_status(&mut self, lease: LeaseId) {\n"""
service = replace_once(
    service,
    maybe_activate_end,
    maybe_start,
    "member execution start helper",
)

service = replace_once(
    service,
    """            if let Err(err) = self.rebind_lease_machine(lease, machine) {\n                eprintln!(\"archon: adopting lease {lease} on machine {machine} failed: {err}\");\n                self.recovery_events\n                    .push(RecoveryEvent::MemberRevokedRebindFailed { lease, machine });\n                self.commit_lenient(Command::RevokeLease { lease });\n                finished.push(lease);\n                self.settle_machine_recovery(lease, machine);\n                return;\n            }\n            self.resolve_observed_completion(lease, finished);\n""",
    """            if let Err(err) = self.rebind_lease_machine(lease, machine) {\n                eprintln!(\"archon: adopting lease {lease} on machine {machine} failed: {err}\");\n                self.recovery_events\n                    .push(RecoveryEvent::MemberRevokedRebindFailed { lease, machine });\n                self.commit_lenient(Command::RevokeLease { lease });\n                finished.push(lease);\n                self.settle_machine_recovery(lease, machine);\n                return;\n            }\n            // A running/exited member proves that this Agent still owns the\n            // previously-active Provider endpoints. Rebinding moves those\n            // proofs to the current controller session without respawning.\n            for binding in self.cluster.bindings_for(lease) {\n                if let Some(record) = self.cluster.bindings.get(&binding)\n                    && record.state == archon_kernel::BindingState::Active\n                    && self.cluster.graph.machine_of(record.node) == Some(machine)\n                {\n                    self.provider_activations\n                        .insert((binding, record.agent_session, record.fence));\n                }\n            }\n            if let Some(session) = self.cluster.sessions.get(&machine).copied() {\n                self.execution_started.insert((lease, machine, session));\n            }\n            self.resolve_observed_completion(lease, finished);\n""",
    "recovery adoption proofs",
)

service = replace_once(
    service,
    """        // Reconcile re-drives a machine's live bindings onto its (fresh)\n        // agent: rebind each still-Active binding to the machine's new\n        // session, then ActivateBinding — idempotent for Active bindings,\n        // forward-moving for Preparing ones. The resulting Activate effects\n        // spawn the work again on the new agent process. Revoked or expired\n        // leases stay dead.\n""",
    """        // Reconcile re-drives a machine's live resource Bindings onto\n        // its fresh Agent: rebind each still-Active Binding to the new\n        // session, then ActivateBinding. Current-generation Provider acks\n        // feed the global activation barrier; only then can the member's\n        // separate StartExecution request be sent. Revoked/expired leases\n        // stay dead.\n""",
    "reconcile comment",
)

old_activate = """            Effect::Activate { .. } => AgentRequest::Activate {\n                binding: binding_id.as_u64(),\n                lease: lease.as_u64(),\n                node: record_node.as_u64(),\n                provider: record_provider.as_u64(),\n                scope: record_scope,\n                session,\n                fence,\n                epoch,\n                command: self\n                    .requests\n                    .get(&lease)\n                    .map(|(workload, _)| workload.execution.command.clone())\n                    .unwrap_or_default(),\n                limits: self.lease_limits_on_machine(lease, machine)?,\n                image: self\n                    .requests\n                    .get(&lease)\n                    .and_then(|(workload, _)| workload.execution.image.clone())\n                    .unwrap_or_default(),\n                storage: self\n                    .requests\n                    .get(&lease)\n                    .map(|(workload, _)| workload.execution.storage.clone())\n                    .unwrap_or_default(),\n                ports: self\n                    .requests\n                    .get(&lease)\n                    .map(|(workload, _)| workload.execution.ports.clone())\n                    .unwrap_or_default(),\n                grace_secs: self\n                    .requests\n                    .get(&lease)\n                    .map(|(workload, _)| workload.execution.grace_secs)\n                    .unwrap_or(0),\n                devices: self.lease_devices_on_machine(lease, machine),\n            },\n"""
new_activate = """            Effect::Activate { .. } => AgentRequest::Activate {\n                binding: binding_id.as_u64(),\n                lease: lease.as_u64(),\n                node: record_node.as_u64(),\n                provider: record_provider.as_u64(),\n                scope: record_scope,\n                session,\n                fence,\n                epoch,\n            },\n"""
service = replace_once(service, old_activate, new_activate, "resource-only activate route")

service_path.write_text(service)
