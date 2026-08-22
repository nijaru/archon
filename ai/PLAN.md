# Archon Completion Plan

**Updated:** 2026-08-21

The goal is a **complete system**: every resource kind the graph models has an
enforcement path, workloads run in their designed forms, placement respects
real topology, and failure behavior is explicit and tested. No product or
release work — packaging, install, docs, and commercial questions come after
completeness.

Accepted contracts: [`design/kernel-primitives.md`](design/kernel-primitives.md)
and [`design/lease-fencing.md`](design/lease-fencing.md). The full architecture
is [`design/DISTRIBUTED_RESOURCE_OS.md`](design/DISTRIBUTED_RESOURCE_OS.md).

## Done

- **v0 kernel** (2026-08-18): typed graph, allocation, leases, fencing,
  topology-aware scoring, explanations, nested leases, deterministic replay.
  All nine required scenarios pass; two external review cycles closed 20
  defects.
- **Scheduling policies** (2026-08-20): queues, priorities, reservations,
  EASY-style backfill, fair share (per-owner per-kind), data locality,
  health-influenced placement, preemption rules — all simulator-proven.
- **v1 trigger fired** (2026-08-21) by declaration: build real execution.

## Shipped on the execution track

- Walking skeleton: `archon` binary discovers a machine, admits requests
  through the kernel, runs lease commands as real OS processes; revoke/expiry
  kills them.
- cgroups v2 adapter (Linux): per-lease groups; CPU/memory claims become
  `cpu.max`/`memory.max`; OOM-kill proven. macOS is lifecycle-only.
- Control plane: persistent JSONL command log, replay recovery (revokes live
  work, never re-executes), TCP API, CLI (`serve`/`agent`/`demo`/client).
- Multi-node: agents dial in and register into one cluster graph; Effects
  route per binding's machine; agent restart reconciles (rebind session,
  respawn live work); stable instance identity; token auth on links.

## Remaining, in dependency order

### 1. Heterogeneous placement — done 2026-08-21

Machine-local claims by default (`Request.machine_local`), explicit spread
opt-out for multi-member groups, asymmetric-shape integration tests, and the
session-poisoning fix for read-only probes. Kernel change; all nine sim
scenarios re-verified against it.

### 2. Device claims with real enforcement — the last open item

GPU/NIC/NVMe are graph kinds today but only CPU/memory are enforced. Give each
device kind an enforcement story: exclusive GPU access (NVIDIA MPS off,
device cgroups/CDI), NIC/NVMe via whatever the kernel exposes. Requires GPU
hardware to prove; design now, prove when hardware is available. **Hardware-
gated: work item 7 first.**

### 3. Container execution adapter — done 2026-08-21

`Request.image` selects container execution; `ContainerRuntime` drives
Docker/Podman (engine via ARCHON_CONTAINER_ENGINE) with lease claims mapped
to `--cpus`/`--memory`. Same Effects/Record seam, same revoke/expiry kill.
Proven against real Docker; test skips when no engine is reachable.

### 4. Health-driven operation — done 2026-08-21

Controller probes agents on a timer (--probe-secs); unreachable machines are
marked unhealthy and quarantined (no new placements), live leases fail.
`Request.keep_alive` re-queues and re-places services on survivors, capped
at 5 restarts per request. Agent re-registration clears quarantine and
health marks. Integration test proves the full loop.

### 5. Network and storage providers — v0 done 2026-08-21

`Request.storage` (bind mounts) and `Request.ports` (host publishing) flow
through the seam to the container adapter; proven against real Docker
(volume round-trip + PortBindings). Deeper network policy and managed
volumes remain future — the enforcement hook now exists.

### 6. Durability depth — snapshots/compaction done 2026-08-22

Snapshot + compaction shipped: the controller writes an atomic snapshot
(cluster via kernel serde, plus controller-side state) and truncates the
log past a threshold (--compact-every); boot restores from snapshot and
replays only what followed. Remaining: control-plane HA if single-process
proves limiting; agents already re-dial automatically.

### 7. Workload classes — done 2026-08-22

Batch completion accounting (exit codes complete or fail leases; claims
free when work finishes rather than at lifetime expiry), drain-on-revoke
(Request.grace_secs: SIGTERM/docker stop -t before the kill path), and
exponential restart backoff with a working 5-attempt cap (restart lineage
fixes caps that never fired across generations).

Services vs batch vs run-once semantics; checkpoint-aware restart; then the
compatibility layer (Kubernetes, Slurm, Ray as nested/integrated systems) per
[`spec.md`](spec.md).

## Explicit non-goals until complete

- product/release work: packaging, installers, docs-for-others, marketing;
- replacing Linux, KVM, drivers, NCCL, MPI, Ceph, or vendor stacks;
- delegating resource authority to Kubernetes/Slurm;
- high-rate telemetry in authoritative state;
- global optimization before local placement is explainable;
- cells/federation before local ownership transitions are proven.

## Verification

- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo fmt --check`.
- `cargo run -p archon-sim`: the fault-injection harness; new distributed
  invariants get encoded as sim scenarios.
- Loopback end-to-end with real binaries; Linux enforcement paths checked on
  the desktop workstation (cgroup tests skip without root).
- CI (GitHub Actions) is the authoritative gate.
