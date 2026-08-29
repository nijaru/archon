//! Local machine discovery: build the synthetic-graph representation of this
//! machine for `ApplyGraph`.

use std::collections::BTreeMap;
use std::process::Command;

use serde::{Deserialize, Serialize};

use archon_kernel::{
    Attrs, BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind, Node,
    ProviderId, Quantity, ResourceClass, qty, quantity_get,
};

pub struct LocalMachine {
    #[allow(dead_code)]
    pub machine: archon_kernel::NodeId,
    #[allow(dead_code)]
    pub cpus: Vec<archon_kernel::NodeId>,
    #[allow(dead_code)]
    pub memory: Vec<archon_kernel::NodeId>,
}

struct IdGen {
    next: u64,
}

impl IdGen {
    fn node(&mut self) -> archon_kernel::NodeId {
        self.next += 1;
        archon_kernel::NodeId::from_u64(self.next)
    }
}

pub(crate) const HOST_ID_ATTR: &str = "archon.host-id";
pub(crate) const DEVICE_HOST_PARENT_ATTR: &str = "archon.host-parent";

/// A machine as the agent reports it; the controller builds the graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineDescription {
    /// Stable identity across reconnects; empty for local discovery.
    pub instance_id: String,
    pub name: String,
    pub cpus: u64,
    pub memory_bytes: u64,
    /// Provider-normalized host resources/topology. Empty keeps the legacy
    /// flat Machine -> CPU/Memory shape. A non-empty fragment is authoritative
    /// for host CPU/memory/topology facts and uses provider-local stable ids
    /// for parent references.
    #[serde(default)]
    pub host_nodes: Vec<HostNodeSpec>,
    /// Devices this machine exposes. `ARCHON_DEVICES` can provide explicit
    /// declarations; supported providers may discover the same representation
    /// automatically.
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
}

/// One provider-normalized host resource or topology node. `id` is stable
/// within this machine inventory and is used only to compose containment;
/// Archon assigns the durable Graph NodeId when the machine first joins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostNodeSpec {
    pub id: String,
    pub kind: ResourceClass,
    /// Parent host-node id; None attaches directly to the Machine.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub attrs: Attrs,
    #[serde(default)]
    pub capacity: Quantity,
}

/// One declared device: a stable `id` that survives re-registration and
/// access-path changes, plus the current host path it is reachable through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSpec {
    pub kind: ResourceClass,
    pub id: String,
    pub dev: String,
    /// Provider-normalized host node that contains this device. None means
    /// the device provider makes no hard host-locality assertion.
    #[serde(default)]
    pub host_parent: Option<String>,
    /// Additional host device paths required to use this logical resource.
    /// The primary path remains in `dev`; this list is for provider/runtime
    /// support devices such as NVIDIA's control and UVM nodes.
    #[serde(default)]
    pub access: Vec<String>,
    /// Provider-owned hard facts and capabilities used by placement and
    /// enforcement adapters. The stable identity and current primary path
    /// remain first-class fields for reconciliation.
    #[serde(default)]
    pub attrs: Attrs,
}

fn description_with_devices(devices: Vec<DeviceSpec>) -> MachineDescription {
    MachineDescription {
        instance_id: String::new(),
        name: hostname(),
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()) as u64,
        memory_bytes: total_memory_bytes(),
        host_nodes: Vec::new(),
        devices,
    }
}

/// Discover this machine with an authoritative device inventory. Registration
/// paths use this form so an uncertain provider query can never masquerade as
/// an authoritative empty inventory and retire/fail previously known devices.
pub fn try_describe() -> Result<MachineDescription, String> {
    discovered_devices().map(description_with_devices)
}

/// Best-effort one-shot discovery for callers that do not reconcile an
/// existing machine. Agent registration must use `try_describe` instead.
pub fn describe() -> MachineDescription {
    match try_describe() {
        Ok(description) => description,
        Err(err) => {
            eprintln!("archon: device discovery incomplete: {err}");
            description_with_devices(Vec::new())
        }
    }
}

fn discovered_devices() -> Result<Vec<DeviceSpec>, String> {
    if std::env::var_os("ARCHON_DEVICES").is_some() {
        let spec = std::env::var("ARCHON_DEVICES")
            .map_err(|_| "ARCHON_DEVICES is not valid UTF-8".to_string())?;
        validate_declared_devices(&spec)?;
        return Ok(declared_devices());
    }
    nvidia_devices()
}

fn validate_declared_devices(spec: &str) -> Result<(), String> {
    for raw in spec.split(',').filter(|entry| !entry.trim().is_empty()) {
        let entry = raw.trim();
        let (kind, id, dev) = match entry.split_once('=') {
            Some((kind, rest)) => {
                let (id, dev) = rest.split_once(':').ok_or_else(|| {
                    format!("invalid ARCHON_DEVICES entry {entry:?}: expected kind=id:path")
                })?;
                (kind, id, dev)
            }
            None => {
                let (kind, dev) = entry.split_once(':').ok_or_else(|| {
                    format!("invalid ARCHON_DEVICES entry {entry:?}: expected kind:path")
                })?;
                (kind, dev, dev)
            }
        };
        if id.trim().is_empty() || dev.trim().is_empty() {
            return Err(format!(
                "invalid ARCHON_DEVICES entry {entry:?}: empty id or path"
            ));
        }
        match kind.trim().to_ascii_lowercase().as_str() {
            "gpu" | "nic" | "nvme" => {}
            other => return Err(format!("invalid ARCHON_DEVICES device kind {other:?}")),
        }
    }
    Ok(())
}

/// Discover NVIDIA devices through the vendor's stable management query.
/// An absent `nvidia-smi` means the optional provider is not installed. Once
/// the provider is present, query failure is uncertainty rather than proof
/// that all previously known GPUs disappeared.
fn nvidia_devices() -> Result<Vec<DeviceSpec>, String> {
    let output = match Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,uuid,name,pci.bus_id,memory.total,compute_cap,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("cannot run nvidia-smi discovery: {err}")),
    };
    interpret_nvidia_query(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

fn interpret_nvidia_query(
    success: bool,
    stdout: &str,
    stderr: &str,
) -> Result<Vec<DeviceSpec>, String> {
    if !success {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(if detail.is_empty() {
            "nvidia-smi discovery failed".into()
        } else {
            format!("nvidia-smi discovery failed: {detail}")
        });
    }

    let rows = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let devices = parse_nvidia_devices(stdout);
    if devices.len() != rows {
        return Err(format!(
            "nvidia-smi returned an incomplete GPU inventory: parsed {} of {rows} rows",
            devices.len()
        ));
    }
    Ok(devices)
}

fn parse_nvidia_devices(output: &str) -> Vec<DeviceSpec> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split(',').map(str::trim).collect();
            if fields.len() != 7 {
                eprintln!("archon: ignoring malformed nvidia-smi row: {line:?}");
                return None;
            }
            let index = fields[0].parse::<u32>().ok()?;
            let uuid = fields[1];
            let model = fields[2];
            let pci_bus_id = normalize_pci_bus_id(fields[3]);
            let memory_mib = fields[4].parse::<u64>().ok()?;
            if uuid.is_empty() || model.is_empty() || pci_bus_id.is_empty() {
                eprintln!("archon: ignoring incomplete nvidia-smi row: {line:?}");
                return None;
            }
            let primary = format!("/dev/nvidia{index}");
            let mut access = vec![
                "/dev/nvidiactl".into(),
                "/dev/nvidia-uvm".into(),
                "/dev/nvidia-uvm-tools".into(),
                "/dev/nvidia-modeset".into(),
            ];
            access.retain(|path| std::fs::metadata(path).is_ok());
            let mut attrs = pci_attrs(&pci_bus_id);
            attrs.insert("provider".into(), "nvidia".into());
            attrs.insert("vendor".into(), "nvidia".into());
            attrs.insert("model".into(), model.into());
            attrs.insert("uuid".into(), uuid.into());
            attrs.insert("pci_bus_id".into(), pci_bus_id);
            attrs.insert(
                "memory_bytes".into(),
                (memory_mib * 1024 * 1024).to_string(),
            );
            attrs.insert("compute_capability".into(), fields[5].into());
            if !fields[6].is_empty() {
                attrs.insert("driver_version".into(), fields[6].into());
            }
            attrs.insert("whole_device".into(), "true".into());
            attrs.insert(
                "enforcement".into(),
                "linux-cgroup-device,oci-device".into(),
            );
            attrs.insert("cdi".into(), format!("nvidia.com/gpu={uuid}"));
            Some(DeviceSpec {
                kind: ResourceClass::Gpu,
                id: uuid.into(),
                dev: primary,
                access,
                attrs,
                host_parent: None,
            })
        })
        .collect()
}

fn normalize_pci_bus_id(value: &str) -> String {
    if let Some((domain, _rest)) = value.split_once(':')
        && domain.len() == 8
        && domain.as_bytes().iter().all(u8::is_ascii_hexdigit)
    {
        return value[4..].to_string();
    }
    value.to_string()
}

fn pci_attrs(bus_id: &str) -> Attrs {
    let mut attrs = Attrs::new();
    let path = std::path::Path::new("/sys/bus/pci/devices").join(bus_id);
    if let Ok(numa) = std::fs::read_to_string(path.join("numa_node")) {
        let numa = numa.trim();
        if !numa.is_empty() && numa != "-1" {
            attrs.insert("numa_node".into(), numa.into());
        }
    }
    if let Ok(real) = std::fs::canonicalize(&path)
        && let Some(parent) = real.parent().and_then(std::path::Path::file_name)
    {
        let parent = parent.to_string_lossy();
        if parent.starts_with("0000:") && parent.contains('.') {
            attrs.insert("pci_root".into(), parent.into_owned());
        }
    }
    attrs
}

/// Parse ARCHON_DEVICES entries into device specs; unparsable entries are
/// skipped with a warning. Each entry is `kind:path`, or `kind=id:path` for
/// an explicit stable id (defaulting to the path when omitted).
fn declared_devices() -> Vec<DeviceSpec> {
    let Ok(spec) = std::env::var("ARCHON_DEVICES") else {
        return Vec::new();
    };
    spec.split(',')
        .filter(|entry| !entry.trim().is_empty())
        .filter_map(|entry| {
            let (kind, id, dev) = match entry.split_once('=') {
                Some((kind, rest)) => {
                    let (id, dev) = rest.split_once(':')?;
                    (kind, id, dev)
                }
                None => {
                    let (kind, dev) = entry.split_once(':')?;
                    (kind, dev, dev)
                }
            };
            let kind = match kind.trim().to_lowercase().as_str() {
                "gpu" => archon_kernel::ResourceClass::Gpu,
                "nic" => archon_kernel::ResourceClass::Nic,
                "nvme" => archon_kernel::ResourceClass::Nvme,
                other => {
                    eprintln!("archon: ignoring unknown device kind {other:?}");
                    return None;
                }
            };
            Some(DeviceSpec {
                kind,
                id: id.trim().to_string(),
                dev: dev.to_string(),
                host_parent: None,
                access: Vec::new(),
                attrs: Attrs::new(),
            })
        })
        .collect()
}

fn total_memory_bytes() -> u64 {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("sysctl")
            .arg("-n")
            .arg("hw.memsize")
            .output()
            .expect("run sysctl");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("hw.memsize as integer")
    }
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/meminfo").expect("read /proc/meminfo");
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kib: u64 = rest
                    .trim()
                    .trim_end_matches(" kB")
                    .trim()
                    .parse()
                    .expect("MemTotal as integer");
                return kib * 1024;
            }
        }
        panic!("MemTotal not found in /proc/meminfo");
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        8 * (1 << 30)
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| {
        Command::new("hostname")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_else(|_| "localhost".into())
    })
}

/// Build the Archon graph for a validated machine description. A normalized
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
        if spec.attrs.contains_key(HOST_ID_ATTR) || spec.attrs.contains_key(DEVICE_HOST_PARENT_ATTR)
        {
            return Err(format!(
                "host node {:?} uses a reserved Archon attribute",
                spec.id
            ));
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
                format!(
                    "host node {:?} references unknown parent {parent:?}",
                    spec.id
                )
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
            return Err(
                "returning agent omitted previously authoritative normalized host topology".into(),
            );
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
            || cpu_nodes
                .iter()
                .any(|node| node.capacity != qty(CapacityDimension::Count, 1))
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

/// Current proof-stage providers explicitly declare which normalized
/// capacity they can bind/fence. Unknown/custom capacity remains
/// placement-only until a provider supplies its own claim contract.
pub fn claim_bindings(nodes: &[Node]) -> Vec<ClaimBindingUpdate> {
    nodes
        .iter()
        .filter_map(|node| {
            let (dimension, scope) = match node.kind {
                ResourceClass::Cpu => (CapacityDimension::Count, BindingScope::Exclusive),
                ResourceClass::Memory => (CapacityDimension::Bytes, BindingScope::IndependentShare),
                ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme => {
                    (CapacityDimension::Count, BindingScope::Exclusive)
                }
                _ => return None,
            };
            (node.capacity.get(&dimension).copied().unwrap_or(0) > 0).then_some(
                ClaimBindingUpdate {
                    node: node.id,
                    dimension,
                    binding: Some(ClaimBinding {
                        provider: ProviderId::ENFORCE,
                        scope,
                    }),
                },
            )
        })
        .collect()
}

/// Discover this machine and build its graph.
pub fn discover() -> (LocalMachine, Vec<Node>, Vec<Edge>) {
    build_graph(&describe(), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_identity_and_capabilities() {
        let devices = parse_nvidia_devices(
            "0, GPU-test, NVIDIA Test GPU, 00000000:01:00.0, 24564, 8.9, 610.57.04\n",
        );
        assert_eq!(devices.len(), 1);
        let device = &devices[0];
        assert_eq!(device.kind, ResourceClass::Gpu);
        assert_eq!(device.id, "GPU-test");
        assert_eq!(device.dev, "/dev/nvidia0");
        assert_eq!(device.attrs.get("pci_bus_id"), Some(&"0000:01:00.0".into()));
        assert_eq!(
            device.attrs.get("memory_bytes"),
            Some(&"25757220864".into())
        );
        assert_eq!(
            device.attrs.get("cdi"),
            Some(&"nvidia.com/gpu=GPU-test".into())
        );
        assert_eq!(device.attrs.get("whole_device"), Some(&"true".into()));
    }

    #[test]
    fn malformed_nvidia_rows_are_ignored() {
        assert!(parse_nvidia_devices("not,a,gpu\n").is_empty());
        assert!(
            parse_nvidia_devices(
                "0, GPU-test, NVIDIA Test GPU, 00000000:01:00.0, not-a-number, 8.9, 610.57.04\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn uncertain_nvidia_query_is_not_an_authoritative_empty_inventory() {
        let failed = interpret_nvidia_query(false, "", "driver unavailable").unwrap_err();
        assert!(failed.contains("driver unavailable"));

        let malformed = interpret_nvidia_query(true, "No devices were found\n", "").unwrap_err();
        assert!(malformed.contains("incomplete GPU inventory"));

        let partial = interpret_nvidia_query(
            true,
            "0, GPU-test, NVIDIA Test GPU, 00000000:01:00.0, 24564, 8.9, 610.57.04\nmalformed\n",
            "",
        )
        .unwrap_err();
        assert!(partial.contains("parsed 1 of 2 rows"));

        assert!(interpret_nvidia_query(true, "", "").unwrap().is_empty());
    }

    #[test]
    fn explicit_device_inventory_must_parse_completely() {
        assert!(validate_declared_devices("gpu=g0:/dev/nvidia0,nic=n0:/dev/net0").is_ok());
        assert!(validate_declared_devices("").is_ok());
        assert!(validate_declared_devices("gpu=g0:/dev/nvidia0,broken").is_err());
        assert!(validate_declared_devices("unknown=x:/dev/x").is_err());
        assert!(validate_declared_devices("gpu=:/dev/nvidia0").is_err());
    }

    #[test]
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
            assert_eq!(
                graph.ancestor_of_class(node, ResourceClass::Numa),
                Some(numa)
            );
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
}
