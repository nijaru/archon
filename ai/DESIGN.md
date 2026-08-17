# Fleet Design

**Updated:** 2026-08-17

The accepted v0 kernel contract is
[`design/kernel-primitives.md`](design/kernel-primitives.md). Kernel types are
Cluster, Graph, Node, Edge, Request, Allocation, Lease, Binding, and Agent.

Fleet is a distributed resource operating system, not an inference-only control
plane or a thin wrapper around existing schedulers. Its core question is:

> Which leaseable capabilities should satisfy this workload's constraints and
> objectives, and how can Fleet control, enforce, and recover that decision?

The full architecture is in [`design/DISTRIBUTED_RESOURCE_OS.md`](design/DISTRIBUTED_RESOURCE_OS.md).
The former model-first GPU-serving design remains useful as a first workload
experience, but it is no longer the product boundary.

## Architecture

```text
User/API intent
      ↓
Intent compiler and policy
      ↓
Global planner
      ↓
Cluster allocator and lease service
      ↓
Typed resource graph and materialized indexes
      ↓
Node resource manager
      ↓
Process | OCI | microVM | VM | WASM
      ↓
Linux and hardware
```

Fleet is the scheduler and resource authority for native workload classes. A
nested workload runtime may schedule tasks inside an explicitly delegated lease,
but it cannot allocate outside that lease or replace Fleet's native control path.

## Core ownership

Fleet owns:

- typed resource discovery and graph state;
- allocation, lease renewal, expiry, revocation, fencing, and reclamation;
- placement and scheduling policy boundaries;
- node resource enforcement and execution lifecycle;
- health, reconciliation, recovery, and explanations;
- cell and virtual-cluster boundaries;
- compatibility adapters and the simulator.

Fleet reuses Linux, KVM, cgroups, namespaces, eBPF, WireGuard, OCI, CDI,
container runtimes, Cloud Hypervisor, Firecracker, Wasmtime, Ceph, NVMe-oF,
RDMA, vendor drivers, NCCL/RCCL, MPI, Slurm, Flux, Ray, vLLM, SGLang, and other
mature components.

## Workload model

The API exposes typed workload intent for services, batch jobs, distributed AI,
HPC, inference, VMs, microVMs, WASM, and nested schedulers. Intent includes:

- resource requirements and minimum/preferred/maximum quantities;
- hard constraints and topology requirements;
- QoS, priority, fair-share, reservation, deadline, and preemption policy;
- trust/isolation mode;
- runtime, image, device, storage, and network requirements;
- locality, cache, checkpoint, and restart objectives.

The first runtime adapter may be vLLM, but the node agent receives a generic
workload specification and does not interpret inference-specific semantics.

## Resource graph

The graph models hosts as one node in a larger topology:

```text
CPU ─ NUMA ─ DRAM ─ CXL
 │
PCIe ─ Accelerator ─ NVLink/NVSwitch
 │
NIC ─ RDMA ─ Fabric
 │
NVMe ─ Storage fabric
```

It also represents data objects such as model weights, datasets, checkpoints,
KV caches, OCI layers, and compiled artifacts. Placement can therefore account
for locality and transfer cost.

The graph is a logical contract. It does not require a general graph database;
authoritative control state, topology snapshots, placement indexes, and
telemetry can use different physical stores.

## Lease and scheduler boundaries

The lease is the universal allocation primitive. Lease semantics must cover:

- exclusive and shareable capacity;
- renewal, expiry, revoke, release, and fencing;
- gang and partial allocation;
- parent/child and nested allocations;
- checkpoint-aware preemption;
- failure recovery and explicit ambiguous outcomes.

Scheduling is hierarchical:

```text
GLOBAL PLANNER       region, cell, capacity, cost, energy, data
CELL ALLOCATOR       local queues, gangs, topology, backfill, fairness
NODE RESOURCE MGR    cgroups, NUMA, devices, NICs, NVMe, preemption
WORKLOAD SCHEDULER   Slurm, Flux, Ray, MPI, application runtime
```

Placement uses hard filtering, graph reduction, topology filtering, fast
scoring, atomic lease commit, and optional background optimization. Explanations
are part of the placement result.

## Control plane

```text
Intent
  → typed API
  → command/transaction log
  → replicated state machine
  → materialized indexes
  → planners and reconcilers
```

Cells own local scheduling, leases, nodes, services, and failure handling. A
global plane owns identity, policy, federation, cross-cell placement, and global
reservations. Telemetry is separate from authoritative state.

The control plane must preserve:

- versioned reads;
- atomic ownership transitions;
- lease fencing;
- deterministic replay;
- snapshots and recovery;
- explicit convergence or failure.

## Node and provider interfaces

The target node is minimal immutable Linux with a node agent, runtime stack,
KVM, eBPF programs, drivers, and atomic updates.

Provider interfaces cover:

```text
discover()
capabilities()
topology()
allocate()
prepare()
attach()
detach()
health()
reset()
```

Execution modes are process, OCI container, sandbox, microVM, VM, and WASM.
Device providers handle CDI, VFIO, SR-IOV, vendor APIs, and future accelerator
classes without making the core NVIDIA-specific.

## Networking and storage

Fleet provides a native policy surface over Linux networking and eBPF/XDP for
routing, load balancing, identity, policy, observability, bandwidth, WireGuard,
BGP, SR-IOV, and RDMA awareness. Sidecars are not mandatory.

Storage is a provider boundary for content-addressed artifacts, local NVMe,
persistent volumes, snapshots, replication, migration, and health. Initial
providers may include Ceph, NVMe-oF, NFS, cloud block storage, and object stores.

## Extensions and compatibility

OCI, CDI, OpenTelemetry, Linux/KVM, and standard storage/network protocols are
native interoperability boundaries. Kubernetes, Helm, Slurm, Flux, Ray, MPI,
and other systems are optional integrations or nested workloads. Fleet remains
the first-party scheduler and resource authority rather than delegating its
control plane to them.

Policy and scheduler extensions should prefer capability-limited WASM. Extensions
that require host or device access use explicit provider boundaries.

## Target implementation structure

The target Rust workspace is organized by responsibility:

```text
api / types / resource-graph / lease / state / raft
scheduler-core / scheduler-service / scheduler-batch / scheduler-ai
planner-global / node-agent
runtime-oci / runtime-microvm / runtime-wasm
device / network / storage / baremetal / telemetry
policy-sdk / simulator / cli
```

Pure scheduling/resource logic remains transport-independent and is executed by
the simulator and production control path.

The deleted Go model-serving scaffold is not a language or architecture
decision. Do not restore it as a compatibility shell or let its package
structure define the resource model.

## Staged scope

### v0: resource and lease kernel

- typed graph and synthetic node registration;
- CPU, memory, NUMA, accelerator, PCIe, NIC, and NVMe resources;
- hard constraints, topology filtering, scoring, and explanations;
- atomic lease commit, release, revoke, fencing, and nested leases;
- deterministic replay and simulated node failure;
- native process and OCI boundaries;
- CLI and simulator.

### v1: operational substrate

- gang scheduling, queues, reservations, fair share, backfill;
- accelerator topology, CDI, microVMs, and volume providers;
- health/fencing, data locality, artifact staging, and network foundations;
- virtual clusters and Slurm/Flux compatibility experiments.

### v2: heterogeneous AI/HPC

- performance and goodput models;
- communication graphs and network contention;
- elastic allocation, checkpoint cost, device partitioning/preemption;
- model/cache placement and global/cell planning.

CXL, disaggregated resources, bare-metal lifecycle, multi-region federation,
energy optimization, and formal controller verification follow later.

## Design invariants

- An exclusive lease never overlaps another exclusive lease.
- Fenced owners cannot mutate allocation state.
- A workload sees only attached resources from its allocation.
- Accepted intent converges or terminates with an explicit failure.
- Ownership transitions are atomic at the authority boundary.
- Simulator replay and production scheduling share the same pure decision logic.
