# Fleet

Fleet is a Rust-first distributed resource operating system for heterogeneous
compute infrastructure.

> A datacenter is a graph of leaseable capabilities. A workload is a set of
> constraints and objectives over that graph.

Fleet provides one resource, lease, scheduling, identity, isolation, execution,
health, and recovery model for services, batch/HPC, distributed AI, inference,
processes, OCI containers, microVMs, VMs, and WASM. Fleet is the primary
resource-control and scheduling authority for those workload classes. It reuses
Linux, KVM, OCI, accelerator drivers, storage systems, network fabrics, and
other lower-level substrate rather than replacing them.

The current repository contains an early Go scaffold. The target core is
Rust-first; the scaffold does not define the final architecture.

## Documentation

- [System specification](ai/spec.md)
- [Distributed Resource OS architecture](ai/design/DISTRIBUTED_RESOURCE_OS.md)
- [Design overview](ai/DESIGN.md)
- [Implementation plan](ai/PLAN.md)
- [Current status](ai/STATUS.md)

Inference and private accelerator fleets are first workloads. vLLM, Slurm,
Flux, Ray, MPI, Kubernetes, and other systems are optional integrations or
nested workloads; they do not define Fleet's internal resource, lease, or
scheduling model.

## License

Fleet is currently private. The planned public release boundary is
[AGPL-3.0-or-later](ai/design/LICENSE_BOUNDARY.md) for the core, with
Apache-2.0 for schemas, SDKs, and provider/extension interfaces. The checked-in
scaffold license is retained temporarily while the public release provenance
and contribution policy are prepared.
