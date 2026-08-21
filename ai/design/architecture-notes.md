# Architecture Notes

**Updated:** 2026-08-17

These notes explain the boundaries behind Fleet's distributed resource OS
design. The accepted kernel contract is
[`kernel-primitives.md`](kernel-primitives.md). Long-term architecture is in
[`DISTRIBUTED_RESOURCE_OS.md`](DISTRIBUTED_RESOURCE_OS.md).

## Resource graph over machine abstractions

A host is an important discovery unit, not the fundamental resource container.
CPU, NUMA, memory, PCIe, accelerators, NICs, fabrics, storage, power domains,
and data locality may all determine whether a placement is valid.

The graph is a logical model. It does not imply a general-purpose graph
storage engine. The control plane may persist authoritative commands and state,
maintain placement indexes, snapshot topology, and stream telemetry through
different mechanisms.

## Lease as the ownership boundary

Every execution path consumes an allocation lease. This gives services, batch
jobs, VMs, devices, and nested schedulers one ownership contract.

A lease must be enforceable, not merely descriptive. Node agents and providers
need `Binding.fence` and `Agent.session` so stale agents cannot continue to
mutate resources after a revocation or ownership transition. Parent leases
bound nested schedulers and
virtual clusters.

The contract must explicitly cover renewal, expiry, release, revocation, partial
allocation, co-scheduled allocation, checkpoint-aware preemption, ambiguous
outcomes,
and recovery after node or control-plane failure.

## Hierarchical scheduling

Fleet uses separate policy and timing domains:

```text
Global planner       region/cell, capacity, cost, energy, data
Cell allocator       queues, co-scheduling, reservations, topology, backfill
Node manager         cgroups, NUMA, devices, NICs, NVMe, preemption
Nested scheduler     Slurm, Flux, Ray, MPI, or application runtime
```

This lets the system use a fast local allocator while allowing domain-specific
workload schedulers to operate inside a bounded lease.

The critical path is:

```text
hard constraints
→ graph reduction
→ topology filter
→ fast explainable score
→ atomic lease commit
```

Background optimization may improve a placement later when migration is safe.
A global solver should not be required for every local allocation.

## Cells and authoritative state

A cell owns local scheduling, nodes, leases, services, and failure handling. A
global plane owns identity, federation policy, cross-cell placement, and global
reservations. Cells need a defined degraded mode during global-plane loss.

Authoritative control state and telemetry are separate. Heartbeats, logs,
metrics, traces, and device samples inform health and policy but do not become
an unbounded event log inside the ownership state machine.

The target boundary is:

```text
intent → typed API → command/transaction log → replicated state
        → materialized indexes → planners/reconcilers
```

## Runtime and isolation

Process, OCI container, sandbox, microVM, VM, and WASM are workload properties.
The node agent provides a common lifecycle while runtime adapters handle
execution-specific preparation and health.

Fleet should reuse OCI, containerd where useful, Cloud Hypervisor, Firecracker,
Wasmtime, KVM, CDI, VFIO, and vendor drivers. A first vLLM workload is useful
for validating the model, but the node agent should receive generic workload
intent rather than an inference-only API.

## Network, storage, and data

Networking and storage are provider boundaries. Fleet exposes identity, policy,
QoS, topology, and lifecycle semantics while reusing Linux networking, eBPF,
WireGuard, SR-IOV, RDMA, Ceph, NVMe-oF, NFS, cloud block storage, and object
stores.

Model weights, checkpoints, datasets, OCI layers, compiled kernels, KV caches,
and replicas are graph resources. Placement can then account for cache hotness,
transfer cost, residency, replication, and restart locality.

## Compatibility

OCI, CDI, OpenTelemetry, Linux/KVM, and standard storage/network protocols are
native interoperability boundaries. Kubernetes, Helm, Slurm, Flux, Ray, MPI,
and other systems are adapters or nested schedulers.

Fleet must not import their internal abstractions into the core. A virtual
cluster is a lease with explicit network, storage, identity, and resource
limits.

## Extension model

Policy and scheduling extensions should prefer capability-limited WASM:

```text
read.workloads
read.resources
propose.placement
```

Provider integrations that require device or host access use explicit external
interfaces. Extensions do not receive ambient root authority.

## Correctness invariants

- exclusive leases never overlap;
- fencing prevents stale ownership writes;
- allocated devices are reachable from the workload context;
- accepted intent converges or returns an explicit failure;
- ownership transitions are atomic at the authority boundary;
- volume-writer fencing survives recovery;
- simulator replay and production decisions use the same pure scheduling logic.
