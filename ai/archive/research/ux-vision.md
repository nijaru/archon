# Fleet Operator Experience

**Updated:** 2026-08-16

Fleet should make heterogeneous resource allocation understandable. The
operator works with workloads, allocations, leases, resources, topology,
health, locality, cost, and recovery—not only pods or model endpoints.

## Core UX principle

> Ask for workload intent; inspect the resource lease and the reasons behind its
> placement.

The same workflow should support a service, batch job, distributed AI job, VM,
or nested scheduler.

## Plan before commit

```bash
fleet resource list
fleet workload plan training-job.yaml
fleet allocation explain training-job
fleet lease status allocation-7f2
```

Example plan:

```text
workload: training-job
class: distributed
workers: 32 preferred, 16 minimum
resources/worker: 8 accelerators, 96 CPU, 1 TiB memory
network: RDMA; gang allocation required

candidate:
  cell: west-2
  accelerator island: nvlink-island-04
  nodes: 4
  dataset: cached in cell
  checkpoint path: local NVMe with replicated object-store target

reasons:
  + requested accelerator topology is satisfied
  + dataset is local
  + RDMA fabric is below contention threshold
  - alternate cell has lower energy cost but worse checkpoint locality
```

## Questions the system must answer

- Why did this workload run here?
- Which exact resources does its lease own?
- Why is it waiting or being preempted?
- What happens if this node, fabric, or cell fails?
- Which data and artifacts are local or need transfer?
- Why was another candidate rejected?
- Can a nested scheduler see or consume resources outside its lease?
- Which policy or identity authorized the allocation?

## CLI model

The CLI should expose resource and lifecycle primitives:

```bash
fleet resource graph
fleet workload submit service.yaml
fleet workload submit batch.yaml
fleet allocation explain <id>
fleet lease revoke <id>
fleet node drain <id>
fleet cell status <id>
fleet virtual-cluster create research-team.yaml
```

A later inference workload can reuse the same lease and workload commands:

```bash
fleet model register llama-3.1-8b --source s3://models/llama
fleet workload submit endpoint.yaml
fleet workload logs endpoint/chat
```

## Dashboard focus

- **Resource view:** topology, capacity, health, contention, and failure domains.
- **Workload view:** intent, phase, lease, placement, policy, and events.
- **Allocation view:** owned resources, parent/child leases, renewal, expiry,
  and fencing state.
- **Cell view:** queues, reservations, fairness, failures, and global-policy
  effects.
- **Data view:** replicas, cache hotness, transfer cost, and checkpoint state.
- **Execution view:** process/container/VM/WASM runtime state and provider
  attachments.

## API principles

- Typed workload and resource APIs rather than runtime-specific primitives.
- Versioned schemas and explicit refusal classifications.
- Explainable placement and lifecycle events.
- OpenTelemetry correlation by workload, allocation, lease, resource, and
  generation IDs.
- OpenAI-compatible serving only as one adapter, not the universal API.
