# Archon

[![CI](https://github.com/nijaru/archon/actions/workflows/ci.yml/badge.svg)](https://github.com/nijaru/archon/actions/workflows/ci.yml)

Archon is an experimental Rust orchestrator for heterogeneous Linux clusters. It places workloads using discovered host resources, grants leases, and runs native processes or OCI containers on node agents. Use it in a trusted lab, not as a production or multi-tenant control plane.

## Quickstart

From a source checkout, use stable Rust (`rustup` reads `rust-toolchain.toml`). Run the following in three terminals on the same machine. Keep both listeners and workloads in a trusted environment; this loopback example does not configure authentication.

**1. Start a controller.** Use a fresh directory for the journal so this run does not restore old workloads. `--no-local` disables the controller's built-in executor so this example uses only the dial-in agent.

```sh
lab=$(mktemp -d)
cargo run -p archon-cli -- serve --listen 127.0.0.1:9000 --no-local --log "$lab/archon.log"
```

**2. Register an agent.** On Linux, set `ARCHON_CGROUP_ROOT` or pass `--cgroup-root PATH` for an existing, writable cgroup v2 subtree delegated to the agent, with the CPU and memory controllers available. On macOS, or on Linux without a usable subtree, the agent can register but cannot hold leases. Leave the root unset if you just want to exercise registration and queuing.

```sh
cargo run -p archon-cli -- agent --register 127.0.0.1:9000 --id node-1
```

**3. Submit and inspect a job.** A submission can be accepted without starting. `status` shows the queue and leases; without an eligible enforcement-capable agent, the job stays queued. With enforcement and capacity available, the agent runs `sleep 30`.

```sh
cargo run -p archon-cli -- -c 127.0.0.1:9000 submit -- sleep 30
cargo run -p archon-cli -- -c 127.0.0.1:9000 status
```

Let the job finish before stopping the agent and controller. Keep the journal directory if you want to restart with the same state.

### Link authentication

Without a token, Noise encrypts the connection but does not authenticate peers. Do not expose an unauthenticated listener. Set the same nonempty `ARCHON_TOKEN` for the controller, agent, and client. The `serve` and `agent` commands also accept `--token-file PATH`; the client reads only `ARCHON_TOKEN`. Anyone with the shared token can connect: it does not provide individual identities, project authorization, or auditing. See the [security policy](SECURITY.md).

## Current scope

- **Placement and execution:** exclusive leases over a resource graph, native and OCI execution, Linux cgroup-based CPU and memory enforcement, device claims, and multi-node agents. OCI execution uses Docker or Podman on the node.
- **Workload management:** finite jobs and service groups with fixed, bounded-elastic, or per-machine replica counts. Replica management does not provide revisioned rollouts, ready endpoint discovery, or rollback.
- **Persistence and recovery:** a single-controller journal and targeted agent reconciliation. Jobs submitted with `submit --key KEY` can be retried against the same controller journal. Distributed HA and safe storage-writer failover are not implemented. The recovery checks do not establish end-to-end execution fencing under every failure.

See [lab operations](docs/operations.md) for journal handling, recovery evidence, and when a resource stays occupied. Ordinary tests and deterministic simulation are not substitutes for privileged Linux enforcement tests. Production identity and isolation, packaging, API compatibility, rolling upgrades, and comparative performance remain unqualified.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local checks and CI, and [AGENTS.md](AGENTS.md) for architecture guardrails. Report suspected vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## License

[Apache License, Version 2.0](LICENSE).
