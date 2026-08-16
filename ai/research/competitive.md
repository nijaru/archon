# Competitive Landscape

**Updated:** 2026-08-16

Fleet is a distributed resource operating system, not a model-serving engine.
Its competitive target is the fragmented stack that separately manages
Kubernetes services, Slurm jobs, GPU schedulers, VMs, storage, and cloud
resources.

## Positioning

> An open-source resource operating system that represents compute,
> memory, accelerators, networks, storage, data, and failure domains as a
> typed graph; allocates them through hierarchical leases; and runs multiple
> workload classes through one control plane.

The differentiator is not a new runtime. It is one coherent ownership and
placement model across heterogeneous hardware and workload runtimes.

## Reference systems

| System | Strength | Boundary Fleet studies or preserves |
|---|---|---|
| Kubernetes | Declarative reconciliation and ecosystem | API/reconciliation ideas; avoid Pod as universal resource abstraction |
| Slurm | Queues, fair share, reservations, backfill, HPC | Batch policy and virtual-cluster compatibility |
| Flux/Fluxion | Recursive resource management and graph scheduling | Hierarchical allocation and nested schedulers |
| Nomad | Operational simplicity and native/batch workloads | Small-deployment ergonomics |
| OpenStack | VM, network, storage, and bare-metal decomposition | Provider boundaries without VM-only assumptions |
| Kueue/Volcano/KAI | Admission, gangs, quotas, GPU scheduling | Policy modules and AI workload behavior |
| Ray/MPI/PyTorch/JAX | Workload-local distributed execution | Run inside bounded allocations |
| Cloud platforms | Cells, identity, managed infrastructure | Federation and lifecycle patterns without provider lock-in |
| Cilium/eBPF | Workload networking and policy | Native network control surface |
| Ceph/NVMe-oF | Persistent and disaggregated storage | Storage provider contracts |

## Core research questions

- Can one typed graph represent heterogeneous topology without making the
  control plane too expensive or too dynamic?
- Can leases provide safe fencing and nested ownership across process, device,
  storage, and network resources?
- Can hierarchical scheduling combine fast local decisions with global policy?
- Can a small deployment remain simpler than Kubernetes while scaling to cells?
- Can compatibility adapters preserve adoption without importing incumbent
  abstractions into the core?

## Likely advantages

- topology-aware placement across CPU, accelerator, fabric, and storage;
- one lease and failure model for services, batch, AI, VMs, and nested systems;
- explainable multi-objective placement;
- data/cache/checkpoint locality as scheduling inputs;
- small-to-large deployment continuity;
- workload-local schedulers operating inside explicit virtual clusters.

## Likely weaknesses

- ecosystem depth compared with Kubernetes;
- mature HPC policy depth compared with Slurm;
- vendor-specific device support compared with accelerator vendors;
- operational familiarity and migration tooling;
- proof burden for fencing, recovery, and cell behavior.

## Validation direction

Compare the same workload and topology against the incumbent appropriate to the
workload: Kubernetes, Slurm, a GPU scheduler, direct runtime scripts, VM
orchestration, or a cloud provider. Measure placement validity, lease/recovery
correctness, scheduling latency, fragmentation, data movement, utilization,
operator steps, and failure behavior. Do not claim superiority from synthetic
scheduler throughput alone.
