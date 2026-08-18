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

- Repository: `omendb/fleet`, private; `main` has the v0 kernel and simulator.
- Accepted kernel contract: `ai/design/kernel-primitives.md`.
- Accepted fencing protocol: `ai/design/lease-fencing.md`.
- First workspace: `crates/kernel` (`fleet-kernel`) and `crates/sim`
  (`fleet-sim`). Shared transition function; simulator owns delivery, clock,
  and faults. Placement is deterministic scored selection: Pack, Spread, and
  PreferAttr are soft.
- `tk-byam`, `tk-l8xd`, and `tk-0lvx` are done. Launch/commercial tasks
  `tk-kwzc` and `tk-n8e9` stay later.
- Planned license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
  provider/extension interfaces. See `ai/design/LICENSE_BOUNDARY.md`.

## Constraints

- Do not revive Postgres, NATS, ConnectRPC, model, endpoint, or replica types.
- Do not implement Cell, Host, FenceToken, or NodeIncarnation.
- Do not scaffold the full `ai/research/stack.md` crate map.
- `ai/review/` is superseded history and is not on the session-start path.

## Next action

Add node execution and provider adapters only after a concrete v1 need.
Leave `tk-kwzc` and `tk-n8e9` until launch work starts.
