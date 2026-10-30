# Archon

[![CI](https://github.com/nijaru/archon/actions/workflows/ci.yml/badge.svg)](https://github.com/nijaru/archon/actions/workflows/ci.yml)

Archon is a Rust-first distributed resource operating system for heterogeneous compute infrastructure.

> A datacenter is a graph of leaseable capabilities. A workload is a set of constraints and objectives over that graph.

Archon provides resource discovery, scheduling, enforceable leases, execution lifecycle, health, and recovery across Linux machines. The current implementation supports native processes and OCI containers, multi-node agents, cgroup-based CPU/memory enforcement, device claims, persistent control state, and deterministic simulation/fault testing.

Linux and existing runtime/hardware stacks remain substrate: cgroups, eBPF, OCI runtimes, KVM, device drivers, accelerator stacks, networking, and storage systems are reused rather than reimplemented.

The implementation is lab-proven and is not yet a production-scale claim.

## Prerequisites

- A stable Rust toolchain (`rust-toolchain.toml` pins the channel; `rustup` picks it up automatically).
- Linux for CPU/memory/device enforcement: lease authority requires proven enforcement capability, so a machine without a usable cgroup v2 subtree can register but never holds a lease. macOS builds and runs the control plane, tests, simulator, and client, but cannot enforce.

## Quickstart

Build and verify:

```text
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p archon-sim
```

Run a control plane with a dial-in agent (three terminals):

```text
cargo run -p archon-cli -- serve --listen 127.0.0.1:9000 --log /tmp/archon.log
cargo run -p archon-cli -- agent --register 127.0.0.1:9000 --id node-1
cargo run -p archon-cli -- -c 127.0.0.1:9000 submit -- sleep 30
cargo run -p archon-cli -- -c 127.0.0.1:9000 status
```

The lease admits once an enforcement-capable agent registers; without
one the request waits in the queue, visible via `status`.

All client↔controller and controller↔agent links use encrypted Noise transport. Configure the same shared secret with `--token-file` or `ARCHON_TOKEN` for PSK authentication; without a token the current development mode is encrypted but unauthenticated.

## Resource graph updates

Graph updates cannot change a resource's machine ancestor while a lease holds
its capacity. This includes reservations, preparation, and terminal leases with
unclosed bindings. Reclassifying an ancestor or introducing a nearer machine
must not redirect enforcement or let another machine acknowledge teardown.
Unsafe updates are rejected atomically. Capacity increases and unrelated facts
can still be refreshed; the machine restriction ends once the lease is terminal
and all its bindings are closed. This is not workload migration or validation of
physical device identity.

## Controller persistence

The `--log` journal synchronously persists accepted submissions, service
registration/updates and scale targets, queued payloads, and admitted workload
lineage before acknowledging them or dispatching their execution effects.
A journal write failure terminates the controller rather than accepting volatile
work. Restart restores desired state; live leases still require agent
reconciliation, not blind re-execution.

Adoption requires member status and successful endpoint enumeration from the
returning agent's current session. Every expected open binding must match its
node, provider, scope, binding id, fence, and active phase; a running process alone
is not ownership proof. Missing or inconsistent endpoint evidence blocks
adoption and keeps recovery pending; it does not itself release occupied capacity. Stray endpoint closes must each
be acknowledged; successful empty enumeration completes recovery for an idle
machine. Adoption never reactivates resources or restarts execution. Re-register
the agent to collect fresh recovery evidence; enumeration is not automatically
retried.

`--compact-every N` bounds the journal between snapshots; `0` disables compaction,
not durability. Keep the log and its adjacent `.snapshot` file together and
always restart with the same log path.
Snapshot replay cursors make an interrupted log truncation safe, and recovery
truncates an incomplete final journal record before appending again. Malformed
complete records and gaps in journal sequence numbers fail boot.

This single-controller R&D format copies full controller metadata per journal
record and is intended for small workloads, not high submission rates or
unbounded retained history. Older kernel-only logs and version-1 snapshots can
be read, but cannot recover desired state that the old controller never wrote
or resolve an old version-1 compaction overlap. A legacy lease admission after a
version-1 snapshot is refused because its workload lineage cannot be proven;
preserve those files for operator reconciliation rather than discarding the log.
New snapshots use version 2; older binaries cannot read them.

The controller acquires a nonblocking exclusive OS lock on the journal before
snapshot loading, replay or tail repair, and holds it through compaction until
shutdown. A second controller using the same journal fails boot; process exit
releases the lock without stale-lock cleanup. This is local writer exclusion,
not distributed fencing or HA. Use a local filesystem with working file locks
and fsync; unsupported locking fails boot. The lock is advisory on Unix: do not
modify, delete, replace or externally rotate either durable file while the
controller runs, and do not run older binaries that bypass locking.

## Stateful workloads

Host-path mounts are raw attachments, not storage placement claims or exclusive
volume ownership. The operator must establish data locality and writer fencing;
service replica count does not provide database replication or failover safety.
Database transaction, recovery and query-admission semantics remain owned by the
database. No OmenDB engine requires Archon.

Agent execution/recovery proof and storage-writer fencing remain qualification
work before dependable stateful hosting. The local journal lock does not fence
agents, storage writers or controllers using different journals. Do not infer
storage HA from resource leases or generic service restart.

## Status

What works today: exclusive leases over a resource graph, controller/agent restart reconciliation with fencing, native process and OCI container execution, service groups with fixed/elastic/per-machine cardinality, accelerator device claims, a persistent control-plane command log, and a deterministic simulator with fault-injection proof tests.

Not yet production-hardened: node/client identity and authorization, quota/audit semantics, versioned external APIs, high availability, rolling upgrades, and packaging. See [`AGENTS.md`](AGENTS.md) for the architecture guardrails that govern new work.

## Contributing

Contributions are welcome under the Developer Certificate of Origin. See [`CONTRIBUTING.md`](CONTRIBUTING.md). To report a vulnerability, see [`SECURITY.md`](SECURITY.md) — do not open a public issue.

## License

Licensed under the Apache License, Version 2.0. See [`LICENSE`](LICENSE).
