# Archon

Archon is a Rust-first distributed resource operating system for heterogeneous compute infrastructure.

> A datacenter is a graph of leaseable capabilities. A workload is a set of constraints and objectives over that graph.

Archon provides resource discovery, scheduling, enforceable leases, execution lifecycle, health, and recovery across Linux machines. The current implementation supports native processes and OCI containers, multi-node agents, cgroup-based CPU/memory enforcement, device claims, persistent control state, and deterministic simulation/fault testing.

Linux and existing runtime/hardware stacks remain substrate: cgroups, eBPF, OCI runtimes, KVM, device drivers, accelerator stacks, networking, and storage systems are reused rather than reimplemented.

The implementation is lab-proven and is not yet a production-scale claim.

## Build and verify

```text
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p archon-sim
```

All client↔controller and controller↔agent links use encrypted Noise transport. Configure the same shared secret with `--token-file` or `ARCHON_TOKEN` for PSK authentication; without a token the current development mode is encrypted but unauthenticated.

## Project context

Durable product/design/roadmap context is centralized in the private [Archon project context](https://github.com/nijaru/agent-context/tree/main/projects/github.com/omendb/archon/ai) rather than duplicated in this repository.

Key sources:

- [`spec.md`](https://github.com/nijaru/agent-context/blob/main/projects/github.com/omendb/archon/ai/spec.md) — canonical product/system contract;
- [`STATUS.md`](https://github.com/nijaru/agent-context/blob/main/projects/github.com/omendb/archon/ai/STATUS.md) — current implementation state;
- [`PLAN.md`](https://github.com/nijaru/agent-context/blob/main/projects/github.com/omendb/archon/ai/PLAN.md) — active roadmap;
- [`design/deployment-and-adoption.md`](https://github.com/nijaru/agent-context/blob/main/projects/github.com/omendb/archon/ai/design/deployment-and-adoption.md) — Linux/datacenter deployment and authority-handoff design.

This repository should document behavior that is actually implemented—CLI/API usage, host prerequisites, installation, troubleshooting, and version compatibility as those stabilize. Future product/deployment strategy belongs in the centralized context.

## License

Archon is currently private. The planned public-release boundary is [AGPL-3.0-or-later](https://github.com/nijaru/agent-context/blob/main/projects/github.com/omendb/archon/ai/design/LICENSE_BOUNDARY.md) for the core, with Apache-2.0 for schemas, SDKs, and provider/extension interfaces. Public license text, copyright ownership, and contribution policy will be finalized before release.
