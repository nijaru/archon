# Fleet Status

**Updated:** 2026-08-20

## Direction

**2026-08-21: the v1 trigger fired by declaration. Fleet now runs real
processes.** `crates/node` (via `fleet demo`/`fleet agent`) discovers this machine as a Fleet
graph, admits requests through the kernel, and executes lease commands as
real OS processes — a process spawned on activation dies on lease revoke or
expiry. Enforcement is lifecycle-only today (spawn/kill); cgroups-based
resource isolation on Linux is the next adapter. The agent consumes kernel
`Effect`s through the same seam the simulator uses, so swapping in a remote
agent protocol later does not touch the kernel.

Fleet is now defined as a **distributed resource operating system**.

Its core model is:

> A datacenter is a graph of leaseable capabilities. A workload is a set of
> constraints and objectives over that graph.

Fleet is intended to unify resource discovery, leases, scheduling, identity,
isolation, execution, health, and recovery for services, batch/HPC, distributed
AI, inference, native processes, OCI containers, microVMs, VMs, WASM, and future
composable resources.

The detailed contract is in:

- [`design/kernel-primitives.md`](design/kernel-primitives.md)
- [`design/lease-fencing.md`](design/lease-fencing.md)
- [`spec.md`](spec.md)
- [`DESIGN.md`](DESIGN.md)
- [`design/DISTRIBUTED_RESOURCE_OS.md`](design/DISTRIBUTED_RESOURCE_OS.md)
- [`DECISIONS.md`](DECISIONS.md)
- [`PLAN.md`](PLAN.md)

## What changed

The previous model-first GPU-serving documents described a useful first
workload, but they made the product boundary too narrow. The canonical boundary
is now the resource operating system:

- GPUs are accelerators, not the only resource type;
- inference is one workload class, not the system definition;
- model endpoints are one service experience, not the universal abstraction;
- direct-node execution is one node path, not a rejection of cluster boundaries
  or nested schedulers;
- Kubernetes, Slurm, Flux, Ray, MPI, and other systems are compatibility or
  nested-workload paths, not internal authorities.

The Go model-serving scaffold was deleted. The first Rust tree is the kernel
and simulator. Do not revive Postgres, NATS, ConnectRPC, model, endpoint, or
replica types as a compatibility shell.

## Architectural center

The first principles are:

1. typed resource graph;
2. universal enforceable allocation/lease;
3. separate mechanism and policy;
4. hierarchical global/cluster/node/workload scheduling;
5. topology, data locality, health, and failure domains as first-class state;
6. selectable process, container, microVM, VM, and WASM isolation;
7. compatibility without architectural capture.

The first implementation proof should cover graph registration, hard constraints,
topology-aware scoring, atomic lease commit/release/revoke, nested leases,
explanations, deterministic replay, stale ownership, and simulated node failure.

## Current documentation state

- Accepted v0 kernel contract: `design/kernel-primitives.md`.
- Accepted v0 fencing protocol: `design/lease-fencing.md`.
- First Rust workspace: `crates/kernel` and `crates/sim`.
- All nine required v0 scenarios pass (`cargo test --workspace`: 40 tests).
- Fair-share admission (`admit_fair`) is a per-owner, per-kind budget
  ceiling (TRES-style `KindUsage`) on top of the priority queue; kinds
  without a ceiling are unconstrained, and an empty map matches `admit`.
- Reservations are a Lease in `Reserved` state (`ReserveLease`/`PromoteLease`):
  committed capacity with no Bindings; promotion re-enters the ordinary
  prepare/bind/activate path. Six reservation scenarios pass.
- EASY-style backfill (`admit_backfill`) runs over the priority queue: a
  blocked head's shadow comes from lease expiry event times; a later request
  starts only if it finishes by every blocked head's shadow or claims none of
  that head's shadow claims. Unsatisfiable requests block nobody. Six
  backfill scenarios pass.
- A gpt-5.6-sol review found ten defects; all are fixed and pinned by tests
  (`crates/kernel/tests/harden.rs`): binding-covered activation, descendant
  cascades and subtree sibling accounting, fence-carrying binding acks,
  monotonic sessions, atomic ApplyGraph, state-aware binding retries, backfill
  shadow soundness, fair-share usage from the Cluster's own table, and
  simulator fence-ack withholding until Agent restart.
- Follow-up design passes fixed: topology validated as a forest, root-only
  Bindings, non-revision health (`SetNodeHealth`), capacity-shrink refusal,
  preparing-lease expiry, duplicate-claim rejection, `Queued` type
  unification, and fair-share-carrying backfill.
- A second gpt-5.6-sol review verified the fixes and found ten more issues;
  all fixed and pinned: expiry validation before mutation, summed root
  occupancy, direct-child subtree accounting, fencing-aware backfill
  shadows, quarantine-blocked heads, promotion revalidation against the
  current graph, owner-carrying Admission, edge attr updates, digest
  capacity/edge coverage, bounded scoring tiers, idempotent equal-session
  handshakes, and unclaimable DataObjects.
- Walking skeleton: `fleet demo` + real-process integration tests
  (lease runs `sleep`, revoke/expiry kill it).
- cgroups v2 adapter: per-lease groups on Linux; claims become
  `cpu.max`/`memory.max`; `cgroup.kill` on terminate; OOM-kill proven on
  the workstation.
- Remote agent protocol: controller and agent are separable processes over
  TCP (length-prefixed JSON); cross-machine lease execution proven
  (Mac controller → pacabot-ams agent, cgroup-enforced).
- Control plane: `fleet serve` — persistent JSONL command log, replay-based
  recovery (revokes live work, never re-executes), TCP API server, CLI.
  Crash-restart proven on loopback. 95 tests; clippy clean.
- Canonical long-term architecture: `design/DISTRIBUTED_RESOURCE_OS.md`.
- `spec.md` is the product/system specification.
- `DESIGN.md`, `DECISIONS.md`, and `PLAN.md` now use the resource-OS model.
- Older cell/host/Allocation-as-lease wording maps to the kernel contract and
  must not be implemented as a second vocabulary.
- The old vLLM/direct-node and Go control-plane work is historical only. It is
  not a migration source and must not constrain the kernel.

## Immediate design work

1. Keep the kernel and simulator as the only crates until the v1 transition
   trigger in `ai/brief.md` fires.
2. Do not scaffold the later crate map in `research/stack.md`.
3. Launch and commercial work (`tk-kwzc`, `tk-n8e9`) stays later.
4. Fair share, reservations, and backfill shipped. Co-scheduling resolved:
   atomic multi-member commit is structural; no distinct request class. No
   open scheduling decisions.
5. Data locality and health-influenced placement shipped. Health moves via
   `SetNodeHealth` (non-revision); topology is validated as a forest;
   bindings are root-only. Fair-share ceilings resolved per node kind
   (TRES-style); no open scheduling-policy questions remain.
6. Remaining v1 items (providers, microVMs, volumes, network foundations,
   virtual clusters) wait on the v1 trigger or larger design decisions.
## Open questions

- dynamic topology, contention, and health representation;
- provider contracts for accelerator partitioning and preemption;
- virtual-cluster network, storage, and identity semantics.
