//! Local machine discovery: build the synthetic-graph representation of this
//! machine for `ApplyGraph`.

use std::process::Command;

use serde::{Deserialize, Serialize};

use archon_kernel::{Attrs, CapacityDimension, Edge, EdgeKind, Node, Quantity, ResourceClass, qty};

pub struct LocalMachine {
    #[allow(dead_code)]
    pub machine: archon_kernel::NodeId,
    #[allow(dead_code)]
    pub cpus: Vec<archon_kernel::NodeId>,
    #[allow(dead_code)]
    pub memory: archon_kernel::NodeId,
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

/// A machine as the agent reports it; the controller builds the graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineDescription {
    /// Stable identity across reconnects; empty for local discovery.
    pub instance_id: String,
    pub name: String,
    pub cpus: u64,
    pub memory_bytes: u64,
    /// Devices this machine exposes. `ARCHON_DEVICES` can provide explicit
    /// declarations; supported providers may discover the same representation
    /// automatically.
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
}

/// One declared device: a stable `id` that survives re-registration and
/// access-path changes, plus the current host path it is reachable through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSpec {
    pub kind: ResourceClass,
    pub id: String,
    pub dev: String,
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

/// Build the Archon graph for a machine description: one Machine node, one
/// Cpu node per logical CPU, one Memory node with total bytes. Shared by
/// the local and remote paths so both produce identical graph shapes.
pub fn build_graph(
    description: &MachineDescription,
    first_id: u64,
) -> (LocalMachine, Vec<Node>, Vec<Edge>) {
    let mut ids = IdGen { next: first_id };
    let machine = ids.node();
    let cpus: Vec<_> = (0..description.cpus).map(|_| ids.node()).collect();
    let memory = ids.node();

    let mut name = Attrs::new();
    name.insert("name".into(), description.name.clone());
    name.insert("agent_id".into(), description.instance_id.clone());
    let mut nodes = vec![
        Node {
            id: machine,
            kind: ResourceClass::Machine,
            attrs: name,
            capacity: Quantity::new(),
        },
        Node {
            id: memory,
            kind: ResourceClass::Memory,
            attrs: Attrs::new(),
            capacity: qty(CapacityDimension::Bytes, description.memory_bytes),
        },
    ];
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
    }
    for cpu in &cpus {
        nodes.push(Node {
            id: *cpu,
            kind: ResourceClass::Cpu,
            attrs: Attrs::new(),
            capacity: qty(CapacityDimension::Count, 1),
        });
    }
    let mut edges = vec![Edge {
        from: machine,
        to: memory,
        kind: EdgeKind::Contains,
        attrs: Attrs::new(),
    }];
    for cpu in &cpus {
        edges.push(Edge {
            from: machine,
            to: *cpu,
            kind: EdgeKind::Contains,
            attrs: Attrs::new(),
        });
    }
    for device in &description.devices {
        let Some(device) = nodes.iter().find(|node| {
            node.kind == device.kind && node.attrs.get("id").is_some_and(|id| id == &device.id)
        }) else {
            continue;
        };
        edges.push(Edge {
            from: machine,
            to: device.id,
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
}
