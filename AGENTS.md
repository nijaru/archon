# Archon Compute OS

Rust-first distributed resource OS. Archon is the resource and scheduling
authority. Linux, KVM, OCI, and drivers are substrate. Kubernetes, Slurm, Ray,
and MPI are optional integrations or nested workloads.

> A datacenter is a graph of leaseable capabilities. A workload is a set of
> constraints and objectives over that graph.

## Persistent project context

Archon's private project context is centralized outside this repository at
`~/github/nijaru/agent-context/projects/github.com/omendb/archon/ai/`.
Use the `ai-context` skill to retrieve it. Read `brief.md` first, and never
recreate a repository-local `ai/` directory.

## Session start

1. Read the centralized `brief.md`.
2. Run `tk ready`.
3. Check `git status` before editing.

Load the centralized `design/DISTRIBUTED_RESOURCE_OS.md` only when changing
architecture or kernel contracts. Load `design/mvp-scope.md` only when
implementing the initial kernel. Do not load `archive/review/` on ordinary
startup.

## Implementation order

Prove these before runtime integrations:

1. Graph model: `Node`, `Edge`, `Request`, `Allocation`, `Lease`, `Binding`
2. Deterministic graph/lease/scheduler simulation
3. Binding fences, stale-agent sessions, partial preparation, and
   authority-partition correctness
4. Native Archon scheduling and node enforcement
5. Runtime, device, network, storage, and compatibility providers

Do not start with vLLM, a GUI, Kubernetes, Slurm, Ceph, or cloud APIs.
Do not recreate Postgres, NATS, ConnectRPC, model, endpoint, replica, or
GPU-telemetry surfaces. Create a Cargo workspace only after those types exist,
and keep that first tree to kernel plus simulator.

## Invariants

- Exclusive leases never overlap.
- Older binding fences and agent sessions are rejected at resource endpoints.
- Provider bindings enforce leases at resource endpoints.
- Partial preparation reaches an active lease or an explicit failure.
- One Cluster owns ordinary allocation; global policy delegates bounded capacity.
- Telemetry is not authoritative allocation state.
- Simulator and production share the same pure decision logic.

## Load map

| Trigger | Read |
|---|---|
| Current work | centralized `brief.md`, then `STATUS.md` if needed |
| Product contract | centralized `spec.md` |
| Architecture | centralized `design/DISTRIBUTED_RESOURCE_OS.md` |
| Kernel design | centralized `design/kernel-primitives.md` |
| Initial kernel scope | centralized `design/mvp-scope.md` |
| Rationale | centralized `DECISIONS.md` |
| Implementation stages | centralized `PLAN.md` |
| License claims | centralized `design/LICENSE_BOUNDARY.md` |

Planned public license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
provider/extension interfaces. Do not claim third-party or generated-artifact
licenses without checking notices.

## Verification

- Docs: resolve links in changed files; `git diff --check`.
- Rust: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo run -p archon-sim`. The simulator is the v0 fault-injection harness.
  Do not invent a second implementation stack.
