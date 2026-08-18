# Fleet Plan

**Updated:** 2026-08-17

## Product hypothesis

Fleet is a distributed resource operating system for heterogeneous compute.
It represents compute, memory, accelerators, networks, storage, data, health,
and failure domains as a typed graph; allocates them through hierarchical
leases; and runs services, batch/HPC, AI, containers, microVMs, VMs, and WASM
through one control plane.

The first accelerator/inference workload is a proof of the abstraction, not the
product boundary. vLLM, Slurm, Flux, Ray, MPI, Kubernetes, and other runtimes
may execute inside or beside Fleet allocations.

The first tree is `crates/kernel` plus `crates/sim`. The accepted kernel contract is
[`design/kernel-primitives.md`](design/kernel-primitives.md). The fencing
protocol is [`design/lease-fencing.md`](design/lease-fencing.md). The deleted
Go model-serving scaffold is not a migration source.

## Workstream order

### 1. Resource and lease kernel

Design and implement these primitives before broad runtime integration:

- typed resource graph schema and materialized placement indexes;
- resource registration and topology snapshots;
- allocation request and constraint model;
- lease acquisition, renewal, expiry, release, revocation, and fencing;
- parent/child and nested allocations;
- atomic commit, ambiguous outcomes, and recovery;
- explainable refusal and placement results;
- deterministic simulator using the production decision logic.

Required prototype:

- register 3–10 synthetic nodes;
- model CPU, RAM, NUMA, GPU/accelerator, NIC, PCIe, and NVMe topology;
- submit requests with hard and soft constraints;
- filter candidate subgraphs and score them;
- commit/release/revoke leases atomically;
- create a nested lease;
- replay state deterministically;
- simulate node failure and stale-agent fencing.

### 2. Cell control plane

Add a small-cell control process with:

- typed API and command/transaction log;
- replicated authoritative state;
- materialized resource and lease indexes;
- planner/reconciler loops;
- node registration and heartbeats;
- explicit degraded operation during global-plane loss;
- CLI inspection and placement explanations.

Keep telemetry out of authoritative state. Use separate logs, metrics, traces,
and hardware telemetry streams correlated by resource and allocation IDs.

### 3. Node resource manager

Implement the minimal immutable Linux node boundary:

- resource discovery and health;
- lease enforcement and fencing;
- cgroups/cpusets and NUMA policy;
- OCI execution;
- CDI/VFIO/provider device attachment;
- local NVMe and NIC resource controls;
- process, container, microVM, VM, and WASM execution adapters;
- atomic node updates and rollback.

The first concrete workload can be a vLLM container on an accelerator node.
That path validates the kernel; it does not make inference the architecture.

### 4. Scheduling policy layers

Add policy modules in this order:

- hard constraints and topology filtering;
- fast scoring, spread, affinity, anti-affinity, and bin packing;
- queues, priorities, reservations, fair share, backfill, and gangs;
- local network/storage constraints and data locality;
- preemption and checkpoint-aware restart;
- accelerator performance and goodput models;
- communication graphs, network contention, elasticity, and global planning.

Do not put a giant optimizer in the critical path for every allocation. Use
fast explainable placement first and asynchronous optimization where migration
is safe.

### 5. Native workloads and compatibility

Fleet's own schedulers and node managers are the primary control path for
services, batch, HPC, AI, VMs, and other supported workload classes. Add native
execution and policy for those classes while exposing virtual-cluster leases
and optional integrations for:

- OCI, CDI, OpenTelemetry, Linux/KVM, and standard protocols;
- Slurm, Flux, Kubernetes, Ray, MPI, and PyTorch/JAX runtimes;
- vLLM, SGLang, and other inference runtimes;
- persistent storage providers and artifact stores.

Compatibility integrations support migration and composition. They must not
replace Fleet's resource graph, lease semantics, or native scheduling authority.

### 6. Cells, federation, and disaggregated resources

After local leases, fencing, and failure behavior are proven, add:

- global planner and multi-cell federation;
- cross-cell identity, policy, and reservations;
- multi-region placement, cost, energy, and data residency;
- CXL and disaggregated memory/storage;
- TPU/NPU/FPGA/DPU providers;
- bare-metal lifecycle through Redfish and provisioning;
- formal verification of critical controller invariants.

## Roadmap

### v0: resource/lease kernel

Graph, allocation, lease, fencing, topology-aware scoring, explanations,
nested leases, deterministic replay, synthetic failure simulation, CLI, and
native/OCI execution boundaries.

### v1: operational substrate

Gang scheduling, queues, reservations, fair share, backfill, accelerator
providers, CDI, microVMs, persistent volumes, health, data locality, network
foundations, virtual clusters, and compatibility experiments.

### v2: AI/HPC and federation

Goodput, performance models, communication-aware placement, checkpoint/restart
cost, elasticity, accelerator partitioning/preemption, cache placement, cells,
and global planning.

### Later

Disaggregated resources, multi-region federation, energy optimization,
bare-metal lifecycle, more accelerator providers, and formal controller
verification.

## Explicit non-goals for the first kernel

- replacing Linux, KVM, drivers, NCCL/RCCL, MPI, Ceph, or vendor stacks;
- delegating Fleet's resource-control and scheduling authority to an incumbent
  orchestrator;
- building a general-purpose graph database;
- putting high-rate telemetry in the authoritative state machine;
- supporting every runtime before the lease boundary is correct;
- making Kubernetes or Slurm the internal abstraction;
- making vLLM or GPUs the only workload/resource vocabulary;
- implementing global optimization before local placement is explainable and
  deterministic.

## Decision gates

Advance from the kernel when:

- leases are correctly fenced through simulated failure and stale agents;
- replay produces identical placement/state results;
- resource graph and placement queries remain bounded;
- node execution sees only allocated resources;
- the same workload contract can run as a process and OCI workload;
- a real accelerator workload validates topology and lifecycle behavior.

Advance to cells/federation only when local ownership transitions, recovery,
and provider boundaries are explicit. Add a policy or compatibility layer only
when it exercises a stable primitive rather than hiding an unresolved contract.
