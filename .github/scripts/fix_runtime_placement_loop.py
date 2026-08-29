from pathlib import Path

path = Path("crates/node/src/service.rs")
text = path.read_text()
old = "            for machine in &machines {\n"
new = "            for machine in machines {\n"
if old not in text:
    raise SystemExit("placement machine loop not found")
path.write_text(text.replace(old, new, 1))
