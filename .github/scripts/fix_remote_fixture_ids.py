from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"expected snippet not found in {path}: {old!r}")
    p.write_text(text.replace(old, new, 1))

replace(
    "crates/node/tests/health_ops.rs",
    '                instance_id: "test-agent".into(),\n',
    "                instance_id: instance.to_string(),\n",
)
replace(
    "crates/node/tests/hetero.rs",
    '                instance_id: "test-agent".into(),\n',
    "                instance_id: instance.to_string(),\n",
)
replace(
    "crates/node/tests/hetero.rs",
    "    let _ = instance;\n    addr\n",
    "    addr\n",
)
replace(
    "crates/node/tests/multi.rs",
    "fn spawn_named_agent(_instance_id: &'static str, name: &'static str) -> String {\n",
    "fn spawn_named_agent(instance_id: &'static str, name: &'static str) -> String {\n",
)
replace(
    "crates/node/tests/multi.rs",
    '                instance_id: "test-agent".into(),\n',
    "                instance_id: instance_id.to_string(),\n",
)
