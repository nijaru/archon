# Archon — Private Context

Private context for Archon, a distributed resource operating system: a typed
resource graph, enforceable leases, one scheduling authority, and node agents
that enforce leases with real kernel mechanisms.

## Goal

**Complete the system.** No product/release work — packaging, install, docs,
and commercial questions are deferred until the designed scope exists and
works.

## Structure

| Path | Purpose |
|---|---|
| `brief.md` | Start here: current state, constraints, next action |
| `STATUS.md` | What shipped, in order |
| `PLAN.md` | The completion roadmap: what remains, in dependency order |
| `spec.md` | Product and system specification |
| `DESIGN.md` | Architecture and ownership boundaries |
| `DECISIONS.md` | Decisions with rationale (newest first) |
| `design/kernel-primitives.md` | Accepted kernel contract |
| `design/lease-fencing.md` | Accepted lease/binding fencing protocol |
| `design/DISTRIBUTED_RESOURCE_OS.md` | Full resource-OS architecture |
| `design/mvp-scope.md` | Original kernel scope (v0, done) |
| `design/architecture-notes.md` | Architecture rationale and invariants |
| `archive/` | Superseded research and reviews; not on any load path |

## Session start

1. Read `brief.md`.
2. Run `tk ready`.
3. Check `git status` before editing.

Load design docs only when changing architecture or kernel contracts.
