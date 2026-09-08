# Archon Compute OS

Rust-first distributed resource OS. Archon is the resource/scheduling authority for resources it directly manages; Linux, KVM, OCI, drivers, accelerator/runtime stacks, networking, and storage systems are substrate/providers.

## Persistent context

Primary orientation is this repository: `README.md`, `CONTRIBUTING.md`,
the crate sources, and the verification gates below. They are the
contract every contributor shares.

The maintainer additionally keeps durable product/design/roadmap context
in a private context checkout (locally symlinked as `ai/`, excluded from
git). When it is present, the maintainer session workflow below applies.
When it is absent — any public clone — work from the repository alone
and do not invent roadmap or architecture prose.

Never recreate a repository-local `ai/` tree and do not copy private
product/design/roadmap prose into this repository.

## Current repository workflow

The maintainer works directly on `main` during R&D. External
contributors work from forks and pull requests per `CONTRIBUTING.md`.
Agents: do not open pull requests unless the user explicitly asks for one.

## Session start

1. Check `git status` before editing; preserve unrelated work.
2. Maintainer sessions with the private context checkout present: read
   `ai/brief.md`, then only the canonical context file relevant to the
   task (load map below), and run `tk ready` if using the project task
   tracker.

## Context load map

Maintainer-only; requires the private context checkout.

| Task | Read |
|---|---|
| Product/architecture/scope | centralized `spec.md` |
| Current implementation | centralized `STATUS.md`, then code/tests |
| Roadmap/priorities | centralized `PLAN.md` |
| Durable rationale | centralized `DECISIONS.md` |
| Resource/lease kernel semantics | centralized `design/kernel-primitives.md` |
| Fencing/recovery authority | centralized `design/lease-fencing.md` |
| Scope/complexity/performance expansion | centralized `design/scope-simplicity-performance.md` |
| Linux/datacenter/coexistence/handoff | centralized `design/deployment-and-adoption.md` |
| Generic adaptive workload-runtime resource planning / resizing / safe transitions | centralized `design/adaptive-runtime-resource-contract.md` |
| Inference runtime / Engine specialization | centralized `design/inference-runtime-boundary.md` |
| Device/provider enforcement | centralized `design/device-enforcement.md` |
| License claims | centralized `design/LICENSE_BOUNDARY.md` |

Do not load `archive/` during ordinary work.

## Current implementation order

Maintainer sequencing (private `PLAN.md`). Current sequence:

1. production-safe controller/agent restart reconciliation;
2. real accelerator discovery/attachment/workload proof;
3. mixed long-running + batch + distributed + accelerator workload proof;
4. scale/overhead/utilization baselines;
5. optimize only measured limits;
6. stabilize schemas, deployment identity/security, install/diagnostics, then pilot.

Engine is the primary adjacent project now. Do not interrupt Engine work to implement speculative Archon integration; update Archon implementation only when its own roadmap or a real Engine/Training/runtime consumer demonstrates the need.

## Architecture guardrails

- Keep the resource/workload model broad and Archon's ownership boundary narrow.
- A schedulable exclusive resource has one unambiguous authority at a time.
- Reuse Linux cgroups/namespaces/eBPF, OCI/KVM, CDI/VFIO/SR-IOV, vendor drivers, networking, and storage systems unless evidence requires replacement.
- Machine provisioning is not currently an Archon responsibility.
- Kubernetes, Slurm, Flux, Ray, MPI, and similar systems are integrations, bounded nested workloads, or outer authorities during explicitly delegated evaluation—not competing owners of the same exclusive resource.
- Adaptive runtimes may describe alternative resource/topology/residency plans, pressure, checkpoint/drain readiness, and performance estimates; these remain advisory until ordinary Allocation/Lease/Binding transitions grant authority.
- Inference/training runtimes remain workload-local systems: Archon does not absorb their model/state/execution semantics.
- Runtime resource changes should use accountable additional/replacement Lease units plus prepare/switch/drain semantics rather than hidden in-place ownership mutation unless a proven Provider primitive preserves the same authority guarantees.
- Unsupported enforcement must fail explicitly; never silently weaken an accepted resource/isolation contract.
- Do not add mandatory infrastructure because an incumbent uses it.
- Do not scaffold speculative long-term components or recreate deleted Go model/endpoint/replica/Postgres/NATS/ConnectRPC surfaces.
- Performance, utilization, and simplicity advantages require matched repeatable evidence.

## Invariants

- Exclusive leases never overlap.
- Stale binding fences and agent sessions are rejected at enforcement boundaries.
- Partial preparation reaches active ownership or explicit failure.
- Resource reuse occurs only after prior bindings are closed/fenced.
- Telemetry, runtime recommendations, and performance forecasts cannot grant resource authority.
- Simulator and production share pure resource-decision logic where applicable.
- Small deployments do not inherit unnecessary datacenter complexity.

## Repository documentation boundary

Keep this repository's docs tied to executable behavior: build/test instructions, current CLI/API/wire behavior, supported host prerequisites, installation, troubleshooting, and version compatibility.

Future product strategy, architecture rationale, deployment/adoption strategy, and roadmap live only in centralized context. Link rather than copy.

## Verification

```text
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p archon-sim
```

Use the simulator for distributed ownership/failure invariants and live Linux integration tests for enforcement, process/device behavior, and timing-sensitive proof.
