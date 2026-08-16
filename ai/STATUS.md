# Fleet Status

**Updated:** 2026-08-16

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
- direct-node execution is one node path, not a rejection of cells or nested
  schedulers;
- Kubernetes, Slurm, Flux, Ray, MPI, and other systems are compatibility or
  nested-workload paths, not internal authorities.

The existing Go code is a scaffold and does not define the target language or
architecture. The target core is Rust-first.

## Architectural center

The first principles are:

1. typed resource graph;
2. universal enforceable allocation/lease;
3. separate mechanism and policy;
4. hierarchical global/cell/node/workload scheduling;
5. topology, data locality, health, and failure domains as first-class state;
6. selectable process, container, microVM, VM, and WASM isolation;
7. compatibility without architectural capture.

The first implementation proof should cover graph registration, hard constraints,
topology-aware scoring, atomic lease commit/release/revoke, nested leases,
explanations, deterministic replay, stale ownership, and simulated node failure.

## Current documentation state

- Canonical architecture is recorded in `design/DISTRIBUTED_RESOURCE_OS.md`.
- `spec.md` is the product/system specification.
- `DESIGN.md`, `DECISIONS.md`, and `PLAN.md` now use the resource-OS model.
- The old vLLM/direct-node work remains a first workload and execution-adapter
  reference.

## Immediate design work

1. Define the resource graph schema and placement indexes.
2. Define lease ownership, renewal, expiry, revocation, fencing, and nesting.
3. Define scheduler transaction and control-plane state boundaries.
4. Build the deterministic simulator around the same pure scheduling logic.
5. Add node execution only after the resource/lease contracts are explicit.

## Open questions

- graph representation and materialized indexes;
- cell consensus and federation semantics;
- lease and fencing details under partitions;
- dynamic topology, contention, and health representation;
- provider contracts for accelerator partitioning and preemption;
- virtual-cluster network, storage, and identity semantics;
- Rust-core migration boundary for the current Go scaffold.
