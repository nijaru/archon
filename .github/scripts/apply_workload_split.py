from __future__ import annotations

from pathlib import Path
import re

ROOT = Path('.')
WORKLOAD_FIELDS = {"class", "command", "image", "storage", "ports", "keep_alive", "grace_secs"}

WORKLOAD_RS = r'''//! Workload desired-state and execution intent above the resource kernel.
//!
//! `archon_kernel::Request` describes only schedulable resource intent. This
//! module carries the execution and supervision fields that do not participate
//! in resource authority or placement.

use std::ops::Deref;

use archon_kernel::Request;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkloadClass {
    Service,
    #[default]
    Batch,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// One workload submission. Flattening deliberately preserves the legacy
/// serialized Request shape: snapshots written before this split decode into
/// this type while new kernel Requests remain free of execution payload.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkloadSpec {
    #[serde(flatten)]
    pub resources: Request,
    #[serde(default)]
    pub class: WorkloadClass,
    #[serde(flatten)]
    pub execution: ExecutionSpec,
    #[serde(default)]
    pub keep_alive: bool,
}

impl WorkloadSpec {
    pub fn resource_only(resources: Request) -> Self {
        Self {
            resources,
            class: WorkloadClass::Batch,
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


def write(path: str, text: str) -> None:
    Path(path).write_text(text)


def remove_enum_and_fields() -> None:
    p = Path('crates/kernel/src/types.rs')
    s = p.read_text()
    s = re.sub(
        r'\n#\[derive\(Clone, Copy, PartialEq, Eq, Debug\)\]\n#\[cfg_attr\(feature = "serde", derive\(serde::Serialize, serde::Deserialize\)\)\]\npub enum RequestClass \{\n    Service,\n    Batch,\n\}\n',
        '\n', s, count=1,
    )
    for field in ('class', 'command', 'image', 'storage', 'ports', 'keep_alive', 'grace_secs'):
        # Remove doc comments immediately attached to workload-only fields too.
        s = re.sub(
            rf'(?m)(?:^    ///.*\n)*^    pub {field}: [^\n]+\n',
            '', s,
        )
    s = re.sub(
        r'\n#\[derive\(Clone, Debug, PartialEq, Eq\)\]\n#\[cfg_attr\(feature = "serde", derive\(serde::Serialize, serde::Deserialize\)\)\]\npub struct StorageMount \{.*?\n\}\n\n/// A container port published to the host; None lets the host choose\.\n#\[cfg_attr\(feature = "serde", derive\(serde::Serialize, serde::Deserialize\)\)\]\n#\[derive\(Clone, Debug, PartialEq, Eq\)\]\npub struct PortPublish \{.*?\n\}\n',
        '\n', s, count=1, flags=re.S,
    )
    p.write_text(s)

    p = Path('crates/kernel/src/lib.rs')
    s = p.read_text()
    for token in ('PortPublish', 'RequestClass', 'StorageMount'):
        s = re.sub(rf'\b{token},\s*', '', s)
        s = re.sub(rf',\s*\b{token}\b', '', s)
    p.write_text(s)


def scan_matching(text: str, open_pos: int) -> int:
    depth = 0
    i = open_pos
    quote = None
    line_comment = False
    block_comment = 0
    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ''
        if line_comment:
            if ch == '\n':
                line_comment = False
            i += 1
            continue
        if block_comment:
            if ch == '/' and nxt == '*':
                block_comment += 1; i += 2; continue
            if ch == '*' and nxt == '/':
                block_comment -= 1; i += 2; continue
            i += 1; continue
        if quote:
            if ch == '\\':
                i += 2; continue
            if ch == quote:
                quote = None
            i += 1; continue
        if ch == '/' and nxt == '/':
            line_comment = True; i += 2; continue
        if ch == '/' and nxt == '*':
            block_comment = 1; i += 2; continue
        if ch in ('"', "'"):
            quote = ch; i += 1; continue
        if ch == '{':
            depth += 1
        elif ch == '}':
            depth -= 1
            if depth == 0:
                return i
        i += 1
    raise RuntimeError('unmatched Request literal')


def split_fields(body: str) -> list[str]:
    parts = []
    start = 0
    stack: list[str] = []
    quote = None
    line_comment = False
    block_comment = 0
    pairs = {')': '(', ']': '[', '}': '{'}
    i = 0
    while i < len(body):
        ch = body[i]
        nxt = body[i + 1] if i + 1 < len(body) else ''
        if line_comment:
            if ch == '\n': line_comment = False
            i += 1; continue
        if block_comment:
            if ch == '/' and nxt == '*': block_comment += 1; i += 2; continue
            if ch == '*' and nxt == '/': block_comment -= 1; i += 2; continue
            i += 1; continue
        if quote:
            if ch == '\\': i += 2; continue
            if ch == quote: quote = None
            i += 1; continue
        if ch == '/' and nxt == '/': line_comment = True; i += 2; continue
        if ch == '/' and nxt == '*': block_comment = 1; i += 2; continue
        if ch in ('"', "'"): quote = ch; i += 1; continue
        if ch in '([{': stack.append(ch)
        elif ch in ')]}':
            if stack and stack[-1] == pairs[ch]: stack.pop()
        elif ch == ',' and not stack:
            parts.append(body[start:i])
            start = i + 1
        i += 1
    if body[start:].strip(): parts.append(body[start:])
    return parts


def field_name(part: str) -> str | None:
    m = re.match(r'\s*([A-Za-z_][A-Za-z0-9_]*)\s*:', part, re.S)
    return m.group(1) if m else None


def field_expr(part: str) -> str:
    return part.split(':', 1)[1].strip()


def transform_request_literals(path: Path, wrap: bool) -> None:
    text = path.read_text()
    out = []
    cursor = 0
    pattern = re.compile(r'\bRequest\s*\{')
    while True:
        m = pattern.search(text, cursor)
        if not m:
            out.append(text[cursor:]); break
        open_pos = text.find('{', m.start())
        close_pos = scan_matching(text, open_pos)
        body = text[open_pos + 1:close_pos]
        parts = split_fields(body)
        fields = {field_name(p): p for p in parts if field_name(p)}
        kept = [p for p in parts if field_name(p) not in WORKLOAD_FIELDS]
        pure = 'Request {' + ','.join(kept) + '}'
        if wrap:
            cls = field_expr(fields['class']) if 'class' in fields else 'RequestClass::Batch'
            cls = cls.replace('RequestClass::', 'archon_node::workload::WorkloadClass::')
            def expr(name: str, default: str) -> str:
                return field_expr(fields[name]) if name in fields else default
            replacement = (
                'archon_node::workload::WorkloadSpec {'
                f'resources: {pure},'
                f'class: {cls},'
                'execution: archon_node::workload::ExecutionSpec {'
                f'command: {expr("command", "Vec::new()")},'
                f'image: {expr("image", "None")},'
                f'storage: {expr("storage", "Vec::new()")},'
                f'ports: {expr("ports", "Vec::new()")},'
                f'grace_secs: {expr("grace_secs", "0")},'
                '},'
                f'keep_alive: {expr("keep_alive", "false")},'
                '}'
            )
        else:
            replacement = pure
        out.append(text[cursor:m.start()])
        out.append(replacement)
        cursor = close_pos + 1
    path.write_text(''.join(out))


def migrate_literals() -> None:
    for p in Path('crates').rglob('*.rs'):
        if p.as_posix() in {'crates/kernel/src/types.rs'}:
            continue
        wrap = p.as_posix().startswith(('crates/node/tests/', 'crates/control/', 'crates/cli/'))
        transform_request_literals(p, wrap)
        s = p.read_text()
        # Kernel RequestClass no longer exists. Pure-resource fixtures simply
        # drop the now-irrelevant class import/use.
        if not wrap:
            s = re.sub(r'\bRequestClass,\s*', '', s)
            s = re.sub(r',\s*RequestClass\b', '', s)
            s = re.sub(r'\bRequestClass\s*,', '', s)
        # Workload attachment types now live with execution intent.
        s = s.replace('archon_kernel::StorageMount', 'archon_node::workload::StorageMount')
        s = s.replace('archon_kernel::PortPublish', 'archon_node::workload::PortPublish')
        if wrap:
            # Helpers that used to return the combined Request now return the
            # workload wrapper; submit accepts either wrapper or pure Request.
            s = re.sub(r'->\s*Request\s*\{', '-> archon_node::workload::WorkloadSpec {', s)
        p.write_text(s)


def patch_node_modules() -> None:
    p = Path('crates/node/src/lib.rs')
    s = p.read_text()
    if 'pub mod workload;' not in s:
        s += 'pub mod workload;\n'
    p.write_text(s)
    write('crates/node/src/workload.rs', WORKLOAD_RS)

    p = Path('crates/node/src/protocol.rs')
    s = p.read_text().replace(
        'use archon_kernel::{BindingScope, PortPublish, StorageMount};',
        'use archon_kernel::BindingScope;\nuse crate::workload::{PortPublish, StorageMount};'
    )
    p.write_text(s)

    p = Path('crates/node/src/container.rs')
    s = p.read_text()
    s = s.replace('use archon_kernel::{PortPublish, StorageMount};', 'use crate::workload::{PortPublish, StorageMount};')
    p.write_text(s)


def patch_service() -> None:
    p = Path('crates/node/src/service.rs')
    s = p.read_text()
    s = s.replace('use crate::runtime::ProcessRuntime;\n', 'use crate::runtime::ProcessRuntime;\nuse crate::workload::WorkloadSpec;\n')
    s = re.sub(
        r'    /// Workload payload per queued request, kept outside the kernel log:\n    /// resource decisions never need it, only execution does\.\n    commands: BTreeMap<RequestId, Vec<String>>,\n    /// Command per active lease, recorded when its request is admitted\.\n    lease_commands: BTreeMap<LeaseId, Vec<String>>,\n    /// Container image per active lease; None runs a bare process\.\n    lease_images: BTreeMap<LeaseId, Option<String>>,\n    /// Original request per admitted lease, for keep-alive restarts\.\n    requests: BTreeMap<LeaseId, \(Request, OwnerId\)>,',
        '    /// Desired-state/execution intent for queued requests. Resource ordering\n    /// still uses only `queue`; this map never enters kernel authority.\n    queued_workloads: BTreeMap<RequestId, WorkloadSpec>,\n    /// Workload intent per admitted root Lease, retained for execution and restart.\n    requests: BTreeMap<LeaseId, (WorkloadSpec, OwnerId)>,', s,
    )
    s = s.replace(
        '    pub lease_commands: BTreeMap<LeaseId, Vec<String>>,\n    pub lease_images: BTreeMap<LeaseId, Option<String>>,\n    pub requests: BTreeMap<LeaseId, (Request, OwnerId)>,',
        '    pub requests: BTreeMap<LeaseId, (WorkloadSpec, OwnerId)>,'
    )
    s = s.replace(
        '            commands: BTreeMap::new(),\n            lease_commands: BTreeMap::new(),\n            lease_images: BTreeMap::new(),\n            requests: BTreeMap::new(),',
        '            queued_workloads: BTreeMap::new(),\n            requests: BTreeMap::new(),'
    )
    # submit
    s = re.sub(
        r'    /// Submit a workload: queued for admission; its command runs when the\n    /// lease activates\.\n    pub fn submit\(&mut self, request: Request, owner: OwnerId\) \{\n        self\.commands\.remove\(&request\.id\);\n        self\.next_request_id = self\.next_request_id\.max\(request\.id\.as_u64\(\) \+ 1\);\n        self\.queue\.push\(Queued \{\n            request,\n            owner,\n            submitted_at: self\.cluster\.now,\n        \}\);\n    \}',
        '''    /// Submit workload intent. Only the nested resource Request enters the\n    /// scheduler; execution and desired-state policy stay controller-local.\n    pub fn submit(&mut self, workload: impl Into<WorkloadSpec>, owner: OwnerId) {\n        let workload = workload.into();\n        let request = workload.resources.clone();\n        self.next_request_id = self.next_request_id.max(request.id.as_u64() + 1);\n        self.queued_workloads.insert(request.id, workload);\n        self.queue.push(Queued {\n            request,\n            owner,\n            submitted_at: self.cluster.now,\n        });\n    }''', s,
    )
    # execution exclusions uses workload metadata
    s = s.replace(
        '            let request = &queued.request;\n            if request.command.is_empty() && request.image.is_none() {\n                continue;\n            }',
        '            let request = &queued.request;\n            let Some(workload) = self.queued_workloads.get(&request.id) else {\n                continue;\n            };\n            if !workload.execution.has_program() {\n                continue;\n            }'
    )
    s = s.replace(
        '            let container = request\n                .image\n                .as_deref()\n                .is_some_and(|image| !image.is_empty());',
        '            let container = workload\n                .execution\n                .image\n                .as_deref()\n                .is_some_and(|image| !image.is_empty());'
    )
    # admission promotion
    s = s.replace(
        '        self.commands.remove(&request_id);\n        let command = admission.request.command.clone();\n        self.lease_commands.insert(lease, command);\n        self.lease_images\n            .insert(lease, admission.request.image.clone());\n        self.requests\n            .insert(lease, (admission.request.clone(), admission.owner));',
        '        let workload = self\n            .queued_workloads\n            .remove(&request_id)\n            .unwrap_or_else(|| WorkloadSpec::resource_only(admission.request.clone()));\n        self.requests.insert(lease, (workload, admission.owner));'
    )
    # snapshots
    s = s.replace(
        '            lease_commands: self.lease_commands.clone(),\n            lease_images: self.lease_images.clone(),\n            requests: self.requests.clone(),',
        '            requests: self.requests.clone(),'
    )
    s = s.replace(
        '        self.lease_commands = state.lease_commands;\n        self.lease_images = state.lease_images;\n        self.requests = state.requests;',
        '        self.requests = state.requests;'
    )
    # active execution tracking and command reporting
    s = s.replace(
        '                    && self.lease_commands.contains_key(&lease.id)',
        '                    && self.requests.get(&lease.id).is_some_and(|(workload, _)| workload.execution.has_program())'
    )
    s = s.replace(
        '        self.lease_commands.get(&lease).cloned()',
        '        self.requests\n            .get(&lease)\n            .map(|(workload, _)| workload.execution.command.clone())'
    )
    # Activate payload block
    s = s.replace(
        '                command: self.lease_commands.get(&lease).cloned().unwrap_or_default(),',
        '                command: self.requests.get(&lease).map(|(workload, _)| workload.execution.command.clone()).unwrap_or_default(),'
    )
    s = re.sub(
        r'                image: self\n                    \.lease_images\n                    \.get\(&lease\)\n                    \.cloned\(\)\n                    \.flatten\(\)\n                    \.unwrap_or_default\(\),\n                storage: self\n                    \.requests\n                    \.get\(&lease\)\n                    \.map\(\|\(request, _\)\| request\.storage\.clone\(\)\)\n                    \.unwrap_or_default\(\),\n                ports: self\n                    \.requests\n                    \.get\(&lease\)\n                    \.map\(\|\(request, _\)\| request\.ports\.clone\(\)\)\n                    \.unwrap_or_default\(\),\n                grace_secs: self\n                    \.requests\n                    \.get\(&lease\)\n                    \.map\(\|\(request, _\)\| request\.grace_secs\)\n                    \.unwrap_or\(0\),',
        '''                image: self.requests.get(&lease)\n                    .and_then(|(workload, _)| workload.execution.image.clone())\n                    .unwrap_or_default(),\n                storage: self.requests.get(&lease)\n                    .map(|(workload, _)| workload.execution.storage.clone())\n                    .unwrap_or_default(),\n                ports: self.requests.get(&lease)\n                    .map(|(workload, _)| workload.execution.ports.clone())\n                    .unwrap_or_default(),\n                grace_secs: self.requests.get(&lease)\n                    .map(|(workload, _)| workload.execution.grace_secs)\n                    .unwrap_or(0),''', s,
    )
    # restart API now carries workload intent and mutates nested request id.
    s = s.replace('pub fn take_restarts(&mut self) -> Vec<(Request, OwnerId)> {', 'pub fn take_restarts(&mut self) -> Vec<(WorkloadSpec, OwnerId)> {')
    s = s.replace('            if !request.keep_alive {', '            if !request.keep_alive {')
    s = s.replace('                .get(&request.id)', '.get(&request.resources.id)')
    s = s.replace('                .unwrap_or(request.id);', '.unwrap_or(request.resources.id);')
    s = s.replace('            let mut retry = request.clone();\n            retry.id = RequestId::from_u64(self.next_request_id);', '            let mut retry = request.clone();\n            retry.resources.id = RequestId::from_u64(self.next_request_id);')
    s = s.replace('            self.restart_root.insert(retry.id, root);', '            self.restart_root.insert(retry.resources.id, root);')
    p.write_text(s)


def patch_control() -> None:
    p = Path('crates/control/src/server.rs')
    s = p.read_text()
    s = s.replace('RequestClass, ', '')
    s = s.replace('Request, RequestClass, ', 'Request, ')
    # maintain restart log uses Deref for id but be explicit.
    s = s.replace('request {}", request.id', 'request {}", request.resources.id')
    p.write_text(s)


def cleanup_imports() -> None:
    for p in Path('crates').rglob('*.rs'):
        s = p.read_text()
        # Kernel types moved to node workload module in execution code/tests.
        if p.as_posix().startswith('crates/node/'):
            s = re.sub(r'\bPortPublish,\s*', '', s)
            s = re.sub(r'\bStorageMount,\s*', '', s)
            s = re.sub(r',\s*PortPublish\b', '', s)
            s = re.sub(r',\s*StorageMount\b', '', s)
        # Remove the dead kernel RequestClass import everywhere; wrapped code
        # uses a fully-qualified WorkloadClass.
        s = re.sub(r'\bRequestClass,\s*', '', s)
        s = re.sub(r',\s*RequestClass\b', '', s)
        p.write_text(s)


remove_enum_and_fields()
patch_node_modules()
migrate_literals()
patch_service()
patch_control()
cleanup_imports()
