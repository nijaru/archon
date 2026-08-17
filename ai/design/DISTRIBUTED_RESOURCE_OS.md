# Distributed Resource OS Architecture

**Status:** Fleet's current long-term architecture
**Updated:** 2026-08-17

The accepted v0 kernel contract is [`kernel-primitives.md`](kernel-primitives.md).
Implement `Cluster`, not `Cell`; `Node(kind=Machine)`, not `Host`; `Allocation`
as selected claims and `Lease` as committed authority. Older cell/host wording
in this document is planning language for that contract.

Fleet is a Rust-first distributed resource operating system. It represents a
datacenter as a graph of leaseable capabilities and places workloads against
that graph using constraints, objectives, policy, and failure state.

> A datacenter is a graph of leaseable capabilities. A workload is a set of
> constraints and objectives over that graph.

The system is intended to provide one resource, lease, scheduling, identity,
and control-plane model for services, batch jobs, HPC, distributed AI,
inference, native processes, OCI containers, microVMs, VMs, WASM, and future
composable resources such as CXL memory. It is not a Kubernetes reimplementation
and it does not replace mature workload runtimes or device software.

The deleted Go Fleet scaffold and its model-serving terminology are not an
implementation starting point. The target core is Rust-first. Inference and
private accelerator fleets remain important first workloads because they
exercise topology, cache, health, cost, and runtime interoperability, but
Fleet is not limited to inference or GPUs.

## 1. Product boundary

Fleet owns the resource operating-system layer:

- resource discovery and a typed resource graph;
- identity, authorization, leases, allocation, fencing, and reclamation;
- hierarchical placement and scheduling policy;
- node-level isolation and execution lifecycle;
- health, failure, recovery, and explainable decisions;
- cells, federation, and virtual-cluster boundaries;
- stable compatibility adapters for OCI, CDI, CSI/CNI, Slurm, Flux,
  Kubernetes, Ray, MPI, and other workload systems.

Fleet reuses mature lower-level substrate rather than replacing it:

- Linux, KVM, cgroups, namespaces, eBPF, and WireGuard;
- OCI runtimes, containerd where useful, Cloud Hypervisor, Firecracker, and
  Wasmtime;
- GPU and accelerator drivers, CDI, NCCL/RCCL, MPI, and RDMA stacks;
- Ceph, NVMe-oF, object stores, SPDK, and vendor storage/network systems.

Fleet itself is the primary resource-control and scheduling system for native
workloads. Existing workload runtimes such as vLLM, SGLang, Ray, PyTorch
distributed, JAX, MPI, and Slurm are execution integrations or explicitly
nested compatibility workloads—not authorities that Fleet delegates its core
scheduling model to. A nested runtime receives only its Fleet allocation and
cannot claim resources outside it.

## 2. Design principles

1. **Resource graph, not scalar nodes.** Topology, locality, bandwidth,
   latency, health, capability, cost, and failure domains are first-class.
2. **Lease/allocation is the universal primitive.** Services, jobs, VMs,
   devices, nested schedulers, and virtual clusters consume leases.
3. **Mechanism and policy are separate.** Discovery, state, fencing, and
   enforcement belong to the core; placement and optimization policies are
   replaceable modules.
4. **Scheduling is hierarchical.** Global planning, cell allocation, node
   resource management, and workload-local scheduling have distinct owners.
5. **Isolation is selectable.** Process, container, microVM, VM, and WASM are
   execution choices under one workload model.
6. **Topology is default behavior.** NUMA, PCIe, accelerator links, NICs, RDMA,
   storage paths, racks, power domains, and fabrics influence placement.
7. **AI and HPC are native workloads.** Gang scheduling, reservations,
   elasticity, fair share, goodput, communication patterns, checkpoints, and
   restart topology belong in policy rather than ad hoc integrations.
8. **Small deployments stay small.** A single VPS or small host set should not
   require a distributed service stack.
9. **Compatibility does not define the core.** Adapters ease migration without
   importing Kubernetes, Slurm, or cloud-IaaS internal abstractions.
10. **Reuse mature infrastructure.** Fleet should only replace a subsystem when
    a measured workload or correctness requirement justifies it.

## 3. Workload model

The public API accepts typed workload intent. A workload declares execution,
resource, placement, policy, trust, and lifecycle requirements without naming a
particular scheduler implementation.

Workload classes include:

- **Service:** replicas, rollout, health, autoscaling, routing, SLOs, spread,
  and anti-affinity.
- **Batch job:** queues, priority, fair share, reservations, deadlines,
  preemption, arrays, and backfill.
- **Distributed job:** gang allocation, synchronized start, worker elasticity,
  communication topology, and coordinated restart.
- **AI workload:** accelerator performance models, goodput, model/checkpoint
  locality, phase-specific resources, cache state, and restart cost.
- **VM or microVM:** CPU/memory/device leases, disks, network identity,
  passthrough, and migration where supported.
- **Nested allocation:** a bounded lease containing Slurm, Flux, Kubernetes,
  Ray, MPI, or another workload-local scheduler.

A workload may carry a preferred range rather than one fixed quantity:

```yaml
workers:
  min: 32
  preferred: 64
  max: 128
resources_per_worker:
  accelerator: 8
  cpu: 96
  memory: 1TiB
gang: true
network: rdma
```

The workload model remains stable while execution mode, runtime, accelerator
vendor, and placement policy change.

## 4. Resource graph

Fleet uses a typed property graph as the logical representation of capability.
The graph need not be a general-purpose graph database. Persisted control state,
materialized indexes, topology snapshots, and dynamic telemetry may use
separate representations while preserving one graph contract.

### Resource nodes

```text
Region
Datacenter
Cell
Rack
PowerDomain
Host
Socket
NUMANode
CPUCore
MemoryPool
PCIeRoot
Accelerator
AcceleratorPartition
NIC
Switch
Fabric
NVMe
StoragePool
CXLDevice
DataObject
```

### Resource edges

```text
contains
connected_to
same_numa
same_pcie_root
nvlink
nvswitch
rdma_path
storage_path
cxl_path
replica_of
cached_on
failure_correlated
power_correlated
```

Nodes and edges may expose:

```text
capacity
bandwidth
latency
oversubscription
health
reliability
cost
energy
temperature
driver/runtime capability
security/isolation capability
```

An accelerator is not merely `gpu = 8`. Its representation includes vendor,
architecture, memory, bandwidth, formats, partitioning, preemption level,
driver/runtime compatibility, PCIe and NUMA locality, links, power, and health.
The same provider model supports GPUs, TPUs, NPUs, FPGAs, DPUs, SmartNICs, and
future device classes.

Data and execution state are graph resources too:

- model weights;
- checkpoints and datasets;
- KV caches;
- OCI layers and compiled artifacts;
- replicas, snapshots, and storage allocations.

Useful relations include `cached_on`, `replicated_on`, `accessible_from`, and
transfer-cost annotations. This lets placement account for movement and
warm-state cost rather than treating every node as equivalent.

## 5. Allocation and lease model

The lease is the universal ownership and enforcement boundary.

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

A lease must define:

- exclusive and shareable ownership;
- acquisition, renewal, expiry, revocation, and release;
- fencing tokens for stale agents and writers;
- partial and gang allocation semantics;
- parent/child and nested-lease relationships;
- preemption and checkpoint requirements;
- recovery after node, cell, or control-plane failure;
- authorization and audit identity;
- explainable refusal and termination reasons.

A lease is not an informal scheduler reservation. The node agent and device,
network, and storage providers enforce the lease boundary. A nested scheduler
receives only its parent allocation and cannot allocate outside it.

## 6. Scheduling hierarchy

Fleet does not require one monolithic scheduler.

```text
GLOBAL PLANNER       seconds → hours
        ↓
CELL ALLOCATOR       milliseconds → seconds
        ↓
NODE RESOURCE MGR    microseconds → milliseconds
        ↓
WORKLOAD SCHEDULER   domain-specific
```

### Global planner

Owns region and cell placement, large reservations, capacity planning, data
placement, cost, energy, and global failure domains.

### Cell allocator

Owns local services and jobs, queues, fair share, reservations, backfill,
gang scheduling, topology, accelerator placement, local storage/network
constraints, and cell-level preemption.

### Node resource manager

Owns cgroups/cpusets, NUMA and memory policy, accelerator partitions, device
attachment, NIC queues, local NVMe QoS, and device-level scheduling or
preemption.

### Workload-local scheduler

A workload-local scheduler is optional and runs inside a Fleet lease when a
workload requires it. Examples include Slurm, Flux, Ray, MPI, PyTorch/JAX
distributed runtimes, and application-specific schedulers. Native Fleet
scheduling remains the primary control path for workloads it supports directly;
nesting is an interoperability and composition feature.

Placement uses a staged path:

```text
request
  → hard constraints
  → candidate graph reduction
  → topology filter
  → fast scoring
  → atomic lease commit
  → background optimization
  → optional migration/reallocation
```

The objective may combine fragmentation, latency, network contention, data
movement, energy, monetary cost, reliability risk, and checkpoint/restart cost.
Policies set weights and hard constraints. A giant solver is not on the critical
path for every placement; asynchronous optimization can improve committed
placements later when migration is permitted.

Every decision should be explainable:

```text
selected accelerator island because:
+ same NVLink fabric
+ model already cached
+ RDMA path is not oversubscribed
- higher power cost than the alternate island
```

## 7. Control plane and cells

The control plane separates authoritative intent/state from telemetry:

```text
Intent
  → typed API
  → command/transaction log
  → replicated state machine
  → materialized indexes
  → planners and reconcilers
```

A cell owns local nodes, leases, scheduling, failure handling, and services.
The global layer owns identity, policy, cross-cell placement, federation, and
global reservations. Cells continue operating during temporary global-plane
loss under explicit policy.

Per-cell state should provide:

- versioned reads and transactional intent changes;
- snapshots and replay;
- atomic ownership transitions;
- lease fencing and reconciliation;
- explicit ambiguous-outcome handling;
- deterministic simulator replay;
- bounded indexes for placement queries.

Telemetry does not belong in authoritative control state. Logs, metrics, traces,
profiling, and hardware streams use separate systems and are correlated by
resource, workload, allocation, and generation identifiers.

## 8. Node OS and execution

The node is a minimal immutable Linux system:

```text
Linux kernel
node agent
execution runtimes
KVM
eBPF programs
device drivers
network/storage tooling
atomic updater
```

The node agent discovers capabilities, receives leases, prepares execution,
attaches devices and storage, supervises workloads, reports health, and enforces
reconciliation. It should support:

```text
process
OCI container
sandbox
microVM
VM
WASM
```

Suggested runtimes are OCI-compatible execution, Cloud Hypervisor or
Firecracker for microVMs, Wasmtime for WASM, and KVM/QEMU or Cloud Hypervisor
for full VMs. Runtime choice is a workload property, not a separate
orchestration product.

Node and provider interfaces should cover:

```text
discover
capabilities
topology
allocate
prepare
attach
detach
health
reset
```

The accelerator provider layer remains vendor-neutral in the core. CDI, VFIO,
SR-IOV, vendor APIs, and device-specific control paths belong in providers.

## 9. Network, storage, and data locality

Fleet should expose one native networking control surface over Linux networking,
eBPF/XDP, WireGuard, BGP, SR-IOV, and RDMA-aware policy. It should not require
service-mesh sidecars for the core workload path.

Storage is a provider boundary rather than a filesystem rewrite:

- content-addressed artifacts and local NVMe caches;
- local persistent NVMe/SSD as schedulable resources;
- provider-backed distributed volumes through create, attach, detach,
  snapshot, clone, replicate, migrate, and health operations.

Initial providers may include Ceph, NVMe-oF, NFS, cloud block storage, and
object stores. The scheduler accounts for locality, transfer cost, bandwidth,
replication, cache hotness, and checkpoint movement.

## 10. Compatibility and extensions

Native contracts should support OCI, CDI, OpenTelemetry, Linux/KVM, standard
storage/network protocols, and agent-initiated node connectivity.

Fleet natively schedules and controls its supported workload classes. Migration
adapters may import Kubernetes manifests and Helm workloads, or run Slurm,
Flux, Kubernetes, Ray, or MPI inside a virtual-cluster lease. Compatibility is
an adoption and composition path; it does not make an incumbent scheduler the
internal authority or reduce Fleet to a wrapper around it.

Policy and scheduler extensions should preferably be capability-limited WASM
components. An extension may receive permissions such as:

```text
read.workloads
read.resources
propose.placement
```

Host/device access requires an explicit external provider or plugin boundary.

## 11. Security and failure model

Identity, authorization, lease ownership, and fencing are core mechanisms.
The design must account for:

- trusted and multi-tenant workloads;
- process, container, microVM, VM, and WASM isolation levels;
- secrets and credential attachment;
- node identity and outbound agent authentication;
- stale agents and split-brain prevention;
- correlated hardware, rack, power, and network failures;
- degraded resources that remain usable for lower-priority workloads;
- checkpoint-aware preemption and restart.

Health is continuous state, not a binary node flag. A resource may expose health,
error history, predicted reliability, maintenance state, and failure-domain
correlation.

Core invariants must be executable and eventually model-checked where the
control boundary justifies it:

- exclusive leases never overlap;
- stale ownership cannot write after fencing;
- allocated devices are reachable from the execution context;
- accepted intent converges or terminates with an explicit failure;
- ownership transitions are atomic;
- volume-writer fencing is preserved.

## 12. Implementation sequence

The full architecture is intentionally staged.

### v0: prove the abstraction

Build the pure control/resource kernel and simulator before broad runtime
integration:

- typed resource graph;
- allocation and lease semantics;
- node registration and synthetic topology;
- hard constraints, candidate reduction, scoring, and explanations;
- atomic lease commit, release, revoke, and nested leases;
- deterministic replay and simulated node failure;
- native process and OCI execution boundary;
- CPU, memory, NUMA, accelerator, PCIe, NIC, and NVMe resources;
- CLI and a small-cell control process.

Required prototype: register 3–10 synthetic nodes, submit allocation requests,
select candidate subgraphs, commit leases atomically, release/revoke leases,
create a nested lease, explain placement, replay state deterministically, and
simulate node failure.

### v1: operational substrate

Add:

- gang scheduling, queues, reservations, fair share, and backfill;
- accelerator topology and CDI/provider attachment;
- microVM execution;
- persistent-volume provider boundary;
- node health and resource fencing;
- data locality and artifact staging;
- eBPF/network policy foundations;
- virtual clusters and Slurm/Flux compatibility experiments.

### v2: heterogeneous AI/HPC policy

Add only after the core lease and placement model is stable:

- accelerator performance and goodput models;
- communication-graph and network-contention inputs;
- elastic AI scheduling;
- checkpoint/restart cost;
- accelerator partitioning and preemption;
- model/cache placement;
- cell and global planners.

### Later

CXL and disaggregated resources, TPU/NPU/FPGA providers, bare-metal lifecycle,
multi-region federation, energy optimization, and formal verification of
critical controllers.

Inference through vLLM is a first execution adapter and a useful accelerator
workload. It does not define the resource model or the product boundary.

## 13. Open design questions

1. Which graph representation gives fast placement indexes without turning the
   control plane into a general graph database?
2. What exact semantics cover nested, partial, renewable, revocable, and
   checkpoint-aware leases?
3. Where are transaction boundaries between global planning, cell allocation,
   and node enforcement?
4. Which topology edges are static discovery and which are dynamic contention
   or health signals?
5. When can migration improve placement without destabilizing workloads?
6. Which state store and consensus design support cells without centralizing all
   telemetry?
7. How are device-level preemption and reset exposed across vendors?
8. How are virtual-cluster networking, storage, and identity isolated?
9. Which invariants deserve model checking before production use?

## 14. Success criteria

The architecture is succeeding when:

- a single VPS can run a useful local profile without a service stack;
- a small bare-metal cluster does not require Kubernetes-like operational
  overhead;
- Fleet directly controls services, batch/HPC, and AI workloads through one
  resource model, while nested schedulers compose inside explicit leases;
- heterogeneous accelerator topology influences placement;
- leases and failure recovery remain correct during node and control-plane loss;
- compatibility layers do not dictate internal architecture;
- new resources such as CXL memory fit without redesigning scheduling;
- the same workload/resource contract scales from one host to multiple cells.

## 15. Product and licensing direction

Fleet is planned as an open-source infrastructure product. The core value is
the resource authority, lease, scheduling, node, and compatibility system;
commercial value may come from enterprise governance, certified providers,
offline and signed releases, operational support, managed control planes, and
advanced optimization. The current private release plan is AGPL-3.0-or-later
for the core and Apache-licensed SDK/schema boundaries; see
`LICENSE_BOUNDARY.md` for the release checklist.
