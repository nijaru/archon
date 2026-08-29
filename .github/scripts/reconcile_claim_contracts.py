from pathlib import Path

# Backfill claim contracts when a machine created by a pre-claimability log or
# snapshot returns. Never overwrite a conflicting existing provider contract.
path = Path("crates/node/src/service.rs")
text = path.read_text()
old = '''                if let Err(err) = self.reconcile_device_subtree(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                (machine, false)
'''
new = '''                if let Err(err) = self.reconcile_device_subtree(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_claim_contracts(machine) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                (machine, false)
'''
if old not in text:
    raise SystemExit("returning-agent reconciliation anchor not found")
text = text.replace(old, new, 1)

anchor = '''    /// A returning Agent whose authoritative inventory cannot be reconciled
    /// is not accepted as an execution endpoint. Stop new placement without
'''
helper = '''    /// Reconcile the proof-stage provider contracts for a returning machine.
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
            let expected = update.binding.expect("provider normalization emits additions");
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
if anchor not in text:
    raise SystemExit("claim-contract helper insertion anchor not found")
text = text.replace(anchor, helper + anchor, 1)
path.write_text(text)

# Prove upgrade backfill and conflicting provider ownership.
path = Path("crates/node/tests/claimability.rs")
text = path.read_text()
append = r'''

fn legacy_description() -> MachineDescription {
    MachineDescription {
        instance_id: "legacy-claimability-agent".into(),
        name: "legacy-claimability-agent".into(),
        cpus: 1,
        memory_bytes: 1 << 30,
        host_nodes: Vec::new(),
        devices: Vec::new(),
    }
}

#[test]
fn returning_agent_backfills_claim_contracts_missing_from_older_graph_state() {
    let mut service = NodeService::new();
    let description = legacy_description();
    let (_local, nodes, edges) = archon_node::discover::build_graph(&description, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();

    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let cpu = service.cluster.graph.nodes_of_class(ResourceClass::Cpu)[0];
    let memory = service.cluster.graph.nodes_of_class(ResourceClass::Memory)[0];
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count),
        None
    );

    let returned = service
        .register_agent(
            description,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("validated returning agent backfills current provider contracts");
    assert_eq!(returned, machine);
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count),
        Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        })
    );
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(memory, CapacityDimension::Bytes),
        Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::IndependentShare,
        })
    );
}

#[test]
fn returning_agent_never_overwrites_a_conflicting_claim_provider() {
    let mut service = NodeService::new();
    let description = legacy_description();
    let (_local, nodes, edges) = archon_node::discover::build_graph(&description, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let cpu = service.cluster.graph.nodes_of_class(ResourceClass::Cpu)[0];
    service
        .cluster
        .apply(Command::ApplyResourceFacts {
            nodes: Vec::new(),
            edges: Vec::new(),
            claim_bindings: vec![ClaimBindingUpdate {
                node: cpu,
                dimension: CapacityDimension::Count,
                binding: Some(ClaimBinding {
                    provider: ProviderId::from_u64(99),
                    scope: BindingScope::Exclusive,
                }),
            }],
        })
        .unwrap();

    let error = service
        .register_agent(
            description,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("returning agent must not steal another provider's claim contract");
    assert!(error.to_string().contains("already belongs to provider"));
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count)
            .expect("conflicting ownership remains recorded")
            .provider,
        ProviderId::from_u64(99)
    );
    assert_eq!(
        service.cluster.node_state(machine),
        Some(archon_kernel::NodeState::Unavailable)
    );
}
'''
if "returning_agent_backfills_claim_contracts_missing_from_older_graph_state" in text:
    raise SystemExit("upgrade reconciliation tests already present")
text += append
path.write_text(text)
