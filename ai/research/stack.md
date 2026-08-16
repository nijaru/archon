# Implementation Stack Direction

**Updated:** 2026-08-16

Fleet's target core is Rust-first because the critical surface owns privileged
Linux resources, leases, concurrency, provider lifecycles, and failure
recovery. The current Go repository is a scaffold and may remain useful for
API/operator experiments, but its dependency choices are not the target core
contract.

## Rust core boundaries

| Concern | Direction |
|---|---|
| API/types | Typed Rust domain model with versioned wire schemas |
| Resource graph | Compact in-memory/topology representation plus persisted state and placement indexes |
| Leases/state | Explicit ownership, fencing, renewal, revocation, and recovery boundaries |
| Consensus | Per-cell replicated state machine or equivalent; keep telemetry separate |
| Scheduler | Pure policy-independent core plus modular policy layers |
| Node agent | Rust Linux process with provider and runtime boundaries |
| OCI | Existing OCI/containerd interfaces where useful |
| MicroVM/VM | Cloud Hypervisor, Firecracker, KVM/QEMU |
| WASM policies | Wasmtime and WASI Component Model where permissions fit |
| Device attachment | CDI, VFIO, SR-IOV, and vendor provider APIs |
| Network | Linux networking, eBPF/XDP, WireGuard, BGP, SR-IOV, RDMA |
| Storage | Provider contracts for local NVMe, Ceph, NVMe-oF, NFS, object storage |
| Simulation | Same pure resource/placement code as production |
| Telemetry | OpenTelemetry-compatible streams outside authoritative state |
| CLI | Rust CLI over the typed API |

## Target workspace

```text
crates/
  api/ types/ resource-graph/ lease/ state/ raft/
  scheduler-core/ scheduler-service/ scheduler-batch/ scheduler-ai/
  planner-global/ node-agent/
  runtime-oci/ runtime-microvm/ runtime-wasm/
  device/ network/ storage/ baremetal/ telemetry/
  policy-sdk/ simulator/ cli/
```

Keep pure graph, lease, and scheduler logic transport-independent. The simulator
must be able to execute the same decisions and replay the same state transitions
as the control plane.

## Reuse policy

Do not rewrite Linux, KVM, cgroups, namespaces, eBPF, WireGuard, OCI runtimes,
containerd, Cloud Hypervisor, Firecracker, Wasmtime, Ceph, NVMe-oF, SPDK,
NCCL/RCCL, MPI, RDMA, or vendor drivers without a measured correctness or
capability requirement.

## Existing Go scaffold

The repository currently contains Go packages using Cobra, pgx/sqlc, Goose,
ConnectRPC, chi, zerolog, PostgreSQL, SQLite, and NATS. Those choices remain
historical scaffold context. They should not silently become the architecture
contract for the Rust-first resource OS.

If Go code is retained during the transition, keep it behind stable resource,
lease, and API contracts rather than growing a second resource model.

## Stack questions

- graph/index representation and bounded update cost;
- embedded versus external per-cell state;
- consensus and cell recovery behavior;
- provider ABI and capability versioning;
- WASM policy component boundaries;
- Rust/Go transition and wire compatibility;
- whether a workload runtime or provider requires a language-specific shim.
