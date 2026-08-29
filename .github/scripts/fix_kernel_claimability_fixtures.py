from pathlib import Path

path = Path("crates/kernel/tests/admit.rs")
text = path.read_text()
text = text.replace(
    '''    CapacityDimension, Cluster, Command, Need, Node, NodeId, OwnerId, Quantity, Queued, Request,
    RequestClass, RequestId, ResourceClass, admit, qty,
''',
    '''    BindingScope, CapacityDimension, ClaimBinding, ClaimBindingUpdate, Cluster, Command, Need,
    Node, NodeId, OwnerId, ProviderId, Quantity, Queued, Request, RequestClass, RequestId,
    ResourceClass, admit, qty,
''',
    1,
)
anchor = '''fn cluster() -> Cluster {
'''
helper = '''fn make_cpu_claimable(cluster: &mut Cluster, nodes: &[u64]) {
    cluster
        .apply(Command::ApplyResourceFacts {
            nodes: Vec::new(),
            edges: Vec::new(),
            claim_bindings: nodes
                .iter()
                .map(|id| ClaimBindingUpdate {
                    node: NodeId::from_u64(*id),
                    dimension: CapacityDimension::Count,
                    binding: Some(ClaimBinding {
                        provider: ProviderId::ENFORCE,
                        scope: BindingScope::Exclusive,
                    }),
                })
                .collect(),
        })
        .unwrap();
}

'''
if anchor not in text:
    raise SystemExit("admit cluster anchor not found")
text = text.replace(anchor, helper + anchor, 1)
text = text.replace(
    '''        .unwrap();
    cluster
}

fn cpu_request''',
    '''        .unwrap();
    make_cpu_claimable(&mut cluster, &[2, 3]);
    cluster
}

fn cpu_request''',
    1,
)
text = text.replace(
    '''        .unwrap();
    cluster
}

#[test]
fn hard_request_exclusion_selects_a_compatible_machine''',
    '''        .unwrap();
    make_cpu_claimable(&mut cluster, &[11, 12, 21]);
    cluster
}

#[test]
fn hard_request_exclusion_selects_a_compatible_machine''',
    1,
)
path.write_text(text)
