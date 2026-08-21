---
type: brief
description: Active Archon Compute OS context
updated: 2026-08-21
---

## Name

The project is **Archon**.

Crates.io plan (checked 2026-08-21): bare `archon` is taken by a dormant
alpha crate (Optimism batch submitter, last touched 2023); `archon-cli` is
taken by a small package manager. `archon-kernel`, `archon-sim`,
`archon-node`, `archon-control`, `archon-os` are all available. Our crates
are private (`publish = false`) so nothing blocks today. At release:
distribute the CLI as binaries (GitHub releases, brew) which needs no
crates.io name; if we ever publish libraries, use the available
`archon-*` names; optionally ask the dormant `archon` owner about a
handoff — no guarantee.

## Scope

Archon is an open-source, Rust-first distributed resource operating system for
individual VPSs, multiple VPSs, bare metal, datacenters, AI/HPC, services, batch
jobs, virtual machines, containers, networking, storage, and heterogeneous
accelerators.

Archon is the resource authority and native scheduler. Kubernetes, Slurm, Ray,
MPI, and similar systems are optional integrations or nested workloads. Linux,
KVM, OCI, drivers, eBPF, RDMA, NVMe, and storage implementations are lower-level
substrate to reuse.

## Current truth

- Repository: `omendb/archon` (repo name unchanged), private; `main` has the v0 kernel, simulator,
  and the walking skeleton. CI (`.github/workflows/ci.yml`) enforces fmt,
  clippy `-D warnings`, tests, and the sim run on every push.
- Accepted kernel contract: `ai/design/kernel-primitives.md`.
- Accepted fencing protocol: `ai/design/lease-fencing.md`.
- Workspace: `crates/kernel` (`archon-kernel`), `crates/sim` (`archon-sim`),
  `crates/node` (execution runtime library), `crates/control` (control
  plane library), and `crates/cli` — one `archon` binary with role
  subcommands: `serve` (control plane), `agent` (node daemon), `demo`,
  and `submit`/`status`/`revoke` client. Shared transition
  function; simulator owns delivery, clock, and faults; node executes real
  processes. Placement is deterministic scored selection plus `admit()`:
  priority, then submit time; first feasible request wins. Lower-priority
  occupying roots can be preempted; equal or higher priority cannot.
  Fair-share admission (`admit_fair`) adds per-owner, per-kind budget
  ceilings (TRES-style `KindUsage`) on top of the same queue; kinds without
  a ceiling are unconstrained, and an empty map matches `admit`. Backfill
  (`admit_backfill`) is the full admission discipline with EASY shadows.
- Two gpt-5.6-sol reviews ran (2026-08-20); all 20 findings fixed and
  pinned. 89 tests, clippy clean, CI green.
- **Walking skeleton (2026-08-21): Archon runs real processes.**
  `archon demo` discovers the local machine as a Archon graph, admits
  requests through the kernel, and executes lease commands as real OS
  processes — revoke or expiry kills the process.
- **cgroups v2 adapter (2026-08-21): lease claims are real kernel limits
  on Linux.** Per-lease cgroups under a configurable root; CPU/memory
  claims map to `cpu.max`/`memory.max`; termination kills the group via
  `cgroup.kill`. Proven on the workstation: a runaway process under a
  16 MiB `memory.max` is OOM-killed. macOS stays lifecycle-only.
- **Remote agent protocol (2026-08-21): a second machine joins over TCP.**
  Length-prefixed JSON frames (`crates/node/src/protocol.rs`, serde_json);
  `LeaseAgent` is the agent core shared by in-process and daemon paths;
  `NodeService::connect` boots a cluster over a remote machine's
  discovered graph; session+fence ride on every frame and stale sessions
  are rejected by both kernel endpoints and the agent. Proved cross-machine:
  Mac controller ran and revoked a process on pacabot-ams inside a cgroup.
  Known gap: spawn happens before cgroup attach (brief escape window);
  fix with clone3-into-cgroup when the runtime seam is next touched.
- **Multi-node scheduling (2026-08-21): one control plane, many machines.**
  Agents dial in (`archon agent --register ADDR`), announce their machine,
  and join the cluster graph; Effects route to the agent owning the
  binding's node via `graph.machine_of`. Reconnection is reconciliation:
  same-name re-registration bumps the session, the kernel's Reconcile
  effect lists live bindings, the controller rebinds + re-drives
  ActivateBinding, respawning work on the fresh agent. Unroutable effects
  are dropped honestly (dead agent holds no processes). `--no-local` gives
  a pure control plane. Stable identity:
  agents persist an instance id (XDG state, `--id` overrides) stamped into
  the machine's attrs, so identity survives restarts on both sides and is
  recovered via log replay. Same display name + different instance =
  distinct machine (no takeover). Reconcile only respawns Active leases;
  session numbering resumes above the replayed high-water mark. Link auth:
  every connection opens with a Greeting frame (role + token); `serve
  --token-file`/`ARCHON_TOKEN` enforces it (constant-time compare), open
  dev mode warns loudly. Remaining limits: token auth is not TLS (pair
  with SSH tunnels or add TLS later); single shared token, no per-agent
  credentials.
  `crates/control` (the `archon` binary): every applied command is appended to a JSONL
  log (kernel serde is an opt-in feature; the kernel stays dep-free);
  restart replays the log exactly and revokes live leases instead of
  re-executing them. API server (TCP JSON frames) + CLI (submit/status/
  revoke). Proven: crash mid-lease, restart, state restored from log.
- Verification hosts: loopback on the Mac for platform-neutral logic;
  CI for Linux compile+tests. pacabot is off-limits. cgroup enforcement
  tests need a root Linux host (`desktop` when needed); a disposable VM
  is worth adding only when stress tests (OOM, CPU saturation) get
  aggressive or kernel pinning matters.
- Launch/commercial tasks `tk-kwzc` and `tk-n8e9` stay deferred.
- Planned license: AGPL-3.0-or-later core; Apache-2.0 schemas, SDKs, and
  provider/extension interfaces. See `ai/design/LICENSE_BOUNDARY.md`.

## Constraints

- Do not revive Postgres, NATS, ConnectRPC, model, endpoint, or replica types.
- Do not implement Cell, Host, FenceToken, or NodeIncarnation.
- Do not scaffold the full `ai/research/stack.md` crate map.
- `ai/review/` is superseded history and is not on the session-start path.

## v1 transition trigger — FIRED 2026-08-21

v1 has begun. The trigger fired by declaration: the walking skeleton (a real
process running on a real machine under a Archon lease, dying when the lease
ends) is the concrete need, and building it is the concrete work. The
original triggers below remain the record of what would otherwise have been
required:

1. A named real workload must execute on real hardware through Archon — the
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

v1 execution track, next: multi-node scheduling — route Effects to
per-machine agents so one control plane drives several machines (the
AgentLink seam and protocol are ready; node-to-agent routing is the new
work). Then: reconciliation of agent-reported state after agent restarts.
Launch/commercial tasks (`tk-kwzc`, `tk-n8e9`) stay deferred until release
work starts.
