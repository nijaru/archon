---
type: brief
description: Active Fleet Compute OS context
updated: 2026-08-20
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
  and faults. Placement is deterministic scored selection plus `admit()`:
  priority, then submit time; first feasible request wins. Lower-priority
  occupying roots can be preempted; equal or higher priority cannot. Fair-share
  admission (`admit_fair`) adds a per-owner budget ceiling on top of the same
  queue; an empty ceiling disables it and matches `admit`.
- `tk-byam`, `tk-l8xd`, `tk-0lvx`, `tk-2vsh`, `tk-kmcj`, `tk-1idx`, `tk-e0n5`,
  `tk-s1ff`, and `tk-vbsh` are done. All nine required v0 scenarios pass.
  Reservations (`tk-vbsh`) are a Lease in `Reserved` state: committed capacity,
  no Bindings, `PromoteLease` re-enters the ordinary prepare path; admission,
  preemption, fair-share usage, expiry, and replay all see them through
  existing occupancy. Launch/commercial tasks `tk-kwzc` and `tk-n8e9` stay
  later.
- Planned license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
  provider/extension interfaces. See `ai/design/LICENSE_BOUNDARY.md`.

## Constraints

- Do not revive Postgres, NATS, ConnectRPC, model, endpoint, or replica types.
- Do not implement Cell, Host, FenceToken, or NodeIncarnation.
- Do not scaffold the full `ai/research/stack.md` crate map.
- `ai/review/` is superseded history and is not on the session-start path.

## v1 transition trigger

v1 (operational substrate) begins when at least one of these is true, not
before:

1. A named real workload must execute on real hardware through Fleet — the
   deliverable is running code, not a scheduling decision the simulator can
   already prove.
2. A real deployment target needs a provider adapter (device, accelerator,
   storage, or network) that the simulator cannot validate.
3. A scheduling policy under consideration depends on node timing, enforcement,
   or failure behavior that only a real node can exercise.

Node execution and provider adapters stay out of the workspace until then.
Simulator-first remains the default: policies the simulator can validate fairly
(reservations, backfill, gang semantics) are v1 work that does not need the
trigger.

## Next action

No engineering task is queued. Leave `tk-kwzc` and `tk-n8e9` until launch work
starts. Start v1 only through the trigger above.
