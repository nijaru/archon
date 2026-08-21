use archon_kernel::{
    Cluster, Command, Dimension, Need, Node, NodeId, NodeKind, OwnerId, Quantity, Queued, Request,
    RequestClass, RequestId, admit, qty,
};
use std::collections::BTreeSet;

fn node(id: u64, kind: NodeKind, capacity: Quantity) -> Node {
    Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn cluster() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(1, NodeKind::Machine, Quantity::new()),
                node(2, NodeKind::Cpu, qty(Dimension::Count, 1)),
                node(3, NodeKind::Cpu, qty(Dimension::Count, 1)),
            ],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(1),
                to: NodeId::from_u64(2),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![],
            edges: vec![archon_kernel::Edge {
                from: NodeId::from_u64(1),
                to: NodeId::from_u64(3),
                kind: archon_kernel::EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap();
    cluster
}

fn cpu_request(id: u64, count: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, count),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        machine_local: true,
        lifetime: 10,
        priority,
    }
}

#[test]
fn higher_priority_wins() {
    let cluster = cluster();
    let admission = admit(
        &cluster.graph,
        &cluster.occupancy(),
        &[
            Queued {
                request: cpu_request(1, 1, 1),
                owner: OwnerId::from_u64(1),
                submitted_at: 1,
            },
            Queued {
                request: cpu_request(2, 1, 10),
                owner: OwnerId::from_u64(2),
                submitted_at: 2,
            },
        ],
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(admission.request.id, RequestId::from_u64(2));
}

#[test]
fn earlier_submit_breaks_priority_ties() {
    let cluster = cluster();
    let admission = admit(
        &cluster.graph,
        &cluster.occupancy(),
        &[
            Queued {
                request: cpu_request(2, 1, 5),
                owner: OwnerId::from_u64(2),
                submitted_at: 8,
            },
            Queued {
                request: cpu_request(1, 1, 5),
                owner: OwnerId::from_u64(1),
                submitted_at: 3,
            },
        ],
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(admission.request.id, RequestId::from_u64(1));
}

#[test]
fn skips_infeasible_head() {
    let cluster = cluster();
    let admission = admit(
        &cluster.graph,
        &cluster.occupancy(),
        &[
            Queued {
                request: cpu_request(1, 8, 100),
                owner: OwnerId::from_u64(1),
                submitted_at: 1,
            },
            Queued {
                request: cpu_request(2, 1, 1),
                owner: OwnerId::from_u64(2),
                submitted_at: 2,
            },
        ],
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(admission.request.id, RequestId::from_u64(2));
}
