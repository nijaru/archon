from pathlib import Path

admit = Path('crates/kernel/src/admit.rs')
text = admit.read_text()

text = text.replace(
'''        queue,\n        &BTreeMap::new(),\n    )\n}''',
'''        queue,\n        &BTreeMap::new(),\n        &BTreeSet::new(),\n    )\n}''',
1,
)

text = text.replace(
'''    queue: &[Queued],\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n) -> Option<Admission> {\n    admit_with_policy(''',
'''    queue: &[Queued],\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n    open_bindings: &BTreeSet<LeaseId>,\n) -> Option<Admission> {\n    admit_with_policy(''',
1,
)
text = text.replace(
'''        queue,\n        leases,\n    )\n}\n\n/// Admission with hard ceilings''',
'''        queue,\n        leases,\n        open_bindings,\n    )\n}\n\n/// Admission with hard ceilings''',
1,
)
text = text.replace(
'''    policy: &AdmissionPolicy,\n    queue: &[Queued],\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n) -> Option<Admission> {\n    let usage = owner_usage(graph, leases);''',
'''    policy: &AdmissionPolicy,\n    queue: &[Queued],\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n    open_bindings: &BTreeSet<LeaseId>,\n) -> Option<Admission> {\n    let usage = owner_usage(graph, leases, open_bindings);''',
1,
)

old = '''/// Per-owner, per-kind consumption from active root leases. Only roots are\n/// counted so nested children are not double-charged against their owner.\npub fn owner_usage(\n    graph: &Graph,\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n) -> BTreeMap<crate::ids::OwnerId, ClassUsage> {\n    let mut usage: BTreeMap<crate::ids::OwnerId, ClassUsage> = BTreeMap::new();\n    for lease in leases.values() {\n        if lease.parent.is_some() {\n            continue;\n        }\n        if !matches!(\n            lease.state,\n            crate::types::LeaseState::Reserved\n                | crate::types::LeaseState::Preparing\n                | crate::types::LeaseState::Active\n        ) {\n            continue;\n        }'''
new = '''/// Per-owner, per-kind consumption from occupying root leases. Only roots are\n/// counted so nested children are not double-charged against their owner. A\n/// terminal root whose Binding is still open remains charged until fencing\n/// closes that authority, matching the Cluster occupancy contract.\npub fn owner_usage(\n    graph: &Graph,\n    leases: &BTreeMap<LeaseId, crate::types::Lease>,\n    open_bindings: &BTreeSet<LeaseId>,\n) -> BTreeMap<crate::ids::OwnerId, ClassUsage> {\n    let mut usage: BTreeMap<crate::ids::OwnerId, ClassUsage> = BTreeMap::new();\n    for lease in leases.values() {\n        if lease.parent.is_some() {\n            continue;\n        }\n        if !lease_occupies(lease, open_bindings.contains(&lease.id)) {\n            continue;\n        }'''
assert old in text
text = text.replace(old, new)
text = text.replace('let usage = owner_usage(graph, leases);', 'let usage = owner_usage(graph, leases, open_bindings);', 1)
admit.write_text(text)

lib = Path('crates/kernel/src/lib.rs')
text = lib.read_text()
text = text.replace(
'''            queue,\n            &self.leases,\n        )\n    }\n\n    /// Admission under an explicit scheduler policy.''',
'''            queue,\n            &self.leases,\n            &self.open_binding_leases(),\n        )\n    }\n\n    /// Admission under an explicit scheduler policy.''',
1,
)
text = text.replace(
'''            policy,\n            queue,\n            &self.leases,\n        )\n    }\n\n    /// EASY-style backfill over the priority queue''',
'''            policy,\n            queue,\n            &self.leases,\n            &self.open_binding_leases(),\n        )\n    }\n\n    /// EASY-style backfill over the priority queue''',
1,
)
lib.write_text(text)

service = Path('crates/node/src/service.rs')
text = service.read_text()
old = '''    /// Admit one request and drive its lease to Active. Returns the admitted\n    /// request id, or None when nothing fits.\n    pub fn admit_one(&mut self) -> Result<Option<RequestId>, Error> {\n        let exclusions = self.execution_exclusions();\n        let Some(admission) = self.cluster.admit_backfill_with_exclusions(\n            &self.queue,\n            &Default::default(),\n            &exclusions,\n        ) else {\n            return Ok(None);\n        };'''
new = '''    /// Admit one request using the default scheduler policy and drive its\n    /// lease to Active. Returns the admitted request id, or None when nothing fits.\n    pub fn admit_one(&mut self) -> Result<Option<RequestId>, Error> {\n        self.admit_one_with_policy(&archon_kernel::AdmissionPolicy::default())\n    }\n\n    /// Admit one request under an explicit scheduler policy. Policy changes\n    /// queue ordering/admission only; the resulting Allocation still enters\n    /// the ordinary conflict-checked Lease/Binding authority path.\n    pub fn admit_one_with_policy(\n        &mut self,\n        policy: &archon_kernel::AdmissionPolicy,\n    ) -> Result<Option<RequestId>, Error> {\n        let exclusions = self.execution_exclusions();\n        let Some(admission) = self.cluster.admit_backfill_with_policy(\n            &self.queue,\n            policy,\n            &exclusions,\n        ) else {\n            return Ok(None);\n        };'''
assert old in text
text = text.replace(old, new)
service.write_text(text)

fair = Path('crates/kernel/tests/fair_share.rs')
text = fair.read_text()
# New low-level policy API receives the set of leases that still occupy via open Bindings.
text = text.replace('&leases,\n    )\n    .expect', '&leases,\n        &BTreeSet::new(),\n    )\n    .expect')

text += r'''

#[test]
fn terminal_lease_with_open_binding_stays_charged_to_owner() {
    let cluster = cluster();
    let lease_id = LeaseId::from_u64(1);
    let mut lease = active_lease(1, 1, 2, ResourceClass::Cpu);
    lease.state = LeaseState::Failed;
    let leases = BTreeMap::from([(lease_id, lease)]);

    let charged = archon_kernel::owner_usage(
        &cluster.graph,
        &leases,
        &BTreeSet::from([lease_id]),
    );
    assert_eq!(
        charged[&OwnerId::from_u64(1)][&ResourceClass::Cpu][&CapacityDimension::Count],
        1
    );

    let fenced = archon_kernel::owner_usage(&cluster.graph, &leases, &BTreeSet::new());
    assert!(!fenced.contains_key(&OwnerId::from_u64(1)));
}

#[test]
fn fair_share_ordering_composes_with_backfill_shadow_safety() {
    let cluster = cluster();
    let running_id = LeaseId::from_u64(1);
    let running = active_lease(1, 1, 2, ResourceClass::Cpu);
    let leases = BTreeMap::from([(running_id, running)]);
    let open_bindings = BTreeSet::new();
    let occupancy = archon_kernel::occupancy_from_leases(
        leases.values(),
        &open_bindings,
        &BTreeSet::new(),
    );

    let mut head = request(20, 10);
    head.needs[0].quantity = qty(CapacityDimension::Count, 4);
    let queue = vec![
        Queued {
            request: head,
            owner: OwnerId::from_u64(3),
            submitted_at: 1,
        },
        Queued {
            request: request(21, 5),
            owner: OwnerId::from_u64(1),
            submitted_at: 2,
        },
        Queued {
            request: request(22, 5),
            owner: OwnerId::from_u64(2),
            submitted_at: 3,
        },
    ];
    let policy = AdmissionPolicy {
        fair_share: true,
        ..AdmissionPolicy::default()
    };
    let admission = archon_kernel::admit_backfill_with_policy(
        &cluster.graph,
        &occupancy,
        &BTreeSet::new(),
        &Default::default(),
        &policy,
        &queue,
        &archon_kernel::BackfillCtx {
            now: 0,
            leases: &leases,
            open_bindings: &open_bindings,
        },
    )
    .expect("short lower-share work can backfill before the protected head");
    assert_eq!(admission.owner, OwnerId::from_u64(2));
    assert_eq!(admission.request.id, RequestId::from_u64(22));
}
'''
fair.write_text(text)

Path('crates/node/tests/fair_share_service.rs').write_text(r'''use std::collections::BTreeMap;

use archon_kernel::{
    AdmissionPolicy, CapacityDimension, Need, OwnerId, Request, RequestClass, RequestId,
    ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::MachineDescription;
use archon_node::protocol::{ExecutionCapabilities, RuntimeCapabilities};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LocalExecutor, NodeService};

fn request(id: u64) -> Request {
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
        lifetime: 100,
        priority: 5,
        keep_alive: false,
        machine_local: true,
        grace_secs: 0,
    }
}

#[test]
fn node_service_uses_fair_share_before_lease_commit() {
    let mut service = NodeService::new();
    let description = MachineDescription {
        instance_id: "fair-share-machine".into(),
        name: "fair-share-machine".into(),
        cpus: 4,
        memory_bytes: 0,
        host_nodes: vec![],
        devices: vec![],
    };
    let executor = LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()));
    service
        .register_agent_with_capabilities(
            description,
            Box::new(executor),
            ExecutionCapabilities {
                process: RuntimeCapabilities {
                    available: true,
                    cpu_limit: true,
                    memory_limit: false,
                    device_isolation: false,
                    physical_cpu_placement: false,
                    numa_memory_placement: false,
                },
                container: RuntimeCapabilities::default(),
            },
        )
        .expect("register proof machine");

    let policy = AdmissionPolicy {
        fair_share: true,
        owner_weights: BTreeMap::new(),
        ..AdmissionPolicy::default()
    };

    service.submit(request(1), OwnerId::from_u64(1));
    assert_eq!(
        service.admit_one_with_policy(&policy).unwrap(),
        Some(RequestId::from_u64(1))
    );

    service.submit(request(2), OwnerId::from_u64(1));
    service.submit(request(3), OwnerId::from_u64(2));
    assert_eq!(
        service.admit_one_with_policy(&policy).unwrap(),
        Some(RequestId::from_u64(3)),
        "the owner with no current dominant share should be admitted first"
    );
}
''')
