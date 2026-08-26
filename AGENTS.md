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
architecture or kernel contracts. Load `design/mvp-scope.md` only for historical
v0 context. Read `design/scope-simplicity-performance.md` before adding a new
mandatory subsystem or expanding product ownership. Read
`design/deployment-and-adoption.md` before changing Linux host requirements,
node onboarding, hardware/provider integration, scheduler coexistence,
resource handoff, or datacenter deployment behavior.

## Implementation order

The proof foundation exists. Follow the active centralized `PLAN.md`, currently:

1. production-safe controller/agent restart reconciliation;
2. real accelerator discovery/attachment/workload proof;
3. mixed long-running + batch + distributed workload evidence;
4. scale/overhead/simplicity baselines;
5. optimize only measured limits;
6. stabilize external schemas, deployment identity/security, packaging, and
   pilots.

Do not add long-term architecture features merely because they appear in the
full design. Do not recreate Postgres, NATS, ConnectRPC, model, endpoint,
replica, GPU-telemetry, service-mesh, storage-engine, or bare-metal provisioning
surfaces without a measured or named workload requirement.

## Deployment and authority

Linux is the current host substrate. Reuse cgroups, namespaces, eBPF, OCI, KVM,
CDI/VFIO/SR-IOV, vendor drivers, networking, and storage systems rather than
replacing them.

Machine provisioning is not currently Archon's responsibility. A machine joins
when a prepared Linux environment starts an Archon agent and registers the
capabilities it can enforce.

A schedulable exclusive resource has one unambiguous authority at a time. Do
not implement a path where Archon and Kubernetes/Slurm/Flux/Nomad independently
believe they own the same resource. Coexistence requires disjoint resource sets
or explicit parent/delegated allocations. Handoff requires drain/revoke,
confirmed binding close/fence, then transfer of authority.

See `docs/deployment.md` for the repository-local implementation summary; the
centralized `design/deployment-and-adoption.md` is authoritative.

## Invariants

- Exclusive leases never overlap.
- Older binding fences and agent sessions are rejected at resource endpoints.
- Provider bindings enforce leases at resource endpoints.
- Partial preparation reaches an active lease or an explicit failure.
- One Cluster owns ordinary allocation; wider/global policy delegates bounded
  capacity rather than sharing ambiguous authority.
- A resource is not handed to another scheduler/authority until Archon bindings
  are closed or fenced.
- Telemetry is not authoritative allocation state.
- Simulator and production share the same pure decision logic where applicable.
- Unsupported enforcement is explicit; do not silently weaken an accepted
  resource or isolation contract.

## Load map

| Trigger | Read |
|---|---|
| Current work | centralized `brief.md`, then `STATUS.md` if needed |
| Product contract | centralized `spec.md` |
| Architecture | centralized `DESIGN.md`, then `design/DISTRIBUTED_RESOURCE_OS.md` if needed |
| Scope / complexity / performance | centralized `design/scope-simplicity-performance.md` |
| Deployment / Linux / datacenter integration | centralized `design/deployment-and-adoption.md` and local `docs/deployment.md` |
| Kernel design | centralized `design/kernel-primitives.md` |
| Initial kernel history | centralized `design/mvp-scope.md` |
| Rationale | centralized `DECISIONS.md` |
| Implementation stages | centralized `PLAN.md` |
| License claims | centralized `design/LICENSE_BOUNDARY.md` |

Planned public license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
provider/extension interfaces. Do not claim third-party or generated-artifact
licenses without checking notices.

## Verification

- Docs: resolve links in changed files; `git diff --check`.
- Rust: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo run -p archon-sim`.
- Use the simulator for distributed ownership/failure invariants and live Linux
  integration tests for enforcement, process/device behavior, and timing.
- Do not invent a second implementation stack.
