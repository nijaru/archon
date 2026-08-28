from pathlib import Path
import re

# This script refines the branch-only normalized-host patch before it is
# compiled. The final verified commit removes both patch scripts.

# --- discovery schema and graph normalization ----------------------------
path = Path("crates/node/src/discover.rs")
text = path.read_text()
text = text.replace(
    "    pub memory: archon_kernel::NodeId,\n",
    "    pub memory: Vec<archon_kernel::NodeId>,\n",
    1,
)
text = text.replace(
    "    pub topology: Vec<TopologySpec>,\n",
    "    pub host_nodes: Vec<HostNodeSpec>,\n",
    1,
)
text = text.replace("pub struct TopologySpec {", "pub struct HostNodeSpec {", 1)
text = text.replace(
    "/// One normalized host resource/topology node. `id` is stable only within\n"
    "/// this machine/provider inventory and exists to make parent references\n"
    "/// deterministic; Archon assigns the durable Graph NodeId on first admission.\n",
    "/// One provider-normalized host resource or topology node. `id` is stable\n"
    "/// within this machine inventory and is used only to compose containment;\n"
    "/// Archon assigns the durable Graph NodeId when the machine first joins.\n",
    1,
)
text = text.replace(
    "    /// Parent topology id; None attaches directly to the Machine.\n",
    "    /// Parent host-node id; None attaches directly to the Machine.\n",
    1,
)
text = text.replace(
    "        topology: Vec::new(),\n        devices,\n",
    "        host_nodes: Vec::new(),\n        devices,\n",
    1,
)

# Device providers may reference a host-provider node explicitly. This is the
# composition seam; free-form capability attrs do not decide containment.
anchor = '''    pub dev: String,
    /// Additional host device paths required to use this logical resource.
'''
replacement = '''    pub dev: String,
    /// Provider-normalized host node that contains this device. None means
    /// the device provider makes no hard host-locality assertion.
    #[serde(default)]
    pub host_parent: Option<String>,
    /// Additional host device paths required to use this logical resource.
'''
if text.count(anchor) != 1:
    raise RuntimeError("DeviceSpec host-parent anchor changed")
text = text.replace(anchor, replacement, 1)

# Reserved composition attrs are internal Graph facts, never provider attrs.
insert_before = "/// A machine as the agent reports it; the controller builds the graph.\n"
constants = '''pub(crate) const HOST_ID_ATTR: &str = "archon.host-id";
pub(crate) const DEVICE_HOST_PARENT_ATTR: &str = "archon.host-parent";

'''
if text.count(insert_before) != 1:
    raise RuntimeError("MachineDescription comment anchor changed")
text = text.replace(insert_before, constants + insert_before, 1)

build_start = text.index("/// Build the Archon graph for a machine description. A normalized topology")
build_end = text.index("/// Discover this machine and build its graph.", build_start)
new_build = r'''/// Build the Archon graph for a validated machine description. A normalized
/// host inventory produces the provider-authored containment tree; an empty
/// inventory retains the exclusive-v0 flat Machine -> CPU/Memory shape.
pub fn build_graph(
    description: &MachineDescription,
    first_id: u64,
) -> (LocalMachine, Vec<Node>, Vec<Edge>) {
    let mut ids = IdGen { next: first_id };
    let machine = ids.node();

    let mut name = Attrs::new();
    name.insert("name".into(), description.name.clone());
    name.insert("agent_id".into(), description.instance_id.clone());
    let mut nodes = vec![Node {
        id: machine,
        kind: ResourceClass::Machine,
        attrs: name,
        capacity: Quantity::new(),
    }];
    let mut edges = Vec::new();
    let mut host_ids = BTreeMap::new();

    let (cpus, memory) = if description.host_nodes.is_empty() {
        // The current endpoint model is exclusive per (provider, Node), so
        // each logical CPU remains an exclusive count=1 Node. Aggregating CPU
        // pools is only correct after independent shared-capacity Bindings.
        let cpus: Vec<_> = (0..description.cpus).map(|_| ids.node()).collect();
        let memory = ids.node();
        nodes.push(Node {
            id: memory,
            kind: ResourceClass::Memory,
            attrs: Attrs::new(),
            capacity: qty(CapacityDimension::Bytes, description.memory_bytes),
        });
        edges.push(Edge {
            from: machine,
            to: memory,
            kind: EdgeKind::Contains,
            attrs: Attrs::new(),
        });
        for cpu in &cpus {
            nodes.push(Node {
                id: *cpu,
                kind: ResourceClass::Cpu,
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            });
            edges.push(Edge {
                from: machine,
                to: *cpu,
                kind: EdgeKind::Contains,
                attrs: Attrs::new(),
            });
        }
        (cpus, vec![memory])
    } else {
        let mut specs: Vec<_> = description.host_nodes.iter().collect();
        specs.sort_by(|left, right| left.id.cmp(&right.id));
        for spec in &specs {
            let id = ids.node();
            host_ids.insert(spec.id.clone(), id);
            let mut attrs = spec.attrs.clone();
            attrs.insert(HOST_ID_ATTR.into(), spec.id.clone());
            nodes.push(Node {
                id,
                kind: spec.kind,
                attrs,
                capacity: spec.capacity.clone(),
            });
        }
        for spec in &specs {
            let child = host_ids[&spec.id];
            let parent = spec
                .parent
                .as_ref()
                .map(|parent| host_ids[parent])
                .unwrap_or(machine);
            edges.push(Edge {
                from: parent,
                to: child,
                kind: EdgeKind::Contains,
                attrs: Attrs::new(),
            });
        }
        let cpus = specs
            .iter()
            .filter(|spec| spec.kind == ResourceClass::Cpu)
            .map(|spec| host_ids[&spec.id])
            .collect();
        let memory = specs
            .iter()
            .filter(|spec| spec.kind == ResourceClass::Memory)
            .map(|spec| host_ids[&spec.id])
            .collect();
        (cpus, memory)
    };

    for device_spec in &description.devices {
        let device = ids.node();
        let mut attrs = device_spec.attrs.clone();
        attrs.insert("id".into(), device_spec.id.clone());
        attrs.insert("dev".into(), device_spec.dev.clone());
        if !device_spec.access.is_empty() {
            attrs.insert(
                "access".into(),
                serde_json::to_string(&device_spec.access).expect("device access is serializable"),
            );
        }
        if let Some(parent) = &device_spec.host_parent {
            attrs.insert(DEVICE_HOST_PARENT_ATTR.into(), parent.clone());
        }
        nodes.push(Node {
            id: device,
            kind: device_spec.kind,
            attrs,
            capacity: qty(CapacityDimension::Count, 1),
        });
        let parent = device_spec
            .host_parent
            .as_ref()
            .map(|parent| host_ids[parent])
            .unwrap_or(machine);
        edges.push(Edge {
            from: parent,
            to: device,
            kind: EdgeKind::Contains,
            attrs: Attrs::new(),
        });
    }

    (
        LocalMachine {
            machine,
            cpus,
            memory,
        },
        nodes,
        edges,
    )
}

/// Validate the complete provider-normalized machine inventory before it can
/// become authoritative Graph state. Host topology and device inventories have
/// distinct writers, joined only by DeviceSpec.host_parent.
pub(crate) fn validate_machine_description(description: &MachineDescription) -> Result<(), String> {
    let mut host_by_id = BTreeMap::new();
    let mut cpu_count = 0u64;
    let mut memory_total = 0u64;
    let mut memory_nodes = 0usize;

    for spec in &description.host_nodes {
        if spec.id.trim().is_empty() {
            return Err("host node id must be non-empty".into());
        }
        if spec.attrs.contains_key(HOST_ID_ATTR) || spec.attrs.contains_key(DEVICE_HOST_PARENT_ATTR) {
            return Err(format!("host node {:?} uses a reserved Archon attribute", spec.id));
        }
        if host_by_id.insert(spec.id.clone(), spec).is_some() {
            return Err(format!("duplicate host node id {:?}", spec.id));
        }
        match spec.kind {
            ResourceClass::Socket | ResourceClass::Numa | ResourceClass::PcieRoot => {
                if !spec.capacity.is_empty() {
                    return Err(format!(
                        "structural host node {:?} must not advertise claimable capacity",
                        spec.id
                    ));
                }
            }
            ResourceClass::Cpu => {
                if spec.capacity != qty(CapacityDimension::Count, 1) {
                    return Err(format!(
                        "logical CPU {:?} must advertise exactly count=1 under the exclusive v0 binding model",
                        spec.id
                    ));
                }
                cpu_count += 1;
            }
            ResourceClass::Memory => {
                if spec
                    .capacity
                    .keys()
                    .any(|dimension| *dimension != CapacityDimension::Bytes)
                {
                    return Err(format!(
                        "memory resource {:?} must advertise only byte capacity",
                        spec.id
                    ));
                }
                memory_total = memory_total
                    .checked_add(quantity_get(&spec.capacity, CapacityDimension::Bytes))
                    .ok_or_else(|| "host memory total overflows".to_string())?;
                memory_nodes += 1;
            }
            other => {
                return Err(format!(
                    "resource class {other} is not valid in the host inventory"
                ));
            }
        }
    }

    for spec in &description.host_nodes {
        if let Some(parent) = &spec.parent {
            let parent_spec = host_by_id.get(parent).ok_or_else(|| {
                format!("host node {:?} references unknown parent {parent:?}", spec.id)
            })?;
            if matches!(parent_spec.kind, ResourceClass::Cpu | ResourceClass::Memory) {
                return Err(format!(
                    "host node {:?} cannot be contained by claimable leaf {parent:?}",
                    spec.id
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut current = Some(spec.id.as_str());
        while let Some(id) = current {
            if !seen.insert(id) {
                return Err(format!("host containment cycle through {id:?}"));
            }
            current = host_by_id.get(id).and_then(|node| node.parent.as_deref());
        }
    }

    if !description.host_nodes.is_empty() {
        if cpu_count != description.cpus {
            return Err(format!(
                "host inventory reports {cpu_count} logical CPUs but machine summary reports {}",
                description.cpus
            ));
        }
        if memory_nodes == 0 || memory_total != description.memory_bytes {
            return Err(format!(
                "host inventory reports {memory_total} memory bytes across {memory_nodes} nodes but machine summary reports {}",
                description.memory_bytes
            ));
        }
    }

    let mut device_ids = std::collections::BTreeSet::new();
    for device in &description.devices {
        if device.id.trim().is_empty() || device.dev.trim().is_empty() {
            return Err("device stable id and path must be non-empty".into());
        }
        if !matches!(
            device.kind,
            ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
        ) {
            return Err(format!(
                "resource class {:?} is not valid in a device inventory",
                device.kind
            ));
        }
        if !device_ids.insert(device.id.clone()) {
            return Err(format!(
                "provider reported duplicate stable device id {:?}",
                device.id
            ));
        }
        if device.attrs.contains_key(HOST_ID_ATTR)
            || device.attrs.contains_key(DEVICE_HOST_PARENT_ATTR)
        {
            return Err(format!(
                "device {:?} uses a reserved Archon attribute",
                device.id
            ));
        }
        if let Some(parent) = &device.host_parent {
            let parent_spec = host_by_id.get(parent).ok_or_else(|| {
                format!(
                    "device {:?} references unknown host parent {parent:?}",
                    device.id
                )
            })?;
            if matches!(parent_spec.kind, ResourceClass::Cpu | ResourceClass::Memory) {
                return Err(format!(
                    "device {:?} cannot be contained by claimable host leaf {parent:?}",
                    device.id
                ));
            }
        }
    }
    Ok(())
}

/// Verify that a returning Agent reports exactly the host facts already
/// committed for its machine. Graph replacement/reparenting is not yet an
/// atomic operation, so a hard host change fails closed instead of silently
/// scheduling against stale CPU/memory/topology state.
pub(crate) fn verify_registered_host(
    graph: &archon_kernel::Graph,
    machine: archon_kernel::NodeId,
    description: &MachineDescription,
) -> Result<(), String> {
    let descendants = graph.descendants(machine);
    let normalized = descendants.iter().any(|id| {
        graph
            .node(*id)
            .is_some_and(|node| node.attrs.contains_key(HOST_ID_ATTR))
    });

    if description.host_nodes.is_empty() {
        if normalized {
            return Err("returning agent omitted previously authoritative normalized host topology".into());
        }
        let cpu_nodes: Vec<_> = descendants
            .iter()
            .filter_map(|id| graph.node(*id))
            .filter(|node| node.kind == ResourceClass::Cpu)
            .collect();
        let memory_nodes: Vec<_> = descendants
            .iter()
            .filter_map(|id| graph.node(*id))
            .filter(|node| node.kind == ResourceClass::Memory)
            .collect();
        if cpu_nodes.len() as u64 != description.cpus
            || cpu_nodes.iter().any(|node| {
                node.capacity != qty(CapacityDimension::Count, 1)
            })
            || memory_nodes.len() != 1
            || quantity_get(&memory_nodes[0].capacity, CapacityDimension::Bytes)
                != description.memory_bytes
        {
            return Err("returning agent changed host CPU or memory facts".into());
        }
        return Ok(());
    }

    let mut actual = Vec::new();
    for id in descendants {
        let Some(node) = graph.node(id) else { continue };
        let Some(host_id) = node.attrs.get(HOST_ID_ATTR) else {
            continue;
        };
        let parent = match graph.parent(id) {
            Some(parent) if parent == machine => None,
            Some(parent) => Some(
                graph
                    .node(parent)
                    .and_then(|node| node.attrs.get(HOST_ID_ATTR))
                    .cloned()
                    .ok_or_else(|| {
                        format!("host node {host_id:?} has a non-host containment parent")
                    })?,
            ),
            None => return Err(format!("host node {host_id:?} lost containment")),
        };
        let mut attrs = node.attrs.clone();
        attrs.remove(HOST_ID_ATTR);
        actual.push(HostNodeSpec {
            id: host_id.clone(),
            kind: node.kind,
            parent,
            attrs,
            capacity: node.capacity.clone(),
        });
    }
    let mut expected = description.host_nodes.clone();
    actual.sort_by(|left, right| left.id.cmp(&right.id));
    expected.sort_by(|left, right| left.id.cmp(&right.id));
    if actual != expected {
        return Err("returning agent changed authoritative host topology or capacity".into());
    }
    Ok(())
}

/// Resolve one explicit device -> host-provider containment reference.
pub(crate) fn host_parent_node(
    graph: &archon_kernel::Graph,
    machine: archon_kernel::NodeId,
    host_parent: &str,
) -> Result<archon_kernel::NodeId, String> {
    graph
        .descendants(machine)
        .into_iter()
        .find(|id| {
            graph
                .node(*id)
                .and_then(|node| node.attrs.get(HOST_ID_ATTR))
                .is_some_and(|id| id == host_parent)
        })
        .ok_or_else(|| format!("unknown committed host parent {host_parent:?}"))
}

'''
text = text[:build_start] + new_build + text[build_end:]

# Replace the branch-only tests added by the first patch with tests for the
# refined ownership/composition contract.
test_start = text.index("    #[test]\n    fn normalized_topology_builds_cpu_memory_and_device_locality()")
test_end = text.rfind("}\n")
new_tests = r'''    #[test]
    fn normalized_host_nodes_build_explicit_device_locality() {
        let mut numa_attrs = Attrs::new();
        numa_attrs.insert("os_index".into(), "0".into());
        let description = MachineDescription {
            instance_id: "topology-test".into(),
            name: "topology-box".into(),
            cpus: 1,
            memory_bytes: 4096,
            host_nodes: vec![
                HostNodeSpec {
                    id: "numa/0".into(),
                    kind: ResourceClass::Numa,
                    parent: None,
                    attrs: numa_attrs,
                    capacity: Quantity::new(),
                },
                HostNodeSpec {
                    id: "cpu/0".into(),
                    kind: ResourceClass::Cpu,
                    parent: Some("numa/0".into()),
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
                HostNodeSpec {
                    id: "memory/0".into(),
                    kind: ResourceClass::Memory,
                    parent: Some("numa/0".into()),
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Bytes, 4096),
                },
                HostNodeSpec {
                    id: "pcie/0000:00:01.0".into(),
                    kind: ResourceClass::PcieRoot,
                    parent: Some("numa/0".into()),
                    attrs: Attrs::new(),
                    capacity: Quantity::new(),
                },
            ],
            devices: vec![DeviceSpec {
                kind: ResourceClass::Gpu,
                id: "gpu0".into(),
                dev: "/dev/gpu0".into(),
                host_parent: Some("pcie/0000:00:01.0".into()),
                access: Vec::new(),
                attrs: Attrs::new(),
            }],
        };
        validate_machine_description(&description).expect("valid normalized inventory");
        let (_local, nodes, edges) = build_graph(&description, 0);
        let mut graph = archon_kernel::Graph::new();
        graph.apply(nodes, edges).expect("apply normalized graph");
        let cpu = graph.nodes_of_class(ResourceClass::Cpu)[0];
        let memory = graph.nodes_of_class(ResourceClass::Memory)[0];
        let gpu = graph.nodes_of_class(ResourceClass::Gpu)[0];
        let numa = graph.nodes_of_class(ResourceClass::Numa)[0];
        let pcie = graph.nodes_of_class(ResourceClass::PcieRoot)[0];
        assert_eq!(graph.parent(gpu), Some(pcie));
        assert_eq!(graph.parent(pcie), Some(numa));
        for node in [cpu, memory, gpu] {
            assert_eq!(graph.ancestor_of_class(node, ResourceClass::Numa), Some(numa));
        }
    }

    #[test]
    fn normalized_host_inventory_must_be_complete_acyclic_and_owned() {
        let mut description = MachineDescription {
            instance_id: "bad-topology".into(),
            name: "bad".into(),
            cpus: 1,
            memory_bytes: 1024,
            host_nodes: vec![HostNodeSpec {
                id: "cpu/0".into(),
                kind: ResourceClass::Cpu,
                parent: Some("missing".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            }],
            devices: Vec::new(),
        };
        assert!(
            validate_machine_description(&description)
                .unwrap_err()
                .contains("unknown parent")
        );
        description.host_nodes = vec![
            HostNodeSpec {
                id: "numa/0".into(),
                kind: ResourceClass::Numa,
                parent: Some("socket/0".into()),
                attrs: Attrs::new(),
                capacity: Quantity::new(),
            },
            HostNodeSpec {
                id: "socket/0".into(),
                kind: ResourceClass::Socket,
                parent: Some("numa/0".into()),
                attrs: Attrs::new(),
                capacity: Quantity::new(),
            },
            HostNodeSpec {
                id: "cpu/0".into(),
                kind: ResourceClass::Cpu,
                parent: Some("numa/0".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "memory/0".into(),
                kind: ResourceClass::Memory,
                parent: Some("numa/0".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Bytes, 1024),
            },
        ];
        assert!(
            validate_machine_description(&description)
                .unwrap_err()
                .contains("cycle")
        );
    }
'''
text = text[:test_start] + new_tests + text[test_end:]
path.write_text(text)

# --- add DeviceSpec.host_parent=None to all existing proof constructors ---
pattern = re.compile(
    r"(DeviceSpec\s*\{.*?\n(?P<indent>\s*)dev\s*:\s*[^\n]+,\n)(?P=indent)(access\s*:)",
    re.S,
)
for rust in Path("crates").rglob("*.rs"):
    source = rust.read_text()
    updated = pattern.sub(
        lambda m: m.group(1)
        + m.group("indent")
        + "host_parent: None,\n"
        + m.group("indent")
        + m.group(3),
        source,
    )
    rust.write_text(updated)

# The new tests above intentionally set an explicit parent; restore it after
# the generic constructor migration if the regex inserted a duplicate None.
path = Path("crates/node/src/discover.rs")
text = path.read_text().replace(
    '''                dev: "/dev/gpu0".into(),
                host_parent: None,
                host_parent: Some("pcie/0000:00:01.0".into()),
''',
    '''                dev: "/dev/gpu0".into(),
                host_parent: Some("pcie/0000:00:01.0".into()),
''',
)
path.write_text(text)

# --- wire field naming: host_nodes, not a raw hwloc/topology mirror -------
path = Path("crates/node/src/protocol.rs")
text = path.read_text()
text = text.replace(
    "topology: Vec<crate::discover::TopologySpec>",
    "host_nodes: Vec<crate::discover::HostNodeSpec>",
)
path.write_text(text)

path = Path("crates/node/src/agent.rs")
text = path.read_text().replace(
    "topology: description.topology,",
    "host_nodes: description.host_nodes,",
)
path.write_text(text)

path = Path("crates/cli/src/main.rs")
text = path.read_text().replace(
    "topology: description.topology.clone(),",
    "host_nodes: description.host_nodes.clone(),",
)
path.write_text(text)

path = Path("crates/control/src/server.rs")
text = path.read_text()
text = text.replace(
    "                topology,\n                devices,",
    "                host_nodes,\n                devices,",
    1,
)
text = text.replace(
    "                    topology,\n                    devices,",
    "                    host_nodes,\n                    devices,",
    1,
)
text = text.replace(
    "        topology: Vec<archon_node::discover::TopologySpec>,",
    "        host_nodes: Vec<archon_node::discover::HostNodeSpec>,",
    1,
)
text = text.replace(
    "            topology,\n            devices,",
    "            host_nodes,\n            devices,",
    1,
)
path.write_text(text)

# Existing MachineDescription literals were made explicit by the first patch.
# Rename only the newly-added exact field spelling; Request topology uses vec![]
# in the current source and is unaffected.
for rust in Path("crates").rglob("*.rs"):
    source = rust.read_text().replace("topology: Vec::new(),", "host_nodes: Vec::new(),")
    rust.write_text(source)

# --- controller registration: validate and fail closed on hard fact change -
path = Path("crates/node/src/service.rs")
text = path.read_text()
# Role-only greeting is not a machine inventory; it carries an empty fragment.
old = '''        let greeting = crate::protocol::Greeting::Agent {
            instance_id: String::new(),
            name: String::new(),
            cpus: 0,
            memory_bytes: 0,
            devices: Vec::new(),
        };
'''
new = '''        let greeting = crate::protocol::Greeting::Agent {
            instance_id: String::new(),
            name: String::new(),
            cpus: 0,
            memory_bytes: 0,
            host_nodes: Vec::new(),
            devices: Vec::new(),
        };
'''
if text.count(old) != 1:
    raise RuntimeError("role-only Agent greeting changed")
text = text.replace(old, new, 1)
text = text.replace(
    '''                cpus,
                memory_bytes,
                topology,
                devices,
            } => Ok(crate::discover::MachineDescription {
                instance_id: String::new(),
                name,
                cpus,
                memory_bytes,
                topology,
                devices,
''',
    '''                cpus,
                memory_bytes,
                host_nodes,
                devices,
            } => Ok(crate::discover::MachineDescription {
                instance_id: String::new(),
                name,
                cpus,
                memory_bytes,
                host_nodes,
                devices,
''',
    1,
)
validation_start = text.index("        crate::discover::validate_topology(&description)")
identity_start = text.index("        // Identity is the agent's instance id", validation_start)
text = (
    text[:validation_start]
    + '''        crate::discover::validate_machine_description(&description).map_err(|explanation| {
            Error::Refused { explanation }
        })?;

'''
    + text[identity_start:]
)
old = '''            Some(machine) => {
                self.reconcile_device_subtree(machine, &description.devices)?;
                (machine, false)
            }
'''
new = '''            Some(machine) => {
                if let Err(explanation) = crate::discover::verify_registered_host(
                    &self.cluster.graph,
                    machine,
                    &description,
                ) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(Error::Refused {
                        explanation: format!(
                            "returning agent host inventory changed; explicit topology reconciliation is required: {explanation}"
                        ),
                    });
                }
                if let Err(err) = self.reconcile_device_subtree(machine, &description.devices) {
                    self.mark_inventory_mismatch(machine)?;
                    return Err(err);
                }
                (machine, false)
            }
'''
if text.count(old) != 1:
    raise RuntimeError("known-machine registration branch changed")
text = text.replace(old, new, 1)
marker = '''    /// Restore a reconciled machine's health marks, deferred during
'''
method = '''    /// A returning Agent whose authoritative inventory cannot be reconciled
    /// is not accepted as an execution endpoint. Stop new placement without
    /// weakening an existing drain/quarantine/retired state; outstanding
    /// authority remains occupied until its ordinary reconciliation closes.
    fn mark_inventory_mismatch(&mut self, machine: NodeId) -> Result<(), Error> {
        if matches!(
            self.cluster.node_state(machine),
            Some(archon_kernel::NodeState::Joining | archon_kernel::NodeState::Schedulable)
        ) {
            self.commit(Command::SetNodeState {
                node: machine,
                state: archon_kernel::NodeState::Unavailable,
            })?;
        }
        Ok(())
    }

'''
if text.count(marker) != 1:
    raise RuntimeError("recovery helper anchor changed")
text = text.replace(marker, method + marker, 1)

# Device identity remains stable through fact refresh, but an explicit host
# containment assertion cannot silently move or disappear.
loop_anchor = '''        for spec in devices {
            let id = if let Some(id) = existing.remove(&spec.id) {
'''
loop_replacement = '''        for spec in devices {
            let requested_parent = spec
                .host_parent
                .as_deref()
                .map(|parent| {
                    crate::discover::host_parent_node(&self.cluster.graph, machine, parent)
                        .map_err(|explanation| Error::Refused { explanation })
                })
                .transpose()?;
            let id = if let Some(id) = existing.remove(&spec.id) {
'''
if text.count(loop_anchor) != 1:
    raise RuntimeError("device reconciliation loop anchor changed")
text = text.replace(loop_anchor, loop_replacement, 1)
node_anchor = '''                let node = self.cluster.graph.node(id).ok_or(Error::UnknownNode(id))?;
                if node.kind != spec.kind {
'''
node_replacement = '''                let node = self.cluster.graph.node(id).ok_or(Error::UnknownNode(id))?;
                let recorded_parent = node
                    .attrs
                    .get(crate::discover::DEVICE_HOST_PARENT_ATTR)
                    .map(String::as_str);
                match (recorded_parent, spec.host_parent.as_deref()) {
                    (Some(previous), Some(current)) if previous != current => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "device {:?} changed host containment from {previous:?} to {current:?}; explicit topology reconciliation is required",
                                spec.id
                            ),
                        });
                    }
                    (Some(previous), None) => {
                        return Err(Error::Refused {
                            explanation: format!(
                                "device {:?} withdrew authoritative host containment {previous:?}; explicit topology reconciliation is required",
                                spec.id
                            ),
                        });
                    }
                    _ => {}
                }
                if let Some(parent) = requested_parent
                    && self.cluster.graph.parent(id) != Some(parent)
                {
                    return Err(Error::Refused {
                        explanation: format!(
                            "device {:?} changed physical containment; explicit topology reconciliation is required",
                            spec.id
                        ),
                    });
                }
                if node.kind != spec.kind {
'''
if text.count(node_anchor) != 1:
    raise RuntimeError("existing device validation anchor changed")
text = text.replace(node_anchor, node_replacement, 1)
fresh_anchor = '''                edges.push(Edge {
                    from: machine,
                    to: fresh,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                });
'''
fresh_replacement = '''                edges.push(Edge {
                    from: requested_parent.unwrap_or(machine),
                    to: fresh,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                });
'''
if text.count(fresh_anchor) != 1:
    raise RuntimeError("new device containment anchor changed")
text = text.replace(fresh_anchor, fresh_replacement, 1)
attrs_anchor = '''            attrs.insert("id".into(), spec.id.clone());
            attrs.insert("dev".into(), spec.dev.clone());
'''
attrs_replacement = '''            attrs.insert("id".into(), spec.id.clone());
            attrs.insert("dev".into(), spec.dev.clone());
            if let Some(parent) = &spec.host_parent {
                attrs.insert(
                    crate::discover::DEVICE_HOST_PARENT_ATTR.into(),
                    parent.clone(),
                );
            }
'''
if text.count(attrs_anchor) != 1:
    raise RuntimeError("device attrs anchor changed")
text = text.replace(attrs_anchor, attrs_replacement, 1)
path.write_text(text)

# --- make the existing nested-device regression a complete legacy machine --
path = Path("crates/node/tests/devices.rs")
text = path.read_text()
text = text.replace(
    "use archon_node::discover::{DeviceSpec, MachineDescription};",
    "use archon_node::discover::{DeviceSpec, HostNodeSpec, MachineDescription};",
    1,
)
old = '''    let machine = NodeId::from_u64(100);
    let numa = NodeId::from_u64(101);
    let gpu = NodeId::from_u64(102);
'''
new = '''    let machine = NodeId::from_u64(100);
    let numa = NodeId::from_u64(101);
    let gpu = NodeId::from_u64(102);
    let cpu0 = NodeId::from_u64(103);
    let cpu1 = NodeId::from_u64(104);
    let memory = NodeId::from_u64(105);
'''
if text.count(old) != 1:
    raise RuntimeError("nested-device ids changed")
text = text.replace(old, new, 1)
old = '''                Node {
                    id: gpu,
                    kind: ResourceClass::Gpu,
                    attrs: gpu_attrs,
                    capacity: qty(CapacityDimension::Count, 1),
                },
'''
new = '''                Node {
                    id: gpu,
                    kind: ResourceClass::Gpu,
                    attrs: gpu_attrs,
                    capacity: qty(CapacityDimension::Count, 1),
                },
                Node {
                    id: cpu0,
                    kind: ResourceClass::Cpu,
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
                Node {
                    id: cpu1,
                    kind: ResourceClass::Cpu,
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
                Node {
                    id: memory,
                    kind: ResourceClass::Memory,
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Bytes, 0),
                },
'''
# This GPU node shape occurs once in the nested test after PR #3.
if text.count(old) < 1:
    raise RuntimeError("nested-device node anchor changed")
# Replace the last occurrence to avoid touching earlier helpers.
idx = text.rfind(old)
text = text[:idx] + new + text[idx + len(old):]
edge_anchor = '''                Edge {
                    from: numa,
                    to: gpu,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
'''
edge_replacement = '''                Edge {
                    from: numa,
                    to: gpu,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
                Edge {
                    from: machine,
                    to: cpu0,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
                Edge {
                    from: machine,
                    to: cpu1,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
                Edge {
                    from: machine,
                    to: memory,
                    kind: EdgeKind::Contains,
                    attrs: Attrs::new(),
                },
'''
if text.count(edge_anchor) < 1:
    raise RuntimeError("nested-device edge anchor changed")
idx = text.rfind(edge_anchor)
text = text[:idx] + edge_replacement + text[idx + len(edge_anchor):]

# Hard host changes must stop admission rather than retaining stale facts.
text += r'''

#[test]
fn returning_agent_with_changed_host_shape_fails_closed() {
    let mut service = NodeService::new();
    service
        .register_agent(
            description("inst-host-change", "/dev/gpuA"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("first registration");
    let machine = service.cluster.graph.nodes_of_class(ResourceClass::Machine)[0];
    let mut changed = description("inst-host-change", "/dev/gpuA");
    changed.cpus = 3;
    let err = service
        .register_agent(
            changed,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("changed host facts must not be silently accepted");
    assert!(err.to_string().contains("host inventory changed"));
    assert_eq!(
        service.cluster.node_state(machine),
        Some(NodeState::Unavailable)
    );
}

fn normalized_description(instance: &str, parent: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: "normalized-gpu-box".into(),
        cpus: 2,
        memory_bytes: 4096,
        host_nodes: vec![
            HostNodeSpec {
                id: "numa/0".into(),
                kind: ResourceClass::Numa,
                parent: None,
                attrs: Attrs::new(),
                capacity: Quantity::new(),
            },
            HostNodeSpec {
                id: "numa/1".into(),
                kind: ResourceClass::Numa,
                parent: None,
                attrs: Attrs::new(),
                capacity: Quantity::new(),
            },
            HostNodeSpec {
                id: "cpu/0".into(),
                kind: ResourceClass::Cpu,
                parent: Some("numa/0".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "cpu/1".into(),
                kind: ResourceClass::Cpu,
                parent: Some("numa/1".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            },
            HostNodeSpec {
                id: "memory/0".into(),
                kind: ResourceClass::Memory,
                parent: Some("numa/0".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Bytes, 2048),
            },
            HostNodeSpec {
                id: "memory/1".into(),
                kind: ResourceClass::Memory,
                parent: Some("numa/1".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Bytes, 2048),
            },
        ],
        devices: vec![DeviceSpec {
            kind: ResourceClass::Gpu,
            id: "gpu0".into(),
            dev: "/dev/gpuA".into(),
            host_parent: Some(parent.into()),
            access: Vec::new(),
            attrs: Attrs::new(),
        }],
    }
}

#[test]
fn explicit_device_host_parent_is_used_for_new_inventory() {
    let mut service = NodeService::new();
    let mut first = normalized_description("inst-parent", "numa/0");
    first.devices.clear();
    service
        .register_agent(
            first,
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("host registration");
    service
        .register_agent(
            normalized_description("inst-parent", "numa/0"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("new nested device");
    let gpu = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];
    let parent = service.cluster.graph.parent(gpu).expect("GPU parent");
    assert_eq!(
        service
            .cluster
            .graph
            .node(parent)
            .and_then(|node| node.attrs.get("archon.host-id"))
            .map(String::as_str),
        Some("numa/0")
    );
}

#[test]
fn same_device_cannot_silently_move_between_host_domains() {
    let mut service = NodeService::new();
    service
        .register_agent(
            normalized_description("inst-reparent", "numa/0"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("first registration");
    let gpu = service.cluster.graph.nodes_of_class(ResourceClass::Gpu)[0];
    let original_parent = service.cluster.graph.parent(gpu);
    let err = service
        .register_agent(
            normalized_description("inst-reparent", "numa/1"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect_err("hard containment change requires explicit reconciliation");
    assert!(err.to_string().contains("containment"));
    assert_eq!(service.cluster.graph.parent(gpu), original_parent);
}
'''
path.write_text(text)
