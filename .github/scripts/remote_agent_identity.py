from pathlib import Path
import re


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"expected snippet not found in {path}: {old[:80]!r}")
    p.write_text(text.replace(old, new, count))

# LeaseAgent owns the identity it advertises in controller-initiated mode.
replace(
    "crates/node/src/agent.rs",
    "    process: ProcessRuntime,\n    containers: crate::container::ContainerRuntime,\n",
    "    process: ProcessRuntime,\n    containers: crate::container::ContainerRuntime,\n    /// Stable identity advertised to controllers that connect to this agent.\n    instance_id: String,\n    /// Optional display-name override from the agent CLI.\n    name: Option<String>,\n",
)
replace(
    "crates/node/src/agent.rs",
    "            containers: crate::container::ContainerRuntime::new(\n                std::env::var(\"ARCHON_CONTAINER_ENGINE\").unwrap_or_else(|_| \"docker\".to_string()),\n            ),\n            grace: BTreeMap::new(),\n",
    "            containers: crate::container::ContainerRuntime::new(\n                std::env::var(\"ARCHON_CONTAINER_ENGINE\").unwrap_or_else(|_| \"docker\".to_string()),\n            ),\n            instance_id: String::new(),\n            name: None,\n            grace: BTreeMap::new(),\n",
)
replace(
    "crates/node/src/agent.rs",
    "    pub fn handle(&mut self, request: AgentRequest) -> AgentResponse {\n",
    "    /// Set the stable registration identity used by controller-initiated\n    /// connections. Local in-process agents may leave it empty because they\n    /// register directly from `MachineDescription`.\n    pub fn with_identity(mut self, instance_id: String, name: Option<String>) -> Self {\n        self.instance_id = instance_id;\n        self.name = name;\n        self\n    }\n\n    pub fn handle(&mut self, request: AgentRequest) -> AgentResponse {\n",
)
replace(
    "crates/node/src/agent.rs",
    "        AgentResponse::Welcome {\n            name: description.name,\n            cpus: description.cpus,\n",
    "        AgentResponse::Welcome {\n            instance_id: self.instance_id.clone(),\n            name: self.name.clone().unwrap_or(description.name),\n            cpus: description.cpus,\n",
)

# Controller-initiated registration consumes the identity from Welcome and
# refuses legacy/misconfigured agents that would otherwise all alias "".
replace(
    "crates/node/src/service.rs",
    "        let description = Self::hello(&mut executor)?;\n        self.register_agent(description, Box::new(executor))\n",
    "        let description = Self::hello(&mut executor)?;\n        if description.instance_id.is_empty() {\n            return Err(Error::Refused {\n                explanation: \"remote agent returned an empty stable instance id\".into(),\n            });\n        }\n        self.register_agent(description, Box::new(executor))\n",
)
replace(
    "crates/node/src/service.rs",
    "            AgentResponse::Welcome {\n                name,\n                cpus,\n",
    "            AgentResponse::Welcome {\n                instance_id,\n                name,\n                cpus,\n",
)
replace(
    "crates/node/src/service.rs",
    "            } => Ok(crate::discover::MachineDescription {\n                instance_id: String::new(),\n                name,\n",
    "            } => Ok(crate::discover::MachineDescription {\n                instance_id,\n                name,\n",
)

# Listening mode previously parsed --id/--name but discarded both. Reuse the
# same persisted identity mechanism as dial-in mode and advertise it on Hello.
replace(
    "crates/cli/src/main.rs",
    "    let Some(listen) = listen else {\n        usage();\n    };\n    let token = load_token(&token_file);\n",
    "    let Some(listen) = listen else {\n        usage();\n    };\n    let instance_id = load_instance_id(id);\n    let token = load_token(&token_file);\n",
)
replace(
    "crates/cli/src/main.rs",
    "        let runtime = build_runtime(&cgroup_root);\n        let mut agent = archon_node::agent::LeaseAgent::new(runtime);\n",
    "        let runtime = build_runtime(&cgroup_root);\n        let mut agent = archon_node::agent::LeaseAgent::new(runtime)\n            .with_identity(instance_id.clone(), name.clone());\n",
)
replace(
    "crates/cli/src/main.rs",
    "                let runtime = build_runtime(&cgroup_root);\n                let mut lease_agent = archon_node::agent::LeaseAgent::new(runtime);\n",
    "                let runtime = build_runtime(&cgroup_root);\n                let mut lease_agent = archon_node::agent::LeaseAgent::new(runtime)\n                    .with_identity(instance_id.clone(), name.clone());\n",
)

# Remote protocol regression fixtures use explicit stable identities.
replace(
    "crates/node/tests/remote.rs",
    "fn spawn_agent() -> String {\n",
    "fn spawn_agent(instance_id: &'static str) -> String {\n",
)
replace(
    "crates/node/tests/remote.rs",
    "            let mut agent = LeaseAgent::new(ProcessRuntime::new());\n",
    "            let mut agent = LeaseAgent::new(ProcessRuntime::new())\n                .with_identity(instance_id.to_string(), None);\n",
)
replace(
    "crates/node/tests/remote.rs",
    "    let addr = spawn_agent();\n",
    "    let addr = spawn_agent(\"remote-a\");\n",
)

p = Path("crates/node/tests/remote.rs")
text = p.read_text()
marker = "\n#[test]\nfn stale_sessions_are_rejected_by_the_agent() {"
if marker not in text:
    raise SystemExit("remote test insertion marker missing")
new_test = '''\n#[test]\nfn controller_preserves_distinct_listening_agent_identities() {\n    let first = spawn_agent("remote-a");\n    let second = spawn_agent("remote-b");\n    let mut service = NodeService::new();\n    let first_machine = service.register_remote(&first).expect("register first agent");\n    let second_machine = service\n        .register_remote(&second)\n        .expect("register second agent");\n    assert_ne!(first_machine, second_machine);\n    assert_eq!(\n        service\n            .cluster\n            .graph\n            .node(first_machine)\n            .and_then(|node| node.attrs.get("agent_id"))\n            .map(String::as_str),\n        Some("remote-a")\n    );\n    assert_eq!(\n        service\n            .cluster\n            .graph\n            .node(second_machine)\n            .and_then(|node| node.attrs.get("agent_id"))\n            .map(String::as_str),\n        Some("remote-b")\n    );\n}\n\n#[test]\nfn controller_refuses_a_listening_agent_without_stable_identity() {\n    let addr = spawn_agent("");\n    let mut service = NodeService::new();\n    let err = service\n        .register_remote(&addr)\n        .expect_err("empty identity must fail closed");\n    assert!(err.to_string().contains("empty stable instance id"));\n}\n'''
p.write_text(text.replace(marker, new_test + marker, 1))

# Synthetic listening-agent fixtures predate the Welcome identity field. If a
# fixture participates in controller registration it must model a stable peer,
# so give it one deterministic test identity.
welcome = re.compile(
    r"(?m)^(?P<indent>\s*)let welcome = (?P<ty>(?:archon_node::protocol::)?AgentResponse)::Welcome \{\n"
)
for path in Path("crates").rglob("*.rs"):
    text = path.read_text()
    updated = welcome.sub(
        lambda match: match.group(0)
        + match.group("indent")
        + "    instance_id: \"test-agent\".into(),\n",
        text,
    )
    if updated != text:
        path.write_text(updated)

replace(
    "crates/node/tests/recovery.rs",
    "                        AgentRequest::Hello => AgentResponse::Welcome {\n                            name: \"fake\".into(),\n",
    "                        AgentRequest::Hello => AgentResponse::Welcome {\n                            instance_id: \"recovery-agent\".into(),\n                            name: \"fake\".into(),\n",
)
