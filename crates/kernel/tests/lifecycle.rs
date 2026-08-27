use archon_kernel::{
    CapacityDimension, Cluster, Command, Edge, EdgeKind, Error, LeaseId, Need, Node, NodeId,
    NodeState, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};

fn cluster() -> (Cluster, NodeId, NodeId) {
    let machine = NodeId::from_u64(1);
    let cpu = NodeId::from_u64(2);
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                Node {
                    id: machine,
                    kind: ResourceClass::Machine,
                    attrs: Default::default(),
                    capacity: Default::default(),
                },
                Node {
                    id: cpu,
                    kind: ResourceClass::Cpu,
                    attrs: Default::default(),
                    capacity: qty(CapacityDimension::Count, 1),
                },
            ],
            edges: vec![Edge {
                from: machine,
                to: cpu,
                kind: EdgeKind::Contains,
                attrs: Default::default(),
            }],
        })
        .unwrap();
    (cluster, machine, cpu)
}

fn request() -> Request {
    Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec![],
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 10,
        priority: 1,
        keep_alive: false,
        machine_local: true,
        grace_secs: 0,
    }
}

#[test]
fn non_schedulable_state_blocks_descendant_placement() {
    for state in [
        NodeState::Joining,
        NodeState::Draining,
        NodeState::Unavailable,
        NodeState::Quarantined,
        NodeState::Retired,
    ] {
        let (mut cluster, machine, cpu) = cluster();
        cluster
            .apply(Command::SetNodeState {
                node: machine,
                state,
            })
            .unwrap();
        assert_eq!(cluster.node_state(machine), Some(state));
        assert_eq!(cluster.node_state(cpu), Some(NodeState::Schedulable));
        assert!(cluster.allocate(&request()).is_err());
    }
}

#[test]
fn draining_preserves_existing_lease_authority() {
    let (mut cluster, machine, cpu) = cluster();
    let allocation = cluster.allocate(&request()).unwrap();
    cluster
        .apply(Command::ReserveLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation,
            expires_at: 100,
            priority: 1,
        })
        .unwrap();

    cluster
        .apply(Command::SetNodeState {
            node: machine,
            state: NodeState::Draining,
        })
        .unwrap();
    assert!(cluster.occupies(LeaseId::from_u64(1)));
    assert!(cluster.allocate(&request()).is_err());

    cluster
        .apply(Command::SetNodeState {
            node: machine,
            state: NodeState::Schedulable,
        })
        .unwrap();
    assert!(cluster.occupies(LeaseId::from_u64(1)));
    assert!(cluster.allocate(&request()).is_err());
    assert_eq!(cluster.node_state(cpu), Some(NodeState::Schedulable));
}

#[test]
fn retirement_waits_for_authority_and_cannot_reuse_identity() {
    let (mut cluster, machine, _) = cluster();
    let allocation = cluster.allocate(&request()).unwrap();
    cluster
        .apply(Command::ReserveLease {
            lease: LeaseId::from_u64(1),
            owner: OwnerId::from_u64(1),
            allocation,
            expires_at: 100,
            priority: 1,
        })
        .unwrap();

    let err = cluster
        .apply(Command::SetNodeState {
            node: machine,
            state: NodeState::Retired,
        })
        .unwrap_err();
    assert_eq!(err, Error::ResourceBusy { node: machine });
    assert_eq!(cluster.node_state(machine), Some(NodeState::Schedulable));

    cluster
        .apply(Command::ReleaseLease {
            lease: LeaseId::from_u64(1),
        })
        .unwrap();
    cluster
        .apply(Command::SetNodeState {
            node: machine,
            state: NodeState::Retired,
        })
        .unwrap();
    assert_eq!(cluster.node_state(machine), Some(NodeState::Retired));
    assert!(cluster.allocate(&request()).is_err());

    let err = cluster
        .apply(Command::SetNodeState {
            node: machine,
            state: NodeState::Schedulable,
        })
        .unwrap_err();
    assert_eq!(
        err,
        Error::Invalid("retired resources require a new identity")
    );
}

#[test]
fn lifecycle_commands_replay_with_state() {
    let (mut cluster, machine, _) = cluster();
    cluster
        .apply(Command::SetNodeState {
            node: machine,
            state: NodeState::Joining,
        })
        .unwrap();
    cluster
        .apply(Command::SetNodeState {
            node: machine,
            state: NodeState::Schedulable,
        })
        .unwrap();

    let replayed = Cluster::replay(&cluster.log).unwrap();
    assert_eq!(replayed.digest(), cluster.digest());
    assert_eq!(replayed.node_state(machine), Some(NodeState::Schedulable));
    assert_eq!(replayed.node_state(NodeId::from_u64(99)), None);
}
