# Fleet Compute OS — Second-Pass Research Memo

**Date:** 2026-08-16  
**Scope:** Fixed. Fleet remains a general-purpose open-source Rust Compute OS spanning VPSs, bare metal, datacenters, services, batch/HPC, AI, VMs, containers, networking, storage, and heterogeneous accelerators.  
**Purpose:** Sharpen the architecture, identify actual novelty, and falsify weak assumptions before implementation.

## Executive conclusions

**Established:** No current open system cleanly implements the complete Fleet thesis. The closest ideas are distributed across several systems:

- **Mesos** is historically closest to the idea of a thin substrate allocating resources to multiple higher-level frameworks.
- **Flux/Fluxion** is closest to Fleet's proposed hierarchical allocation and graph-resource model.
- **Twine and Hydra** provide some of the strongest production evidence for regional-scale sharding/federation.
- **OpenStack** covers the broadest infrastructure surface—compute, VMs, networking, storage, and bare metal—but through multiple service-specific control planes rather than one resource authority.
- **NVIDIA Mission Control + Run:ai/KAI + Slurm** is currently the closest commercial convergence of AI/HPC infrastructure management, but it remains a composition of Kubernetes, Run:ai, Slurm, BCM, fabric management, and associated systems rather than a unified substrate.
- **Kubernetes DRA** is moving Kubernetes toward richer resource claims and hardware-independent device management, validating part of Fleet's direction while also illustrating the complexity of retrofitting it into the Pod architecture.

**Recommendation:** Fleet should own the **resource authority layer**:

```text
resource identity
+ typed topology/capability model
+ lease semantics
+ fencing epochs
+ placement transactions
+ hierarchical delegation
+ scheduler framework
+ node enforcement
+ provider protocol
+ virtual clusters
+ correctness model
```

It should **not** own every underlying implementation of storage, networking, virtualization, or accelerator execution.

**Hypothesis:** The potentially novel contribution is not any single scheduler algorithm. It is a coherent, formally specified resource substrate that can expose Kubernetes-like services, Slurm-like jobs, Flux-like nested allocations, OpenStack-like infrastructure resources, and modern AI accelerator scheduling without requiring those systems to be stacked together.

---

# 1. Closest systems to the full Fleet vision

## 1.1 Apache Mesos — closest historical abstraction

Mesos was explicitly designed as a thin resource-sharing layer beneath multiple distributed frameworks. Its master offered CPU, memory, storage, and other resources to framework-specific schedulers rather than imposing one application scheduler. Mesos also supported pluggable resource isolation and container execution.

Conceptually:

```text
             Mesos resource substrate
                /       |       \
             Spark    Marathon   MPI/etc.
```

That is significantly closer to Fleet's intended role than Kubernetes's model:

```text
Kubernetes
    ↓
Pod
    ↓
everything adapted into Pod semantics
```

**Fleet should recover the good Mesos idea:** higher-level runtimes can receive resources without the substrate needing to understand their entire execution model.

**But improve it:** Mesos resource offers were primarily node/resource quantities. Fleet should allocate **typed subgraphs with enforceable leases**, including topology, devices, fabrics, state locality, and nested delegation.

---

## 1.2 Flux / Fluxion — closest technical foundation

Flux is probably the single project Fleet should study most deeply.

Flux supports fully hierarchical resource management: an allocation can launch another Flux instance which receives a subset of its parent's resources and becomes a resource manager itself. This recursion can continue, and each instance can use its own scheduling configuration.

Fluxion separately provides graph-based resource scheduling, specifically to represent relationships among increasingly heterogeneous HPC resources.

That maps closely to:

```text
Fleet Cell
    ↓ lease
Virtual Cluster
    ↓ lease
Workflow
    ↓ lease
Application-local scheduler
```

**Established:** hierarchical/nested resource allocation and graph scheduling are prior art.

**Potential Fleet contribution:** generalize them beyond HPC to services, VMs, storage, networking, heterogeneous accelerators, bare metal, and geographically partitioned infrastructure.

---

## 1.3 Twine — strongest large-scale cluster-manager precedent

Meta reports that Twine can use a single regional control plane across roughly one million machines and that it orchestrates containers across millions of servers. Its design avoids requiring users to bind applications permanently to individual clusters.

Meta's broader infrastructure is nevertheless divided into specialized systems: Twine for compute orchestration, Tectonic for storage, Shard Manager for distributed data placement, Delos for global infrastructure control, and others.

This is important.

It demonstrates both:

```text
regional-scale orchestration is possible
```

and:

```text
one universal global state machine is not how hyperscale
infrastructure has generally evolved
```

Fleet should therefore unify **resource semantics**, not necessarily implementation of every distributed subsystem.

---

## 1.4 Microsoft Hydra / Apollo — important scheduling architecture

Apollo used distributed scheduling with loosely coordinated global information and considered future resource availability rather than only current availability. It was deployed at Microsoft on clusters with tens of thousands of machines.

Hydra later used a federated model consisting of loosely coordinating subclusters, delegating actual task placement while centrally coordinating tenant resource shares. Microsoft reported Hydra scheduling nearly one trillion tasks in its production data-lake environment.

This is strong evidence for:

```text
global policy
      ↓
delegated capacity
      ↓
local scheduler
```

rather than:

```text
every placement
      ↓
global consensus/scheduler
```

Fleet cells should follow the former.

---

## 1.5 OpenStack — closest infrastructure breadth

OpenStack already spans:

```text
VMs
bare metal
networking
block storage
images
identity
placement
```

Ironic can provision physical machines either with Nova or independently, while Nova, Neutron, and other OpenStack services compose the broader cloud.

Its limitation relative to Fleet is architectural fragmentation:

```text
Nova
Neutron
Cinder
Ironic
Glance
Keystone
...
```

each owns a domain-specific state model and protocol.

Fleet's goal should **not** be "rewrite OpenStack."

It should instead make these categories of resources available through one resource authority:

```text
Resource Graph
       ↓
Allocation
       ↓
bindings:
    compute
    device
    network
    storage
```

---

## 1.6 Nomad — closest operational philosophy

Nomad supports service, batch, system, and sysbatch workloads and can execute Docker containers, isolated executables, Java processes, QEMU guests, and other task drivers. It also models devices such as GPUs, FPGAs, and TPUs as scheduling requirements.

Nomad therefore demonstrates that:

> one scheduler does not need to equate one workload with one container runtime.

Its gap is that its core resource model is still much less expressive than the proposed Fleet graph, and it does not attempt to unify HPC scheduling, fabric topology, infrastructure provisioning, storage authority, and heterogeneous AI scheduling.

Fleet should retain **Nomad-like operational simplicity**.

---

## 1.7 Kubernetes — strongest ecosystem, increasingly relevant resource model

Kubernetes DRA has become substantially more capable. Its current model supports ResourceClaims, device classes, sharing and consumable capacity, partitionable devices, health information, external device readiness/binding conditions, and increasingly CPU/memory-related integration.

The especially relevant architectural lesson is that Kubernetes already separates:

```text
scheduler chooses resource
        ↓
driver prepares resource
        ↓
kubelet grants workload access
```

External controllers can participate in attachment/readiness before a workload is finally bound.

That is close to the provider model Fleet needs.

The difference is that Fleet can make the model foundational rather than fitting it underneath Pods.

---

## 1.8 NVIDIA Mission Control / Run:ai / KAI / Slurm

This is the strongest strategic validation.

NVIDIA announced its acquisition of Run:ai in 2024; Run:ai is explicitly Kubernetes-based. NVIDIA then open-sourced its KAI scheduling core under Apache 2.0 in 2025. KAI implements mechanisms including hierarchical queues, gang scheduling, consolidation, reclaim, and preemption.

NVIDIA acquired SchedMD, the primary developer of Slurm, in December 2025 and committed to continuing Slurm as open-source, vendor-neutral software.

Current DGX SuperPOD/Mission Control documentation supports **Slurm, Run:ai, or both**. Run:ai itself operates on Kubernetes, while Mission Control incorporates broader cluster deployment, monitoring, recovery, and fabric-management functions.

That gives us:

```text
                 Mission Control
                       │
         ┌─────────────┴─────────────┐
         │                           │
       Slurm                     Kubernetes
                                     │
                                   Run:ai
                                     │
                                    KAI
```

That is convergence at the product level, but **not convergence at the abstraction level**.

Fleet's opportunity is the latter.

---

# 2. What is established versus genuinely underexplored?

## Well-established prior art

Fleet should not claim novelty for:

- declarative desired-state control;
- reconciliation;
- resource reservations;
- quotas and fair sharing;
- backfill scheduling;
- gang scheduling;
- preemption;
- resource offers;
- nested schedulers;
- hierarchical scheduling;
- graph-based resource representation;
- topology-aware placement;
- heterogeneous GPU scheduling;
- goodput-aware AI scheduling;
- NUMA-aware allocation;
- container/microVM/VM isolation;
- accelerator partitioning;
- cell/federated schedulers;
- data-locality-aware placement;
- formal verification of controllers.

Flux, Mesos, Borg/Omega, Slurm, Kubernetes, Gavel/Pollux/Sia, Anvil, and others provide substantial precedent for these individually.

## More interesting / underexplored combination

The less-explored design space is:

```text
typed physical + virtual resource graph
                 +
unified ownership/lease semantics
                 +
resource-specific fencing
                 +
hierarchical delegation
                 +
service + batch + AI scheduling
                 +
containers + VMs + native execution
                 +
storage/network/device attachment
                 +
single VPS → hyperscale cells
```

I would frame Fleet's novelty as **semantic integration**, not algorithmic novelty.

A particularly interesting research question is whether one lease model can define consistent ownership across resources whose enforcement mechanisms are fundamentally different.

That leads directly to the first critical correction.

---

# 3. Is a universal lease actually sufficient?

## Answer

**Yes as a semantic primitive. No as a physical protocol.**

Define:

```text
Lease
    ├── ownership
    ├── authority
    ├── lifetime
    ├── priority
    ├── epoch
    ├── parent
    └── Bindings[]
```

Then:

```text
Binding::Compute
Binding::Accelerator
Binding::Storage
Binding::Network
Binding::Memory
```

Each binding uses its own resource-specific enforcement protocol.

### Why?

A distributed lock or lease alone cannot stop a stale owner from acting on an external resource.

Chubby explicitly addressed this using **sequencers containing a lock generation number**. A server receiving an operation could verify that the sequencer was still current and reject requests from stale lock holders.

That is the model Fleet should generalize.

## Proposed Fleet semantics

```text
Lease {
    lease_id
    authority_cell
    epoch
    owner
    parent
    policy
    bindings[]
}
```

Each binding receives:

```text
Binding {
    resource_id
    lease_id
    fencing_epoch
    provider
    state
}
```

For example:

```text
ComputeBinding
    → node executor validates epoch

GpuBinding
    → device broker validates ownership generation

StorageBinding
    → storage provider establishes writer fencing

NetworkBinding
    → network provider attaches route/VIP with generation

VmBinding
    → hypervisor accepts only current incarnation
```

### Critical rule

```text
lease expiry != resource fencing
```

A lease becoming invalid in the control plane is insufficient unless the resource endpoint rejects stale actors.

Therefore Fleet needs:

> **one lease API, multiple fencing implementations.**

Kubernetes DRA is converging on a related separation: the scheduler owns resource selection while drivers and external controllers perform preparation, binding, status, and device-specific actions.

## Do not use distributed 2PC across every resource

A workload may require:

```text
CPU
GPU
network attachment
volume
```

Trying to perform one classical distributed transaction across Linux, a SAN, a switch, and a GPU driver would be fragile.

Prefer:

```text
PLAN
 ↓
RESERVE logical resources
 ↓
PREPARE bindings
 ↓
COMMIT lease
 ↓
ACTIVATE
```

with:

- idempotent operations;
- monotonically increasing epochs;
- explicit compensation;
- fail-closed exclusive resources;
- durable reconciliation.

This is effectively a strongly committed control-plane intent plus provider-specific sagas.

---

# 4. Can the resource graph scale?

## Answer

**Yes, provided “resource graph” describes the data model rather than the storage architecture.**

Do **not** build:

```text
one gigantic distributed graph database
containing every core
every GPU
every route
every metric
every cache line
every telemetry update
```

Instead use:

```text
GLOBAL
  summarized resource graph
        │
        ▼
CELL
  schedulable topology graph
        │
        ▼
NODE
  detailed hardware graph
```

Flux already provides a useful divide-and-conquer precedent: nested instances own subsets of resources and schedule them independently, while Fluxion performs graph-based scheduling inside those scopes.

Hydra similarly scaled by delegating placement to subclusters while retaining centralized control of aggregate tenant shares; Twine independently demonstrates aggressive sharding at regional scale.

## Split the model into three classes of state

### 1. Topology

Slow-changing:

```text
CPU → NUMA
GPU → PCIe root
GPU ↔ NVLink
NIC → fabric
host → rack
rack → power domain
NVMe → host
```

Persist and index this.

### 2. Allocation state

Strongly consistent:

```text
free
reserved
leased
binding
active
draining
revoked
```

This belongs in the cell state machine.

### 3. Telemetry

High-rate and approximate:

```text
GPU temperature
NIC utilization
queue depth
CPU pressure
storage latency
power draw
error rates
```

**Do not replicate this through Raft.**

Feed summarized/staleness-tagged telemetry into scheduling views.

## Scheduler representation

The scheduler does not execute arbitrary graph queries.

Compile common predicates into indexes:

```text
gpu.vendor == nvidia
gpu.memory >= 80GiB
same_nvlink_domain(count=8)
same_numa(cpu, gpu)
rdma_path.bandwidth >= X
dataset.cached == true
```

Candidate selection becomes:

```text
indexed filter
→ small candidate subgraphs
→ topology matching
→ scoring/optimizer
```

This makes the graph a richer equivalent of a kernel hardware topology model, rather than Neo4j for a datacenter.

---

# 5. Scheduling architecture

The second-pass research supports four levels.

```text
GLOBAL RESOURCE PLANNER
          ↓
      CELL SCHEDULER
          ↓
 NODE RESOURCE MANAGER
          ↓
WORKLOAD-LOCAL SCHEDULER
```

## Global planner

Own:

```text
cell capacity
region placement
delegated quotas
reservations
cost
energy
large topology islands
global data placement
```

Do not place ordinary individual tasks globally.

## Cell scheduler

Own:

```text
admission
services
batch
gang jobs
queues
fairness
backfill
topology
accelerators
local data
preemption
```

## Node resource manager

Own:

```text
cgroups
cpusets
NUMA
memory
GPU partitions
device attachment
NIC queues
local storage QoS
runtime
```

## Workload-local scheduler

Allow:

```text
Flux
Slurm
Ray
MPI
PyTorch
custom scheduler
Fleet nested scheduler
```

to schedule within delegated resources.

This directly combines lessons from Flux hierarchy, Mesos frameworks, and Hydra-style delegation.

---

# 6. State-of-the-art allocation research to incorporate

## Heterogeneous AI

Alibaba's OSDI 2026 production study covers a six-month trace containing **155,410 GPUs across multiple vendors and generations**. It finds significant unusable capacity arising from fragmented placement, CPU matching, network-locality restrictions, and operational headroom—not merely fractional GPU slicing. Alibaba reports improving GPU allocation from 68% to 93% with its preemption-cost-aware SpotGPU mechanism, and has released the trace for research.

This argues strongly for Fleet's typed multi-resource topology model.

The scheduler needs to optimize:

```text
CPU
accelerator
accelerator type
accelerator topology
host memory
network
job elasticity
preemption cost
```

together.

## Accelerator-local scheduling

XSched generalizes preemptive scheduling across GPUs, NPUs, ASICs, and FPGAs using a preemptible command-queue abstraction and a multi-level hardware model.

Fleet should therefore distinguish:

```text
cluster allocation
```

from:

```text
device execution scheduling
```

A provider may implement both.

## Optimization

Firmament demonstrated globally informed cluster placement formulated as min-cost flow. More recently, OSDI 2025's DeDe shows that resource-allocation optimization can often be decomposed into parallel resource-side and demand-side subproblems, including cluster scheduling, traffic engineering, and load balancing.

Fleet should therefore support:

```text
FAST PATH
constraint filters
→ heuristic/scored placement

OPTIMIZER
→ improve placement asynchronously
→ reservation planning
→ defragmentation
→ migration suggestions
```

rather than choosing between "heuristics" and "optimization."

---

# 7. Strongest correctness model

This should be one of Fleet's primary differentiators.

## Per-cell authority

Every resource has exactly one authoritative cell at any given epoch.

```text
Resource
    owner_cell
    authority_epoch
```

Every allocation-changing operation must go through the quorum-owning cell.

The cell's allocation state should be **linearizable**.

## Fencing

Every exclusive lease receives a monotonically increasing fencing epoch.

```text
lease 91 → epoch 184
lease 92 → epoch 185
```

A delayed operation from epoch 184 must fail once 185 exists.

This is the generalized Chubby sequencer model.

## Agent incarnation

Each node boot gets:

```text
NodeIncarnationId
```

Therefore:

```text
node-42 / incarnation A
```

cannot return after a network partition and accidentally resume authority granted before:

```text
node-42 / incarnation B
```

replaced it.

## Cell partition

On loss of quorum:

```text
existing local execution:
    may continue according to lease policy

new exclusive allocation:
    forbidden

unsafe failover:
    forbidden
```

This intentionally chooses correctness over allocation availability for exclusive resources.

Network partitions are not hypothetical edge cases; empirical research into distributed systems has found that partition-handling defects can cause severe failures and should be explicitly fault-injected during testing.

## Global-plane loss

The global plane should **not own ordinary execution leases**.

Instead:

```text
Global
  delegates capacity/budget
        ↓
Cell
  makes authoritative allocations
```

If global disappears:

```text
running workloads continue
cell scheduling continues within delegated rights
cross-cell rebalance stops
global policy changes stop
```

## Global fairness vs availability

There is an unavoidable tradeoff.

Fleet cannot simultaneously provide:

```text
perfect instantaneous global quota enforcement
```

and:

```text
independent cell scheduling during arbitrary partitions
```

Recommendation:

```text
global plane delegates quota credits / reservations
```

Cells spend only delegated capacity.

That bounds divergence without putting the global plane in every placement.

This resembles Hydra's division between global share coordination and delegated subcluster placement.

## Controller correctness

Anvil demonstrates formal verification of Rust reconciliation controllers against an "eventually stable reconciliation" liveness specification. Kivi demonstrates exhaustive model checking of controller/configuration interactions and interleavings.

Fleet should use both techniques:

```text
formal/specification proofs
    for core invariants

model checking
    for interacting state machines

deterministic simulation
    for scale/failure behavior

fault injection
    for real implementation
```

---

# 8. What Fleet should own

## Build

### Resource schema

Fleet owns the canonical representation of:

```text
compute
memory
accelerators
storage
network
fabric
data
failure domains
power
capabilities
```

### Lease and fencing semantics

This is the kernel-level ownership contract.

### Placement transaction engine

Own:

```text
plan
reserve
prepare
commit
activate
revoke
release
```

### Scheduler framework

Own:

```text
constraints
candidate generation
topology matching
scoring
queues
fairness
backfill
gang scheduling
reservations
preemption
optimizer interface
```

### Cell state machine

Own allocation authority and consensus state.

### Node executor

Own the enforcement boundary between Fleet and Linux/runtime/device providers.

### Provider ABI/protocol

For:

```text
runtime
device
storage
network
bare metal
fabric
```

### Virtual clusters / nested leases

This is central to migration and extensibility.

### Simulation and explanation engine

The production scheduler and simulator should ideally execute the same core scheduling library.

### Security identity model

Resource and workload identity must be intrinsic.

---

# 9. What Fleet should reuse

Do not rewrite:

```text
Linux kernel
KVM
cgroups v2
namespaces
seccomp
eBPF kernel facilities
WireGuard
OCI image format
vendor accelerator drivers
CUDA/ROCm/etc.
NCCL/RCCL
RDMA stack
NVMe / NVMe-oF
SPDK
Ceph
Redfish
```

Candidate runtime implementations:

```text
OCI      → youki-compatible Rust path
microVM  → Cloud Hypervisor / Firecracker
WASM     → Wasmtime
VM       → Cloud Hypervisor; QEMU where necessary
```

Fleet owns the **policy and lifecycle above these components**, not their internal data planes.

For storage, for example:

```text
Fleet:
    who gets the volume?
    where should workload run?
    what fencing epoch applies?
    when attach/detach?
    what locality/cost exists?

Ceph/NVMe/cloud provider:
    actually store and replicate bytes
```

That distinction is essential to keeping the project feasible.

---

# 10. One VPS to hyperscale without separate products

Use the same core protocols and objects at every scale.

## One VPS

```text
fleetd
├ control role
├ scheduler role
├ node role
└ executor role
```

One local state-machine member.

No mandatory:

```text
3-node etcd
external scheduler
separate CNI controller
service mesh
CSI controller stack
```

Capabilities are discovered.

No KVM?

```text
microvm = unavailable
```

No GPU?

```text
accelerator resources = []
```

The API is unchanged.

## Several VPSs

```text
3 fleetd control members
N executor nodes

one cell
```

Same resource model.

## Datacenter

```text
Cell
├ 3/5 control members
├ scheduler workers
└ thousands of executors
```

Add providers:

```text
BGP
RDMA
Ceph
bare metal
accelerators
```

## Hyperscale

```text
                  Global Plane
                       │
          delegated capacity/policy
        ┌──────────────┼──────────────┐
        ▼              ▼              ▼
      Cell A         Cell B         Cell C
        │              │              │
     nodes          nodes          nodes
```

Global topology contains summaries.

Cell topology contains schedulable detail.

Node topology contains hardware detail.

The user still submits:

```text
Service
Job
DistributedJob
VM
Allocation
```

There is no "Fleet Enterprise Datacenter product" with a different architecture.

---

# 11. Hardest contradictions and risks

## Risk 1 — universal lease versus physical semantics

Resolved only if Fleet separates:

```text
logical ownership
```

from:

```text
resource-specific fencing
```

Do not pretend every provider has equivalent revocation semantics.

---

## Risk 2 — atomic heterogeneous placement

Allocating:

```text
GPU + CPU + volume + IP + RDMA endpoint
```

crosses multiple external systems.

Strict distributed atomicity will often be impractical.

Fleet needs a carefully specified prepare/commit/reconcile protocol.

This may become one of the hardest engineering areas.

---

## Risk 3 — resource graph churn

A graph containing topology is useful.

A graph containing every telemetry sample is disastrous.

Keep authoritative topology/allocation state and telemetry separate.

---

## Risk 4 — global fairness versus partition tolerance

This is a real distributed-systems tradeoff, not something an algorithm eliminates.

Use delegated budgets/credits.

---

## Risk 5 — one substrate becoming lowest-common-denominator

Services and HPC jobs genuinely have different semantics.

Do not unify them into one generic "Workload" object with hundreds of optional fields.

Instead:

```text
Service
BatchJob
DistributedJob
VM
Function
```

compile to:

```text
AllocationPlan
```

The lease substrate is universal.

The user-facing workload semantics are not.

---

## Risk 6 — preemption isn't universal

CPU shares can be reclaimed easily.

A GPU training job may require a checkpoint.

A storage writer may require fencing.

A VM may require migration or termination.

A network allocation may require route convergence.

Therefore every resource/provider advertises:

```text
RevocationCapability {
    immediate
    graceful
    checkpointable
    migratable
    non_preemptible
}
```

---

## Risk 7 — nested schedulers hide information

A child scheduler may know things the parent doesn't.

That is acceptable.

Parent authority should enforce:

```text
you may use exactly this resource subgraph
```

not:

```text
I must understand every task you schedule
```

This is essentially the strongest part of the Mesos/Flux idea.

---

## Risk 8 — becoming a storage/network project

Fleet should orchestrate those resources without attempting to invent a new Ceph, RDMA transport, network switch OS, and hypervisor simultaneously.

The scope remains broad.

The **implementation boundary remains disciplined**.

---

# 12. Benchmark and simulation suite

"State of the art" should be a testable claim.

Build the benchmark suite alongside the scheduler, not afterward.

## A. Deterministic Fleet simulator

Run the real:

```text
resource model
allocation engine
scheduler policies
cell logic
```

against simulated executors/providers.

Support simulated clusters of:

```text
1
10
100
1,000
10,000
100,000
1,000,000 nodes
```

without requiring real hardware.

---

## B. Scheduler scalability

Measure:

```text
placements/sec
p50/p95/p99 scheduling latency
memory/resource
graph-index size/resource
state update throughput
scheduler CPU cost
```

Sweep:

```text
node count
resource heterogeneity
graph complexity
pending queue length
scheduler concurrency
```

Compare where meaningful against:

```text
Kubernetes
Slurm
Flux
Nomad
```

Do not claim one global winner because they solve different subsets.

---

## C. Placement quality

Measure:

```text
CPU fragmentation
GPU fragmentation
NUMA violations
NVLink locality
network oversubscription
data movement
rack concentration
failure-domain concentration
power cost
```

Use identical synthetic workload sets for every policy.

---

## D. HPC

Measure:

```text
queue wait
bounded slowdown
makespan
utilization
fairness
backfill effectiveness
reservation satisfaction
gang-start latency/skew
```

Baseline:

```text
Slurm
Flux
```

---

## E. AI

Measure:

```text
GPU allocation ratio
goodput
job completion time
checkpoint waste
preemption cost
communication contention
model-placement cost
elastic scaling efficiency
```

Alibaba's newly released 155,410-GPU ASI trace provides a particularly valuable modern workload trace for this category.

Baseline where appropriate:

```text
KAI
Kubernetes scheduler
Slurm
Flux
research policies
```

---

## F. Service workloads

Measure:

```text
deployment latency
autoscaling latency
recovery latency
rolling-update disruption
tail request latency
placement stability
```

---

## G. Runtime overhead

Measure independently:

```text
native
container
microVM
VM
WASM
```

for:

```text
startup
memory
CPU overhead
network
I/O
checkpoint/restore
```

This prevents scheduler results being confused with isolation/runtime results.

---

## H. Correctness suite

This is mandatory.

Inject:

```text
leader crash
minority partition
cell partition
global-plane partition
agent pause
agent restart
stale agent return
duplicated RPC
lost RPC
reordered RPC
clock skew
node power loss
device disappearance
storage timeout
partial binding
provider crash
route-programming failure
```

Assert invariants after every generated execution.

Examples:

```text
no two exclusive active leases overlap

no stale epoch performs a protected operation

no node incarnation can reclaim prior authority

no resource disappears from accounting

every committed allocation reaches:
    Active
    or explicit terminal failure

every partial binding is eventually:
    committed
    or compensated
```

This should become a competitive feature of Fleet itself.

---

# 13. Current state-of-the-art signals Fleet should track

Several 2025–2026 results reinforce the architecture:

**DeDe:** scalable decomposition rather than one giant optimizer.

**Alibaba ASI:** real AI fleets are heterogeneous across accelerator vendors/generations and resource/locality mismatches dominate allocation efficiency.

**Kubernetes DRA 1.36:** increasingly generic dynamic resources, partitionable devices, consumable capacity, health, binding conditions, and even movement toward CPU/memory integration.

**Anvil/Kivi:** cluster-control correctness is sufficiently difficult that formal verification/model checking is becoming a practical research direction.

**XSched:** heterogeneous accelerators need scheduling beneath the cluster-placement layer as well as above it.

**OpenTela:** OSDI 2026 even explores a user-space orchestration overlay spanning fragmented HPC clusters, further illustrating demand for resource access across existing administrative/scheduler boundaries—though it is an overlay, not the clean-sheet substrate Fleet proposes.

None of these invalidates Fleet.

They make the resource/substrate boundary more important.

---

# 14. Open-source strategy

## Recommendation

Use a **permissive core license, probably Apache-2.0**, for:

```text
resource model
wire protocols
control plane
scheduler
node executor
provider SDK
CLI
simulator
reference providers
```

NVIDIA itself chose Apache-2.0 when releasing KAI Scheduler, demonstrating that permissive licensing is compatible with commercial AI-infrastructure strategy.

Do not make essential resource providers proprietary.

The rule should be:

> Any operator can build and run a complete Fleet installation without a commercial license.

Commercial opportunities can remain around:

```text
managed Fleet
enterprise support
fleet operations
hardware certification
compliance
hosted global control plane
advanced analytics
capacity planning
support SLAs
```

without crippling the open substrate.

## Provider neutrality

Design vendor extensions such that:

```text
NVIDIA provider
AMD provider
Intel provider
TPU provider
cloud provider
storage provider
network provider
```

can compete through the same public protocol.

A vendor-specific Fleet fork should provide no structural scheduling advantage.

---

# 15. Acquisition strategy

## Established evidence

NVIDIA's acquisitions of both Run:ai and SchedMD show that **resource orchestration and scheduling are strategically valuable parts of the accelerated-computing stack**, not commodity glue. NVIDIA's continued support for both Slurm and Kubernetes/Run:ai also indicates that neither existing abstraction has subsumed the other's workload domain.

## Recommendation

Do **not** architect Fleet to maximize acquisition dependence on one hardware vendor.

Architect it to maximize the value of the neutral layer.

What creates strategic value:

```text
widely adopted resource API
provider ecosystem
hardware compatibility
scheduler quality
operational data/experience
verification/correctness
migration tooling
virtual-cluster ecosystem
vendor certifications
```

The company can be acquired.

The protocol should survive.

A neutral open project is potentially more valuable precisely because:

```text
NVIDIA
AMD
Intel
clouds
OEMs
storage vendors
network vendors
HPC centers
```

can all target it without handing control of their resource model to a direct competitor.

## Speculative hypothesis

If Fleet became meaningful infrastructure, likely strategic interest would come from some combination of:

```text
accelerator vendors
cloud providers
server/OEM vendors
networking vendors
AI infrastructure companies
large infrastructure software vendors
```

But maximizing independence first is more likely to create leverage than building specifically for one acquirer.

---

# 16. What Fleet owns that Kubernetes + Slurm + NVIDIA cannot currently provide cleanly

This is the strongest concise project thesis.

Kubernetes:

```text
excellent service ecosystem
increasingly sophisticated devices
Pod-centric architecture
```

Slurm:

```text
excellent batch/HPC semantics
queues/reservations/backfill/topology
job-centric architecture
```

NVIDIA:

```text
excellent accelerator integration
hardware lifecycle
Run:ai + Slurm + Kubernetes convergence
vendor platform
```

Fleet would own:

```text
                  RESOURCE AUTHORITY

physical + virtual typed resource graph
                         │
                    Lease/Epoch
                         │
            resource-specific binding
                         │
              hierarchical delegation
                         │
     ┌───────────────────┼────────────────────┐
     ▼                   ▼                    ▼
 Service scheduler   Batch/HPC scheduler   AI scheduler
     │                   │                    │
     └───────────────────┼────────────────────┘
                         ▼
                 execution boundary
     process | OCI | WASM | microVM | VM
                         │
                         ▼
          provider-neutral infrastructure
```

The important distinction is:

> **Fleet does not make Kubernetes and Slurm coexist. Fleet tries to identify the lower-level primitive that makes their separate resource authorities unnecessary.**

That lower-level primitive is not merely a lease.

It is:

> **a typed resource graph + hierarchical lease/fencing model + provider binding protocol + scheduler framework.**

---

# 17. Revised core primitives

The implementation should begin with **six**, rather than four, primitives.

```text
1. Resource
2. ResourceGraph
3. Lease
4. Binding
5. AllocationPlan
6. Cell
```

## Resource

Anything that can constrain placement or be owned/consumed.

## ResourceGraph

Relationships among resources.

## Lease

Who has authority to consume resources.

## Binding

How a lease is physically enforced by a particular provider.

## AllocationPlan

Transactional plan mapping workload intent onto resources and bindings.

## Cell

The consistency and failure boundary that owns authoritative resources.

These are sufficient to begin the architecture without prematurely defining Kubernetes-like workload objects.

---

# 18. Revised v0 research prototype

Keep the product scope unchanged.

Narrow only implementation sequence.

The first prototype should simulate:

```text
3 cells
1,000–100,000 synthetic nodes
CPU
NUMA
memory
GPU
GPU interconnect
NIC
rack/fabric
NVMe
```

Implement:

```text
ResourceGraph
Lease
Binding
Cell
AllocationPlan
```

Then test:

```text
hierarchical lease
gang allocation
lease fencing
node incarnation
cell partition
stale agent
resource preparation failure
cross-cell capacity delegation
topology-aware placement
```

Do **not** start with:

```text
container runtime integration
GUI
Kubernetes importer
Slurm compatibility
Ceph
eBPF networking
cloud APIs
```

Those are implementation integrations.

They do not prove the architecture.

---

# 19. Go/no-go questions for the prototype

Before Fleet becomes a large software project, answer these experimentally:

### Graph

Can candidate placement over a 100k–1M-node simulated resource graph remain fast and memory-efficient when the scheduler uses hierarchical indexes rather than arbitrary graph traversal?

### Lease

Can the same lease state machine safely control simulated CPU, device, storage, and network bindings using different fencing mechanisms?

### Cells

Can cells continue scheduling safely under global-plane loss without violating delegated quota/resource ownership?

### Nested allocation

Can a parent grant a child an arbitrary subgraph and let the child scheduler operate without global coordination?

### Scheduler quality

Can Fleet match or beat domain schedulers on their own workloads:

```text
Slurm/Flux → HPC
KAI → AI
Kubernetes/Nomad → services
```

without making those policy models interfere with one another?

### Correctness

Can the simulator/model checker demonstrate:

```text
no double allocation
no stale resource authority
no unsafe failover
bounded recovery after partial operations
```

under adversarial failures?

If the answer to those is yes, the architecture has substance.

---

# 20. Final thesis

The second research pass does **not** suggest narrowing Fleet.

It suggests defining the boundary more precisely.

Fleet should not be:

```text
a Kubernetes replacement
a Slurm replacement
an AI scheduler
a container orchestrator
an OpenStack rewrite
```

individually.

It should be:

> **An open-source Rust distributed resource authority for heterogeneous compute infrastructure.**

Its kernel abstraction is:

```text
typed resources
       +
relationships
       +
leases
       +
fenced bindings
       +
hierarchical delegation
```

and everything else is built on that.

The closest historical precedent is **Mesos's thin resource substrate**.

The closest scheduling architecture is **Flux/Fluxion**.

The strongest hyperscale precedents are **Twine, Hydra, Borg/Omega/Apollo**.

The closest infrastructure breadth is **OpenStack**.

The closest current commercial convergence is **NVIDIA Mission Control + Run:ai/KAI + Slurm**.

The closest emerging resource API inside Kubernetes is **DRA**.

But none currently combines those ideas into a provider-neutral substrate with one explicit resource authority and correctness model.

That is the defensible Fleet thesis.