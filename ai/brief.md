---
type: brief
description: Active Fleet Compute OS context
updated: 2026-08-17
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

- Repository: `omendb/fleet`, private at `../fleet`. Working tree clean on
  `main` at `b0ae68e`.
- Implementation: none. No `Cargo.toml`, `crates/`, toolchain, or CI. The Go
  model-serving scaffold was deleted and is not a migration source.
- Target core: Rust-first resource/lease kernel and simulator.
- Canonical architecture: `ai/design/DISTRIBUTED_RESOURCE_OS.md`.
- Kernel primitives: `Resource`, `ResourceGraph`, `Lease`, `Binding`,
  `AllocationPlan`, and `Cell`.
- Ready work: `tk-byam`, then `tk-l8xd` and `tk-0lvx`. Launch/commercial tasks
  `tk-kwzc` and `tk-n8e9` stay later.
- Planned license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
  provider/extension interfaces. See `ai/design/LICENSE_BOUNDARY.md`.

## Constraints

- Do not revive Postgres, NATS, ConnectRPC, model, endpoint, or replica types.
- Do not scaffold the full `ai/research/stack.md` crate map. First Rust code
  follows `tk-byam` and should be a tiny kernel/simulator workspace.
- `ai/review/` is superseded history and is not on the session-start path.

## Next action

Start `tk-byam`: define the six kernel primitives and resource-graph indexes
before adding a Cargo workspace or runtime integrations.
