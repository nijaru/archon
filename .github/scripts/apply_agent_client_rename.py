from pathlib import Path

IDENTIFIERS = {
    "LeaseExecutor": "AgentClient",
    "LocalExecutor": "LocalAgentClient",
    "RemoteExecutor": "RemoteAgentClient",
}

for path in Path("crates").rglob("*.rs"):
    text = path.read_text()
    updated = text
    for old, new in IDENTIFIERS.items():
        updated = updated.replace(old, new)
    if path == Path("crates/node/src/service.rs"):
        updated = updated.replace(".execute(", ".call(")
        updated = updated.replace(
            "/// The controller side of the enforcement seam.\n",
            "/// One controller-side client for the Agent request/response protocol.\n",
        )
        updated = updated.replace(
            "/// In-process execution: the agent runs in this same process.\n",
            "/// In-process Agent client: the Agent runs in this same process.\n",
        )
        updated = updated.replace(
            "/// Remote execution over TCP; one encrypted connection per agent.\n",
            "/// Remote Agent client over TCP; one encrypted connection per agent.\n",
        )
        updated = updated.replace(
            "    /// The executor moves onto a dedicated worker thread; registration and\n",
            "    /// The Agent client moves onto a dedicated worker thread; registration and\n",
        )
    if updated != text:
        path.write_text(updated)
