# Fleet Status

**Updated:** 2026-08-17

## Direction

Fleet is now defined as a **distributed resource operating system**.

Its core model is:

> A datacenter is a graph of leaseable capabilities. A workload is a set of
> constraints and objectives over that graph.

Fleet is intended to unify resource discovery, leases, scheduling, identity,
isolation, execution, health, and recovery for services, batch/HPC, distributed
AI, inference, native processes, OCI containers, microVMs, VMs, WASM, and future
composable resources.

The detailed contract is in:

- [`ai/spec.md`](spec.md)
- [`ai/DESIGN.md`](DESIGN.md)
- [`ai/design/DISTRIBUTED_RESOURCE_OS.md`](design/DISTRIBUTED_RESOURCE_OS.md)
- [`ai/DECISIONS.md`](DECISIONS.md)
- [`ai/PLAN.md`](PLAN.md)

## What changed

The previous model-first GPU-serving documents described a useful first
workload, but they made the product boundary too narrow. The canonical boundary
is now the resource operating system:

- GPUs are accelerators, not the only resource type;
- inference is one workload class, not the system definition;
- model endpoints are one service experience, not the universal abstraction;
- direct-node execution is one node path, not a rejection of cluster boundaries
  or nested schedulers;
- Kubernetes, Slurm, Flux, Ray, MPI, and other systems are compatibility or
  nested-workload paths, not internal authorities.

The Go model-serving scaffold was deleted. The repository is design-only until
the Rust resource/lease kernel exists. Do not revive Postgres, NATS,
ConnectRPC, model, endpoint, or replica types as a compatibility shell.

## Architectural center

The first principles are:

1. typed resource graph;
2. universal enforceable allocation/lease;
3. separate mechanism and policy;
4. hierarchical global/cluster/node/workload scheduling;
5. topology, data locality, health, and failure domains as first-class state;
6. selectable process, container, microVM, VM, and WASM isolation;
7. compatibility without architectural capture.

The first implementation proof should cover graph registration, hard constraints,
topology-aware scoring, atomic lease commit/release/revoke, nested leases,
explanations, deterministic replay, stale ownership, and simulated node failure.

## Current documentation state

- Accepted v0 kernel contract: `design/kernel-primitives.md`.
- Canonical long-term architecture: `design/DISTRIBUTED_RESOURCE_OS.md`.
- `spec.md` is the product/system specification.
- `DESIGN.md`, `DECISIONS.md`, and `PLAN.md` now use the resource-OS model.
- Older cell/host/Allocation-as-lease wording maps to the kernel contract and
  must not be implemented as a second vocabulary.
- The old vLLM/direct-node and Go control-plane work is historical only. It is
  not a migration source and must not constrain the kernel.

## Immediate design work

1. `tk-l8xd`: specify lease renewal, binding fences, and partition recovery.
2. `tk-0lvx`: build the deterministic simulator around the same decision logic.
3. Add a tiny Rust workspace only after those types exist. Do not scaffold the
   full later crate map in `research/stack.md`.
4. Add node execution only after the resource/lease contracts are explicit.

## Open questions

- lease and binding-fence details under partitions;
- dynamic topology, contention, and health representation;
- provider contracts for accelerator partitioning and preemption;
- virtual-cluster network, storage, and identity semantics;
- first Rust crate/workspace layout for the kernel and simulator.
