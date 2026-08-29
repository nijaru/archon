from pathlib import Path
import re

# --- discovery model + deterministic graph construction -----------------
path = Path("crates/node/src/discover.rs")
text = path.read_text()
text = text.replace(
    "use std::process::Command;\n",
    "use std::collections::BTreeMap;\nuse std::process::Command;\n",
    1,
)
text = text.replace(
    "use archon_kernel::{Attrs, CapacityDimension, Edge, EdgeKind, Node, Quantity, ResourceClass, qty};",
    "use archon_kernel::{\n    Attrs, CapacityDimension, Edge, EdgeKind, Node, Quantity, ResourceClass, qty, quantity_get,\n};",
    1,
)
old = '''    pub memory_bytes: u64,
    /// Devices this machine exposes. `ARCHON_DEVICES` can provide explicit
'''
new = '''    pub memory_bytes: u64,
    /// Provider-normalized host resources/topology. Empty keeps the legacy
    /// flat Machine -> CPU/Memory shape. A non-empty fragment is authoritative
    /// for host CPU/memory/topology facts and uses provider-local stable ids
    /// for parent references.
    #[serde(default)]
    pub topology: Vec<TopologySpec>,
    /// Devices this machine exposes. `ARCHON_DEVICES` can provide explicit
'''
if text.count(old) != 1:
    raise RuntimeError("MachineDescription anchor changed")
text = text.replace(old, new, 1)

anchor = '''/// One declared device: a stable `id` that survives re-registration and
/// access-path changes, plus the current host path it is reachable through.
'''
topology_def = '''/// One normalized host resource/topology node. `id` is stable only within
/// this machine/provider inventory and exists to make parent references
/// deterministic; Archon assigns the durable Graph NodeId on first admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologySpec {
    pub id: String,
    pub kind: ResourceClass,
    /// Parent topology id; None attaches directly to the Machine.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub attrs: Attrs,
    #[serde(default)]
    pub capacity: Quantity,
}

'''
if text.count(anchor) != 1:
    raise RuntimeError("DeviceSpec anchor changed")
text = text.replace(anchor, topology_def + anchor, 1)

old = '''        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()) as u64,
        memory_bytes: total_memory_bytes(),
        devices,
'''
new = '''        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()) as u64,
        memory_bytes: total_memory_bytes(),
        topology: Vec::new(),
        devices,
'''
if text.count(old) != 1:
    raise RuntimeError("description_with_devices anchor changed")
text = text.replace(old, new, 1)

build_start = text.index("/// Build the Archon graph for a machine description:")
build_end = text.index("/// Discover this machine and build its graph.", build_start)
new_build = r'''/// Build the Archon graph for a machine description. A normalized topology
/// fragment produces the provider-authored containment tree; an empty fragment
/// retains the legacy flat Machine -> CPU/Memory shape.
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
    let mut topology_ids = BTreeMap::new();
    let (cpus, memory) = if description.topology.is_empty() {
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
        (cpus, memory)
    } else {
        let mut specs: Vec<_> = description.topology.iter().collect();
        specs.sort_by(|left, right| left.id.cmp(&right.id));
        for spec in &specs {
            let id = ids.node();
            topology_ids.insert(spec.id.clone(), id);
            nodes.push(Node {
                id,
                kind: spec.kind,
                attrs: spec.attrs.clone(),
                capacity: spec.capacity.clone(),
            });
        }
        for spec in &specs {
            let child = topology_ids[&spec.id];
            let parent = spec
                .parent
                .as_ref()
                .map(|parent| {
                    *topology_ids
                        .get(parent)
                        .expect("validated topology parent must exist")
                })
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
            .map(|spec| topology_ids[&spec.id])
            .collect();
        let memory = specs
            .iter()
            .find(|spec| spec.kind == ResourceClass::Memory)
            .map(|spec| topology_ids[&spec.id])
            .expect("validated topology must expose memory");
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
        nodes.push(Node {
            id: device,
            kind: device_spec.kind,
            attrs,
            capacity: qty(CapacityDimension::Count, 1),
        });

        let topology_parent = device_spec
            .attrs
            .get("pci_root")
            .and_then(|pci_root| {
                description.topology.iter().find(|spec| {
                    spec.kind == ResourceClass::PcieRoot
                        && spec.attrs.get("pci_root") == Some(pci_root)
                })
            })
            .or_else(|| {
                device_spec.attrs.get("numa_node").and_then(|numa_node| {
                    description.topology.iter().find(|spec| {
                        spec.kind == ResourceClass::Numa
                            && spec.attrs.get("os_index") == Some(numa_node)
                    })
                })
            })
            .and_then(|spec| topology_ids.get(&spec.id).copied())
            .unwrap_or(machine);
        edges.push(Edge {
            from: topology_parent,
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

/// Validate a non-empty normalized host fragment before it can become
/// authoritative Graph state. This proof schema intentionally accepts only
/// host topology plus CPU/memory resources; devices remain in DeviceSpec so
/// their enforcement/reconciliation contract has one writer.
pub(crate) fn validate_topology(description: &MachineDescription) -> Result<(), String> {
    if description.topology.is_empty() {
        return Ok(());
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut cpu_count = 0u64;
    let mut memory_total = 0u64;
    let mut memory_nodes = 0usize;
    for spec in &description.topology {
        if spec.id.trim().is_empty() {
            return Err("host topology id must be non-empty".into());
        }
        if !ids.insert(spec.id.clone()) {
            return Err(format!("duplicate host topology id {:?}", spec.id));
        }
        match spec.kind {
            ResourceClass::Socket | ResourceClass::Numa | ResourceClass::PcieRoot => {
                if !spec.capacity.is_empty() {
                    return Err(format!(
                        "structural host topology node {:?} must not advertise claimable capacity",
                        spec.id
                    ));
                }
            }
            ResourceClass::Cpu => {
                if spec.capacity != qty(CapacityDimension::Count, 1) {
                    return Err(format!(
                        "logical CPU {:?} must advertise exactly count=1",
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
                    .ok_or_else(|| "host topology memory total overflows".to_string())?;
                memory_nodes += 1;
            }
            other => {
                return Err(format!(
                    "resource class {other} is not valid in the host topology fragment"
                ));
            }
        }
    }
    for spec in &description.topology {
        if let Some(parent) = &spec.parent {
            if parent == &spec.id {
                return Err(format!("host topology node {:?} contains itself", spec.id));
            }
            if !ids.contains(parent) {
                return Err(format!(
                    "host topology node {:?} references unknown parent {parent:?}",
                    spec.id
                ));
            }
        }
    }
    if cpu_count != description.cpus {
        return Err(format!(
            "host topology reports {cpu_count} logical CPUs but machine summary reports {}",
            description.cpus
        ));
    }
    if memory_nodes == 0 || memory_total != description.memory_bytes {
        return Err(format!(
            "host topology reports {memory_total} memory bytes across {memory_nodes} nodes but machine summary reports {}",
            description.memory_bytes
        ));
    }
    Ok(())
}

'''
text = text[:build_start] + new_build + text[build_end:]

# Add normalized graph tests before the end of the discover test module.
insert = r'''

    #[test]
    fn normalized_topology_builds_cpu_memory_and_device_locality() {
        let mut numa_attrs = Attrs::new();
        numa_attrs.insert("os_index".into(), "0".into());
        let mut pcie_attrs = Attrs::new();
        pcie_attrs.insert("pci_root".into(), "0000:00:01.0".into());
        let mut gpu_attrs = Attrs::new();
        gpu_attrs.insert("numa_node".into(), "0".into());
        gpu_attrs.insert("pci_root".into(), "0000:00:01.0".into());
        let description = MachineDescription {
            instance_id: "topology-test".into(),
            name: "topology-box".into(),
            cpus: 1,
            memory_bytes: 4096,
            topology: vec![
                TopologySpec {
                    id: "numa/0".into(),
                    kind: ResourceClass::Numa,
                    parent: None,
                    attrs: numa_attrs,
                    capacity: Quantity::new(),
                },
                TopologySpec {
                    id: "cpu/0".into(),
                    kind: ResourceClass::Cpu,
                    parent: Some("numa/0".into()),
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
                TopologySpec {
                    id: "memory/0".into(),
                    kind: ResourceClass::Memory,
                    parent: Some("numa/0".into()),
                    attrs: Attrs::new(),
                    capacity: qty(CapacityDimension::Bytes, 4096),
                },
                TopologySpec {
                    id: "pcie/0000:00:01.0".into(),
                    kind: ResourceClass::PcieRoot,
                    parent: Some("numa/0".into()),
                    attrs: pcie_attrs,
                    capacity: Quantity::new(),
                },
            ],
            devices: vec![DeviceSpec {
                kind: ResourceClass::Gpu,
                id: "gpu0".into(),
                dev: "/dev/gpu0".into(),
                access: Vec::new(),
                attrs: gpu_attrs,
            }],
        };
        validate_topology(&description).expect("valid normalized topology");
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
    fn normalized_topology_must_be_complete_and_consistent() {
        let mut description = MachineDescription {
            instance_id: "bad-topology".into(),
            name: "bad".into(),
            cpus: 1,
            memory_bytes: 1024,
            topology: vec![TopologySpec {
                id: "cpu/0".into(),
                kind: ResourceClass::Cpu,
                parent: Some("missing".into()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            }],
            devices: Vec::new(),
        };
        assert!(validate_topology(&description).unwrap_err().contains("unknown parent"));
        description.topology[0].parent = None;
        assert!(validate_topology(&description).unwrap_err().contains("memory bytes"));
    }
'''
# Insert before final module brace.
pos = text.rfind("}\n")
text = text[:pos] + insert + text[pos:]
path.write_text(text)

# --- propagate topology over the agent wire ------------------------------
path = Path("crates/node/src/protocol.rs")
text = path.read_text()
for needle in [
    '''        memory_bytes: u64,\n        /// Devices this machine exposes.\n        #[serde(default)]\n        devices: Vec<crate::discover::DeviceSpec>,''',
    '''        memory_bytes: u64,\n        /// Devices this machine exposes.\n        #[serde(default)]\n        devices: Vec<crate::discover::DeviceSpec>,''',
]:
    pass
# The first two identical blocks are Greeting::Agent and AgentRequest::Register.
old = '''        memory_bytes: u64,
        /// Devices this machine exposes.
        #[serde(default)]
        devices: Vec<crate::discover::DeviceSpec>,
'''
new = '''        memory_bytes: u64,
        #[serde(default)]
        topology: Vec<crate::discover::TopologySpec>,
        /// Devices this machine exposes.
        #[serde(default)]
        devices: Vec<crate::discover::DeviceSpec>,
'''
if text.count(old) != 2:
    raise RuntimeError(f"expected two Agent inventory protocol blocks, got {text.count(old)}")
text = text.replace(old, new)
old = '''        memory_bytes: u64,
        #[serde(default)]
        devices: Vec<crate::discover::DeviceSpec>,
'''
new = '''        memory_bytes: u64,
        #[serde(default)]
        topology: Vec<crate::discover::TopologySpec>,
        #[serde(default)]
        devices: Vec<crate::discover::DeviceSpec>,
'''
if text.count(old) != 1:
    raise RuntimeError("Welcome inventory block changed")
text = text.replace(old, new, 1)
path.write_text(text)

# Listening agent Hello response.
path = Path("crates/node/src/agent.rs")
text = path.read_text()
old = '''            cpus: description.cpus,
            memory_bytes: description.memory_bytes,
            devices: description.devices,
'''
new = '''            cpus: description.cpus,
            memory_bytes: description.memory_bytes,
            topology: description.topology,
            devices: description.devices,
'''
if text.count(old) != 1:
    raise RuntimeError("agent Welcome construction changed")
path.write_text(text.replace(old, new, 1))

# Controller-initiated Hello parsing and registration validation.
path = Path("crates/node/src/service.rs")
text = path.read_text()
old = '''                cpus,
                memory_bytes,
                devices,
            } => Ok(crate::discover::MachineDescription {
                instance_id: String::new(),
                name,
                cpus,
                memory_bytes,
                devices,
'''
new = '''                cpus,
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
'''
if text.count(old) != 1:
    raise RuntimeError("service Welcome parsing changed")
text = text.replace(old, new, 1)
old = '''    ) -> Result<NodeId, Error> {
        let mut device_ids = BTreeSet::new();
'''
new = '''    ) -> Result<NodeId, Error> {
        crate::discover::validate_topology(&description).map_err(|explanation| Error::Refused {
            explanation,
        })?;
        let mut device_ids = BTreeSet::new();
'''
if text.count(old) != 1:
    raise RuntimeError("register_agent validation anchor changed")
text = text.replace(old, new, 1)
path.write_text(text)

# Dial-in CLI greeting.
path = Path("crates/cli/src/main.rs")
text = path.read_text()
old = '''                    cpus: description.cpus,
                    memory_bytes: description.memory_bytes,
                    devices: description.devices.clone(),
'''
new = '''                    cpus: description.cpus,
                    memory_bytes: description.memory_bytes,
                    topology: description.topology.clone(),
                    devices: description.devices.clone(),
'''
if text.count(old) != 1:
    raise RuntimeError("dial-in greeting construction changed")
path.write_text(text.replace(old, new, 1))

# Control-plane dial-in registration.
path = Path("crates/control/src/server.rs")
text = path.read_text()
old = '''                cpus,
                memory_bytes,
                devices,
            } => {
                if let Err(err) = this.lock().unwrap().register_dial_in(
                    stream,
                    instance_id,
                    name,
                    cpus,
                    memory_bytes,
                    devices,
'''
new = '''                cpus,
                memory_bytes,
                topology,
                devices,
            } => {
                if let Err(err) = this.lock().unwrap().register_dial_in(
                    stream,
                    instance_id,
                    name,
                    cpus,
                    memory_bytes,
                    topology,
                    devices,
'''
if text.count(old) != 1:
    raise RuntimeError("dial-in greeting match changed")
text = text.replace(old, new, 1)
old = '''        cpus: u64,
        memory_bytes: u64,
        devices: Vec<archon_node::discover::DeviceSpec>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let description = archon_node::discover::MachineDescription {
            instance_id,
            name,
            cpus,
            memory_bytes,
            devices,
'''
new = '''        cpus: u64,
        memory_bytes: u64,
        topology: Vec<archon_node::discover::TopologySpec>,
        devices: Vec<archon_node::discover::DeviceSpec>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let description = archon_node::discover::MachineDescription {
            instance_id,
            name,
            cpus,
            memory_bytes,
            topology,
            devices,
'''
if text.count(old) != 1:
    raise RuntimeError("register_dial_in signature changed")
path.write_text(text.replace(old, new, 1))

# Any remaining MachineDescription literals are existing flat test/proof inputs.
# Add an explicit empty fragment so compile-time construction stays honest.
pattern = re.compile(
    r"(MachineDescription\s*\{.*?\n(?P<indent>\s*)memory_bytes\s*:\s*[^\n]+,\n)(?P=indent)(devices\s*:)",
    re.S,
)
for rust in Path("crates").rglob("*.rs"):
    source = rust.read_text()
    updated = pattern.sub(
        lambda m: m.group(1) + m.group("indent") + "topology: Vec::new(),\n" + m.group("indent") + m.group(3),
        source,
    )
    rust.write_text(updated)
