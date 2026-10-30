use archon_kernel::{
    Allocation, BindingId, BindingScope, CapacityDimension, Claim, ClaimBinding,
    ClaimBindingUpdate, Cluster, Command, Edge, EdgeKind, Effect, Error, FactWriterId, LeaseId,
    Node, NodeId, OwnerId, ProviderFactBatch, ProviderId, Quantity, ResourceClass, qty,
};

const MACHINE: NodeId = NodeId::from_u64(1);
const RESOURCE: NodeId = NodeId::from_u64(2);
const OTHER_MACHINE: NodeId = NodeId::from_u64(9);
const LEASE: LeaseId = LeaseId::from_u64(1);
const BINDING: BindingId = BindingId::from_u64(1);

#[derive(Clone, Copy, Debug)]
enum GraphCommand {
    Legacy,
    Resource,
    Provider,
}

const GRAPH_COMMANDS: [GraphCommand; 3] = [
    GraphCommand::Legacy,
    GraphCommand::Resource,
    GraphCommand::Provider,
];

impl GraphCommand {
    fn command(self, nodes: Vec<Node>, edges: Vec<Edge>) -> Command {
        match self {
            Self::Legacy => Command::ApplyGraph { nodes, edges },
            Self::Resource => Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings: vec![],
            },
            Self::Provider => Command::ApplyProviderFacts {
                batches: vec![ProviderFactBatch {
                    writer: FactWriterId::from_u64(1),
                    nodes,
                    edges,
                }],
                claim_bindings: vec![],
            },
        }
    }
}

fn node(id: u64, kind: ResourceClass, capacity: Quantity) -> Node {
    Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn contains(from: u64, to: u64) -> Edge {
    Edge {
        from: NodeId::from_u64(from),
        to: NodeId::from_u64(to),
        kind: EdgeKind::Contains,
        attrs: Default::default(),
    }
}

fn cluster(command: GraphCommand) -> Cluster {
    let nodes = vec![
        node(1, ResourceClass::Machine, Quantity::new()),
        node(2, ResourceClass::Memory, qty(CapacityDimension::Bytes, 100)),
        node(3, ResourceClass::Numa, Quantity::new()),
        node(9, ResourceClass::Machine, Quantity::new()),
    ];
    let edges = vec![contains(1, 3), contains(3, 2)];
    let claim_bindings = vec![ClaimBindingUpdate {
        node: RESOURCE,
        dimension: CapacityDimension::Bytes,
        binding: Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        }),
    }];
    let mut cluster = Cluster::new();
    cluster
        .apply(match command {
            GraphCommand::Provider => Command::ApplyProviderFacts {
                batches: vec![ProviderFactBatch {
                    writer: FactWriterId::from_u64(1),
                    nodes,
                    edges,
                }],
                claim_bindings,
            },
            _ => Command::ApplyResourceFacts {
                nodes,
                edges,
                claim_bindings,
            },
        })
        .unwrap();
    for (machine, session) in [(MACHINE, 11), (OTHER_MACHINE, 22)] {
        cluster
            .apply(Command::SetAgentSession { machine, session })
            .unwrap();
    }
    cluster
}

fn claim(cluster: &mut Cluster, reserved: bool) {
    let allocation = Allocation {
        claims: vec![Claim {
            node: RESOURCE,
            quantity: qty(CapacityDimension::Bytes, 40),
        }],
        graph_revision: cluster.graph.revision,
        explanation: "machine target test".into(),
    };
    cluster
        .apply(if reserved {
            Command::ReserveLease {
                lease: LEASE,
                owner: OwnerId::from_u64(1),
                allocation,
                expires_at: 1_000,
                priority: 1,
            }
        } else {
            Command::OpenLease {
                lease: LEASE,
                owner: OwnerId::from_u64(1),
                allocation,
                parent: None,
                expires_at: 1_000,
                prepare_deadline: 1_000,
                priority: 1,
            }
        })
        .unwrap();
}

fn bind(cluster: &mut Cluster) {
    cluster
        .apply(Command::OpenBinding {
            binding: BINDING,
            lease: LEASE,
            node: RESOURCE,
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        })
        .unwrap();
}

fn move_to_other_machine(command: GraphCommand) -> Command {
    command.command(
        vec![node(1, ResourceClass::Numa, Quantity::new())],
        vec![contains(9, 1)],
    )
}

fn assert_busy(cluster: &mut Cluster, command: Command) {
    let before = cluster.digest();
    let log = cluster.log.clone();
    assert_eq!(
        cluster.apply(command),
        Err(Error::ResourceBusy { node: RESOURCE })
    );
    assert_eq!(cluster.digest(), before, "rejection must be atomic");
    assert_eq!(cluster.log, log, "rejected facts must not enter replay");
}

#[test]
fn graph_commands_cannot_redirect_or_remove_an_occupied_machine_target() {
    for command in GRAPH_COMMANDS {
        let updates = [
            move_to_other_machine(command),
            command.command(vec![node(1, ResourceClass::Numa, Quantity::new())], vec![]),
            // The original machine remains, but is no longer nearest.
            command.command(
                vec![node(3, ResourceClass::Machine, Quantity::new())],
                vec![],
            ),
        ];
        for update in updates {
            let mut cluster = cluster(command);
            claim(&mut cluster, false);
            bind(&mut cluster);
            assert_busy(&mut cluster, update);
            assert_eq!(cluster.graph.machine_of(RESOURCE), Some(MACHINE));
            assert_eq!(cluster.machine_bindings(MACHINE), vec![BINDING]);
            assert!(cluster.machine_bindings(OTHER_MACHINE).is_empty());

            let effects = cluster
                .apply(Command::RevokeLease { lease: LEASE })
                .unwrap();
            assert!(matches!(
                effects.as_slice(),
                [Effect::Fence { session: 11, .. }]
            ));
            assert_eq!(
                cluster.apply(Command::RecordBindingFenced {
                    binding: BINDING,
                    session: 22,
                    fence: 1,
                }),
                Err(Error::StaleSession {
                    expected: 11,
                    got: 22
                })
            );
            assert!(cluster.occupancy().is_used(RESOURCE));
            cluster
                .apply(Command::RecordBindingFenced {
                    binding: BINDING,
                    session: 11,
                    fence: 1,
                })
                .unwrap();
            assert!(!cluster.occupancy().is_used(RESOURCE));
        }
    }
}

#[test]
fn reservations_and_unbound_preparations_already_pin_the_machine() {
    for reserved in [true, false] {
        let mut cluster = cluster(GraphCommand::Resource);
        claim(&mut cluster, reserved);
        assert_busy(&mut cluster, move_to_other_machine(GraphCommand::Resource));
    }
}

#[test]
fn active_and_terminal_leases_keep_the_machine_until_authority_is_closed() {
    for terminal in [
        None,
        Some(Command::RevokeLease { lease: LEASE }),
        Some(Command::RecordBindingFailed {
            binding: BINDING,
            session: 11,
            fence: 1,
            reason: "enforcement outcome unknown".into(),
        }),
    ] {
        let mut cluster = cluster(GraphCommand::Resource);
        claim(&mut cluster, false);
        bind(&mut cluster);
        cluster
            .apply(Command::RecordBindingPrepared {
                binding: BINDING,
                session: 11,
                provider_handle: 1,
                fence: 1,
            })
            .unwrap();
        cluster
            .apply(Command::ActivateLease { lease: LEASE })
            .unwrap();
        if let Some(terminal) = terminal {
            cluster.apply(terminal).unwrap();
        }
        assert_busy(&mut cluster, move_to_other_machine(GraphCommand::Resource));
    }
}

#[test]
fn harmless_fact_refreshes_preserve_live_targets() {
    for command in GRAPH_COMMANDS {
        let mut cluster = cluster(command);
        claim(&mut cluster, false);
        bind(&mut cluster);
        let mut memory = node(2, ResourceClass::Memory, qty(CapacityDimension::Bytes, 120));
        memory.attrs.insert("label".into(), "updated".into());
        let revision = cluster.graph.revision;
        cluster
            .apply(command.command(
                vec![
                    memory.clone(),
                    node(77, ResourceClass::Machine, Quantity::new()),
                ],
                vec![],
            ))
            .unwrap();
        assert_eq!(cluster.graph.revision, revision + 1);
        assert_eq!(cluster.graph.node(RESOURCE), Some(&memory));
        assert_eq!(cluster.graph.machine_of(RESOURCE), Some(MACHINE));
        assert_eq!(
            cluster.occupancy().used_on(RESOURCE),
            qty(CapacityDimension::Bytes, 40)
        );
        assert!(matches!(
            cluster
                .apply(Command::FenceBinding { binding: BINDING })
                .unwrap()
                .as_slice(),
            [Effect::Fence { session: 11, .. }]
        ));
    }
}

#[test]
fn closed_bindings_do_not_pin_a_machine_after_the_lease_releases_authority() {
    let mut cluster = cluster(GraphCommand::Resource);
    claim(&mut cluster, false);
    bind(&mut cluster);
    cluster
        .apply(Command::RecordBindingFenced {
            binding: BINDING,
            session: 11,
            fence: 1,
        })
        .unwrap();
    // Closing a binding alone does not terminate its lease's reservation.
    assert_busy(&mut cluster, move_to_other_machine(GraphCommand::Resource));
    cluster
        .apply(Command::RevokeLease { lease: LEASE })
        .unwrap();
    cluster
        .apply(move_to_other_machine(GraphCommand::Resource))
        .unwrap();
    assert_eq!(cluster.graph.machine_of(RESOURCE), Some(OTHER_MACHINE));
    assert!(!cluster.occupancy().is_used(RESOURCE));
}
