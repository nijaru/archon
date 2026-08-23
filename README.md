# Archon

Archon is a Rust-first distributed resource operating system for heterogeneous
compute infrastructure.

> A datacenter is a graph of leaseable capabilities. A workload is a set of
> constraints and objectives over that graph.

Archon provides one resource, lease, scheduling, identity, isolation, execution,
health, and recovery model for services, batch/HPC, distributed AI, inference,
processes, OCI containers, microVMs, VMs, and WASM. Archon is the primary
resource-control and scheduling authority for those workload classes. It reuses
Linux, KVM, OCI, accelerator drivers, storage systems, network fabrics, and
other lower-level substrate rather than replacing them.

The first implementation is a Rust resource/lease kernel and deterministic
simulator in `crates/kernel` and `crates/sim`. Do not treat deleted Go
model-serving code as the architecture.

```text
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p archon-sim
```

## Documentation

Private design and status context is maintained in the central
[Archon project context](https://github.com/nijaru/agent-context/tree/main/projects/github.com/omendb/archon/ai).
The repository contains the implementation and its tests; the context
contains the product specification, architecture, decisions, plan, and status.

Inference and private accelerator fleets are first workloads. vLLM, Slurm,
Flux, Ray, MPI, Kubernetes, and other systems are optional integrations or
nested workloads; they do not define Archon's internal resource, lease, or
scheduling model.

## License

Archon is currently private. The planned public release boundary is
[AGPL-3.0-or-later](https://github.com/nijaru/agent-context/blob/main/projects/github.com/omendb/archon/ai/design/LICENSE_BOUNDARY.md)
for the core, with Apache-2.0 for schemas, SDKs, and provider/extension
interfaces. Public license text, copyright ownership, and contribution policy
will be published before open-source release.
