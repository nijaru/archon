# Fleet Compute OS

## Purpose

Fleet is the monorepo's primary product direction: an open-source, Rust-first
distributed resource operating system for VPSs, bare metal, datacenters, AI/HPC,
services, batch jobs, VMs, containers, networking, storage, and heterogeneous
accelerators.

The core thesis is:

> A datacenter is a graph of leaseable capabilities. A workload is a set of
> constraints and objectives over that graph.

Fleet owns the resource authority, leases, fencing, allocation plans,
hierarchical scheduling, node enforcement, lifecycle, health, recovery, cells,
and federation. It reuses lower-level substrate such as Linux, KVM, OCI,
drivers, eBPF, RDMA, NVMe, and storage implementations. Kubernetes, Slurm,
Ray, MPI, and similar orchestrators are optional integrations or nested
workloads, not Fleet's architectural foundation.

## Session start

1. Read `ai/STATUS.md`, `ai/brief.md`, and the relevant Fleet design document.
2. From this directory, run `tk ready` for the Fleet-local task queue.
3. Read `ai/design/DISTRIBUTED_RESOURCE_OS.md` before changing architecture or
   core design.
4. Read `ai/design/mvp-scope.md` before implementing the initial kernel.
5. Check repository status before editing; unrelated DBNext changes may be
   present in the parent worktree.

## Current implementation boundary

The existing Go code is an early scaffold and does not define the target
architecture. The target implementation is Rust-first. Do not expand the old
model-serving/control-plane scaffold as if it were the Compute OS kernel.

The first implementation sequence is:

1. `Resource`, `ResourceGraph`, `Lease`, `Binding`, `AllocationPlan`, and `Cell`.
2. Deterministic graph/lease/scheduler simulation.
3. Fencing, node incarnation, partial binding, and cell-partition correctness.
4. Native Fleet scheduling and node enforcement.
5. Runtime, device, network, storage, and compatibility providers.

Do not begin with a vLLM/container integration, GUI, Kubernetes importer, Slurm
adapter, Ceph integration, or cloud API. Those follow proof of the core
resource/lease model.

## Design invariants

- Fleet is the native resource and scheduling authority for supported workloads.
- Exclusive leases never overlap.
- Fenced epochs and node incarnations reject stale authority.
- Provider bindings enforce leases at resource endpoints.
- A partial allocation reaches committed state or an explicit terminal failure.
- Cells own ordinary allocation authority; global policy delegates bounded
  capacity and does not sit in every placement decision.
- Telemetry is separate from authoritative allocation state.
- Simulator and production scheduling share the same pure decision logic.

## Documentation and licensing

- Canonical architecture: `ai/design/DISTRIBUTED_RESOURCE_OS.md`
- Product specification: `ai/spec.md`
- Current state: `ai/STATUS.md`
- Current implementation plan: `ai/PLAN.md`
- Decisions: `ai/DECISIONS.md`
- Licensing boundary: `ai/design/LICENSE_BOUNDARY.md`

Fleet is intended to open source under AGPL-3.0-or-later for the core. Wire
schemas, SDKs, and provider/extension interfaces should be separated cleanly
before public release and may use Apache-2.0. Do not make licensing claims for
third-party dependencies or generated artifacts without checking their notices.

## Verification

For documentation changes, run link and `git diff --check` validation. For Go
scaffold changes, run:

```bash
go test ./...
```

For the future Rust core, add the project-specific `cargo test`, Clippy,
simulation, model-checking, and fault-injection commands to this file when the
workspace exists.
