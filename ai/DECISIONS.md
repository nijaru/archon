# Fleet Decisions

**Updated:** 2026-08-16

## Current direction

Fleet is a distributed resource operating system. The product owns resource
control, leases, scheduling, node execution, and lifecycle across heterogeneous
compute. Inference and private accelerator fleets are first workloads, not the
definition of Fleet. Existing schedulers and runtimes are optional integrations
or nested workloads; Fleet is not merely a substrate that delegates control to
them.

The complete architecture is [`design/DISTRIBUTED_RESOURCE_OS.md`](design/DISTRIBUTED_RESOURCE_OS.md).

## Principles

- **Resource graph, not scalar node capacity.** Topology, locality, bandwidth,
  health, capability, cost, and failure domains are first-class.
- **Lease-first ownership.** Services, jobs, devices, VMs, and nested schedulers
  consume enforceable leases.
- **Mechanism separated from policy.** Discovery, state, fencing, and execution
  are core; placement and optimization are modular.
- **Hierarchical scheduling.** Global planning, cell allocation, node resource
  management, and workload-local scheduling have separate responsibilities.
- **Runtime and isolation neutrality.** Process, OCI, microVM, VM, and WASM are
  execution choices under one workload model.
- **AI/HPC are native policy domains.** Gang scheduling, reservations, goodput,
  communication topology, checkpoint cost, and elasticity are not afterthoughts.
- **Reuse mature infrastructure.** Linux, KVM, eBPF, OCI, CDI, storage,
  networking, accelerator drivers, and workload runtimes remain dependencies.
- **Compatibility without architectural capture.** Kubernetes, Slurm, Flux,
  Ray, MPI, and other systems are optional integrations or nested schedulers;
  Fleet remains the native scheduler and resource authority for its workload
  classes.
- **Small deployments stay small.** A single VPS or small cell should not need a
  mandatory multi-service stack.
- **Explainability is a contract.** Placement, refusal, preemption, and
  recovery decisions include reasons and relevant resource state.

## Locked direction

| Decision | Choice | Rationale |
|---|---|---|
| Product category | Distributed resource operating system | One substrate for services, batch, HPC, AI, VMs, and WASM |
| Core primitive | Typed resource graph plus enforceable lease | Unifies heterogeneous capacity and ownership |
| Scheduler shape | Global planner → cell allocator → node manager → native Fleet workload scheduler, with optional nested schedulers | Keeps policy and timing domains separate without delegating Fleet's control authority |
| Resource scope | CPU, memory, accelerators, networks, storage, data, health, failure domains | Placement depends on more than node CPU/RAM |
| Runtime modes | Process, OCI, sandbox, microVM, VM, WASM | Isolation is a workload property |
| Implementation direction | Rust-first | Privileged infrastructure, concurrency, provider boundaries, WASM integration |
| First workload | Accelerator-aware inference and services | Exercises topology, cache, health, cost, and runtime adapters |
| Compatibility | OCI, CDI, OpenTelemetry, Linux/KVM, standard protocols; optional integrations for K8s/Slurm/Flux/Ray/MPI | Adoption and composition without delegating native control |
| Node model | Minimal immutable Linux with an agent | Clear ownership, rollback, and hardware access |
| Extension model | Capability-limited WASM where practical | Policy extensibility without ambient host authority |
| License direction | ELv2 core; Apache 2.0 SDKs, schemas, examples, adapter interfaces | Preserve a usable source-available core while retaining commercial options |

## Reused systems

Fleet should not rewrite:

- Linux, KVM, cgroups, namespaces, eBPF, WireGuard;
- OCI runtimes, containerd, Cloud Hypervisor, Firecracker, Wasmtime;
- Ceph, NVMe-oF, SPDK, NFS, object storage, and cloud block storage;
- GPU/accelerator drivers, CDI, VFIO, SR-IOV, NCCL/RCCL, MPI, and RDMA;
- vLLM, SGLang, Ray, PyTorch distributed, JAX, Slurm, or Flux.

## Superseded framing

The earlier model-first GPU-serving plan is retained only as a workload and UX
reference. Fleet is not limited to:

- GPU-only scheduling;
- online inference;
- model registries and endpoints;
- direct-node deployments;
- Kubernetes avoidance;
- a gateway or runtime-specific control plane.

The direct node agent and vLLM path remain useful first adapters. They do not
constrain the resource, lease, or scheduler model.

## Decision log

| Date | Decision | Rationale |
|---|---|---|
| 2026-08-16 | Reframe Fleet as a distributed resource OS | The resource graph, lease, hierarchy, and nested-scheduler model is the actual long-term product idea |
| 2026-06-07 | Use a workload-agnostic node core | The agent should receive a workload spec and pass workload-specific metadata to runtimes |
| 2026-06-07 | Use direct node execution as an initial path | Host-level device and lifecycle control should not depend on a pod abstraction |
| 2026-06-07 | Use CDI as the initial device attachment boundary | Vendor-neutral device specification for OCI workloads |
| 2026-06-06 | Keep NATS optional for the first local proof | Event durability and fanout should follow an actual control-plane need |
| 2026-06-06 | Keep Fleet runtime-neutral | Fleet owns lifecycle and allocation, not inference-engine internals |
| 2026-05-29 | Keep source-available core and Apache SDK/schema boundary | Adoption and integration need a useful free core |

## Open decisions

- graph representation and placement indexes;
- exact lease renewal, fencing, revocation, and nested-lease semantics;
- cell consensus and global federation boundaries;
- scheduler transaction model and optimistic concurrency;
- static versus dynamic topology and contention edges;
- device-level preemption/reset contracts across vendors;
- virtual-cluster network, storage, and identity semantics;
- security model for hostile multi-tenancy and confidential workloads;
- whether existing Go code remains a compatibility shell during Rust-core work.
