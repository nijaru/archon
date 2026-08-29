from pathlib import Path

path = Path("crates/kernel/src/types.rs")
text = path.read_text()
old = "so fair-share admission can attribute consumption and break ties."
new = "so admission can enforce per-owner budgets and break deterministic ties."
if old not in text:
    raise SystemExit(f"expected admission comment in {path}")
path.write_text(text.replace(old, new, 1))

path = Path("crates/sim/src/world.rs")
text = path.read_text()
old = "/// Admit the head of the queue under an optional per-owner fair-share\n    /// ceiling. An empty ceiling disables the budget."
new = "/// Admit the head of the queue with ordinary deterministic ordering.\n    /// Resource ceilings are exposed separately by `admit_next_with_ceiling`."
if old not in text:
    raise SystemExit(f"expected simulator admission comment in {path}")
path.write_text(text.replace(old, new, 1))
