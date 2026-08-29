from pathlib import Path

path = Path("crates/node/src/service.rs")
text = path.read_text()

old = '''                if let Err(err) = self.reconcile_claim_contracts(machine) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
'''
new = '''                if let Err(err) = self.reconcile_claim_contracts(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
'''
if old not in text:
    raise SystemExit("reconcile call anchor not found")
text = text.replace(old, new, 1)

old = '''    /// Reconcile the proof-stage provider contracts for a returning machine.
    /// Older snapshots/logs predate `ClaimBinding`, so their Graph deserializes
    /// with an empty map. A validated returning Agent may backfill those
    /// missing contracts, but it must never overwrite a different provider or
    /// scope already recorded for the same capacity dimension.
    fn reconcile_claim_contracts(&mut self, machine: NodeId) -> Result<(), Error> {
        let mut ids = self.cluster.graph.descendants(machine);
        ids.push(machine);
        let nodes: Vec<_> = ids
            .into_iter()
            .filter_map(|id| self.cluster.graph.node(id).cloned())
            .collect();
        let mut missing = Vec::new();
        for update in crate::discover::claim_bindings(&nodes) {
            let expected = update
                .binding
                .expect("provider normalization emits additions");
            match self
                .cluster
                .graph
                .claim_binding(update.node, update.dimension)
            {
                None => missing.push(update),
                Some(actual) if actual == expected => {}
                Some(actual) => {
                    return Err(Error::Refused {
                        explanation: format!(
                            "resource {} dimension {} already belongs to provider {} with {:?} scope; returning agent reports provider {} with {:?} scope",
                            update.node,
                            update.dimension,
                            actual.provider,
                            actual.scope,
                            expected.provider,
                            expected.scope,
                        ),
                    });
                }
            }
        }
        if !missing.is_empty() {
            self.commit(Command::ApplyResourceFacts {
                nodes: Vec::new(),
                edges: Vec::new(),
                claim_bindings: missing,
            })?;
        }
        Ok(())
    }
'''
new = '''    fn missing_claim_contracts(
        &self,
        nodes: &[archon_kernel::Node],
    ) -> Result<Vec<archon_kernel::ClaimBindingUpdate>, Error> {
        let mut missing = Vec::new();
        for update in crate::discover::claim_bindings(nodes) {
            let expected = update
                .binding
                .expect("provider normalization emits additions");
            match self
                .cluster
                .graph
                .claim_binding(update.node, update.dimension)
            {
                None => missing.push(update),
                Some(actual) if actual == expected => {}
                Some(actual) => {
                    return Err(Error::Refused {
                        explanation: format!(
                            "resource {} dimension {} already belongs to provider {} with {:?} scope; returning agent reports provider {} with {:?} scope",
                            update.node,
                            update.dimension,
                            actual.provider,
                            actual.scope,
                            expected.provider,
                            expected.scope,
                        ),
                    });
                }
            }
        }
        Ok(missing)
    }

    /// Reconcile proof-stage provider contracts only for resources present in
    /// the returning Agent's validated authoritative inventory. Older Graphs
    /// may have no `ClaimBinding` metadata, but a provider-omitted tombstone
    /// must never regain ownership merely because its stale Node still exists.
    fn reconcile_claim_contracts(
        &mut self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        let current_devices: BTreeSet<String> =
            devices.iter().map(|device| device.id.clone()).collect();
        let mut ids = self.cluster.graph.descendants(machine);
        ids.push(machine);
        let nodes: Vec<_> = ids
            .into_iter()
            .filter_map(|id| self.cluster.graph.node(id))
            .filter(|node| {
                !matches!(
                    node.kind,
                    ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
                ) || node
                    .attrs
                    .get("id")
                    .is_some_and(|id| current_devices.contains(id))
            })
            .cloned()
            .collect();
        let missing = self.missing_claim_contracts(&nodes)?;
        if !missing.is_empty() {
            self.commit(Command::ApplyResourceFacts {
                nodes: Vec::new(),
                edges: Vec::new(),
                claim_bindings: missing,
            })?;
        }
        Ok(())
    }
'''
if old not in text:
    raise SystemExit("reconcile helper anchor not found")
text = text.replace(old, new, 1)

old = '''        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut returning = BTreeSet::new();
'''
new = '''        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut returning = BTreeSet::new();
        let mut reported_existing = BTreeSet::new();
'''
if old not in text:
    raise SystemExit("device staging anchor not found")
text = text.replace(old, new, 1)

old = '''                if node.kind != spec.kind {
                    return Err(Error::Refused {
                        explanation: format!(
                            "device {:?} changed resource class; replacement requires a new stable id",
                            spec.id
                        ),
                    });
                }
                match self.cluster.node_state(id) {
'''
new = '''                if node.kind != spec.kind {
                    return Err(Error::Refused {
                        explanation: format!(
                            "device {:?} changed resource class; replacement requires a new stable id",
                            spec.id
                        ),
                    });
                }
                reported_existing.insert(id);
                match self.cluster.node_state(id) {
'''
if old not in text:
    raise SystemExit("reported existing anchor not found")
text = text.replace(old, new, 1)

old = '''        if !nodes.is_empty() || !edges.is_empty() {
            let claim_bindings = crate::discover::claim_bindings(&nodes);
            self.commit(Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings,
            })?;
        }
'''
new = '''        let existing_nodes: Vec<_> = reported_existing
            .iter()
            .filter_map(|id| self.cluster.graph.node(*id).cloned())
            .collect();
        let mut claim_bindings = self.missing_claim_contracts(&existing_nodes)?;
        let fresh_nodes: Vec<_> = nodes
            .iter()
            .filter(|node| !reported_existing.contains(&node.id))
            .cloned()
            .collect();
        claim_bindings.extend(crate::discover::claim_bindings(&fresh_nodes));
        if !nodes.is_empty() || !edges.is_empty() || !claim_bindings.is_empty() {
            self.commit(Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings,
            })?;
        }
'''
if old not in text:
    raise SystemExit("device commit anchor not found")
text = text.replace(old, new, 1)
path.write_text(text)

path = Path("crates/node/tests/claimability.rs")
text = path.read_text()
text = text.replace(
    "use archon_node::discover::MachineDescription;",
    "use archon_node::discover::{DeviceSpec, MachineDescription};",
    1,
)
append = r'''

fn legacy_device(id: &str, dev: &str) -> DeviceSpec {
    DeviceSpec {
        kind: ResourceClass::Gpu,
        id: id.into(),
        dev: dev.into(),
        host_parent: None,
        access: Vec::new(),
        attrs: Default::default(),
    }
}

fn device_node(service: &NodeService, stable_id: &str) -> NodeId {
    service
        .cluster
        .graph
        .nodes_of_class(ResourceClass::Gpu)
        .iter()
        .copied()
        .find(|id| {
            service
                .cluster
                .graph
                .node(*id)
                .and_then(|node| node.attrs.get("id"))
                .is_some_and(|id| id == stable_id)
        })
        .expect("device is present in legacy Graph")
}

#[test]
fn returning_agent_backfills_only_devices_in_current_provider_inventory() {
    let mut service = NodeService::new();
    let mut legacy = legacy_description();
    legacy.devices = vec![
        legacy_device("gpu-keep", "/dev/gpu-keep"),
        legacy_device("gpu-gone", "/dev/gpu-gone"),
    ];
    let (_local, nodes, edges) = archon_node::discover::build_graph(&legacy, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let kept = device_node(&service, "gpu-keep");
    let gone = device_node(&service, "gpu-gone");

    let mut returning = legacy;
    returning.devices = vec![legacy_device("gpu-keep", "/dev/gpu-keep")];
    service
        .register_agent(
            returning,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("current provider inventory reconciles");

    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(kept, CapacityDimension::Count),
        Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        })
    );
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(gone, CapacityDimension::Count),
        None
    );
    assert_eq!(
        service.cluster.node_state(gone),
        Some(archon_kernel::NodeState::Unavailable)
    );
}

#[test]
fn returning_device_fact_change_never_overwrites_conflicting_provider_contract() {
    let mut service = NodeService::new();
    let mut legacy = legacy_description();
    legacy.devices = vec![legacy_device("gpu-1", "/dev/gpu-old")];
    let (_local, nodes, edges) = archon_node::discover::build_graph(&legacy, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let gpu = device_node(&service, "gpu-1");
    service
        .cluster
        .apply(Command::ApplyResourceFacts {
            nodes: Vec::new(),
            edges: Vec::new(),
            claim_bindings: vec![ClaimBindingUpdate {
                node: gpu,
                dimension: CapacityDimension::Count,
                binding: Some(ClaimBinding {
                    provider: ProviderId::from_u64(99),
                    scope: BindingScope::Exclusive,
                }),
            }],
        })
        .unwrap();

    let mut returning = legacy;
    returning.devices = vec![legacy_device("gpu-1", "/dev/gpu-new")];
    let error = service
        .register_agent(
            returning,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("device fact refresh must not steal another provider contract");
    assert!(error.to_string().contains("already belongs to provider"));
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(gpu, CapacityDimension::Count)
            .expect("conflicting provider remains recorded")
            .provider,
        ProviderId::from_u64(99)
    );
    assert_eq!(
        service
            .cluster
            .graph
            .node(gpu)
            .and_then(|node| node.attrs.get("dev"))
            .map(String::as_str),
        Some("/dev/gpu-old")
    );
    assert_eq!(
        service.cluster.node_state(machine),
        Some(archon_kernel::NodeState::Unavailable)
    );
}
'''
if "returning_agent_backfills_only_devices_in_current_provider_inventory" in text:
    raise SystemExit("semantic reconciliation tests already present")
text += append
path.write_text(text)
