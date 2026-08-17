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

- Repository: `omendb/fleet`, private; clean work is on `main`.
- Accepted kernel contract: `ai/design/kernel-primitives.md`.
  Cluster → Graph → Node/Edge → Request → Allocation → Lease → Binding → Agent.
- Accepted fencing protocol: `ai/design/lease-fencing.md`.
- Implementation: none. No `Cargo.toml`, `crates/`, toolchain, or CI. The Go
  model-serving scaffold was deleted and is not a migration source.
- Target core: Rust-first resource/lease kernel and simulator.
- Canonical long-term architecture: `ai/design/DISTRIBUTED_RESOURCE_OS.md`.
- `tk-byam` and `tk-l8xd` are done. Next: `tk-0lvx` (deterministic simulator
  and first tiny Rust workspace). Launch/commercial tasks `tk-kwzc` and
  `tk-n8e9` stay later.
- Planned license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
  provider/extension interfaces. See `ai/design/LICENSE_BOUNDARY.md`.

## Constraints

- Do not revive Postgres, NATS, ConnectRPC, model, endpoint, or replica types.
- Do not implement Cell, Host, FenceToken, or NodeIncarnation.
- Do not scaffold the full `ai/research/stack.md` crate map. First Rust code
  is a tiny kernel/simulator workspace for `tk-0lvx`.
- `ai/review/` is superseded history and is not on the session-start path.

## Next action

Start `tk-0lvx`: tiny Rust workspace with the kernel types and a deterministic
simulator of the accepted commit and fencing protocols.
