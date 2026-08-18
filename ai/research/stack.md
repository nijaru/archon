# Implementation Stack Direction

**Updated:** 2026-08-17

Fleet's target core is Rust-first because the critical surface owns privileged
Linux resources, leases, concurrency, provider lifecycles, and failure
recovery. The former Go model-serving scaffold was deleted and is not a
compatibility shell, API experiment surface, or dependency contract.

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

This is the long-term responsibility map, not the current crate cut. The first
workspace is `crates/kernel` and `crates/sim`. Do not create the rest of this
tree until a later stage needs it.

```text
crates/
  api/ types/ resource-graph/ lease/ state/ raft/
  scheduler-core / scheduler-service / scheduler-batch / scheduler-ai/
  planner-global / node-agent/
  runtime-oci / runtime-microvm / runtime-wasm/
  device / network / storage / baremetal / telemetry/
  policy-sdk / simulator / cli/
```

Keep pure graph, lease, and scheduler logic transport-independent. The simulator
must be able to execute the same decisions and replay the same state transitions
as the control plane.

## Reuse policy

Do not rewrite Linux, KVM, cgroups, namespaces, eBPF, WireGuard, OCI runtimes,
containerd, Cloud Hypervisor, Firecracker, Wasmtime, Ceph, NVMe-oF, SPDK,
NCCL/RCCL, MPI, RDMA, or vendor drivers without a measured correctness or
capability requirement.

## Deleted Go scaffold

The former Go packages used Cobra, pgx/sqlc, Goose, ConnectRPC, chi, zerolog,
PostgreSQL, and NATS to express a model/endpoint control plane. That stack is
historical only. Do not recreate it, and do not grow a second resource model
in another language while the Rust kernel is undefined.

## Stack questions

- graph/index representation and bounded update cost;
- embedded versus external per-cell state;
- consensus and cell recovery behavior;
- provider ABI and capability versioning;
- WASM policy component boundaries;
- whether a workload runtime or provider requires a language-specific shim.
