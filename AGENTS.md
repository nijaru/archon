# Archon Compute OS

Rust-first distributed resource OS. Archon is the resource/scheduling authority for resources it directly manages; Linux, KVM, OCI, drivers, accelerator/runtime stacks, networking, and storage systems are substrate/providers.

## Persistent context

Durable project context is centralized at:

`~/github/nijaru/agent-context/projects/github.com/omendb/archon/ai/`

Never recreate a repository-local `ai/` tree and do not copy product/design/roadmap prose into this repository.

## Session start

1. Read centralized `brief.md`.
2. Read only the canonical context file relevant to the task.
3. Run `tk ready` if using the project task tracker.
4. Check `git status` before editing.

## Context load map

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
| Inference runtime / Engine integration | centralized `design/inference-runtime-boundary.md` |
| Device/provider enforcement | centralized `design/device-enforcement.md` |
| License claims | centralized `design/LICENSE_BOUNDARY.md` |

Do not load `archive/` during ordinary work.

## Current implementation order

Follow centralized `PLAN.md`. Current sequence:

1. production-safe controller/agent restart reconciliation;
2. real accelerator discovery/attachment/workload proof;
3. mixed long-running + batch + distributed + accelerator workload proof;
4. scale/overhead/utilization baselines;
5. optimize only measured limits;
6. stabilize schemas, deployment identity/security, install/diagnostics, then pilot.

## Architecture guardrails

- Keep the resource/workload model broad and Archon's ownership boundary narrow.
- A schedulable exclusive resource has one unambiguous authority at a time.
- Reuse Linux cgroups/namespaces/eBPF, OCI/KVM, CDI/VFIO/SR-IOV, vendor drivers, networking, and storage systems unless evidence requires replacement.
- Machine provisioning is not currently an Archon responsibility.
- Kubernetes, Slurm, Flux, Ray, MPI, and similar systems are integrations, bounded nested workloads, or outer authorities during explicitly delegated evaluation—not competing owners of the same exclusive resource.
- Inference/training runtimes remain workload-local systems: they may describe resource/topology requirements and performance alternatives, but they do not become Archon resource authorities and Archon does not absorb their model/state semantics.
- Unsupported enforcement must fail explicitly; never silently weaken an accepted resource/isolation contract.
- Do not add mandatory infrastructure because an incumbent uses it.
- Do not scaffold speculative long-term components or recreate deleted Go model/endpoint/replica/Postgres/NATS/ConnectRPC surfaces.
- Performance, utilization, and simplicity advantages require matched repeatable evidence.

## Invariants

- Exclusive leases never overlap.
- Stale binding fences and agent sessions are rejected at enforcement boundaries.
- Partial preparation reaches active ownership or explicit failure.
- Resource reuse occurs only after prior bindings are closed/fenced.
- Telemetry cannot grant resource authority.
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
