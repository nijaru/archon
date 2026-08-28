from pathlib import Path

path = Path("crates/node/src/service.rs")
text = path.read_text()
old = '''    pub fn register_agent(
        &mut self,
        description: crate::discover::MachineDescription,
        executor: Box<dyn LeaseExecutor>,
    ) -> Result<NodeId, Error> {
        // Identity is the agent's instance id, stored in the machine's
'''
new = '''    pub fn register_agent(
        &mut self,
        description: crate::discover::MachineDescription,
        executor: Box<dyn LeaseExecutor>,
    ) -> Result<NodeId, Error> {
        let mut device_ids = BTreeSet::new();
        for device in &description.devices {
            if device.id.trim().is_empty() || device.dev.trim().is_empty() {
                return Err(Error::Refused {
                    explanation: "device stable id and path must be non-empty".into(),
                });
            }
            if !matches!(
                device.kind,
                ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
            ) {
                return Err(Error::Refused {
                    explanation: format!(
                        "resource class {:?} is not valid in a device inventory",
                        device.kind
                    ),
                });
            }
            if !device_ids.insert(device.id.clone()) {
                return Err(Error::Refused {
                    explanation: format!(
                        "provider reported duplicate stable device id {:?}",
                        device.id
                    ),
                });
            }
        }

        // Identity is the agent's instance id, stored in the machine's
'''
assert text.count(old) == 1
text = text.replace(old, new, 1)
old = '''        let mut returning = BTreeSet::new();
        let mut seen = BTreeSet::new();

        for spec in devices {
            if !seen.insert(spec.id.clone()) {
                return Err(Error::Refused {
                    explanation: format!(
                        "provider reported duplicate stable device id {:?}",
                        spec.id
                    ),
                });
            }

            let id = if let Some(id) = existing.remove(&spec.id) {
'''
new = '''        let mut returning = BTreeSet::new();

        for spec in devices {
            let id = if let Some(id) = existing.remove(&spec.id) {
'''
assert text.count(old) == 1
path.write_text(text.replace(old, new, 1))

path = Path("crates/node/tests/devices.rs")
tests = path.read_text()
assert "fn initial_registration_rejects_invalid_device_inventory()" not in tests
tests += r'''

#[test]
fn initial_registration_rejects_invalid_device_inventory() {
    let mut service = NodeService::new();
    let mut duplicate = description("inst-invalid", "/dev/gpuA");
    duplicate.devices.push(duplicate.devices[0].clone());
    assert!(
        service
            .register_agent(
                duplicate,
                Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
            )
            .is_err()
    );
    assert!(service.cluster.graph.nodes_of_class(ResourceClass::Machine).is_empty());

    let mut invalid_kind = description("inst-invalid-kind", "/dev/gpuA");
    invalid_kind.devices[0].kind = ResourceClass::Cpu;
    assert!(
        service
            .register_agent(
                invalid_kind,
                Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
            )
            .is_err()
    );
    assert!(service.cluster.graph.nodes_of_class(ResourceClass::Machine).is_empty());
}
'''
path.write_text(tests)
