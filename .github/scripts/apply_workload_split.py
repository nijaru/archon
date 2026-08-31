from __future__ import annotations

from pathlib import Path
import re

WORKLOAD_FIELDS = {"command", "image", "storage", "ports", "keep_alive", "grace_secs"}
REQUEST_FIELDS = WORKLOAD_FIELDS | {
    "id", "class", "needs", "topology", "preferences", "data", "lifetime", "priority", "machine_local"
}

WORKLOAD_RS = r'''//! Workload desired-state and execution intent above the resource kernel.
//!
//! `archon_kernel::Request` describes schedulable resource intent. Execution
//! payload and restart policy live here instead of participating in resource
//! authority, placement, or the kernel command log.

use std::ops::Deref;

use archon_kernel::Request;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageMount {
    pub host_path: String,
    pub mount_path: String,
}

/// A container port published to the host; None lets the host choose.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortPublish {
    pub container_port: u16,
    pub host_port: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionSpec {
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub storage: Vec<StorageMount>,
    #[serde(default)]
    pub ports: Vec<PortPublish>,
    #[serde(default)]
    pub grace_secs: u32,
}

impl ExecutionSpec {
    pub fn has_program(&self) -> bool {
        !self.command.is_empty() || self.image.is_some()
    }
}

/// One workload submission. The flattened representation intentionally matches
/// the legacy combined Request JSON shape, so old controller snapshots remain
/// readable while the kernel Request itself no longer carries execution data.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkloadSpec {
    #[serde(flatten)]
    pub resources: Request,
    #[serde(flatten)]
    pub execution: ExecutionSpec,
    #[serde(default)]
    pub keep_alive: bool,
}

impl WorkloadSpec {
    pub fn resource_only(resources: Request) -> Self {
        Self {
            resources,
            execution: ExecutionSpec::default(),
            keep_alive: false,
        }
    }
}

impl From<Request> for WorkloadSpec {
    fn from(resources: Request) -> Self {
        Self::resource_only(resources)
    }
}

impl Deref for WorkloadSpec {
    type Target = Request;

    fn deref(&self) -> &Self::Target {
        &self.resources
    }
}

impl AsRef<Request> for WorkloadSpec {
    fn as_ref(&self) -> &Request {
        &self.resources
    }
}
'''


def field_match(part: str):
    stripped = part.strip()
    if stripped in WORKLOAD_FIELDS:
        return stripped, stripped
    match = re.match(r"\s*([A-Za-z_][A-Za-z0-9_]*)\s*:", part, re.S)
    if not match:
        return None
    return match.group(1), part[match.end():].strip()


def scan_matching(text: str, open_pos: int) -> int:
    depth = 0
    quote = False
    line_comment = False
    block_comment = 0
    i = open_pos
    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""
        if line_comment:
            if ch == "\n": line_comment = False
            i += 1; continue
        if block_comment:
            if ch == "/" and nxt == "*": block_comment += 1; i += 2; continue
            if ch == "*" and nxt == "/": block_comment -= 1; i += 2; continue
            i += 1; continue
        if quote:
            if ch == "\\": i += 2; continue
            if ch == '"': quote = False
            i += 1; continue
        if ch == "/" and nxt == "/": line_comment = True; i += 2; continue
        if ch == "/" and nxt == "*": block_comment = 1; i += 2; continue
        if ch == '"': quote = True; i += 1; continue
        if ch == "{": depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0: return i
        i += 1
    raise RuntimeError("unmatched brace")


def split_fields(body: str) -> list[str]:
    parts: list[str] = []
    start = 0
    stack: list[str] = []
    angle_depth = 0
    quote = False
    line_comment = False
    block_comment = 0
    pairs = {")": "(", "]": "[", "}": "{"}
    i = 0
    while i < len(body):
        ch = body[i]
        nxt = body[i + 1] if i + 1 < len(body) else ""
        if line_comment:
            if ch == "\n": line_comment = False
            i += 1; continue
        if block_comment:
            if ch == "/" and nxt == "*": block_comment += 1; i += 2; continue
            if ch == "*" and nxt == "/": block_comment -= 1; i += 2; continue
            i += 1; continue
        if quote:
            if ch == "\\": i += 2; continue
            if ch == '"': quote = False
            i += 1; continue
        if ch == "/" and nxt == "/": line_comment = True; i += 2; continue
        if ch == "/" and nxt == "*": block_comment = 1; i += 2; continue
        if ch == '"': quote = True; i += 1; continue
        if ch in "([{": stack.append(ch)
        elif ch in ")]}":
            if stack and stack[-1] == pairs[ch]: stack.pop()
        elif ch == "<" and (angle_depth > 0 or body[max(0, i - 2):i] == "::"):
            angle_depth += 1
        elif ch == ">" and angle_depth > 0: angle_depth -= 1
        elif ch == "," and not stack and angle_depth == 0:
            parts.append(body[start:i]); start = i + 1
        i += 1
    if body[start:].strip(): parts.append(body[start:])
    return parts


def normalized(expr: str) -> str:
    return re.sub(r"\s+", "", expr)


def payload_is_meaningful(fields: dict[str, str]) -> bool:
    defaults = {
        "command": {"vec![]", "Vec::new()", "Default::default()"},
        "image": {"None"},
        "storage": {"vec![]", "Vec::new()", "Default::default()"},
        "ports": {"vec![]", "Vec::new()", "Default::default()"},
        "keep_alive": {"false"},
        "grace_secs": {"0"},
    }
    for name, allowed in defaults.items():
        expr = normalized(fields.get(name, next(iter(allowed))))
        if expr not in {normalized(value) for value in allowed}: return True
    return False


def transform_request_literals(path: Path, can_wrap: bool) -> None:
    text = path.read_text()
    out: list[str] = []
    cursor = 0
    pattern = re.compile(r"\bRequest\s*\{")
    while True:
        match = pattern.search(text, cursor)
        if not match:
            out.append(text[cursor:]); break
        open_pos = text.find("{", match.start())
        close_pos = scan_matching(text, open_pos)
        body = text[open_pos + 1:close_pos]
        parts = split_fields(body)
        parsed = [field_match(part) for part in parts]
        fields = {item[0]: item[1] for item in parsed if item is not None}

        # A return type (`-> Request {`) has no Request fields at the top level.
        # Real literals may be full or struct-update forms, so any recognized
        # Request field is sufficient to distinguish them.
        if not REQUEST_FIELDS.intersection(fields):
            out.append(text[cursor:match.end()]); cursor = match.end(); continue

        kept = [part for part, item in zip(parts, parsed) if item is None or item[0] not in WORKLOAD_FIELDS]
        pure = "Request {" + ",".join(kept) + "}"
        if can_wrap and payload_is_meaningful(fields):
            def expr(name: str, default: str) -> str: return fields.get(name, default)
            replacement = (
                "archon_node::workload::WorkloadSpec {"
                f"resources: {pure},"
                "execution: archon_node::workload::ExecutionSpec {"
                f"command: {expr('command', 'Vec::new())},"
                f"image: {expr('image', 'None')},"
                f"storage: {expr('storage', 'Vec::new())},"
                f"ports: {expr('ports', 'Vec::new())},"
                f"grace_secs: {expr('grace_secs', '0')},"
                "},"
                f"keep_alive: {expr('keep_alive', 'false')},"
                "}"
            )
        else:
            replacement = pure
        out.append(text[cursor:match.start()]); out.append(replacement); cursor = close_pos + 1
    path.write_text("".join(out))


def update_wrapped_return_types(path: Path) -> None:
    text = path.read_text()
    replacements: list[tuple[int, int]] = []
    for match in re.finditer(r"->\s*Request\b", text):
        open_pos = text.find("{", match.end())
        if open_pos < 0: continue
        try: close_pos = scan_matching(text, open_pos)
        except RuntimeError: continue
        if "archon_node::workload::WorkloadSpec {" in text[open_pos + 1:close_pos]:
            replacements.append((match.start(), match.end()))
    for start, end in reversed(replacements):
        text = text[:start] + "-> archon_node::workload::WorkloadSpec" + text[end:]
    path.write_text(text)


def remove_kernel_execution_fields() -> None:
    path = Path("crates/kernel/src/types.rs")
    text = path.read_text()
    for field in WORKLOAD_FIELDS:
        text = re.sub(rf"(?m)(?:^    ///.*\n)*^    pub {field}: [^\n]+\n", "", text)
    text = re.sub(
        r"\n#\[derive\(Clone, Debug, PartialEq, Eq\)\]\n#\[cfg_attr\(feature = \"serde\", derive\(serde::Serialize, serde::Deserialize\)\)\]\npub struct StorageMount \{.*?\n\}\n\n/// A container port published to the host; None lets the host choose\.\n#\[cfg_attr\(feature = \"serde\", derive\(serde::Serialize, serde::Deserialize\)\)\]\n#\[derive\(Clone, Debug, PartialEq, Eq\)\]\npub struct PortPublish \{.*?\n\}\n",
        "\n", text, count=1, flags=re.S,
    )
    path.write_text(text)
    path = Path("crates/kernel/src/lib.rs")
    text = path.read_text()
    for token in ("PortPublish", "StorageMount"):
        text = re.sub(rf"\b{token},\s*", "", text)
        text = re.sub(rf",\s*\b{token}\b", "", text)
    path.write_text(text)


def add_workload_module() -> None:
    Path("crates/node/src/workload.rs").write_text(WORKLOAD_RS)
    path = Path("crates/node/src/lib.rs")
    text = path.read_text()
    if "pub mod workload;" not in text: text += "pub mod workload;\n"
    path.write_text(text)
    path = Path("crates/node/src/protocol.rs")
    path.write_text(path.read_text().replace(
        "use archon_kernel::{BindingScope, PortPublish, StorageMount};",
        "use archon_kernel::BindingScope;\nuse crate::workload::{PortPublish, StorageMount};",
    ))
    path = Path("crates/node/src/container.rs")
    path.write_text(path.read_text().replace(
        "use archon_kernel::{LeaseId, PortPublish, StorageMount};",
        "use archon_kernel::LeaseId;\n\nuse crate::workload::{PortPublish, StorageMount};",
    ))


def migrate_request_literals() -> None:
    for path in Path("crates").rglob("*.rs"):
        if path.as_posix() == "crates/kernel/src/types.rs": continue
        can_wrap = path.as_posix().startswith(("crates/node/", "crates/control/", "crates/cli/"))
        transform_request_literals(path, can_wrap)
        if can_wrap: update_wrapped_return_types(path)
        text = path.read_text()
        text = text.replace("archon_kernel::StorageMount", "archon_node::workload::StorageMount")
        text = text.replace("archon_kernel::PortPublish", "archon_node::workload::PortPublish")
        path.write_text(text)

    for path in Path("crates/node/tests").glob("*.rs"):
        text = path.read_text()
        if "StorageMount" in text:
            text = re.sub(r"\bStorageMount,\s*", "", text)
            text = re.sub(r",\s*StorageMount\b", "", text)
            text = re.sub(r"(?<!::)\bStorageMount\b", "archon_node::workload::StorageMount", text)
        if "PortPublish" in text:
            text = re.sub(r"\bPortPublish,\s*", "", text)
            text = re.sub(r",\s*PortPublish\b", "", text)
            text = re.sub(r"(?<!::)\bPortPublish\b", "archon_node::workload::PortPublish", text)
        text = re.sub(r"\b(req|request)\.grace_secs\s*=", r"\1.execution.grace_secs =", text)
        path.write_text(text)


def patch_service() -> None:
    path = Path("crates/node/src/service.rs")
    text = path.read_text()
    text = text.replace(" ProviderId, Queued, Request,\n", " ProviderId, Queued,\n")
    text = text.replace("use crate::runtime::ProcessRuntime;\n", "use crate::runtime::ProcessRuntime;\nuse crate::workload::WorkloadSpec;\n")
    text = text.replace(
        "    /// Workload payload per queued request, kept outside the kernel log:\n    /// resource decisions never need it, only execution does.\n    commands: BTreeMap<RequestId, Vec<String>>,\n    /// Command per active lease, recorded when its request is admitted.\n    lease_commands: BTreeMap<LeaseId, Vec<String>>,\n    /// Container image per active lease; None runs a bare process.\n    lease_images: BTreeMap<LeaseId, Option<String>>,\n    /// Original request per admitted lease, for keep-alive restarts.\n    requests: BTreeMap<LeaseId, (Request, OwnerId)>,",
        "    /// Desired-state/execution intent for queued requests. Resource ordering\n    /// still uses only `queue`; this map never enters kernel authority.\n    queued_workloads: BTreeMap<RequestId, WorkloadSpec>,\n    /// Workload intent per admitted root Lease, retained for execution and restart.\n    requests: BTreeMap<LeaseId, (WorkloadSpec, OwnerId)>,",
    )
    text = text.replace(
        "    pub lease_commands: BTreeMap<LeaseId, Vec<String>>,\n    pub lease_images: BTreeMap<LeaseId, Option<String>>,\n    pub requests: BTreeMap<LeaseId, (Request, OwnerId)>,",
        "    pub requests: BTreeMap<LeaseId, (WorkloadSpec, OwnerId)>,",
    )
    text = text.replace(
        "            commands: BTreeMap::new(),\n            lease_commands: BTreeMap::new(),\n            lease_images: BTreeMap::new(),\n            requests: BTreeMap::new(),",
        "            queued_workloads: BTreeMap::new(),\n            requests: BTreeMap::new(),",
    )
    text = text.replace(
        "    /// Submit a workload: queued for admission; its command runs when the\n    /// lease activates.\n    pub fn submit(&mut self, request: Request, owner: OwnerId) {\n        self.commands.remove(&request.id);\n        self.next_request_id = self.next_request_id.max(request.id.as_u64() + 1);\n        self.queue.push(Queued {\n            request,\n            owner,\n            submitted_at: self.cluster.now,\n        });\n    }",
        "    /// Submit workload intent. Only the nested resource Request enters the\n    /// scheduler; execution and desired-state policy stay controller-local.\n    pub fn submit(&mut self, workload: impl Into<WorkloadSpec>, owner: OwnerId) {\n        let workload = workload.into();\n        let request = workload.resources.clone();\n        self.next_request_id = self.next_request_id.max(request.id.as_u64() + 1);\n        self.queued_workloads.insert(request.id, workload);\n        self.queue.push(Queued {\n            request,\n            owner,\n            submitted_at: self.cluster.now,\n        });\n    }",
    )
    text = text.replace(
        "            let request = &queued.request;\n            if request.command.is_empty() && request.image.is_none() {\n                continue;\n            }",
        "            let request = &queued.request;\n            let Some(workload) = self.queued_workloads.get(&request.id) else {\n                continue;\n            };\n            if !workload.execution.has_program() {\n                continue;\n            }",
    )
    text = text.replace(
        "            let container = request\n                .image\n                .as_deref()\n                .is_some_and(|image| !image.is_empty());",
        "            let container = workload\n                .execution\n                .image\n                .as_deref()\n                .is_some_and(|image| !image.is_empty());",
    )
    text = text.replace(
        "        self.commands.remove(&request_id);\n        let command = admission.request.command.clone();\n        self.lease_commands.insert(lease, command);\n        self.lease_images\n            .insert(lease, admission.request.image.clone());\n        self.requests\n            .insert(lease, (admission.request.clone(), admission.owner));",
        "        let workload = self\n            .queued_workloads\n            .remove(&request_id)\n            .unwrap_or_else(|| WorkloadSpec::resource_only(admission.request.clone()));\n        self.requests.insert(lease, (workload, admission.owner));",
    )
    text = text.replace("            lease_commands: self.lease_commands.clone(),\n            lease_images: self.lease_images.clone(),\n            requests: self.requests.clone(),", "            requests: self.requests.clone(),")
    text = text.replace("        self.lease_commands = state.lease_commands;\n        self.lease_images = state.lease_images;\n        self.requests = state.requests;", "        self.requests = state.requests;")
    text = text.replace("    pub fn take_restarts(&mut self) -> Vec<(Request, OwnerId)> {", "    pub fn take_restarts(&mut self) -> Vec<(WorkloadSpec, OwnerId)> {")
    text = text.replace(".get(&request.id)\n                .copied()\n                .unwrap_or(request.id);", ".get(&request.resources.id)\n                .copied()\n                .unwrap_or(request.resources.id);")
    text = text.replace("            let mut fresh = request.clone();\n            fresh.id = RequestId::from_u64(self.next_request_id);\n            self.next_request_id += 1;\n            self.restart_root.insert(fresh.id, root);", "            let mut fresh = request.clone();\n            fresh.resources.id = RequestId::from_u64(self.next_request_id);\n            self.next_request_id += 1;\n            self.restart_root.insert(fresh.resources.id, root);")
    text = text.replace("                    && self.lease_commands.contains_key(&lease.id)", "                    && self\n                        .requests\n                        .get(&lease.id)\n                        .is_some_and(|(workload, _)| workload.execution.has_program())")
    text = text.replace("        self.lease_commands.get(&lease).cloned()", "        self.requests\n            .get(&lease)\n            .map(|(workload, _)| workload.execution.command.clone())")
    text = text.replace("                command: self.lease_commands.get(&lease).cloned().unwrap_or_default(),", "                command: self\n                    .requests\n                    .get(&lease)\n                    .map(|(workload, _)| workload.execution.command.clone())\n                    .unwrap_or_default(),")
    old = """                image: self
                    .lease_images
                    .get(&lease)
                    .cloned()
                    .flatten()
                    .unwrap_or_default(),
                storage: self
                    .requests
                    .get(&lease)
                    .map(|(request, _)| request.storage.clone())
                    .unwrap_or_default(),
                ports: self
                    .requests
                    .get(&lease)
                    .map(|(request, _)| request.ports.clone())
                    .unwrap_or_default(),
                grace_secs: self
                    .requests
                    .get(&lease)
                    .map(|(request, _)| request.grace_secs)
                    .unwrap_or(0),"""
    new = """                image: self
                    .requests
                    .get(&lease)
                    .and_then(|(workload, _)| workload.execution.image.clone())
                    .unwrap_or_default(),
                storage: self
                    .requests
                    .get(&lease)
                    .map(|(workload, _)| workload.execution.storage.clone())
                    .unwrap_or_default(),
                ports: self
                    .requests
                    .get(&lease)
                    .map(|(workload, _)| workload.execution.ports.clone())
                    .unwrap_or_default(),
                grace_secs: self
                    .requests
                    .get(&lease)
                    .map(|(workload, _)| workload.execution.grace_secs)
                    .unwrap_or(0),"""
    text = text.replace(old, new)
    path.write_text(text)


def add_legacy_shape_test() -> None:
    Path("crates/node/tests/workload_spec.rs").write_text(r'''use archon_kernel::{CapacityDimension, Need, Request, RequestClass, RequestId, ResourceClass, qty};
use archon_node::workload::WorkloadSpec;

#[test]
fn legacy_flat_request_json_decodes_as_workload_spec() {
    let legacy = serde_json::json!({
        "id": 7,
        "class": "Service",
        "needs": [{"kind":"cpu","quantity":{"count":1},"filters":[]}],
        "topology": [], "preferences": [], "data": [],
        "command": ["sleep", "30"], "image": "busybox:latest",
        "storage": [{"host_path":"/tmp/input","mount_path":"/input"}],
        "ports": [{"container_port":8080,"host_port":18080}],
        "lifetime": 60, "priority": 4, "keep_alive": true,
        "machine_local": true, "grace_secs": 5
    });
    let workload: WorkloadSpec = serde_json::from_value(legacy).expect("legacy workload shape");
    assert_eq!(workload.resources.id, RequestId::from_u64(7));
    assert_eq!(workload.resources.class, RequestClass::Service);
    assert_eq!(workload.resources.needs, vec![Need { kind: ResourceClass::Cpu, quantity: qty(CapacityDimension::Count, 1), filters: Vec::new() }]);
    assert_eq!(workload.execution.command, vec!["sleep", "30"]);
    assert_eq!(workload.execution.image.as_deref(), Some("busybox:latest"));
    assert_eq!(workload.execution.storage.len(), 1);
    assert_eq!(workload.execution.ports.len(), 1);
    assert_eq!(workload.execution.grace_secs, 5);
    assert!(workload.keep_alive);
    let encoded = serde_json::to_value(&workload).expect("new workload shape");
    assert_eq!(encoded["id"], 7);
    assert_eq!(encoded["command"], serde_json::json!(["sleep", "30"]));
    assert!(encoded.get("resources").is_none());
}

#[test]
fn pure_resource_request_promotes_without_execution_intent() {
    let request = Request {
        id: RequestId::from_u64(8), class: RequestClass::Batch, needs: Vec::new(),
        topology: Vec::new(), preferences: Vec::new(), data: Vec::new(),
        lifetime: 10, priority: 1, machine_local: true,
    };
    let workload = WorkloadSpec::resource_only(request.clone());
    assert_eq!(workload.resources, request);
    assert!(!workload.execution.has_program());
    assert!(!workload.keep_alive);
}
''')


remove_kernel_execution_fields()
add_workload_module()
migrate_request_literals()
patch_service()
add_legacy_shape_test()
