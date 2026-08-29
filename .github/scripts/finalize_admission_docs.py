from pathlib import Path

path = Path("crates/kernel/src/types.rs")
text = path.read_text()
old = "so fair-share admission can attribute consumption and break ties."
new = "so admission can enforce per-owner budgets and break deterministic ties."
if old not in text:
    raise SystemExit(f"expected admission comment in {path}")
path.write_text(text.replace(old, new, 1))
