use archon_kernel::{
    Allocation, BindingScope, CapacityDimension, Claim, ClaimBinding, ClaimBindingUpdate, Cluster,
    Command, Edge, EdgeKind, LeaseId, Need, Node, NodeId, OwnerId, PreemptionOutcome, ProviderId,
    Quantity, Request, RequestClass, RequestId, ResourceClass, preempt_victims, preemption_plan,
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

fn cluster(cpu_count: u64) -> Cluster {
    let mut cluster = Cluster::new();
    let machine = NodeId::from_u64(1);
    let mut nodes = vec![node(1, ResourceClass::Machine, Quantity::new())];
    let mut edges = Vec::new();
    let mut claim_bindings = Vec::new();
    for offset in 0..cpu_count {
        let id = 2 + offset;
        nodes.push(node(
            id,
            ResourceClass::Cpu,
            qty(CapacityDimension::Count, 1),
        ));
        edges.push(Edge {
            from: machine,
            to: NodeId::from_u64(id),
            kind: EdgeKind::Contains,
            attrs: Default::default(),
        });
        claim_bindings.push(ClaimBindingUpdate {
            node: NodeId::from_u64(id),
            dimension: CapacityDimension::Count,
            binding: Some(ClaimBinding {
                provider: ProviderId::ENFORCE,
                scope: BindingScope::Exclusive,
            }),
        });
    }
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes,
            edges,
            claim_bindings,
        })
        .unwrap();
    cluster
}

fn request(id: u64, cpus: u64, priority: u32) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, cpus),
            filters: Vec::new(),
        }],
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        lifetime: 100,
        priority,
        machine_local: true,
    }
}

fn reserve(cluster: &mut Cluster, lease: u64, owner: u64, cpu: u64, priority: u32) {
    cluster
        .apply(Command::ReserveLease {
            lease: LeaseId::from_u64(lease),
            owner: OwnerId::from_u64(owner),
            allocation: Allocation {
                claims: vec![Claim {
                    node: NodeId::from_u64(cpu),
                    quantity: qty(CapacityDimension::Count, 1),
                }],
                graph_revision: cluster.graph.revision,
                explanation: "fixture".into(),
            },
            expires_at: 100,
            priority,
        })
        .unwrap();
}

#[test]
fn already_feasible_request_needs_no_preemption() {
    let cluster = cluster(1);
    let request = request(10, 1, 10);
    let before = cluster.digest();

    let plan = preemption_plan(&cluster, &request);

    assert_eq!(plan.request, request.id);
    assert_eq!(plan.request_priority, 10);
    assert_eq!(plan.outcome, PreemptionOutcome::AlreadyFeasible);
    assert!(plan.victims.is_empty());
    assert!(plan.steps.is_empty());
    assert_eq!(preempt_victims(&cluster, &request), Some(Vec::new()));
    assert_eq!(
        cluster.digest(),
        before,
        "planning must not mutate authority"
    );
}

#[test]
fn plan_explains_the_lower_priority_victim_that_makes_request_feasible() {
    let mut cluster = cluster(1);
    reserve(&mut cluster, 1, 7, 2, 2);
    let request = request(10, 1, 10);
    let before = cluster.digest();

    let plan = preemption_plan(&cluster, &request);

    assert_eq!(plan.outcome, PreemptionOutcome::Planned);
    assert_eq!(plan.victims, vec![LeaseId::from_u64(1)]);
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].rank, 0);
    assert_eq!(plan.steps[0].lease, LeaseId::from_u64(1));
    assert_eq!(plan.steps[0].owner, OwnerId::from_u64(7));
    assert_eq!(plan.steps[0].priority, 2);
    assert!(plan.steps[0].feasible_after_eviction);
    assert_eq!(
        preempt_victims(&cluster, &request),
        Some(plan.victims.clone())
    );
    assert!(cluster.occupies(LeaseId::from_u64(1)));
    assert_eq!(
        cluster.digest(),
        before,
        "planning cannot revoke its victim"
    );
}

#[test]
fn planner_orders_multiple_victims_and_marks_the_feasibility_boundary() {
    let mut cluster = cluster(2);
    reserve(&mut cluster, 1, 1, 2, 2);
    reserve(&mut cluster, 2, 2, 3, 1);
    let request = request(10, 2, 10);

    let plan = preemption_plan(&cluster, &request);

    assert_eq!(plan.outcome, PreemptionOutcome::Planned);
    assert_eq!(
        plan.victims,
        vec![LeaseId::from_u64(2), LeaseId::from_u64(1)],
        "lower priority is considered first"
    );
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[0].priority, 1);
    assert!(!plan.steps[0].feasible_after_eviction);
    assert_eq!(plan.steps[1].priority, 2);
    assert!(plan.steps[1].feasible_after_eviction);
}

#[test]
fn equal_or_higher_priority_authority_is_not_a_preemption_candidate() {
    let mut cluster = cluster(1);
    reserve(&mut cluster, 1, 1, 2, 10);
    let request = request(10, 1, 10);

    let plan = preemption_plan(&cluster, &request);

    assert_eq!(plan.outcome, PreemptionOutcome::NotFeasible);
    assert!(plan.victims.is_empty());
    assert!(plan.steps.is_empty());
    assert_eq!(preempt_victims(&cluster, &request), None);
    assert!(cluster.occupies(LeaseId::from_u64(1)));
}
