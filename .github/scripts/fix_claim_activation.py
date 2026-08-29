from pathlib import Path

path = Path("crates/kernel/src/cluster.rs")
text = path.read_text()
old = '''        // A root lease must enforce every enforced claim through a prepared
        // Binding before it becomes Active; accounting-only children may
        // activate without Bindings.
        if parent.is_none() {
            for claim in &claims {
                let enforced = self
                    .graph
                    .node(claim.node)
                    .is_some_and(|node| node.kind.is_enforced());
                if !enforced {
                    continue;
                }
                let covered = self.bindings.values().any(|binding| {
                    binding.lease == id
                        && binding.node == claim.node
                        && binding.state == BindingState::Preparing
                        && binding.provider_handle.is_some()
                });
                if !covered {
                    return Err(Error::BindingsNotPrepared { lease: id });
                }
            }
        }
'''
new = '''        // Every root-lease Claim was proven claimable when authority was
        // reserved, so each one must have a prepared Binding before the Lease
        // becomes Active. Accounting-only child leases reuse their parent's
        // Bindings and therefore do not open a second endpoint authority.
        if parent.is_none() {
            for claim in &claims {
                let covered = self.bindings.values().any(|binding| {
                    binding.lease == id
                        && binding.node == claim.node
                        && binding.state == BindingState::Preparing
                        && binding.provider_handle.is_some()
                });
                if !covered {
                    return Err(Error::BindingsNotPrepared { lease: id });
                }
            }
        }
'''
if old not in text:
    raise SystemExit("root claim activation block not found")
text = text.replace(old, new, 1)
path.write_text(text)
