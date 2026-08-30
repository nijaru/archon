from pathlib import Path

admit = Path('crates/kernel/src/admit.rs')
text = admit.read_text()

text = text.replace(
"pub type ClassUsage = BTreeMap<ResourceClass, Quantity>;\n\n/// Per-request hard node exclusions",
"pub type ClassUsage = BTreeMap<ResourceClass, Quantity>;\n\n/// Optional weighted fair-share policy over current authoritative resource usage.\n/// `OwnerId` is the proof-stage scheduling identity; the product-level Project/Queue\n/// model remains above the resource kernel. Missing or zero weights are treated as 1.\n#[derive(Clone, Debug, Default, PartialEq, Eq)]\npub struct AdmissionPolicy {\n    pub owner_ceiling: ClassUsage,\n    pub fair_share: bool,\n    pub owner_weights: BTreeMap<OwnerId, u32>,\n}\n\n/// Per-request hard node exclusions"
)

old = '''pub fn admit_with_ceiling(\n    graph: &Graph,\n    occupancy: &Occupancy,\n    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,\n    owner_ceiling: &ClassUsage,\n    queue: &[Queued],\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n) -> Option<Admission> {\n    let usage = owner_usage(graph, leases);\n    for index in order_queue(queue) {\n        let queued = &queue[index];\n        if !within_budget(\n            usage.get(&queued.owner).cloned().unwrap_or_default(),\n            &queued.request,\n            owner_ceiling,\n        ) {\n            continue;\n        }\n        if let Ok(allocation) = select(graph, occupancy, &queued.request, quarantine) {\n            return Some(Admission {\n                request: queued.request.clone(),\n                owner: queued.owner,\n                allocation,\n            });\n        }\n    }\n    None\n}\n'''
new = '''pub fn admit_with_ceiling(\n    graph: &Graph,\n    occupancy: &Occupancy,\n    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,\n    owner_ceiling: &ClassUsage,\n    queue: &[Queued],\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n) -> Option<Admission> {\n    admit_with_policy(\n        graph,\n        occupancy,\n        quarantine,\n        &AdmissionPolicy {\n            owner_ceiling: owner_ceiling.clone(),\n            ..AdmissionPolicy::default()\n        },\n        queue,\n        leases,\n    )\n}\n\n/// Admission with hard ceilings plus optional weighted dominant-share ordering.\n/// Priority remains the first ordering key; fair share only orders requests\n/// within the same priority class. With `fair_share=false`, ordering is exactly\n/// the legacy priority/submit-time/request-id order.\npub fn admit_with_policy(\n    graph: &Graph,\n    occupancy: &Occupancy,\n    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,\n    policy: &AdmissionPolicy,\n    queue: &[Queued],\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n) -> Option<Admission> {\n    let usage = owner_usage(graph, leases);\n    let capacity = claimable_capacity(graph);\n    for index in order_queue_with_policy(queue, &usage, &capacity, policy) {\n        let queued = &queue[index];\n        if !within_budget(\n            usage.get(&queued.owner).cloned().unwrap_or_default(),\n            &queued.request,\n            &policy.owner_ceiling,\n        ) {\n            continue;\n        }\n        if let Ok(allocation) = select(graph, occupancy, &queued.request, quarantine) {\n            return Some(Admission {\n                request: queued.request.clone(),\n                owner: queued.owner,\n                allocation,\n            });\n        }\n    }\n    None\n}\n'''
assert old in text
text = text.replace(old, new)

anchor = '''fn order_queue(queue: &[Queued]) -> Vec<usize> {\n    let mut order: Vec<usize> = (0..queue.len()).collect();\n    order.sort_by(|&left, &right| {\n        let left_request = &queue[left].request;\n        let right_request = &queue[right].request;\n        right_request\n            .priority\n            .cmp(&left_request.priority)\n            .then(queue[left].submitted_at.cmp(&queue[right].submitted_at))\n            .then(left_request.id.cmp(&right_request.id))\n    });\n    order\n}\n'''
addition = anchor + '''\nfn claimable_capacity(graph: &Graph) -> ClassUsage {\n    let mut capacity = ClassUsage::new();\n    for node in graph.nodes() {\n        for (dimension, amount) in &node.capacity {\n            if *amount == 0 || graph.claim_binding(node.id, *dimension).is_none() {\n                continue;\n            }\n            let total = capacity\n                .entry(node.kind)\n                .or_default()\n                .entry(*dimension)\n                .or_insert(0);\n            *total = total.saturating_add(*amount);\n        }\n    }\n    capacity\n}\n\nfn weighted_dominant_share(\n    usage: &ClassUsage,\n    capacity: &ClassUsage,\n    weight: u32,\n) -> u128 {\n    const SCALE: u128 = 1_000_000_000_000_000_000;\n    let weight = u128::from(weight.max(1));\n    let mut dominant = 0u128;\n    for (kind, quantities) in usage {\n        let Some(total) = capacity.get(kind) else {\n            continue;\n        };\n        for (dimension, used) in quantities {\n            let available = quantity_get(total, *dimension);\n            if available == 0 {\n                continue;\n            }\n            let denominator = u128::from(available) * weight;\n            let share = u128::from(*used) * SCALE / denominator;\n            dominant = dominant.max(share);\n        }\n    }\n    dominant\n}\n\nfn order_queue_with_policy(\n    queue: &[Queued],\n    usage: &BTreeMap<OwnerId, ClassUsage>,\n    capacity: &ClassUsage,\n    policy: &AdmissionPolicy,\n) -> Vec<usize> {\n    if !policy.fair_share {\n        return order_queue(queue);\n    }\n    let mut order: Vec<usize> = (0..queue.len()).collect();\n    order.sort_by(|&left, &right| {\n        let left_queued = &queue[left];\n        let right_queued = &queue[right];\n        let left_request = &left_queued.request;\n        let right_request = &right_queued.request;\n        let score = |owner: OwnerId| {\n            weighted_dominant_share(\n                usage.get(&owner).unwrap_or(&ClassUsage::new()),\n                capacity,\n                policy.owner_weights.get(&owner).copied().unwrap_or(1),\n            )\n        };\n        right_request\n            .priority\n            .cmp(&left_request.priority)\n            .then(score(left_queued.owner).cmp(&score(right_queued.owner)))\n            .then(left_queued.submitted_at.cmp(&right_queued.submitted_at))\n            .then(left_request.id.cmp(&right_request.id))\n    });\n    order\n}\n'''
assert anchor in text
text = text.replace(anchor, addition)

old = '''pub fn admit_backfill_with_exclusions(\n    graph: &Graph,\n    occupancy: &Occupancy,\n    quarantine: &std::collections::BTreeSet<NodeId>,\n    exclusions: &RequestExclusions,\n    owner_ceiling: &ClassUsage,\n    queue: &[Queued],\n    ctx: &BackfillCtx<'_>,\n) -> Option<Admission> {\n    let BackfillCtx {\n        now,\n        leases,\n        open_bindings,\n    } = ctx;\n    let usage = owner_usage(graph, leases);\n    let mut blocked: Vec<BlockedHead> = Vec::new();\n    for index in order_queue(queue) {\n'''
new = '''pub fn admit_backfill_with_exclusions(\n    graph: &Graph,\n    occupancy: &Occupancy,\n    quarantine: &std::collections::BTreeSet<NodeId>,\n    exclusions: &RequestExclusions,\n    owner_ceiling: &ClassUsage,\n    queue: &[Queued],\n    ctx: &BackfillCtx<'_>,\n) -> Option<Admission> {\n    admit_backfill_with_policy(\n        graph,\n        occupancy,\n        quarantine,\n        exclusions,\n        &AdmissionPolicy {\n            owner_ceiling: owner_ceiling.clone(),\n            ..AdmissionPolicy::default()\n        },\n        queue,\n        ctx,\n    )\n}\n\n/// EASY-style backfill with hard exclusions, hard ceilings, and optional\n/// weighted fair-share ordering. Fair share changes scheduler ordering only;\n/// the existing shadow model and Lease authority path remain unchanged.\npub fn admit_backfill_with_policy(\n    graph: &Graph,\n    occupancy: &Occupancy,\n    quarantine: &std::collections::BTreeSet<NodeId>,\n    exclusions: &RequestExclusions,\n    policy: &AdmissionPolicy,\n    queue: &[Queued],\n    ctx: &BackfillCtx<'_>,\n) -> Option<Admission> {\n    let BackfillCtx {\n        now,\n        leases,\n        open_bindings,\n    } = ctx;\n    let usage = owner_usage(graph, leases);\n    let capacity = claimable_capacity(graph);\n    let mut blocked: Vec<BlockedHead> = Vec::new();\n    for index in order_queue_with_policy(queue, &usage, &capacity, policy) {\n'''
assert old in text
text = text.replace(old, new)
text = text.replace('''            owner_ceiling,\n        ) {''', '''            &policy.owner_ceiling,\n        ) {''', 1)

admit.write_text(text)

lib = Path('crates/kernel/src/lib.rs')
text = lib.read_text()
text = text.replace(
'''pub use admit::{\n    Admission, BackfillCtx, ClassUsage, RequestExclusions, admit, admit_backfill,\n    admit_backfill_with_exclusions, admit_with_ceiling, owner_usage, refuse_reason,\n};''',
'''pub use admit::{\n    Admission, AdmissionPolicy, BackfillCtx, ClassUsage, RequestExclusions, admit, admit_backfill,\n    admit_backfill_with_exclusions, admit_backfill_with_policy, admit_with_ceiling,\n    admit_with_policy, owner_usage, refuse_reason,\n};'''
)

needle = '''    pub fn admit_with_ceiling(\n        &self,\n        queue: &[Queued],\n        owner_ceiling: &ClassUsage,\n    ) -> Option<Admission> {\n        let blocked = self.placement_blocked();\n        admit_with_ceiling(\n            &self.graph,\n            &self.occupancy(),\n            &blocked,\n            owner_ceiling,\n            queue,\n            &self.leases,\n        )\n    }\n'''
replacement = needle + '''\n    /// Admission under an explicit scheduler policy. Policy can enable\n    /// weighted dominant-share ordering without changing Lease authority.\n    pub fn admit_with_policy(\n        &self,\n        queue: &[Queued],\n        policy: &AdmissionPolicy,\n    ) -> Option<Admission> {\n        let blocked = self.placement_blocked();\n        admit_with_policy(\n            &self.graph,\n            &self.occupancy(),\n            &blocked,\n            policy,\n            queue,\n            &self.leases,\n        )\n    }\n'''
assert needle in text
text = text.replace(needle, replacement)

needle = '''    pub fn admit_backfill_with_exclusions(\n        &self,\n        queue: &[Queued],\n        owner_ceiling: &ClassUsage,\n        exclusions: &RequestExclusions,\n    ) -> Option<Admission> {\n        let blocked = self.placement_blocked();\n        admit_backfill_with_exclusions(\n            &self.graph,\n            &self.occupancy(),\n            &blocked,\n            exclusions,\n            owner_ceiling,\n            queue,\n            &crate::admit::BackfillCtx {\n                now: self.now,\n                leases: &self.leases,\n                open_bindings: &self.open_binding_leases(),\n            },\n        )\n    }\n'''
replacement = needle + '''\n    /// EASY-style backfill under an explicit scheduler policy.\n    pub fn admit_backfill_with_policy(\n        &self,\n        queue: &[Queued],\n        policy: &AdmissionPolicy,\n        exclusions: &RequestExclusions,\n    ) -> Option<Admission> {\n        let blocked = self.placement_blocked();\n        admit_backfill_with_policy(\n            &self.graph,\n            &self.occupancy(),\n            &blocked,\n            exclusions,\n            policy,\n            queue,\n            &crate::admit::BackfillCtx {\n                now: self.now,\n                leases: &self.leases,\n                open_bindings: &self.open_binding_leases(),\n            },\n        )\n    }\n'''
assert needle in text
text = text.replace(needle, replacement)
lib.write_text(text)

Path('crates/kernel/tests/fair_share.rs').write_text(r'''use std::collections::{BTreeMap, BTreeSet};

use archon_kernel::{
    AdmissionPolicy, Allocation, BindingScope, CapacityDimension, Claim, ClaimBinding,
    ClaimBindingUpdate, Cluster, Command, Edge, EdgeKind, Lease, LeaseId, LeaseState, Need, Node,
    NodeId, OwnerId, ProviderId, Quantity, Queued, Request, RequestClass, RequestId, ResourceClass,
    qty,
};

fn node(id: u64, kind: ResourceClass, capacity: Quantity) -> Node {
    Node {
        id: NodeId::from_u64(id),
        kind,
        attrs: Default::default(),
        capacity,
    }
}

fn cluster() -> Cluster {
    let mut cluster = Cluster::new();
    let mut nodes = vec![node(1, ResourceClass::Machine, Quantity::new())];
    let mut edges = Vec::new();
    let mut bindings = Vec::new();
    for id in 2..=5 {
        nodes.push(node(id, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)));
        edges.push(Edge {
            from: NodeId::from_u64(1),
            to: NodeId::from_u64(id),
            kind: EdgeKind::Contains,
            attrs: Default::default(),
        });
        bindings.push(ClaimBindingUpdate {
            node: NodeId::from_u64(id),
            dimension: CapacityDimension::Count,
            binding: Some(ClaimBinding {
                provider: ProviderId::ENFORCE,
                scope: BindingScope::Exclusive,
            }),
        });
    }
    nodes.push(node(6, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)));
    edges.push(Edge {
        from: NodeId::from_u64(1),
        to: NodeId::from_u64(6),
        kind: EdgeKind::Contains,
        attrs: Default::default(),
    });
    bindings.push(ClaimBindingUpdate {
        node: NodeId::from_u64(6),
        dimension: CapacityDimension::Count,
        binding: Some(ClaimBinding {
            provider: ProviderId::ENFORCE,
            scope: BindingScope::Exclusive,
        }),
    });
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes,
            edges,
            claim_bindings: bindings,
        })
        .unwrap();
    cluster
}

fn request(id: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
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
        priority,
        keep_alive: false,
        machine_local: true,
        grace_secs: 0,
    }
}

fn active_lease(id: u64, owner: u64, node: u64, kind: ResourceClass) -> Lease {
    let dimension = match kind {
        ResourceClass::Memory => CapacityDimension::Bytes,
        _ => CapacityDimension::Count,
    };
    Lease {
        id: LeaseId::from_u64(id),
        owner: OwnerId::from_u64(owner),
        allocation: Allocation {
            claims: vec![Claim {
                node: NodeId::from_u64(node),
                quantity: qty(dimension, 1),
            }],
            graph_revision: 1,
            explanation: "fixture".into(),
        },
        parent: None,
        expires_at: 100,
        prepare_deadline: 10,
        priority: 1,
        state: LeaseState::Active,
        exit_code: None,
    }
}

fn queue(owner1_priority: u32, owner2_priority: u32) -> Vec<Queued> {
    vec![
        Queued {
            request: request(10, owner1_priority),
            owner: OwnerId::from_u64(1),
            submitted_at: 1,
        },
        Queued {
            request: request(11, owner2_priority),
            owner: OwnerId::from_u64(2),
            submitted_at: 2,
        },
    ]
}

#[test]
fn weighted_fair_share_prefers_lower_dominant_share_within_priority() {
    let cluster = cluster();
    let leases = BTreeMap::from([
        (LeaseId::from_u64(1), active_lease(1, 1, 2, ResourceClass::Cpu)),
        (LeaseId::from_u64(2), active_lease(2, 2, 3, ResourceClass::Cpu)),
    ]);
    let occupancy = archon_kernel::occupancy_from_leases(
        leases.values(),
        &BTreeSet::new(),
        &BTreeSet::new(),
    );
    let policy = AdmissionPolicy {
        fair_share: true,
        owner_weights: BTreeMap::from([(OwnerId::from_u64(1), 1), (OwnerId::from_u64(2), 2)]),
        ..AdmissionPolicy::default()
    };
    let admission = archon_kernel::admit_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue(5, 5),
        &leases,
    )
    .expect("one CPU remains available");
    assert_eq!(admission.owner, OwnerId::from_u64(2));
}

#[test]
fn explicit_priority_still_precedes_fair_share() {
    let cluster = cluster();
    let leases = BTreeMap::from([
        (LeaseId::from_u64(1), active_lease(1, 1, 2, ResourceClass::Cpu)),
        (LeaseId::from_u64(2), active_lease(2, 2, 3, ResourceClass::Cpu)),
    ]);
    let occupancy = archon_kernel::occupancy_from_leases(
        leases.values(),
        &BTreeSet::new(),
        &BTreeSet::new(),
    );
    let policy = AdmissionPolicy {
        fair_share: true,
        owner_weights: BTreeMap::from([(OwnerId::from_u64(1), 1), (OwnerId::from_u64(2), 100)]),
        ..AdmissionPolicy::default()
    };
    let admission = archon_kernel::admit_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue(10, 1),
        &leases,
    )
    .expect("one CPU remains available");
    assert_eq!(admission.owner, OwnerId::from_u64(1));
}

#[test]
fn dominant_resource_share_compares_heterogeneous_usage() {
    let cluster = cluster();
    let leases = BTreeMap::from([
        (LeaseId::from_u64(1), active_lease(1, 1, 6, ResourceClass::Gpu)),
        (LeaseId::from_u64(2), active_lease(2, 2, 2, ResourceClass::Cpu)),
    ]);
    let occupancy = archon_kernel::occupancy_from_leases(
        leases.values(),
        &BTreeSet::new(),
        &BTreeSet::new(),
    );
    let policy = AdmissionPolicy {
        fair_share: true,
        ..AdmissionPolicy::default()
    };
    let admission = archon_kernel::admit_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue(5, 5),
        &leases,
    )
    .expect("CPU capacity remains");
    assert_eq!(admission.owner, OwnerId::from_u64(2));
}

#[test]
fn disabled_fair_share_preserves_legacy_submit_order() {
    let cluster = cluster();
    let leases = BTreeMap::from([
        (LeaseId::from_u64(1), active_lease(1, 1, 6, ResourceClass::Gpu)),
        (LeaseId::from_u64(2), active_lease(2, 2, 2, ResourceClass::Cpu)),
    ]);
    let occupancy = archon_kernel::occupancy_from_leases(
        leases.values(),
        &BTreeSet::new(),
        &BTreeSet::new(),
    );
    let policy = AdmissionPolicy::default();
    let admission = archon_kernel::admit_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &policy,
        &queue(5, 5),
        &leases,
    )
    .expect("CPU capacity remains");
    assert_eq!(admission.owner, OwnerId::from_u64(1));
}
''')
