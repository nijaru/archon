from pathlib import Path
import re

# Existing OpenBinding call sites predate BindingScope. Preserve the existing
# exclusive endpoint contract unless a focused path opts into another scope.
needle = "Command::OpenBinding {"
for path in Path("crates").rglob("*.rs"):
    text = path.read_text()
    pos = 0
    changed = False
    while True:
        start = text.find(needle, pos)
        if start < 0:
            break
        brace = text.find("{", start)
        depth = 0
        end = None
        for index in range(brace, len(text)):
            if text[index] == "{":
                depth += 1
            elif text[index] == "}":
                depth -= 1
                if depth == 0:
                    end = index
                    break
        if end is None:
            raise RuntimeError(f"unterminated OpenBinding in {path}")
        block = text[start : end + 1]
        has_scope = re.search(r"(?m)^\s*scope(?:\s*:|\s*,)", block)
        if not has_scope and "provider:" in block:
            line_start = text.rfind("\n", 0, end) + 1
            close_indent = text[line_start:end]
            field_indent = close_indent + "    "
            insertion = (
                field_indent
                + "scope: archon_kernel::BindingScope::Exclusive,\n"
            )
            text = text[:line_start] + insertion + text[line_start:]
            end += len(insertion)
            changed = True
        pos = end + 1
    if changed:
        path.write_text(text)

# Legacy direct-agent tests construct Prepare requests by hand.
path = Path("crates/node/tests/remote.rs")
text = path.read_text()
pattern = re.compile(
    r"(?P<prefix>\s*binding:\s*\d+,\n\s*lease:\s*\d+,\n)(?P<indent>\s*)session:"
)

def expand(match: re.Match[str]) -> str:
    indent = match.group("indent")
    return (
        match.group("prefix")
        + indent
        + "node: 1,\n"
        + indent
        + "provider: 1,\n"
        + indent
        + "scope: archon_kernel::BindingScope::Exclusive,\n"
        + indent
        + "session:"
    )

text, count = pattern.subn(expand, text)
if count != 2:
    raise RuntimeError(f"expected two legacy Prepare fixtures, updated {count}")
text = text.replace(
    "        fence: 1,\n    });",
    "        fence: 1,\n        epoch: 1,\n    });",
)
path.write_text(text)

# A new Agent process has no provider endpoint memory. Its new Agent session
# invalidates every command from the prior process, so reconciliation may
# adopt the Cluster's current Binding generation directly on Activate. Once an
# endpoint has existed in this process, its closed/superseded state remains
# authoritative and Activate cannot recreate it.
path = Path("crates/node/src/agent.rs")
text = path.read_text()
old = '''        let key = Self::endpoint_key(scope, binding, provider, node);
        let create = matches!(op, EndpointOp::Prepare | EndpointOp::Release | EndpointOp::Fence);
        if create {
            self.endpoints.entry(key).or_insert_with(|| {
                Endpoint::new(
                    ProviderId::from_u64(provider),
                    NodeId::from_u64(node),
                    session,
                )
            });
        }
        let endpoint = self
            .endpoints
            .get_mut(&key)
            .ok_or_else(|| "binding endpoint has not been prepared".to_string())?;
        endpoint.handshake(session);
        endpoint
            .apply(op, BindingId::from_u64(binding), fence, session)
            .map_err(|err| format!("provider endpoint rejected generation: {err:?}"))
'''
new = '''        let key = Self::endpoint_key(scope, binding, provider, node);
        let adopt_on_activate =
            matches!(op, EndpointOp::Activate) && !self.endpoints.contains_key(&key);
        let create = adopt_on_activate
            || matches!(op, EndpointOp::Prepare | EndpointOp::Release | EndpointOp::Fence);
        if create {
            self.endpoints.entry(key).or_insert_with(|| {
                Endpoint::new(
                    ProviderId::from_u64(provider),
                    NodeId::from_u64(node),
                    session,
                )
            });
        }
        let endpoint = self
            .endpoints
            .get_mut(&key)
            .ok_or_else(|| "binding endpoint has not been prepared".to_string())?;
        endpoint.handshake(session);
        if adopt_on_activate {
            endpoint
                .apply(
                    EndpointOp::Prepare,
                    BindingId::from_u64(binding),
                    fence,
                    session,
                )
                .map_err(|err| format!("provider endpoint rejected adoption: {err:?}"))?;
        }
        endpoint
            .apply(op, BindingId::from_u64(binding), fence, session)
            .map_err(|err| format!("provider endpoint rejected generation: {err:?}"))
'''
if text.count(old) != 1:
    raise RuntimeError("endpoint adoption anchor changed")
text = text.replace(old, new, 1)

anchor = '''    #[test]
    fn stale_cluster_epoch_is_rejected_by_the_real_agent_path() {
'''
test = '''    #[test]
    fn fresh_agent_session_can_adopt_current_active_generation() {
        let mut agent = LeaseAgent::new(ProcessRuntime::new());
        assert!(
            apply(
                &mut agent,
                EndpointOp::Activate,
                10,
                BindingScope::Exclusive,
                4,
                7,
            )
            .is_ok(),
            "a fresh agent process must adopt the current generation during reconcile"
        );
        apply(
            &mut agent,
            EndpointOp::Fence,
            10,
            BindingScope::Exclusive,
            4,
            7,
        )
        .unwrap();
        assert!(
            apply(
                &mut agent,
                EndpointOp::Activate,
                10,
                BindingScope::Exclusive,
                4,
                7,
            )
            .is_err(),
            "adoption must not reopen a generation already closed in this process"
        );
    }

'''
if text.count(anchor) != 1:
    raise RuntimeError("endpoint test anchor changed")
path.write_text(text.replace(anchor, test + anchor, 1))
