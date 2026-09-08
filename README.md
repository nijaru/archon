# Archon

[![CI](https://github.com/nijaru/archon/actions/workflows/ci.yml/badge.svg)](https://github.com/nijaru/archon/actions/workflows/ci.yml)

Archon is a Rust-first distributed resource operating system for heterogeneous compute infrastructure.

> A datacenter is a graph of leaseable capabilities. A workload is a set of constraints and objectives over that graph.

Archon provides resource discovery, scheduling, enforceable leases, execution lifecycle, health, and recovery across Linux machines. The current implementation supports native processes and OCI containers, multi-node agents, cgroup-based CPU/memory enforcement, device claims, persistent control state, and deterministic simulation/fault testing.

Linux and existing runtime/hardware stacks remain substrate: cgroups, eBPF, OCI runtimes, KVM, device drivers, accelerator stacks, networking, and storage systems are reused rather than reimplemented.

The implementation is lab-proven and is not yet a production-scale claim.

## Prerequisites

- A stable Rust toolchain (`rust-toolchain.toml` pins the channel; `rustup` picks it up automatically).
- Linux for CPU/memory/device enforcement: lease authority requires proven enforcement capability, so a machine without a usable cgroup v2 subtree can register but never holds a lease. macOS builds and runs the control plane, tests, simulator, and client, but cannot enforce — the demo refuses loudly there instead of running work unenforced.

## Quickstart

Build and verify:

```text
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p archon-sim
```

Run the walking-skeleton demo (needs an enforcement-capable machine, see above):

```text
cargo run -p archon-cli -- demo
```

Run a control plane with a dial-in agent (three terminals):

```text
cargo run -p archon-cli -- serve --listen 127.0.0.1:9000 --log /tmp/archon.log
cargo run -p archon-cli -- agent --register 127.0.0.1:9000 --id node-1
cargo run -p archon-cli -- -c 127.0.0.1:9000 submit -- sleep 30
cargo run -p archon-cli -- -c 127.0.0.1:9000 status
```

All client↔controller and controller↔agent links use encrypted Noise transport. Configure the same shared secret with `--token-file` or `ARCHON_TOKEN` for PSK authentication; without a token the current development mode is encrypted but unauthenticated.

## Status

What works today: exclusive leases over a resource graph, controller/agent restart reconciliation with fencing, native process and OCI container execution, service groups with fixed/elastic/per-machine cardinality, accelerator device claims, a persistent control-plane command log, and a deterministic simulator with fault-injection proof tests.

Not yet production-hardened: node/client identity and authorization, quota/audit semantics, versioned external APIs, high availability, rolling upgrades, and packaging. See [`AGENTS.md`](AGENTS.md) for the architecture guardrails that govern new work.

## Contributing

Contributions are welcome under the Developer Certificate of Origin. See [`CONTRIBUTING.md`](CONTRIBUTING.md). To report a vulnerability, see [`SECURITY.md`](SECURITY.md) — do not open a public issue.

## License

Licensed under the Apache License, Version 2.0. See [`LICENSE`](LICENSE).
