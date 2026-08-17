# Fleet — Private Context

Private planning context for Fleet, a distributed resource operating system.

Fleet's canonical architecture is the typed resource graph, enforceable lease,
hierarchical scheduler, node resource manager, and compatibility boundary
around mature runtimes and hardware systems.

## Structure

| Path | Purpose |
|---|---|
| `spec.md` | Product and system specification |
| `STATUS.md` | Current direction and open design work |
| `DESIGN.md` | Architecture and ownership boundaries |
| `PLAN.md` | Staged implementation plan |
| `DECISIONS.md` | Principles, decisions, and open questions |
| `design/DISTRIBUTED_RESOURCE_OS.md` | Detailed resource OS architecture |
| `design/architecture-notes.md` | Architecture rationale and invariants |
| `design/mvp-scope.md` | Initial resource/lease kernel scope |
| `research/` | Competitive, stack, UX, and commercial research |
| `review/` | Historical reviews and evaluations |
| `.tasks/` | Fleet task tracking |

## Current model

- **Resource graph:** CPU, memory, accelerators, fabrics, storage, data,
  locality, health, and failure domains.
- **Lease:** universal ownership, fencing, renewal, release, and nested
  allocation boundary.
- **Scheduler hierarchy:** global planner, cell allocator, node manager, and
  workload-local scheduler.
- **Execution:** process, OCI, sandbox, microVM, VM, and WASM.
- **Compatibility:** OCI, CDI, OpenTelemetry, Linux/KVM, and adapters for
  Kubernetes, Slurm, Flux, Ray, MPI, and existing AI runtimes.

There is no implementation yet. The Go model-serving scaffold was deleted so it
cannot shadow the Rust-first kernel. Inference and accelerator fleets are first
workloads, not the product boundary.

## Working order

1. Define resource graph and placement indexes.
2. Define lease, fencing, renewal, revocation, and nested-lease semantics.
3. Define scheduler transactions and cell state boundaries.
4. Build deterministic simulation and failure replay.
5. Add node execution and runtime/provider adapters.
