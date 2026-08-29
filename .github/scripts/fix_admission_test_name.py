from pathlib import Path

path = Path("crates/kernel/tests/harden.rs")
text = path.read_text()
old = "backfill_honors_the_owner_ceiling_ceiling"
new = "backfill_honors_the_owner_ceiling"
if old not in text:
    raise SystemExit(f"expected {old!r} in {path}")
path.write_text(text.replace(old, new, 1))
