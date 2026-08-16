---
type: brief
description: Active Fleet Compute OS context
updated: 2026-08-16
---

## Scope

Fleet is an open-source, Rust-first distributed resource operating system for
individual VPSs, multiple VPSs, bare metal, datacenters, AI/HPC, services, batch
jobs, virtual machines, containers, networking, storage, and heterogeneous
accelerators.

Fleet is the resource authority and native scheduler. Kubernetes, Slurm, Ray,
MPI, and similar systems are optional integrations or nested workloads. Linux,
KVM, OCI, drivers, eBPF, RDMA, NVMe, and storage implementations are lower-level
substrate to reuse.

## Current truth

- Repository: `omendb/fleet`, private at `../fleet`.
- Existing implementation: early Go scaffold only.
- Target core: Rust-first.
- Canonical architecture: `ai/design/DISTRIBUTED_RESOURCE_OS.md`.
- Kernel primitives: `Resource`, `ResourceGraph`, `Lease`, `Binding`,
  `AllocationPlan`, and `Cell`.
- Planned license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
  provider/extension interfaces. See `ai/design/LICENSE_BOUNDARY.md`.

## Next action

Define the six primitives, build the deterministic simulator, and test lease
epochs, node incarnations, provider fencing, partial bindings, cell authority,
partition behavior, and topology-aware placement before runtime integrations.
