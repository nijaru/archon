# Fleet Kernel Vocabulary and Model

**Status:** working v0 design for `tk-byam`
**Updated:** 2026-08-17

This is the v0 vocabulary under review. Older architecture docs retain the
broader planning vocabulary until this contract is accepted; do not implement
both vocabularies.

This vocabulary is chosen as a set. The terms should read naturally in one
story, not win isolated naming contests.

## The story

> A **Cluster** maintains a **Graph** of **Nodes** and **Edges**. A workload
> sends a **Request**. The scheduler returns an **Allocation**. The Cluster
> commits a **Lease** for that allocation. Providers enforce the lease through
> **Bindings**. An **Agent** runs on each machine node.

```text
Cluster
├── Graph
│   ├── Node(kind=Machine)
│   │   ├── Node(kind=Cpu)
│   │   ├── Node(kind=Gpu)
│   │   └── Node(kind=Nic)
│   └── Edge
├── Request → Allocation
└── Lease → Binding → Provider
                 ↑
               Agent
```

Example:

> The Cluster allocated two GPUs and their NUMA-local CPUs from its Graph,
> created a Lease for job 42 until 12:00, and activated Bindings through the
> Agents on the selected machine Nodes. When the Lease expired, the Bindings
> were fenced and the Nodes became available again.

## Vocabulary

| Name | Job |
|---|---|
| `Cluster` | One linearizable authority boundary: log, graph, and leases |
| `Graph` | Nodes plus typed topology and locality edges |
| `Node` | One graph vertex; a resource or placement constraint |
| `Edge` | A typed relationship between Nodes |
| `Request` | Abstract workload requirements and policy |
| `Allocation` | Concrete Nodes selected for a Request |
| `Lease` | Committed, time-bounded right to use an Allocation |
| `Binding` | One Lease enforced on one Node by one Provider |
| `Agent` | Long-running process on a machine Node that discovers and enforces |

`resource` remains the domain word for anything Fleet can consume or use for
placement. In the kernel data model, those things are `Node`s. There is no
second `Resource` struct competing with `Node`.

## Why these names

These names borrow conventional systems language where the semantics match:

- Flux calls the logical object a resource graph and the concrete assigned set
  a resource set; Slurm calls concrete selected resources an allocation.
- Kubernetes and HPC schedulers call the machine a Node.
- `Lease` is the conventional time-bounded ownership/authority primitive.
- `Binding` is the conventional act of making a claim or assignment concrete
  at a target/provider.
- `Agent` names the process role. `daemon` describes how Linux supervises it.

The names are not copied as architecture. Fleet's distinctive contract is the
combination of graph selection, enforceable leases, provider bindings, and
hierarchical authority.

Evidence used for the terminology:

- [Flux RFC 14](https://flux-framework.readthedocs.io/projects/flux-rfc/en/latest/spec_14.html) separates abstract resource requests from concrete assignments.
- [Flux RFC 20](https://flux-framework.readthedocs.io/projects/flux-rfc/en/latest/spec_20.html) calls a concrete set of assigned resources a resource set.
- [Slurm `salloc`](https://slurm.schedmd.com/salloc.html) calls a granted set of resources a job allocation.
- [Kubernetes Nodes](https://kubernetes.io/docs/concepts/architecture/nodes/) uses Node for the managed machine and its capacity.
- [Nomad scheduling](https://developer.hashicorp.com/nomad/docs/concepts/scheduling/how-scheduling-works) uses a plan for scheduler changes, not the selected resource set.

## Cluster

A **Cluster** is the unit that may say yes. It owns one authoritative command
log, one Graph, and one lease table. Its replicas agree on ownership
transitions. Ordinary allocation stays inside the Cluster; a wider Fleet may
delegate capacity or policy to it without sitting on every local decision.

A Cluster is not a Node and is not a `NodeKind`. Physical failure domains such
as Region, Datacenter, Rack, and PowerDomain are Nodes in its Graph. A Fleet
may manage multiple Clusters; a small installation has one.

`Cell` is not used in the kernel vocabulary. It is precise in Borg and Chubby,
but has different meanings in Nova, storage systems, and topology discussions.
`Cluster` is the term a new operator already understands for a managed set of
machines and its authority.

## Graph, Node, and Edge

A **Graph** is the logical placement picture. It is not a graph database. The
Cluster log is authoritative; the Graph and its indexes are derived and
rebuildable.

```text
Edge
  from
  to
  kind       Contains | SameNuma | SamePcie | Connected
  attrs      bandwidth, latency, or other edge data
```

A **Node** is one vertex in the Graph. Its `kind` distinguishes a machine from
an accelerator or a topology/failure-domain object:

```text
Machine, Rack, PowerDomain, Socket, Numa, Cpu, Memory,
PcieRoot, Gpu, Nic, Nvme
```

A machine Node contains sockets, memory, devices, and local storage. An Agent
runs on a machine Node. A GPU Node is not a machine and does not run an Agent.
This resolves the two common meanings of “node” without a second generic
resource type: Graph code uses Node; operators can say machine, GPU, or rack.

The Graph has a `revision` and materialized indexes for:

- ID and kind lookup;
- containment;
- free exclusive capacity;
- adjacency by Edge kind;
- attributes used in hard constraints.

Indexes never grant or revoke a Lease. Health, temperature, and contention may
influence an Allocation, but are not authoritative ownership state.

## Request and Allocation

A **Request** is abstract intent. It contains resource quantities, hard
constraints, preferences, topology requirements, lifetime, priority, and
execution requirements. It does not name concrete Nodes.

An **Allocation** is the concrete result of matching a Request against a Graph
revision:

```text
Allocation
  nodes
  graph_revision
  explanation
```

Allocation is a value, not authority and not a lifecycle. The scheduler
computes it; the Cluster may discard it if provider preparation fails.

This is **Allocation**, not `Placement` or `Plan`:

- placement describes the act or policy of selecting;
- plan describes workflow and may include unrelated changes;
- allocation names the concrete resources assigned to a workload in Slurm and
  Flux.

The distinction from Lease is direct:

- **Allocation:** which Nodes;
- **Lease:** who may use them, until when, and under which authority.

## Lease

A **Lease** is the committed right for an owner to use an Allocation:

```text
Lease
  id
  owner
  allocation
  parent                 optional enclosing Lease
  expires_at
  state                  Preparing | Active | Released | Expired | Revoked | Failed
```

A child Lease may use only Nodes already covered by its parent. An exclusive
Node capacity cannot be covered by two active exclusive Leases.

The lifecycle is ordinary and explicit:

- it **expires** when `expires_at` passes;
- it is **released** when its owner gives it back;
- it is **revoked** when the Cluster takes it back;
- it **fails** when required provider preparation cannot complete.

`expires_at` is the deadline. `Expired` is the resulting state. Expiration is
not fencing by itself; every Binding must be closed or fenced before its Nodes
are available to another Lease.

## Binding

A **Binding** is one Lease enforced on one Node through one Provider:

```text
Binding
  id
  lease
  node
  provider
  fence                  monotonic order for this provider endpoint
  agent_session
  state                  Preparing | Active | Released | Fenced | Failed
  provider_handle        opaque provider state, if needed
```

A compute Binding may install cgroups. An accelerator Binding may configure a
device broker. A storage Binding may establish writer fencing. The Cluster
owns Lease intent; the Provider owns endpoint-specific enforcement.

There is no `FenceToken` type. `Binding.fence` is a scalar carried in a
provider request. The endpoint remembers its latest fence for that Node and
rejects an older one. The exact protocol belongs to `tk-l8xd`.

A Lease ID or expiration time cannot stop a delayed old operation. The endpoint
must reject an older fence. Closing or fencing all Bindings happens before
making their Nodes available to a new Lease.

## Agent

An **Agent** is the long-running process on a machine Node. It discovers Nodes,
prepares and enforces Bindings, reports health, and reconciles actual state. It
does not own Leases.

`Agent` names the role. `daemon` describes deployment under Linux or systemd.
A restart creates a new `session` value, so a late message from the previous
process can be rejected without inventing `NodeIncarnation` or `AgentEpoch`.

## Versions and identities

Use each word for one kind of version or identity:

| Field | Meaning |
|---|---|
| `Cluster.epoch` | Authority period; advances after authority loss and recovery |
| `Graph.revision` | Graph snapshot used by an Allocation |
| `Binding.fence` | Monotonic order checked at one provider endpoint |
| `Agent.session` | Identity of one running Agent process; changes on restart |

`epoch` is appropriate for the Cluster authority period. `revision` is
appropriate for a Graph snapshot. `fence` is appropriate for endpoint
ordering. `session` is appropriate for process identity. None needs a wrapper
struct in the first kernel.

## Commit path

```text
Request
  → filter hard constraints
  → score Graph candidates
  → Allocation
  → open Lease in Preparing state
  → prepare required Bindings
  → atomically record Lease + active Bindings
  → or release prepared Bindings and record Lease Failed
```

A gang Request prepares every member before commit. There is no silent partial
Lease. Every refusal and Allocation includes an explanation.

When a Lease expires, is released, or is revoked, the Cluster closes or fences
every Binding before making its Nodes available to another Lease. Revoking a
parent fences its children.

## Split brain and replay

If a Cluster cannot establish authoritative agreement:

- existing work may continue under its Lease policy;
- new exclusive Leases are refused;
- unsafe failover is refused.

After recovery, `Cluster.epoch` advances and commands from the previous epoch
are rejected. `Graph.revision` advances when topology or capacity state
changes.

The command log plus injected clock, health snapshot, and faults is the replay
input. Production and the simulator use the same state-transition function.
The same input must produce the same Graph revision, Allocations, Leases,
Binding states, and digest.

Telemetry is not part of authoritative ownership state.

## v0 proof

1. Register machine Nodes with CPU, memory, NUMA, GPU, PCIe, NIC, and NVMe
   Nodes.
2. Build Graph indexes and replay them from the Cluster log.
3. Produce explainable Allocations for service, batch, and gang Requests.
4. Activate a Lease and prove exclusive capacity does not overlap.
5. Activate a child Lease and prove it stays within its parent.
6. Expire, release, and revoke Leases; prove Bindings close or fence.
7. Restart an Agent; reject messages from its old session.
8. Replay the trace and match the state digest.
9. Fail a machine Node or Provider and produce an explicit recovery decision.

Runtime integrations, a Cargo workspace, consensus implementation details, and
production provider protocols follow this contract.
