from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    return text.replace(old, new, 1)


types_path = Path('crates/kernel/src/types.rs')
types = types_path.read_text()
types = replace_once(
    types,
    '''#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Allocation {
    pub claims: Vec<Claim>,
    pub graph_revision: u64,
    pub explanation: String,
}
''',
    '''#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PlacementReason {
    MachineSelected {
        machine: NodeId,
    },
    CandidateScore {
        node: NodeId,
        score: i64,
        data_local: Vec<NodeId>,
        degraded_ancestor: Option<NodeId>,
    },
    TopologyConstraint {
        index: usize,
        relation: TopologyRelation,
        selected_left: Vec<NodeId>,
        selected_right: Vec<NodeId>,
        rejected: Option<(NodeId, NodeId)>,
    },
}

/// Human-readable placement summary plus typed evidence that policy and UI
/// layers can inspect without parsing prose. Legacy persisted Allocations that
/// encoded this field as a string deserialize as a summary with no reasons.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PlacementExplanation {
    pub summary: String,
    pub reasons: Vec<PlacementReason>,
}

impl PlacementExplanation {
    pub fn new(summary: impl Into<String>, reasons: Vec<PlacementReason>) -> Self {
        Self {
            summary: summary.into(),
            reasons,
        }
    }
}

impl From<String> for PlacementExplanation {
    fn from(summary: String) -> Self {
        Self::new(summary, Vec::new())
    }
}

impl From<&str> for PlacementExplanation {
    fn from(summary: &str) -> Self {
        Self::from(summary.to_owned())
    }
}

impl fmt::Display for PlacementExplanation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary)
    }
}

impl AsRef<str> for PlacementExplanation {
    fn as_ref(&self) -> &str {
        &self.summary
    }
}

impl std::ops::Deref for PlacementExplanation {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for PlacementExplanation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Structured {
            summary: String,
            #[serde(default)]
            reasons: Vec<PlacementReason>,
        }

        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Representation {
            Legacy(String),
            Structured(Structured),
        }

        match Representation::deserialize(deserializer)? {
            Representation::Legacy(summary) => Ok(Self::from(summary)),
            Representation::Structured(value) => Ok(Self::new(value.summary, value.reasons)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Allocation {
    pub claims: Vec<Claim>,
    pub graph_revision: u64,
    pub explanation: PlacementExplanation,
}
''',
    'allocation explanation type',
)
types_path.write_text(types)

lib_path = Path('crates/kernel/src/lib.rs')
lib = lib_path.read_text()
lib = replace_once(
    lib,
    '''    Filter, IdentifierError, Lease, LeaseState, Need, Node, NodeState, PortPublish, Preference,
    ProviderFactBatch, Quantity, Queued, Request, RequestClass, ResourceClass, StorageMount,
    TopologyConstraint, TopologyRelation, qty, quantity_get,
''',
    '''    Filter, IdentifierError, Lease, LeaseState, Need, Node, NodeState, PlacementExplanation,
    PlacementReason, PortPublish, Preference, ProviderFactBatch, Quantity, Queued, Request,
    RequestClass, ResourceClass, StorageMount, TopologyConstraint, TopologyRelation, qty,
    quantity_get,
''',
    'lib exports',
)
lib_path.write_text(lib)

select_path = Path('crates/kernel/src/select.rs')
select = select_path.read_text()
select = replace_once(
    select,
    '''    Allocation, CapacityDimension, Claim, Need, Preference, Request, RequestClass, ResourceClass,
    TopologyRelation, qty, quantity_get,
''',
    '''    Allocation, CapacityDimension, Claim, Need, PlacementExplanation, PlacementReason, Preference,
    Request, RequestClass, ResourceClass, TopologyRelation, qty, quantity_get,
''',
    'select imports',
)
select = replace_once(
    select,
    '''    notes.extend(topology_notes(graph, &picked, request, &topology_trace));
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
''',
    '''    notes.extend(topology_notes(graph, &picked, request, &topology_trace));
    let reasons = placement_reasons(graph, occupancy, request, mode, &picked, &topology_trace)?;
    let claim_count = claims.len();
    Ok(Allocation {
        claims,
        graph_revision: graph.revision,
        explanation: PlacementExplanation::new(
            format!(
                "{:?} {:?} selected {claim_count} claims{}",
                request.class,
                mode,
                if notes.is_empty() {
                    String::new()
                } else {
                    format!("; {}", notes.join("; "))
                }
            ),
            reasons,
        ),
    })
''',
    'ordinary allocation explanation',
)
select = replace_once(
    select,
    '''            return Ok(machine_allocation(
                graph, request, mode, machine, claims, notes,
            ));
''',
    '''            let reasons = placement_reasons(
                graph,
                occupancy,
                request,
                mode,
                &picked,
                &topology_trace,
            )?;
            return Ok(machine_allocation(
                graph, request, mode, machine, claims, notes, reasons,
            ));
''',
    'greedy machine allocation call',
)
select = replace_once(
    select,
    '''            return Ok(machine_allocation(
                graph, request, mode, machine, claims, notes,
            ));
''',
    '''            let reasons = placement_reasons(
                graph,
                occupancy,
                request,
                mode,
                &fallback,
                &topology_trace,
            )?;
            return Ok(machine_allocation(
                graph, request, mode, machine, claims, notes, reasons,
            ));
''',
    'fallback machine allocation call',
)
select = replace_once(
    select,
    '''fn machine_allocation(
    graph: &Graph,
    request: &Request,
    mode: PackMode,
    machine: NodeId,
    claims: Vec<Claim>,
    notes: Vec<String>,
) -> Allocation {
    let claim_count = claims.len();
    Allocation {
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
    }
}
''',
    '''fn machine_allocation(
    graph: &Graph,
    request: &Request,
    mode: PackMode,
    machine: NodeId,
    claims: Vec<Claim>,
    notes: Vec<String>,
    mut reasons: Vec<PlacementReason>,
) -> Allocation {
    reasons.insert(0, PlacementReason::MachineSelected { machine });
    let claim_count = claims.len();
    Allocation {
        claims,
        graph_revision: graph.revision,
        explanation: PlacementExplanation::new(
            format!(
                "{:?} {:?} selected {claim_count} claims on {machine}{}",
                request.class,
                mode,
                if notes.is_empty() {
                    String::new()
                } else {
                    format!("; {}", notes.join("; "))
                }
            ),
            reasons,
        ),
    }
}
''',
    'machine allocation definition',
)
anchor = '''fn topology_notes(
    graph: &Graph,
    picked: &[Vec<Claim>],
    request: &Request,
    trace: &TopologyTrace,
) -> Vec<String> {
'''
if select.count(anchor) != 1:
    raise SystemExit('topology notes anchor missing')
helper = '''fn placement_reasons(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    mode: PackMode,
    picked: &[Vec<Claim>],
    trace: &TopologyTrace,
) -> Result<Vec<PlacementReason>, Error> {
    let mut reasons = Vec::new();
    for (index, group) in picked.iter().enumerate() {
        let need = &request.needs[index];
        let ctx = ScoreCtx {
            graph,
            occupancy,
            request,
            already: &picked[..index],
            mode,
            memory_want: quantity_get(&consumable_quantity(need), CapacityDimension::Bytes),
        };
        let mut chosen = Vec::new();
        for claim in group {
            let data_local = request
                .data
                .iter()
                .copied()
                .filter(|data| graph.caches(claim.node, *data))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            reasons.push(PlacementReason::CandidateScore {
                node: claim.node,
                score: score_node(&ctx, &chosen, claim.node)?,
                data_local,
                degraded_ancestor: graph.degraded_ancestor(claim.node),
            });
            chosen.push(claim.clone());
        }
    }
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
        reasons.push(PlacementReason::TopologyConstraint {
            index: *index,
            relation: constraint.relation,
            selected_left: left.iter().map(|claim| claim.node).collect(),
            selected_right: right.iter().map(|claim| claim.node).collect(),
            rejected: Some((failure.left, failure.right)),
        });
    }
    Ok(reasons)
}

'''
select = select.replace(anchor, helper + anchor, 1)
select_path.write_text(select)

select_test_path = Path('crates/kernel/tests/select.rs')
select_test = select_test_path.read_text()
select_test = replace_once(
    select_test,
    '''    assert!(allocation.explanation.contains("under numa"));
}
''',
    '''    assert!(allocation.explanation.contains("under numa"));
    assert!(allocation.explanation.reasons.iter().any(|reason| {
        matches!(
            reason,
            archon_kernel::PlacementReason::MachineSelected { machine }
                if *machine == NodeId::from_u64(200)
        )
    }));
    assert!(allocation.explanation.reasons.iter().any(|reason| {
        matches!(
            reason,
            archon_kernel::PlacementReason::TopologyConstraint {
                index: 0,
                relation: TopologyRelation::SameAncestor { class },
                selected_left,
                selected_right,
                rejected: Some((left, right)),
            } if *class == ResourceClass::Numa
                && selected_left == &vec![NodeId::from_u64(202)]
                && selected_right == &vec![NodeId::from_u64(203)]
                && *left == NodeId::from_u64(103)
                && *right == NodeId::from_u64(104)
        )
    }));
}
''',
    'structured topology test',
)
select_test_path.write_text(select_test)
