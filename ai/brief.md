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
  admission (`admit_fair`) adds per-owner, per-kind budget ceilings
  (TRES-style `KindUsage`) on top of the same queue; kinds without a
  ceiling are unconstrained, and an empty map matches `admit`.
- `tk-byam`, `tk-l8xd`, `tk-0lvx`, `tk-2vsh`, `tk-kmcj`, `tk-1idx`, `tk-e0n5`,
  `tk-s1ff`, `tk-vbsh`, and `tk-b9xb` are done. All nine required v0 scenarios
  pass. Reservations (`tk-vbsh`) are a Lease in `Reserved` state: committed
  capacity, no Bindings, `PromoteLease` re-enters the ordinary prepare path.
  Backfill (`tk-b9xb`) is EASY-style over the priority queue with shadow times
  from lease expiry. Launch/commercial tasks `tk-kwzc` and `tk-n8e9` stay
  later.
- Planned license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
  provider/extension interfaces. See `ai/design/LICENSE_BOUNDARY.md`.

## Constraints

- Do not revive Postgres, NATS, ConnectRPC, model, endpoint, or replica types.
- Do not implement Cell, Host, FenceToken, or NodeIncarnation.
- Do not scaffold the full `ai/research/stack.md` crate map.
- `ai/review/` is superseded history and is not on the session-start path.

## v1 transition trigger — FIRED 2026-08-21

v1 has begun. The trigger fired by declaration: the walking skeleton (a real
process running on a real machine under a Fleet lease, dying when the lease
ends) is the concrete need, and building it is the concrete work. The
original triggers below remain the record of what would otherwise have been
required:

1. A named real workload must execute on real hardware through Fleet — the
   deliverable is running code, not a scheduling decision the simulator can
   already prove.
2. A real deployment target needs a provider adapter (device, accelerator,
   storage, or network) that the simulator cannot validate.
3. A scheduling policy under consideration depends on node timing, enforcement,
   or failure behavior that only a real node can exercise.

Skeleton scope: one machine (this one), one execution adapter (real process
spawn/kill; cgroups come next on Linux), the agent consuming kernel Effects
through the same seam the simulator uses. Still out of scope until the
skeleton runs: additional providers, microVMs, volumes, networking.

## Next action

No engineering task is queued. Leave `tk-kwzc` and `tk-n8e9` until launch work
starts. Start v1 only through the trigger above.
