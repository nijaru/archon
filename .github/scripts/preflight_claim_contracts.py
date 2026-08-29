from pathlib import Path

path = Path("crates/node/src/service.rs")
text = path.read_text()
old = '''                if let Err(err) = self.reconcile_device_subtree(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_claim_contracts(machine, &description.devices) {
'''
new = '''                if let Err(err) = self.preflight_claim_contracts(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_device_subtree(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                if let Err(err) = self.reconcile_claim_contracts(machine, &description.devices) {
'''
if old not in text:
    raise SystemExit("registration preflight anchor not found")
text = text.replace(old, new, 1)

old = '''    /// Reconcile proof-stage provider contracts only for resources present in
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
'''
new = '''    fn current_claim_contract_nodes(
        &self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Vec<archon_kernel::Node> {
        let current_devices: BTreeSet<String> =
            devices.iter().map(|device| device.id.clone()).collect();
        let mut ids = self.cluster.graph.descendants(machine);
        ids.push(machine);
        ids.into_iter()
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
            .collect()
    }

    /// Reject conflicting current provider contracts before returning-device
    /// reconciliation can mutate facts, availability, or Lease state.
    fn preflight_claim_contracts(
        &self,
        machine: NodeId,
        devices: &[crate::discover::DeviceSpec],
    ) -> Result<(), Error> {
        let nodes = self.current_claim_contract_nodes(machine, devices);
        self.missing_claim_contracts(&nodes).map(|_| ())
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
        let nodes = self.current_claim_contract_nodes(machine, devices);
        let missing = self.missing_claim_contracts(&nodes)?;
'''
if old not in text:
    raise SystemExit("contract-node factoring anchor not found")
text = text.replace(old, new, 1)
path.write_text(text)

path = Path("crates/node/tests/claimability.rs")
text = path.read_text()
append = r'''

#[test]
fn returning_host_contract_conflict_prevents_device_reconciliation_mutation() {
    let mut service = NodeService::new();
    let mut legacy = legacy_description();
    legacy.devices = vec![legacy_device("gpu-1", "/dev/gpu-old")];
    let (_local, nodes, edges) = archon_node::discover::build_graph(&legacy, 0);
    service
        .cluster
        .apply(Command::ApplyGraph { nodes, edges })
        .unwrap();
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let cpu = service.cluster.graph.nodes_of_class(ResourceClass::Cpu)[0];
    let gpu = device_node(&service, "gpu-1");
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

    let mut returning = legacy;
    returning.devices = vec![legacy_device("gpu-1", "/dev/gpu-new")];
    let error = service
        .register_agent(
            returning,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("host provider conflict must fail before device reconciliation");
    assert!(error.to_string().contains("already belongs to provider"));
    assert_eq!(
        service
            .cluster
            .graph
            .claim_binding(cpu, CapacityDimension::Count)
            .expect("conflicting host provider remains recorded")
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
if "returning_host_contract_conflict_prevents_device_reconciliation_mutation" in text:
    raise SystemExit("preflight regression already present")
text += append
path.write_text(text)
