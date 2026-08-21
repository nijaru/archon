//! Local machine discovery: build the synthetic-graph representation of this
//! machine for `ApplyGraph`.

use std::process::Command;

use fleet_kernel::{Attrs, Dimension, Edge, EdgeKind, Node, NodeKind, Quantity, qty};

pub struct LocalMachine {
    pub machine: fleet_kernel::NodeId,
    pub cpus: Vec<fleet_kernel::NodeId>,
    pub memory: fleet_kernel::NodeId,
}

struct IdGen {
    next: u64,
}

impl IdGen {
    fn node(&mut self) -> fleet_kernel::NodeId {
        self.next += 1;
        fleet_kernel::NodeId::from_u64(self.next)
    }
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

/// Discover this machine as a Fleet graph: one Machine node, one Cpu node per
/// logical CPU, one Memory node with total bytes.
pub fn discover() -> (LocalMachine, Vec<Node>, Vec<Edge>) {
    let mut ids = IdGen { next: 0 };
    let machine = ids.node();
    let cpus: Vec<_> = (0..std::thread::available_parallelism().map_or(1, |n| n.get()))
        .map(|_| ids.node())
        .collect();
    let memory = ids.node();

    let mut name = Attrs::new();
    name.insert("name".into(), hostname());
    let mut nodes = vec![
        Node {
            id: machine,
            kind: NodeKind::Machine,
            attrs: name,
            capacity: Quantity::new(),
        },
        Node {
            id: memory,
            kind: NodeKind::Memory,
            attrs: Attrs::new(),
            capacity: qty(Dimension::Bytes, total_memory_bytes()),
        },
    ];
    for cpu in &cpus {
        nodes.push(Node {
            id: *cpu,
            kind: NodeKind::Cpu,
            attrs: Attrs::new(),
            capacity: qty(Dimension::Count, 1),
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
