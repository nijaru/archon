# Initial Kernel Scope

**Updated:** 2026-08-17

Kernel types and commit path are in [`kernel-primitives.md`](kernel-primitives.md).

Fleet's first implementation should prove the resource operating-system
abstraction before integrating a large runtime surface. The first scope is a
control/resource kernel and simulator, not a complete Kubernetes/Slurm
replacement and not an inference-only product.

## v0: resource and lease kernel

### Required

- typed resource graph;
- synthetic registration for 3–10 nodes;
- CPU, memory, NUMA, accelerator, PCIe, NIC, and NVMe topology;
- hard constraints and candidate graph reduction;
- topology filtering and fast scoring;
- atomic lease commit;
- release, renewal, revoke, expiry, and fencing semantics;
- parent/child and nested leases;
- explainable placement and refusal results;
- deterministic state replay;
- simulated node/device failure;
- CLI and simulator using the production decision logic.

### Required scenarios

1. Register heterogeneous synthetic nodes.
2. Submit a service and batch allocation request.
3. Reject candidates that violate hard topology or capability constraints.
4. Commit a lease and verify exclusive claims cannot overlap.
5. Create a nested lease and verify the child cannot escape its parent.
6. Revoke or fence a lease and reject stale-agent writes.
7. Release resources and prove they become schedulable again.
8. Replay the trace and reproduce the same placement and state digest.
9. Fail a node and produce an explicit recovery or rescheduling decision.

### Explicit non-features

- global multi-region optimization;
- every accelerator vendor;
- full Kubernetes or Slurm compatibility;
- distributed storage implementation;
- custom inference engine;
- high-rate telemetry in authoritative state;
- giant MIP/SAT optimization in the placement critical path;
- production bare-metal provisioning.

## v1: operational substrate

Status (2026-08-22): much of this shipped in the working system — single
control plane with snapshot/compaction, node registration, process and OCI
execution, cgroups, devices, queues/reservations/fair share/backfill,
health/fencing/reconciliation. Still open: replicated control-plane state
(HA), microVMs, virtual-cluster leases, CDI providers beyond device
passthrough, artifact staging, the Slurm/Flux compatibility experiment.

Remaining from the original list:

- cell control plane and replicated state;
- native process and OCI execution;
- cgroups/cpusets, NUMA policy, and device attachment;
- CDI and accelerator providers;
- queues, reservations, fair share, backfill, and co-scheduled placement;
- health, fencing, and reconciliation;
- data locality and artifact staging;
- network/storage provider boundaries;
- microVM execution;
- virtual-cluster leases;
- a first compatibility experiment with Slurm or Flux.

A vLLM workload on real accelerator hardware is a useful validation workload
for v1. It is not the universal workload API.

## v2: heterogeneous AI/HPC and federation

Add only after v1 ownership and recovery contracts are stable:

- accelerator performance and goodput models;
- communication graph and network contention inputs;
- elastic worker ranges;
- checkpoint/restart cost;
- accelerator partitioning and preemption;
- model/cache placement;
- global planner and multi-cell federation;
- cost, energy, and data-residency policy.

## Success criteria

The initial kernel succeeds when:

- the same resource model represents service and batch requests;
- topology affects placement rather than serving as metadata only;
- lease ownership is correct through release, revoke, stale writers, and failure;
- placement and refusal decisions are explainable;
- replay is deterministic;
- the simulator and production path share decision logic;
- a process and OCI workload can consume the same allocation contract.
