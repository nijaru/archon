from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"expected snippet not found in {path}: {old[:120]!r}")
    p.write_text(text.replace(old, new, count))

# Admission gets request-specific hard exclusions. Unlike quarantine these are
# permanent for the current execution contract and must not create backfill
# debt for a request that can run only on excluded nodes.
replace(
    "crates/kernel/src/admit.rs",
    "use crate::ids::{LeaseId, NodeId, OwnerId};\n",
    "use crate::ids::{LeaseId, NodeId, OwnerId, RequestId};\n",
)
replace(
    "crates/kernel/src/admit.rs",
    "pub type ClassUsage = BTreeMap<ResourceClass, Quantity>;\n",
    "pub type ClassUsage = BTreeMap<ResourceClass, Quantity>;\n\n/// Per-request hard node exclusions supplied by an execution/provider layer.\n/// These differ from quarantine: excluded nodes are not expected to become\n/// usable merely because time advances or another Lease releases capacity.\npub type RequestExclusions = BTreeMap<RequestId, BTreeSet<NodeId>>;\n",
)
old = '''pub fn admit_backfill(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<NodeId>,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> Option<Admission> {
    let BackfillCtx {
        now,
        leases,
        open_bindings,
    } = ctx;
    let usage = owner_usage(graph, leases);
    let mut blocked: Vec<BlockedHead> = Vec::new();
    for index in order_queue(queue) {
        let queued = &queue[index];
        if !within_budget(
            usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            owner_ceiling,
        ) {
            continue;
        }
        match select(graph, occupancy, &queued.request, quarantine) {
            Ok(allocation) => {
                let claims: BTreeSet<NodeId> =
                    allocation.claims.iter().map(|claim| claim.node).collect();
                let finishes = now.saturating_add(queued.request.lifetime);
                let safe = blocked.iter().all(|head| match head.shadow {
                    Some(shadow) => finishes <= shadow || claims.is_disjoint(&head.shadow_claims),
                    // No finite release is proven: only a job that never
                    // touches the head's nodes can be sure not to delay it.
                    None => claims.is_disjoint(&head.shadow_claims),
                });
                if safe {
                    return Some(Admission {
                        request: queued.request.clone(),
                        owner: queued.owner,
                        allocation,
                    });
                }
            }
            Err(_) => {
                // A request the cluster could satisfy but for quarantine is
                // temporarily blocked: no proven release time exists, so
                // only claim-disjoint jobs may backfill past it.
                if let Ok(allocation) = select(graph, occupancy, &queued.request, &BTreeSet::new())
                {
                    blocked.push(BlockedHead {
                        shadow: None,
                        shadow_claims: claims_by_node(&allocation.claims).into_keys().collect(),
                    });
                } else if let Some(head) = shadow_head(
                    graph,
                    quarantine,
                    leases,
                    open_bindings,
                    &queued.request,
                    *now,
                ) {
                    blocked.push(head);
                }
            }
        }
    }
    None
}
'''
new = '''pub fn admit_backfill(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<NodeId>,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> Option<Admission> {
    admit_backfill_with_exclusions(
        graph,
        occupancy,
        quarantine,
        &RequestExclusions::new(),
        owner_ceiling,
        queue,
        ctx,
    )
}

/// EASY-style backfill with per-request hard placement exclusions. Hard
/// exclusions represent an execution/provider incompatibility, not a
/// temporarily unavailable resource: a head that could run only on excluded
/// nodes does not reserve a shadow or block compatible work behind it.
pub fn admit_backfill_with_exclusions(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<NodeId>,
    exclusions: &RequestExclusions,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> Option<Admission> {
    let BackfillCtx {
        now,
        leases,
        open_bindings,
    } = ctx;
    let usage = owner_usage(graph, leases);
    let mut blocked: Vec<BlockedHead> = Vec::new();
    for index in order_queue(queue) {
        let queued = &queue[index];
        if !within_budget(
            usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            owner_ceiling,
        ) {
            continue;
        }
        let mut effective = quarantine.clone();
        let hard = exclusions.get(&queued.request.id).cloned().unwrap_or_default();
        effective.extend(hard.iter().copied());
        match select(graph, occupancy, &queued.request, &effective) {
            Ok(allocation) => {
                let claims: BTreeSet<NodeId> =
                    allocation.claims.iter().map(|claim| claim.node).collect();
                let finishes = now.saturating_add(queued.request.lifetime);
                let safe = blocked.iter().all(|head| match head.shadow {
                    Some(shadow) => finishes <= shadow || claims.is_disjoint(&head.shadow_claims),
                    // No finite release is proven: only a job that never
                    // touches the head's nodes can be sure not to delay it.
                    None => claims.is_disjoint(&head.shadow_claims),
                });
                if safe {
                    return Some(Admission {
                        request: queued.request.clone(),
                        owner: queued.owner,
                        allocation,
                    });
                }
            }
            Err(_) => {
                // If removing only the temporary quarantine makes this request
                // fit, it is temporarily blocked and must retain the existing
                // unbounded shadow semantics. Hard execution exclusions remain
                // in force in that projection.
                if let Ok(allocation) = select(graph, occupancy, &queued.request, &hard) {
                    blocked.push(BlockedHead {
                        shadow: None,
                        shadow_claims: claims_by_node(&allocation.claims).into_keys().collect(),
                    });
                } else if let Some(head) = shadow_head(
                    graph,
                    &effective,
                    leases,
                    open_bindings,
                    &queued.request,
                    *now,
                ) {
                    blocked.push(head);
                }
            }
        }
    }
    None
}
'''
replace("crates/kernel/src/admit.rs", old, new)

replace(
    "crates/kernel/src/lib.rs",
    "    Admission, BackfillCtx, ClassUsage, admit, admit_backfill, admit_with_ceiling, owner_usage,\n    refuse_reason,\n",
    "    Admission, BackfillCtx, ClassUsage, RequestExclusions, admit, admit_backfill,\n    admit_backfill_with_exclusions, admit_with_ceiling, owner_usage, refuse_reason,\n",
)
old = '''    pub fn admit_backfill(
        &self,
        queue: &[Queued],
        owner_ceiling: &ClassUsage,
    ) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit_backfill(
            &self.graph,
            &self.occupancy(),
            &blocked,
            owner_ceiling,
            queue,
            &crate::admit::BackfillCtx {
                now: self.now,
                leases: &self.leases,
                open_bindings: &self.open_binding_leases(),
            },
        )
    }
'''
new = '''    pub fn admit_backfill(
        &self,
        queue: &[Queued],
        owner_ceiling: &ClassUsage,
    ) -> Option<Admission> {
        self.admit_backfill_with_exclusions(queue, owner_ceiling, &RequestExclusions::new())
    }

    /// EASY-style backfill with request-specific hard node exclusions. The
    /// execution/provider layer can use this without mutating resource facts
    /// or schedulability state in the Graph.
    pub fn admit_backfill_with_exclusions(
        &self,
        queue: &[Queued],
        owner_ceiling: &ClassUsage,
        exclusions: &RequestExclusions,
    ) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit_backfill_with_exclusions(
            &self.graph,
            &self.occupancy(),
            &blocked,
            exclusions,
            owner_ceiling,
            queue,
            &crate::admit::BackfillCtx {
                now: self.now,
                leases: &self.leases,
                open_bindings: &self.open_binding_leases(),
            },
        )
    }
'''
replace("crates/kernel/src/lib.rs", old, new)

# The node layer excludes normalized CPU/memory machines for executable work
# until its execution adapters can translate provider host identities into
# cpuset/NUMA enforcement. This is per-request and transient runtime state,
# never a Graph fact.
old = '''    /// Admit one request and drive its lease to Active. Returns the admitted
    /// request id, or None when nothing fits.
    pub fn admit_one(&mut self) -> Result<Option<RequestId>, Error> {
        let Some(admission) = self
            .cluster
            .admit_backfill(&self.queue, &Default::default())
        else {
            return Ok(None);
        };
'''
new = '''    /// Hard placement exclusions imposed by the current execution adapters.
    /// Normalized CPU and Memory Nodes carry provider-authored physical
    /// locality (`archon.host-id`), while today's process/container adapters
    /// enforce only aggregate CPU/memory limits. Until cpuset/NUMA translation
    /// lands, executable work must not claim those physical placements.
    fn execution_exclusions(&self) -> archon_kernel::RequestExclusions {
        let mut exclusions = archon_kernel::RequestExclusions::new();
        let machines = self.cluster.graph.nodes_of_class(ResourceClass::Machine);
        for queued in &self.queue {
            let request = &queued.request;
            if request.command.is_empty() && request.image.is_none() {
                continue;
            }
            let exact_kinds: BTreeSet<ResourceClass> = request
                .needs
                .iter()
                .filter_map(|need| match need.kind {
                    ResourceClass::Cpu | ResourceClass::Memory => Some(need.kind),
                    _ => None,
                })
                .collect();
            if exact_kinds.is_empty() {
                continue;
            }
            for machine in &machines {
                let has_normalized_claim_kind = self
                    .cluster
                    .graph
                    .descendants(*machine)
                    .into_iter()
                    .filter_map(|node| self.cluster.graph.node(node))
                    .any(|node| {
                        exact_kinds.contains(&node.kind)
                            && node.attrs.contains_key(crate::discover::HOST_ID_ATTR)
                    });
                if has_normalized_claim_kind {
                    exclusions.entry(request.id).or_default().insert(*machine);
                }
            }
        }
        exclusions
    }

    /// Admit one request and drive its lease to Active. Returns the admitted
    /// request id, or None when nothing fits.
    pub fn admit_one(&mut self) -> Result<Option<RequestId>, Error> {
        let exclusions = self.execution_exclusions();
        let Some(admission) = self.cluster.admit_backfill_with_exclusions(
            &self.queue,
            &Default::default(),
            &exclusions,
        ) else {
            return Ok(None);
        };
'''
replace("crates/node/src/service.rs", old, new)

# Kernel regression: hard execution exclusions choose compatible resources and
# do not behave like temporary quarantine in backfill.
p = Path("crates/kernel/tests/admit.rs")
text = p.read_text()
text = text.replace(
    "use std::collections::BTreeSet;\n",
    "use std::collections::{BTreeMap, BTreeSet};\n",
    1,
)
text += r'''

fn two_machine_cluster() -> Cluster {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(10, ResourceClass::Machine, Quantity::new()),
                node(11, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(12, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(20, ResourceClass::Machine, Quantity::new()),
                node(21, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                archon_kernel::Edge {
                    from: NodeId::from_u64(10),
                    to: NodeId::from_u64(11),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                archon_kernel::Edge {
                    from: NodeId::from_u64(10),
                    to: NodeId::from_u64(12),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
                archon_kernel::Edge {
                    from: NodeId::from_u64(20),
                    to: NodeId::from_u64(21),
                    kind: archon_kernel::EdgeKind::Contains,
                    attrs: Default::default(),
                },
            ],
        })
        .unwrap();
    cluster
}

#[test]
fn hard_request_exclusion_selects_a_compatible_machine() {
    let cluster = two_machine_cluster();
    let request = cpu_request(10, 1, 1);
    let queue = vec![Queued {
        request: request.clone(),
        owner: OwnerId::from_u64(1),
        submitted_at: 1,
    }];
    let exclusions = BTreeMap::from([(
        request.id,
        BTreeSet::from([NodeId::from_u64(10)]),
    )]);
    let admission = cluster
        .admit_backfill_with_exclusions(&queue, &Default::default(), &exclusions)
        .expect("second machine remains compatible");
    let machine = cluster
        .graph
        .machine_of(admission.allocation.claims[0].node)
        .unwrap();
    assert_eq!(machine, NodeId::from_u64(20));
}

#[test]
fn hard_excluded_head_does_not_create_backfill_debt() {
    let cluster = two_machine_cluster();
    let head = cpu_request(10, 2, 10);
    let later = cpu_request(11, 1, 1);
    let queue = vec![
        Queued {
            request: head.clone(),
            owner: OwnerId::from_u64(1),
            submitted_at: 1,
        },
        Queued {
            request: later.clone(),
            owner: OwnerId::from_u64(2),
            submitted_at: 2,
        },
    ];
    let exclusions = BTreeMap::from([(
        head.id,
        BTreeSet::from([NodeId::from_u64(10)]),
    )]);
    let admission = cluster
        .admit_backfill_with_exclusions(&queue, &Default::default(), &exclusions)
        .expect("permanently incompatible head must not block compatible work");
    assert_eq!(admission.request.id, later.id);
}
'''
p.write_text(text)

# Node regression: normalized physical CPU placement is skipped in favor of a
# flat aggregate-capacity machine; if no compatible machine exists admission
# stays queued without mutating Lease authority.
Path("crates/node/tests/execution_placement.rs").write_text(r'''use archon_kernel::{
    Attrs, CapacityDimension, Need, OwnerId, Quantity, Request, RequestClass, RequestId,
    ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::{HostNodeSpec, MachineDescription};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LocalExecutor, NodeService};

fn flat(instance: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: instance.into(),
        cpus: 1,
        memory_bytes: 1 << 30,
        host_nodes: Vec::new(),
        devices: Vec::new(),
    }
}

fn normalized(instance: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: instance.into(),
        cpus: 1,
        memory_bytes: 1 << 30,
        host_nodes: vec![
            HostNodeSpec {
                id: "numa/0".into(),
                kind: ResourceClass::Numa,
                parent: None,
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
                capacity: qty(CapacityDimension::Bytes, 1 << 30),
            },
        ],
        devices: Vec::new(),
    }
}

fn executable_cpu_request(id: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: Vec::new(),
        }],
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        command: vec!["true".into()],
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime: 60,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
    }
}

fn executor() -> Box<LocalExecutor> {
    Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new())))
}

#[test]
fn executable_work_skips_normalized_cpu_placement_without_cpuset_support() {
    let mut service = NodeService::new();
    let normalized_machine = service
        .register_agent(normalized("normalized"), executor())
        .expect("normalized registration");
    let flat_machine = service
        .register_agent(flat("flat"), executor())
        .expect("flat registration");

    service.submit(executable_cpu_request(1), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let lease = service.cluster.leases.values().next().expect("lease");
    let selected = service
        .cluster
        .graph
        .machine_of(lease.allocation.claims[0].node)
        .expect("claim machine");
    assert_eq!(selected, flat_machine);
    assert_ne!(selected, normalized_machine);
}

#[test]
fn normalized_cpu_only_machine_stays_unadmitted_until_placement_is_enforceable() {
    let mut service = NodeService::new();
    service
        .register_agent(normalized("normalized-only"), executor())
        .expect("normalized registration");
    service.submit(executable_cpu_request(1), OwnerId::from_u64(1));

    assert_eq!(service.admit_one().unwrap(), None);
    assert!(
        service.cluster.leases.is_empty(),
        "unsupported execution placement must be rejected before Lease authority mutates"
    );
}
''')
