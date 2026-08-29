from pathlib import Path

path = Path("crates/node/tests/hetero.rs")
text = path.read_text()
old = '''    service
        .cluster
        .apply(archon_kernel::Command::ApplyGraph { nodes, edges })
        .unwrap();
'''
new = '''    let claim_bindings = archon_node::discover::claim_bindings(&nodes);
    service
        .cluster
        .apply(archon_kernel::Command::ApplyResourceFacts {
            nodes,
            edges,
            claim_bindings,
        })
        .unwrap();
'''
if old not in text:
    raise SystemExit("heterogeneous device ApplyGraph fixture not found")
text = text.replace(old, new, 1)
path.write_text(text)
