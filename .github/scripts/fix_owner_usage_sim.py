from pathlib import Path

path = Path('crates/sim/tests/reserve.rs')
text = path.read_text()
old = 'let usage = archon_kernel::owner_usage(&world.cluster.graph, &world.cluster.leases);'
new = '''let open_bindings: std::collections::BTreeSet<_> = world
        .cluster
        .bindings
        .values()
        .filter(|binding| !binding.state.is_closed())
        .map(|binding| binding.lease)
        .collect();
    let usage = archon_kernel::owner_usage(
        &world.cluster.graph,
        &world.cluster.leases,
        &open_bindings,
    );'''
assert old in text
path.write_text(text.replace(old, new, 1))
