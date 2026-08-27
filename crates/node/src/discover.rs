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
    /// Devices this machine exposes. Declared via ARCHON_DEVICES
    /// (`gpu:/dev/nvidia0` or `gpu=gpu0:/dev/nvidia0`); real discovery is a
    /// later upgrade behind the same representation.
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
}

pub fn describe() -> MachineDescription {
    MachineDescription {
        instance_id: String::new(),
        name: hostname(),
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()) as u64,
        memory_bytes: total_memory_bytes(),
        devices: declared_devices(),
    }
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
        let mut attrs = Attrs::new();
        attrs.insert("id".into(), device_spec.id.clone());
        attrs.insert("dev".into(), device_spec.dev.clone());
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
