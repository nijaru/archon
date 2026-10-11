# Archon

Rust-first compute orchestrator in early R&D. The kernel is the resource/scheduling
authority for its explicitly managed resources; Linux, execution runtimes, drivers,
networking and storage are substrate.

## Navigation and workflow

- `README.md`: current CLI, host prerequisites and behavior limitations.
- `CONTRIBUTING.md`: contribution policy, DCO sign-off and required CI.
- `crates/kernel`: pure resource decisions; `crates/sim`: deterministic failure scenarios.
- `crates/node`: providers, execution, agent/controller reconciliation and persistence.
- `crates/control` and `crates/cli`: current wire/API and user entry points.

The maintainer works on `main` during R&D. External contributions follow
`CONTRIBUTING.md`. Do not open a pull request unless requested.

## Architecture guardrails

- Keep resource authority distinct from workload lifecycle and execution outcomes.
- A schedulable exclusive resource has one authority at a time. Nested workloads and
  outer schedulers need explicit, enforced delegation—not overlapping ownership.
- Planner proposals, telemetry and runtime recommendations are advisory; they cannot
  grant resource rights.
- Preserve exact provider targets and generations through graph refresh, restart and
  teardown. Reject stale sessions/generations at enforcement boundaries. Session
  freshness is not execution identity.
- Partial preparation reaches explicit activation or failure. Unknown effects retain
  occupancy; timeouts, process liveness and root exit are not universal closure proof.
- Reuse Linux/OCI/device/network/storage implementations. Unsupported enforcement
  fails explicitly; do not silently weaken an accepted contract.
- Keep application/runtime state semantics out of the resource kernel.
- Share pure resource-decision logic between production and simulation. Small
  installations must not inherit unnecessary distributed infrastructure.

These are required contracts, not blanket claims that current code satisfies them.
Do not treat a design proposal as shipped behavior or freeze proof-stage abstractions
when an authorized redesign shows they need replacement.

## Verification

```text
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p archon-sim
```

Dependency changes also require:
`cargo deny check advisories bans licenses sources`.

CI's `privileged-enforcement` job runs the real Linux cgroup/device suite with
`ARCHON_REQUIRE_PRIVILEGED_CGROUP=1`; ordinary unprivileged or macOS tests are not
equivalent evidence. Use simulation for distributed ownership decisions, live Linux
tests for enforcement and real multi-controller tests for HA claims.

## Documentation boundary

Public documentation describes implemented behavior, supported contracts, prerequisites,
compatibility, troubleshooting and otherwise-lost maintenance rationale. Do not publish
private product strategy, research or roadmap, or add private-store discovery paths here.
