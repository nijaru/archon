from pathlib import Path

select_path = Path("crates/kernel/src/select.rs")
text = select_path.read_text()

old = '''struct ScoreCtx<'a> {
    graph: &'a Graph,
    occupancy: &'a Occupancy,
    request: &'a Request,
    already: &'a [Vec<Claim>],
    mode: PackMode,
    memory_want: u64,
}

pub fn select(
'''
new = '''struct ScoreCtx<'a> {
    graph: &'a Graph,
    occupancy: &'a Occupancy,
    request: &'a Request,
    already: &'a [Vec<Claim>],
    mode: PackMode,
    memory_want: u64,
}

#[derive(Clone, Copy)]
struct TopologyFailure {
    constraint: usize,
    left: NodeId,
    right: NodeId,
}

#[derive(Default)]
struct TopologyTrace {
    failures: BTreeMap<usize, TopologyFailure>,
}

impl TopologyTrace {
    fn record(&mut self, failure: TopologyFailure) {
        self.failures.entry(failure.constraint).or_insert(failure);
    }
}

pub fn select(
'''
assert text.count(old) == 1, "ScoreCtx anchor changed"
text = text.replace(old, new, 1)

old = '''    let ctx = SelectCtx {
        graph,
        occupancy,
        request,
        mode,
        quarantine,
    };
    let mut picked: Vec<Vec<Claim>> = Vec::new();
    let mut notes = Vec::new();
    for need in &request.needs {
        let (claims, need_notes) = select_need(&ctx, need, &picked)?;
        notes.extend(need_notes);
        picked.push(claims);
    }
    let mut claims = Vec::new();
    for group in &picked {
        claims.extend(group.iter().cloned());
    }
    if !topology_holds(graph, &picked, request) {
        return Err(Error::Refused {
            explanation: "topology constraints were not satisfied".into(),
        });
    }
    Ok(Allocation {
        claims,
        graph_revision: graph.revision,
        explanation: format!(
            "{:?} {:?} selected {} claims{}",
            request.class,
            mode,
            notes.len(),
            if notes.is_empty() {
                String::new()
            } else {
                format!("; {}", notes.join("; "))
            }
        ),
    })
'''
new = '''    let ctx = SelectCtx {
        graph,
        occupancy,
        request,
        mode,
        quarantine,
    };
    let mut picked: Vec<Vec<Claim>> = Vec::new();
    let mut notes = Vec::new();
    let mut topology_trace = TopologyTrace::default();
    for need in &request.needs {
        let (claims, need_notes) = select_need(&ctx, need, &picked, &mut topology_trace)?;
        notes.extend(need_notes);
        picked.push(claims);
    }
    let mut claims = Vec::new();
    for group in &picked {
        claims.extend(group.iter().cloned());
    }
    if !topology_holds(graph, &picked, request) {
        return Err(Error::Refused {
            explanation: "topology constraints were not satisfied".into(),
        });
    }
    notes.extend(topology_notes(graph, &picked, request, &topology_trace));
    let claim_count = claims.len();
    Ok(Allocation {
        claims,
        graph_revision: graph.revision,
        explanation: format!(
            "{:?} {:?} selected {claim_count} claims{}",
            request.class,
            mode,
            if notes.is_empty() {
                String::new()
            } else {
                format!("; {}", notes.join("; "))
            }
        ),
    })
'''
assert text.count(old) == 1, "select body anchor changed"
text = text.replace(old, new, 1)

old = '''    let machines = graph.nodes_of_class(ResourceClass::Machine);
    for &machine in machines {
'''
new = '''    let machines = graph.nodes_of_class(ResourceClass::Machine);
    let mut topology_trace = TopologyTrace::default();
    for &machine in machines {
'''
assert text.count(old) == 1, "machine loop anchor changed"
text = text.replace(old, new, 1)

old = '''            match select_need_in(&ctx, need, &picked, Some(&allowed)) {
'''
new = '''            match select_need_in(
                &ctx,
                need,
                &picked,
                Some(&allowed),
                &mut topology_trace,
            ) {
'''
assert text.count(old) == 1, "machine select_need_in anchor changed"
text = text.replace(old, new, 1)

old = '''        return Ok(Allocation {
            claims,
            graph_revision: graph.revision,
            explanation: format!(
                "{:?} {:?} selected {} claims on {machine}",
                request.class,
                mode,
                notes.len()
            ),
        });
'''
new = '''        notes.extend(topology_notes(graph, &picked, request, &topology_trace));
        let claim_count = claims.len();
        return Ok(Allocation {
            claims,
            graph_revision: graph.revision,
            explanation: format!(
                "{:?} {:?} selected {claim_count} claims on {machine}{}",
                request.class,
                mode,
                if notes.is_empty() {
                    String::new()
                } else {
                    format!("; {}", notes.join("; "))
                }
            ),
        });
'''
assert text.count(old) == 1, "machine explanation anchor changed"
text = text.replace(old, new, 1)

old = '''fn select_need(
    ctx: &SelectCtx<'_>,
    need: &Need,
    already: &[Vec<Claim>],
) -> Result<(Vec<Claim>, Vec<String>), Error> {
    select_need_in(ctx, need, already, None)
}
'''
new = '''fn select_need(
    ctx: &SelectCtx<'_>,
    need: &Need,
    already: &[Vec<Claim>],
    topology_trace: &mut TopologyTrace,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
    select_need_in(ctx, need, already, None, topology_trace)
}
'''
assert text.count(old) == 1, "select_need signature anchor changed"
text = text.replace(old, new, 1)

old = '''fn select_need_in(
    ctx: &SelectCtx<'_>,
    need: &Need,
    already: &[Vec<Claim>],
    allowed: Option<&std::collections::BTreeSet<NodeId>>,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
'''
new = '''fn select_need_in(
    ctx: &SelectCtx<'_>,
    need: &Need,
    already: &[Vec<Claim>],
    allowed: Option<&std::collections::BTreeSet<NodeId>>,
    topology_trace: &mut TopologyTrace,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
'''
assert text.count(old) == 1, "select_need_in signature anchor changed"
text = text.replace(old, new, 1)

old = '''            if topology_holds(graph, &trial, request) {
                let score = score_node(&ctx, &[], node)?;
'''
new = '''            if topology_holds_traced(graph, &trial, request, topology_trace) {
                let score = score_node(&ctx, &[], node)?;
'''
assert text.count(old) == 1, "consumable topology anchor changed"
text = text.replace(old, new, 1)

old = '''        return select_spread(graph, request, already, need, &ctx, candidates, count);
'''
new = '''        return select_spread(
            graph,
            request,
            already,
            need,
            &ctx,
            candidates,
            count,
            topology_trace,
        );
'''
assert text.count(old) == 1, "spread call anchor changed"
text = text.replace(old, new, 1)

old = '''            if topology_holds(graph, &trial, request) {
                let score = score_node(&ctx, &chosen, node)?;
'''
new = '''            if topology_holds_traced(graph, &trial, request, topology_trace) {
                let score = score_node(&ctx, &chosen, node)?;
'''
assert text.count(old) == 1, "machine-local topology anchor changed"
text = text.replace(old, new, 1)

old = '''    candidates: Vec<NodeId>,
    count: u64,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
'''
new = '''    candidates: Vec<NodeId>,
    count: u64,
    topology_trace: &mut TopologyTrace,
) -> Result<(Vec<Claim>, Vec<String>), Error> {
'''
assert text.count(old) == 1, "select_spread signature anchor changed"
text = text.replace(old, new, 1)

old = '''        if topology_holds(graph, &trial, request) {
            let score = score_node(ctx, &chosen, node)?;
'''
new = '''        if topology_holds_traced(graph, &trial, request, topology_trace) {
            let score = score_node(ctx, &chosen, node)?;
'''
assert text.count(old) == 1, "spread topology anchor changed"
text = text.replace(old, new, 1)

old = '''fn topology_holds(graph: &Graph, picked: &[Vec<Claim>], request: &Request) -> bool {
    // Constraints referencing needs not yet selected cannot be evaluated
    // mid-placement; select() refuses malformed requests up front.
    for constraint in &request.topology {
        if constraint.left >= picked.len() || constraint.right >= picked.len() {
            continue;
        }
        for left in &picked[constraint.left] {
            for right in &picked[constraint.right] {
                if !graph.satisfies(left.node, right.node, constraint.relation) {
                    return false;
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    for group in picked {
        for claim in group {
            if !seen.insert(claim.node) {
                return false;
            }
        }
    }
    true
}
'''
new = '''fn topology_failure(
    graph: &Graph,
    picked: &[Vec<Claim>],
    request: &Request,
) -> Option<TopologyFailure> {
    // Constraints referencing needs not yet selected cannot be evaluated
    // mid-placement; select() refuses malformed requests up front.
    for (index, constraint) in request.topology.iter().enumerate() {
        if constraint.left >= picked.len() || constraint.right >= picked.len() {
            continue;
        }
        for left in &picked[constraint.left] {
            for right in &picked[constraint.right] {
                if !graph.satisfies(left.node, right.node, constraint.relation) {
                    return Some(TopologyFailure {
                        constraint: index,
                        left: left.node,
                        right: right.node,
                    });
                }
            }
        }
    }
    None
}

fn claims_are_unique(picked: &[Vec<Claim>]) -> bool {
    let mut seen = BTreeSet::new();
    picked
        .iter()
        .flat_map(|group| group.iter())
        .all(|claim| seen.insert(claim.node))
}

fn topology_holds_traced(
    graph: &Graph,
    picked: &[Vec<Claim>],
    request: &Request,
    trace: &mut TopologyTrace,
) -> bool {
    if !claims_are_unique(picked) {
        return false;
    }
    match topology_failure(graph, picked, request) {
        Some(failure) => {
            trace.record(failure);
            false
        }
        None => true,
    }
}

fn topology_holds(graph: &Graph, picked: &[Vec<Claim>], request: &Request) -> bool {
    claims_are_unique(picked) && topology_failure(graph, picked, request).is_none()
}

fn claim_nodes(claims: &[Claim]) -> String {
    claims
        .iter()
        .map(|claim| claim.node.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn ancestor_nodes(graph: &Graph, claims: &[Claim], class: ResourceClass) -> String {
    claims
        .iter()
        .filter_map(|claim| graph.ancestor_of_class(claim.node, class))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|node| node.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn topology_notes(
    graph: &Graph,
    picked: &[Vec<Claim>],
    request: &Request,
    trace: &TopologyTrace,
) -> Vec<String> {
    let mut notes = Vec::new();
    for (index, failure) in &trace.failures {
        let Some(constraint) = request.topology.get(*index) else {
            continue;
        };
        let Some(left) = picked.get(constraint.left) else {
            continue;
        };
        let Some(right) = picked.get(constraint.right) else {
            continue;
        };
        let selected = match constraint.relation {
            TopologyRelation::SameAncestor { class } => format!(
                "selected need[{}]={} and need[{}]={} under {class} {}",
                constraint.left,
                claim_nodes(left),
                constraint.right,
                claim_nodes(right),
                ancestor_nodes(graph, left, class),
            ),
            TopologyRelation::DifferentAncestor { class } => format!(
                "selected need[{}]={} under {class} {} and need[{}]={} under {class} {}",
                constraint.left,
                claim_nodes(left),
                ancestor_nodes(graph, left, class),
                constraint.right,
                claim_nodes(right),
                ancestor_nodes(graph, right, class),
            ),
            TopologyRelation::Contains => format!(
                "selected containment-related need[{}]={} and need[{}]={}",
                constraint.left,
                claim_nodes(left),
                constraint.right,
                claim_nodes(right),
            ),
            TopologyRelation::Connected => format!(
                "selected connected need[{}]={} and need[{}]={}",
                constraint.left,
                claim_nodes(left),
                constraint.right,
                claim_nodes(right),
            ),
            TopologyRelation::CachedOn => format!(
                "selected cache-related need[{}]={} and need[{}]={}",
                constraint.left,
                claim_nodes(left),
                constraint.right,
                claim_nodes(right),
            ),
        };
        let relation = match constraint.relation {
            TopologyRelation::SameAncestor { class } => format!("same-ancestor({class})"),
            TopologyRelation::DifferentAncestor { class } => {
                format!("different-ancestor({class})")
            }
            TopologyRelation::Contains => "contains".into(),
            TopologyRelation::Connected => "connected".into(),
            TopologyRelation::CachedOn => "cached-on".into(),
        };
        notes.push(format!(
            "topology[{index}] {relation} constrained placement (rejected {} vs {}); {selected}",
            failure.left, failure.right
        ));
    }
    notes
}
'''
assert text.count(old) == 1, "topology_holds anchor changed"
text = text.replace(old, new, 1)
select_path.write_text(text)

tests_path = Path("crates/kernel/tests/select.rs")
tests = tests_path.read_text()
old = '''    NodeId, OwnerId, Quantity, Request, RequestClass, RequestId, ResourceClass, qty,
'''
new = '''    NodeId, OwnerId, Quantity, Request, RequestClass, RequestId, ResourceClass,
    TopologyConstraint, TopologyRelation, qty,
'''
assert tests.count(old) == 1, "select test import anchor changed"
tests = tests.replace(old, new, 1)
assert "fn explanation_reports_topology_that_changes_machine_choice()" not in tests
tests += r'''

#[test]
fn explanation_reports_topology_that_changes_machine_choice() {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![
                node(100, ResourceClass::Machine, Quantity::new()),
                node(101, ResourceClass::Numa, Quantity::new()),
                node(102, ResourceClass::Numa, Quantity::new()),
                node(103, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(104, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
                node(200, ResourceClass::Machine, Quantity::new()),
                node(201, ResourceClass::Numa, Quantity::new()),
                node(202, ResourceClass::Cpu, qty(CapacityDimension::Count, 1)),
                node(203, ResourceClass::Gpu, qty(CapacityDimension::Count, 1)),
            ],
            edges: vec![
                contain(100, 101),
                contain(100, 102),
                contain(101, 103),
                contain(102, 104),
                contain(200, 201),
                contain(201, 202),
                contain(201, 203),
            ],
        })
        .unwrap();
    let mut request = request(
        RequestClass::Batch,
        vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: ResourceClass::Gpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
        ],
    );
    request.topology.push(TopologyConstraint {
        left: 0,
        right: 1,
        relation: TopologyRelation::SameAncestor {
            class: ResourceClass::Numa,
        },
    });

    let allocation = cluster.allocate(&request).expect("second machine satisfies NUMA locality");
    assert!(allocation.claims.iter().all(|claim| {
        cluster.graph.machine_of(claim.node) == Some(NodeId::from_u64(200))
    }));
    assert!(
        allocation
            .explanation
            .contains("topology[0] same-ancestor(numa) constrained placement")
    );
    assert!(allocation.explanation.contains("rejected"));
    assert!(allocation.explanation.contains("under numa"));
}

#[test]
fn explanation_omits_topology_that_did_not_change_candidate_choice() {
    let cluster = graph();
    let mut request = request(
        RequestClass::Batch,
        vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
            Need {
                kind: ResourceClass::Gpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            },
        ],
    );
    request.topology.push(TopologyConstraint {
        left: 0,
        right: 1,
        relation: TopologyRelation::SameAncestor {
            class: ResourceClass::Numa,
        },
    });

    let allocation = cluster.allocate(&request).expect("first choice already satisfies topology");
    assert!(!allocation.explanation.contains("topology[0]"));
}
'''
tests_path.write_text(tests)
