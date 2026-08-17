# Fleet Distributed Resource OS Specification

**Status:** canonical product and architecture specification
**Updated:** 2026-08-16

## 1. Executive summary

Fleet is a Rust-first distributed resource operating system for heterogeneous
compute infrastructure.

> A datacenter is a graph of leaseable capabilities. A workload is a set of
> constraints and objectives over that graph.

Fleet unifies resource discovery, topology, leases, allocation, scheduling,
identity, isolation, execution, health, and recovery for:

- services and long-running processes;
- batch and HPC jobs;
- distributed AI training and inference;
- native processes, OCI containers, microVMs, VMs, and WASM;
- local and distributed storage;
- CPU, GPU, TPU, NPU, FPGA, NIC, RDMA, CXL, and future resources.

Fleet is not “Kubernetes in Rust.” It is the primary resource-control,
scheduling, and execution system for its supported workload classes. It reuses
Linux, KVM, OCI, accelerator drivers, storage systems, network stacks, and
workload runtimes as lower-level substrate or explicit integrations.

The target core is Rust-first. There is no implementation in this repository
yet; the former Go model-serving scaffold was removed. Inference and
accelerator fleets remain a first workload and operator experience, not
Fleet's product boundary.

Detailed architecture: [`design/DISTRIBUTED_RESOURCE_OS.md`](design/DISTRIBUTED_RESOURCE_OS.md).

## 2. Product definition

Fleet owns:

- a typed resource graph;
- allocation and lease ownership, renewal, expiry, revocation, and fencing;
- hierarchical planning and scheduling;
- node-level isolation, device attachment, and workload lifecycle;
- health, recovery, reconciliation, and explainable decisions;
- cells, federation, virtual clusters, and compatibility boundaries;
- CLI, APIs, SDKs, simulators, and operational evidence.

Fleet does not replace lower-level substrate:

- Linux, KVM, cgroups, namespaces, eBPF, WireGuard, or vendor drivers;
- containerd, OCI runtimes, Cloud Hypervisor, Firecracker, or Wasmtime;
- Ceph, NVMe-oF, SPDK, object stores, or cloud storage providers;
- NCCL/RCCL and other vendor communication libraries.

Fleet does replace the fragmented resource-control and scheduling layer for
its supported workload classes. Slurm, Flux, Ray, MPI, JAX, PyTorch, vLLM, and
Kubernetes are integrations or optional nested workloads. They may run inside a
Fleet allocation, but Fleet does not delegate its native scheduling authority
to them.

## 3. Core principles

1. Resource graph over scalar node capacity.
2. Lease/allocation as the universal resource primitive.
3. Mechanism separated from policy.
4. Hierarchical scheduling rather than one monolithic scheduler.
5. Selectable isolation under one workload model.
6. Topology, data locality, health, and failure domains as first-class inputs.
7. Native policy support for AI/HPC concerns such as gangs, goodput,
   communication topology, checkpoint cost, and elasticity.
8. Small deployments remain small.
9. Compatibility adapters enable migration without dictating the core.
10. Mature infrastructure is reused unless a measured requirement justifies
    replacement.

## 4. Workload model

A workload is typed intent over resources, execution, placement, policy, trust,
and lifecycle. The API must not require users to understand the internal
scheduler topology.

Supported classes:

| Workload | Native needs |
|---|---|
| Service | replicas, rollout, health, autoscaling, routing, SLOs, spread |
| Batch | queues, priority, fair share, reservations, deadlines, backfill |
| Distributed job | gang allocation, synchronized start, elasticity, restart |
| AI workload | performance/goodput, model locality, communication, checkpoints |
| VM/microVM | CPU/memory/device leases, disk, network identity, migration |
| Nested allocation | a bounded lease for Slurm, Flux, Kubernetes, Ray, or MPI |

A workload may request minimum, preferred, and maximum capacity. A nested
scheduler can schedule only within the parent lease.

## 5. Resource graph

The logical graph includes nodes such as:

```text
Region, Datacenter, Cell, Rack, PowerDomain, Host, Socket, NUMANode,
CPUCore, MemoryPool, PCIeRoot, Accelerator, AcceleratorPartition, NIC,
Switch, Fabric, NVMe, StoragePool, CXLDevice, DataObject
```

Edges include:

```text
contains, connected_to, same_numa, same_pcie_root, nvlink, nvswitch,
rdma_path, storage_path, cxl_path, replica_of, cached_on,
failure_correlated, power_correlated
```

Properties include capacity, bandwidth, latency, oversubscription, health,
reliability, cost, energy, temperature, driver/runtime capability, and
security/isolation capability.

The graph represents data and state as well as hardware:

- model weights, datasets, checkpoints, and compiled artifacts;
- OCI layers and local caches;
- KV caches and replicas;
- storage allocations and snapshots.

The graph is a logical contract, not a requirement for a general-purpose graph
database. Authoritative state, materialized placement indexes, topology
snapshots, and high-rate telemetry may use different physical representations.

## 6. Allocation and lease semantics

The lease is the ownership, enforcement, and recovery boundary.

```rust
struct Allocation {
    id: AllocationId,
    owner: Principal,
    resources: ResourceSet,
    constraints: Constraints,
    qos: Qos,
    priority: Priority,
    preemption: PreemptionPolicy,
    start: Time,
    deadline: Option<Time>,
    parent: Option<AllocationId>,
}
```

The contract must define exclusive/shareable capacity, renewal, expiry,
revocation, fencing, partial and gang allocation, parent/child leases,
preemption, checkpoint requirements, authorization, and explicit refusal or
termination reasons.

An accepted lease must be enforceable by the node and device providers. A stale
agent or writer must not retain authority after fencing.

## 7. Scheduler hierarchy

```text
Global planner       seconds → hours
Cell allocator       milliseconds → seconds
Node resource manager microseconds → milliseconds
Workload scheduler   domain-specific
```

The global planner handles region/cell placement, large reservations, capacity,
data placement, cost, energy, and global failure domains.

The cell allocator handles local services/jobs, queues, fair share,
reservations, backfill, gangs, topology, local storage/network constraints, and
accelerator placement.

The node resource manager handles cgroups/cpusets, NUMA and memory policy,
accelerator partitions, device attachment, NIC queues, local NVMe QoS, and
device-level scheduling.

Placement follows this path:

```text
hard constraints
→ candidate graph reduction
→ topology filter
→ fast scoring
→ atomic lease commit
→ background optimization
→ optional migration/reallocation
```

The objective may combine fragmentation, latency, network contention, data
movement, energy, monetary cost, reliability risk, and checkpoint/restart cost.
Policy modules set weights and hard constraints. Placement explanations are
part of the API.

## 8. Control plane and cells

```text
Intent
→ typed API
→ command/transaction log
→ replicated state machine
→ materialized indexes
→ planners and reconcilers
```

Cells own local scheduling, nodes, leases, services, and failure handling. A
global layer owns identity, policy, cross-cell placement, federation, and global
reservations. Cells have an explicit degraded mode during global-plane loss.

Authoritative control state is separate from telemetry. Logs, metrics, traces,
profiling, and hardware streams are correlated but do not decide ownership.

Required control-plane properties:

- versioned reads and transactional intent changes;
- snapshots and deterministic replay;
- atomic ownership transitions;
- lease fencing and reconciliation;
- explicit ambiguous-outcome handling;
- bounded materialized placement indexes.

## 9. Node OS and runtime model

Fleet targets a minimal immutable Linux node:

```text
Linux kernel, node agent, runtimes, KVM, eBPF, drivers,
network/storage tooling, atomic updater
```

Execution modes are workload properties:

```text
process | OCI container | sandbox | microVM | VM | WASM
```

The node agent discovers capabilities, receives Fleet assignments and leases,
prepares execution, attaches devices/storage, supervises workloads, reports
health, and reconciles state. Providers expose discovery, capabilities,
topology, allocation, preparation, attachment, detachment, health, and reset
operations. The node agent is an enforcement and execution component of Fleet,
not a pass-through agent for another scheduler.

Suggested reused runtimes include OCI-compatible execution, Cloud Hypervisor,
Firecracker, Wasmtime, and KVM/QEMU. Fleet does not make containerd mandatory
in the core path, but maintains interoperability.

## 10. Network, storage, and locality

The networking layer should provide workload routing, service load balancing,
identity, policy, observability, bandwidth/QoS, WireGuard, BGP, SR-IOV, and RDMA
awareness over Linux networking and eBPF/XDP. Sidecars are not mandatory.

Storage is provider-backed:

- content-addressed artifacts and local NVMe caches;
- local persistent storage as schedulable capacity;
- distributed providers with create, attach, detach, snapshot, clone,
  replicate, migrate, and health operations.

Initial integrations may include Ceph, NVMe-oF, NFS, cloud block storage, and
object storage. Placement accounts for residency, bandwidth, transfer cost,
replication, cache hotness, and checkpoint movement.

## 11. Compatibility and extensions

Native support targets OCI, CDI, OpenTelemetry, Linux/KVM, standard storage and
network protocols, and agent-initiated outbound node connectivity.

Fleet natively schedules and controls its supported workload classes. Optional
adapters/importers support Kubernetes manifests, Helm, Slurm, Flux, Ray, MPI,
and other systems. A virtual cluster is a Fleet lease that can host a nested
scheduler without exposing resources outside its allocation; it is an
interoperability path, not the core control-plane model.

Policy and scheduling extensions should prefer capability-limited WASM. Example
permissions:

```text
read.workloads
read.resources
propose.placement
```

Extensions needing host or device access use an explicit provider or external
plugin boundary.

## 12. Health, security, and correctness

Health is continuous state. Resources expose error history, reliability,
maintenance state, and failure-domain correlation in addition to readiness.

The system must handle node, device, rack, power, network, cell, and control-plane
failures. Degraded resources may remain usable for lower-priority workloads.
Preemption and restart are checkpoint-aware where the workload supports it.

Core invariants include:

- exclusive leases never overlap;
- stale ownership cannot write after fencing;
- allocated devices are reachable from the execution context;
- accepted intent converges or terminates explicitly;
- ownership transitions are atomic;
- volume-writer fencing is preserved.

Isolation choices range from trusted processes to containers, microVMs, VMs, and
WASM. Identity, authorization, secrets, node authentication, and auditability
are part of the control-plane contract.

## 13. API and implementation boundaries

The target Rust workspace is organized around stable ownership boundaries:

```text
api, types, resource-graph, lease, state, raft,
scheduler-core, scheduler-service, scheduler-batch, scheduler-ai,
planner-global, node-agent, runtime-oci, runtime-microvm, runtime-wasm,
device, network, storage, baremetal, telemetry, policy-sdk, simulator, cli
```

Pure resource and scheduling logic stays separate from transport. The simulator
executes the same scheduling code as production. Device/provider interfaces and
policy extensions remain replaceable without making the core a plugin marketplace.

## 14. Implementation roadmap

### v0 — resource and lease kernel

- typed graph and synthetic node registration;
- CPU, memory, NUMA, accelerator, PCIe, NIC, and NVMe resources;
- hard constraints, graph reduction, topology filtering, and scoring;
- atomic lease commit, release, revoke, fencing, and nested allocations;
- explainable placement;
- deterministic replay and simulated node failure;
- native process and OCI execution boundaries;
- CLI and simulator.

The first prototype registers 3–10 synthetic nodes, submits requests, chooses
candidate subgraphs, commits leases, releases/revokes them, creates nested
leases, explains decisions, replays state, and simulates failure.

### v1 — operational substrate

- gang scheduling, queues, reservations, fair share, and backfill;
- GPU/accelerator topology and CDI/provider attachment;
- microVMs;
- persistent-volume providers;
- health and fencing;
- data locality and artifact staging;
- network policy foundations;
- virtual clusters and Slurm/Flux compatibility experiments.

### v2 — AI/HPC and cells

- accelerator performance and goodput models;
- communication graphs and network contention;
- elastic AI scheduling;
- checkpoint/restart cost;
- accelerator partitioning/preemption;
- model/cache placement;
- global planning and cell federation.

### Later

CXL and disaggregated resources, TPU/NPU/FPGA providers, bare-metal lifecycle,
multi-region federation, energy optimization, and formal controller verification.

vLLM is a first runtime adapter. It is a valuable workload for validating
accelerator placement and node lifecycle, but not Fleet's defining abstraction.

## 15. Operator experience

Fleet must make resource decisions inspectable rather than exposing only a
successful placement:

```text
allocation placed on accelerator island A because:
+ requested topology is satisfied
+ model artifact is cached locally
+ RDMA path is below contention threshold
- alternate island has lower energy cost but worse restart locality
```

The CLI and UI should expose workloads, allocations, resources, leases, health,
capacity, data locality, cost, and failure history. Operators should be able to
answer why a workload ran where it did, why it waited, what resource it owns,
what will happen if a node fails, and how to recover it.

## 16. Licensing and commercial direction

Fleet is planned as an open-source project. The core resource, lease, scheduler,
node, and compatibility layers are the product. Commercial value may come from
enterprise governance, certified providers, signed/offline releases, support,
managed control planes, and advanced optimization.

The current private release plan is AGPL-3.0-or-later for the core and Apache
2.0 for SDKs, API schemas, examples, and adapter interfaces. See
`design/LICENSE_BOUNDARY.md`; legal and contributor-policy validation remain
required before public release.

## 17. Success criteria

Fleet succeeds when:

- a single VPS can run a useful profile without a mandatory service stack;
- a small bare-metal cluster does not require Kubernetes-like overhead;
- Fleet directly controls services, batch/HPC, and AI workloads through one
  resource model, while nested schedulers compose inside explicit leases;
- topology, locality, health, and failure domains influence placement;
- lease ownership remains correct across failures;
- compatibility layers do not dictate internal architecture;
- new resources such as CXL memory fit without redesigning scheduling;
- one workload/resource contract scales from one host to multiple cells.

## 18. Open questions

- graph storage and materialized placement indexes;
- exact lease, fencing, renewal, revocation, and nested-lease semantics;
- scheduler transaction boundaries and optimistic concurrency;
- dynamic topology, contention, and health representation;
- migration and reallocation safety;
- cell state and federation semantics;
- accelerator provider and device-preemption contracts;
- virtual-cluster networking, storage, identity, and security;
- which control invariants require model checking.
