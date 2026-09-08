//! Local machine discovery: build the synthetic-graph representation of this
//! machine for `ApplyGraph`.

use std::collections::BTreeMap;
use std::process::Command;

use serde::{Deserialize, Serialize};

use archon_kernel::{
    Attrs, BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Edge, EdgeKind,
    FactEdge, FactWriterAssignment, FactWriterId, Node, ProviderFactBatch, ProviderId, Quantity,
    ResourceClass, qty, quantity_get,
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
pub(crate) const HOST_FACT_WRITER: FactWriterId = FactWriterId::from_u64(1);
pub(crate) const DEVICE_FACT_WRITER: FactWriterId = FactWriterId::from_u64(2);

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
    let host_nodes = host_topology();
    MachineDescription {
        instance_id: String::new(),
        name: hostname(),
        // A normalized inventory is authoritative for CPU count and memory
        // bytes: the summary fields exist for the flat legacy shape, so keep
        // them consistent with what the graph will actually advertise.
        cpus: if host_nodes.is_empty() {
            std::thread::available_parallelism().map_or(1, |n| n.get()) as u64
        } else {
            host_nodes
                .iter()
                .filter(|spec| spec.kind == ResourceClass::Cpu)
                .count() as u64
        },
        memory_bytes: if host_nodes.is_empty() {
            total_memory_bytes()
        } else {
            host_nodes
                .iter()
                .filter(|spec| spec.kind == ResourceClass::Memory)
                .map(|spec| quantity_get(&spec.capacity, CapacityDimension::Bytes))
                .sum()
        },
        host_nodes,
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

/// Authoritative normalized host topology as `HostNodeSpec`s: NUMA nodes as
/// structural parents, per-logical-CPU count=1 leaves, one memory node per
/// NUMA node. Linux sysfs is the source (see
/// `ai/research/hwloc-evaluation-2026-09-08.md` for the libhwloc decision);
/// any host where sysfs cannot yield the full normalized inventory reports
/// the flat legacy shape instead of partial topology, because a partially
/// normalized inventory would place CPU/memory claims without their NUMA
/// containment facts.
#[cfg(target_os = "linux")]
fn host_topology() -> Vec<HostNodeSpec> {
    match host_topology_fallible() {
        Ok(specs) => specs,
        Err(reason) => {
            eprintln!("archon: normalized host topology unavailable: {reason}");
            Vec::new()
        }
    }
}

#[cfg(target_os = "linux")]
fn host_topology_fallible() -> Result<Vec<HostNodeSpec>, String> {
    let node_dir = std::fs::read_dir("/sys/devices/system/node")
        .map_err(|err| format!("read /sys/devices/system/node: {err}"))?;
    let mut numa_nodes: Vec<(String, Vec<u32>, String)> = Vec::new();
    for entry in node_dir.flatten() {
        let name = entry.file_name();
        let Some(node) = name.to_str().and_then(|n| n.strip_prefix("node")) else {
            continue;
        };
        if node.parse::<u32>().is_err() {
            continue;
        }
        let path = entry.path();
        let cpulist = path.join("cpulist");
        let cpus = std::fs::read_to_string(&cpulist)
            .map_err(|err| format!("read {}: {err}", cpulist.display()))?;
        let cpus =
            parse_cpu_list(cpus.trim()).map_err(|err| format!("{}: {err}", cpulist.display()))?;
        if cpus.is_empty() {
            return Err(format!("{} reports no CPUs", path.display()));
        }
        numa_nodes.push((format!("numa/{node}"), cpus, node.to_string()));
    }
    if numa_nodes.is_empty() {
        return Err("sysfs reports no NUMA nodes".into());
    }
    numa_nodes.sort_by(|left, right| left.0.cmp(&right.0));

    // NUMA meminfo (not /proc/meminfo MemTotal) is the authoritative per-node
    // byte capacity: the two disagree under kernel reservations, and the
    // normalized memory nodes must sum to what they advertise, not to
    // MemTotal.
    let mut specs = Vec::new();
    for (id, cpus, sysfs_node) in &numa_nodes {
        specs.push(HostNodeSpec {
            id: id.clone(),
            kind: ResourceClass::Numa,
            parent: None,
            attrs: Attrs::new(),
            capacity: Quantity::new(),
        });
        let meminfo_path = format!("/sys/devices/system/node/node{sysfs_node}/meminfo");
        let meminfo = std::fs::read_to_string(&meminfo_path)
            .map_err(|err| format!("read {meminfo_path}: {err}"))?;
        let kib = meminfo
            .lines()
            .find_map(|line| {
                // "Node 0 MemTotal:       32595068 kB": number and unit are
                // separate fields; match the label, not a position.
                let mut fields = line.split_whitespace();
                let label = fields.next()?;
                let _node = fields.next()?;
                let metric = fields.next()?;
                if label != "Node" || metric != "MemTotal:" {
                    return None;
                }
                fields.next()?.parse::<u64>().ok()
            })
            .ok_or_else(|| format!("parse {meminfo_path} MemTotal"))?;
        let bytes = kib
            .checked_mul(1024)
            .ok_or_else(|| format!("{meminfo_path} MemTotal overflows"))?;
        specs.push(HostNodeSpec {
            id: format!("{id}/memory"),
            kind: ResourceClass::Memory,
            parent: Some(id.clone()),
            attrs: Attrs::new(),
            capacity: qty(CapacityDimension::Bytes, bytes),
        });
        for cpu in cpus {
            specs.push(HostNodeSpec {
                id: format!("{id}/cpu/{cpu}"),
                kind: ResourceClass::Cpu,
                parent: Some(id.clone()),
                attrs: Attrs::new(),
                capacity: qty(CapacityDimension::Count, 1),
            });
        }
    }
    Ok(specs)
}

/// Expand a sysfs cpulist ("0-3,8,10-11") into individual CPU numbers.
#[cfg(target_os = "linux")]
pub(crate) fn parse_cpu_list(list: &str) -> Result<Vec<u32>, String> {
    let mut cpus = Vec::new();
    for part in list.split(',').filter(|part| !part.is_empty()) {
        let (start, end) = match part.split_once('-') {
            Some((start, end)) => (start, end),
            None => (part, part),
        };
        let start: u32 = start
            .trim()
            .parse()
            .map_err(|_| format!("invalid CPU range {part:?}"))?;
        let end: u32 = end
            .trim()
            .parse()
            .map_err(|_| format!("invalid CPU range {part:?}"))?;
        if end < start {
            return Err(format!("inverted CPU range {part:?}"));
        }
        for cpu in start..=end {
            cpus.push(cpu);
        }
    }
    Ok(cpus)
}

/// Non-Linux hosts keep the flat legacy inventory until a real host provider
/// for them exists (see the hwloc evaluation for the candidate producer).
#[cfg(not(target_os = "linux"))]
fn host_topology() -> Vec<HostNodeSpec> {
    Vec::new()
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

/// Split one validated machine graph into independently-owned provider fact
/// fragments while retaining one atomic Cluster update. The current Agent
/// aggregates host discovery and device discovery as two writers; the kernel
/// contract supports additional writers without changing ownership semantics.
pub(crate) fn provider_fact_batches(nodes: Vec<Node>, edges: Vec<Edge>) -> Vec<ProviderFactBatch> {
    let mut by_node = BTreeMap::new();
    for node in &nodes {
        let writer = if matches!(
            node.kind,
            ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
        ) {
            DEVICE_FACT_WRITER
        } else {
            HOST_FACT_WRITER
        };
        by_node.insert(node.id, writer);
    }
    let mut host = ProviderFactBatch {
        writer: HOST_FACT_WRITER,
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    let mut device = ProviderFactBatch {
        writer: DEVICE_FACT_WRITER,
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    for node in nodes {
        if by_node[&node.id] == DEVICE_FACT_WRITER {
            device.nodes.push(node);
        } else {
            host.nodes.push(node);
        }
    }
    for edge in edges {
        let writer = by_node.get(&edge.to).copied().unwrap_or(HOST_FACT_WRITER);
        if writer == DEVICE_FACT_WRITER {
            device.edges.push(edge);
        } else {
            host.edges.push(edge);
        }
    }
    [host, device]
        .into_iter()
        .filter(|batch| !batch.nodes.is_empty() || !batch.edges.is_empty())
        .collect()
}

/// Expected ownership for the currently-reported portion of one machine's
/// legacy Graph. Omitted device tombstones are deliberately excluded: a
/// current provider must not acquire authority over a resource it did not
/// report merely because a stale Node remains persisted.
pub(crate) fn current_fact_writer_assignments(
    graph: &archon_kernel::Graph,
    machine: archon_kernel::NodeId,
    devices: &[DeviceSpec],
) -> Vec<FactWriterAssignment> {
    let current_devices: std::collections::BTreeSet<_> =
        devices.iter().map(|device| device.id.as_str()).collect();
    let mut host_nodes = Vec::new();
    let mut device_nodes = Vec::new();
    let mut included = BTreeMap::new();
    let mut ids = graph.descendants(machine);
    ids.push(machine);
    for id in ids {
        let Some(node) = graph.node(id) else { continue };
        let writer = if matches!(
            node.kind,
            ResourceClass::Gpu | ResourceClass::Nic | ResourceClass::Nvme
        ) {
            if !node
                .attrs
                .get("id")
                .is_some_and(|stable| current_devices.contains(stable.as_str()))
            {
                continue;
            }
            DEVICE_FACT_WRITER
        } else {
            HOST_FACT_WRITER
        };
        included.insert(id, writer);
        if writer == DEVICE_FACT_WRITER {
            device_nodes.push(id);
        } else {
            host_nodes.push(id);
        }
    }
    let mut host_edges = Vec::new();
    let mut device_edges = Vec::new();
    for edge in graph.edges() {
        if edge.kind != EdgeKind::Contains {
            continue;
        }
        let Some(writer) = included.get(&edge.to).copied() else {
            continue;
        };
        let fact = FactEdge::new(edge.from, edge.to, edge.kind);
        if writer == DEVICE_FACT_WRITER {
            device_edges.push(fact);
        } else {
            host_edges.push(fact);
        }
    }
    [
        FactWriterAssignment {
            writer: HOST_FACT_WRITER,
            nodes: host_nodes,
            edges: host_edges,
        },
        FactWriterAssignment {
            writer: DEVICE_FACT_WRITER,
            nodes: device_nodes,
            edges: device_edges,
        },
    ]
    .into_iter()
    .filter(|assignment| !assignment.nodes.is_empty() || !assignment.edges.is_empty())
    .collect()
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

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_sysfs_cpulists() {
        assert_eq!(parse_cpu_list("0-3").unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(
            parse_cpu_list("0-3,8,10-11").unwrap(),
            vec![0, 1, 2, 3, 8, 10, 11]
        );
        assert_eq!(parse_cpu_list("5").unwrap(), vec![5]);
        assert_eq!(parse_cpu_list("").unwrap(), Vec::<u32>::new());
        assert!(parse_cpu_list("3-0").is_err(), "inverted range");
        assert!(parse_cpu_list("x-y").is_err(), "garbage");
    }

    /// A normalized producer output must always survive validation: NUMA
    /// parents, count=1 CPU leaves, byte memory nodes, summary fields that
    /// agree with the specs. Failures here mean real agents fall back to
    /// flat topology or registration refuses.
    #[cfg(target_os = "linux")]
    #[test]
    fn produced_host_topology_validates_when_available() {
        let specs = match host_topology_fallible() {
            Ok(specs) => specs,
            Err(reason) => {
                // Containers/CI may lack sysfs nodes; only a full success is
                // provable here, and that is what production hosts run.
                eprintln!("skipping: no normalized host topology: {reason}");
                return;
            }
        };
        assert!(!specs.is_empty());
        let description = description_with_devices(Vec::new());
        assert!(!description.host_nodes.is_empty());
        validate_machine_description(&description).expect("produced host topology must validate");
        // Every CPU leaf must sit under a NUMA parent: partial containment
        // would place claims without locality facts.
        let parents: std::collections::BTreeSet<_> = specs
            .iter()
            .filter(|spec| spec.kind == ResourceClass::Numa)
            .map(|spec| spec.id.clone())
            .collect();
        assert!(!parents.is_empty());
        for spec in &specs {
            if spec.kind == ResourceClass::Cpu {
                let parent = spec.parent.as_ref().expect("cpu under NUMA");
                assert!(parents.contains(parent), "{parent} must exist");
            }
        }
    }

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
