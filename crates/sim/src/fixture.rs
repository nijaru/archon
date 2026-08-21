use archon_kernel::{Attrs, Dimension, Edge, EdgeKind, Node, NodeId, NodeKind, Quantity, qty};

pub const GIB: u64 = 1 << 30;

#[derive(Default)]
pub struct IdGen {
    next: u64,
}

impl IdGen {
    pub fn node(&mut self) -> NodeId {
        self.next += 1;
        NodeId::from_u64(self.next)
    }
}

#[derive(Clone)]
pub struct MachineIds {
    pub machine: NodeId,
    pub numa: NodeId,
    pub cpus: Vec<NodeId>,
    pub memory: NodeId,
    pub gpu: NodeId,
    pub nic: NodeId,
    pub nvme: NodeId,
}

pub struct TinyGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub machines: Vec<MachineIds>,
}

impl TinyGraph {
    /// Mark machine `machine_index` degraded (`health=degraded` on the
    /// machine node). Call before applying the graph.
    pub fn with_degraded(&mut self, machine_index: usize) {
        let machine = self.machines[machine_index].machine;
        if let Some(node) = self.nodes.iter_mut().find(|node| node.id == machine) {
            node.attrs.insert("health".into(), "degraded".into());
        }
    }

    /// Append a DataObject cached on machine `machine_index`'s NUMA node and
    /// return its id. Call before applying the graph.
    pub fn with_dataset(&mut self, machine_index: usize, id: u64) -> NodeId {
        let data = NodeId::from_u64(id);
        let numa = self.machines[machine_index].numa;
        self.nodes.push(node(
            data,
            NodeKind::DataObject,
            qty(Dimension::Bytes, 4 * GIB),
        ));
        self.edges.push(Edge {
            from: data,
            to: numa,
            kind: EdgeKind::CachedOn,
            attrs: Attrs::new(),
        });
        data
    }
}

pub fn tiny_graph(machines: usize) -> TinyGraph {
    let mut ids = IdGen::default();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let rack = ids.node();
    nodes.push(node(rack, NodeKind::Rack, Quantity::new()));

    let mut machine_ids = Vec::new();
    for index in 0..machines {
        let machine = ids.node();
        let socket = ids.node();
        let numa = ids.node();
        let memory = ids.node();
        let pcie = ids.node();
        let gpu = ids.node();
        let nic = ids.node();
        let nvme = ids.node();
        let cpu0 = ids.node();
        let cpu1 = ids.node();
        let label = index.to_string();
        nodes.extend([
            labeled(
                machine,
                NodeKind::Machine,
                Quantity::new(),
                "name",
                &format!("m{label}"),
            ),
            node(socket, NodeKind::Socket, Quantity::new()),
            node(numa, NodeKind::Numa, Quantity::new()),
            node(cpu0, NodeKind::Cpu, qty(Dimension::Count, 1)),
            node(cpu1, NodeKind::Cpu, qty(Dimension::Count, 1)),
            node(memory, NodeKind::Memory, qty(Dimension::Bytes, 8 * GIB)),
            node(pcie, NodeKind::PcieRoot, Quantity::new()),
            labeled(
                gpu,
                NodeKind::Gpu,
                qty(Dimension::Count, 1),
                "model",
                if index % 2 == 0 { "h100" } else { "a100" },
            ),
            node(nic, NodeKind::Nic, qty(Dimension::Count, 1)),
            node(nvme, NodeKind::Nvme, qty(Dimension::Count, 1)),
        ]);
        contain(
            &mut edges,
            &[
                (rack, machine),
                (machine, socket),
                (socket, numa),
                (numa, cpu0),
                (numa, cpu1),
                (numa, memory),
                (numa, pcie),
                (pcie, gpu),
                (pcie, nic),
                (pcie, nvme),
            ],
        );
        machine_ids.push(MachineIds {
            machine,
            numa,
            cpus: vec![cpu0, cpu1],
            memory,
            gpu,
            nic,
            nvme,
        });
    }
    TinyGraph {
        nodes,
        edges,
        machines: machine_ids,
    }
}

fn node(id: NodeId, kind: NodeKind, capacity: Quantity) -> Node {
    Node {
        id,
        kind,
        attrs: Attrs::new(),
        capacity,
    }
}

fn labeled(id: NodeId, kind: NodeKind, capacity: Quantity, key: &str, value: &str) -> Node {
    let mut attrs = Attrs::new();
    attrs.insert(key.into(), value.into());
    Node {
        id,
        kind,
        attrs,
        capacity,
    }
}

fn contain(edges: &mut Vec<Edge>, pairs: &[(NodeId, NodeId)]) {
    for (from, to) in pairs {
        edges.push(Edge {
            from: *from,
            to: *to,
            kind: EdgeKind::Contains,
            attrs: Attrs::new(),
        });
    }
}
