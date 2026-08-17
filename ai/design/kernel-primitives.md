# Fleet Kernel Vocabulary and Model

**Status:** accepted v0 kernel contract for `tk-byam`
**Updated:** 2026-08-17

This is the kernel contract. Implement these types. Older architecture docs
may still say cell, host, or treat Allocation as a lease; those names map
here and must not become a second kernel vocabulary.

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
| `Allocation` | Concrete claims selected for a Request |
| `Lease` | Committed, time-bounded right to use an Allocation |
| `Binding` | One Lease enforced on one Node by one Provider |
| `Agent` | Long-running process on a machine Node that discovers and enforces |

`resource` remains the domain word for anything Fleet can consume or use for
placement. In the kernel data model, those things are `Node`s. There is no
second `Resource` struct competing with `Node`.

A product **Workload** compiles to one or more Requests. It is not a kernel
type.

## Older-doc mapping

| Older name | Kernel name |
|---|---|
| Cell, cell allocator | `Cluster` |
| Host | `Node(kind=Machine)` |
| Allocation as ownership object | `Lease` |
| Resource set / selected resources | `Allocation` |
| FenceToken, fencing token | `Binding.fence` |
| NodeIncarnation, AgentEpoch | `Agent.session` |
| Placement, Plan, AllocationPlan | not kernel types |

Do not implement the older names.

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
log, one Graph, one lease table, and the Binding records for those leases. Its
replicas agree on ownership transitions. Ordinary allocation stays inside the
Cluster; a wider Fleet may delegate capacity or policy to it without sitting on
every local decision.

A Cluster is not a Node and is not a `NodeKind`. Physical failure domains such
as Region, Datacenter, Rack, and PowerDomain are Nodes in its Graph. A Fleet
may manage multiple Clusters; they do not share a lease table. A small
installation has one Cluster.

`Cell` is not used in the kernel vocabulary. It is precise in Borg and Chubby,
but has different meanings in Nova, storage systems, and topology discussions.
`Cluster` is the term a new operator already understands for a managed set of
machines and its authority.

v0 implements one in-memory Cluster over the same log contract. Replication,
membership, and federation are later implementations of this boundary, not new
kernel types.

If a Cluster cannot establish authoritative agreement:

- existing work may continue under its Lease policy;
- new exclusive Leases are refused;
- unsafe failover is refused.

After recovery, `Cluster.epoch` advances and commands from the previous epoch
are rejected.

## Graph, Node, and Edge

A **Graph** is the logical placement picture. It is not a graph database. The
Cluster log is authoritative; the Graph and its indexes are derived and
rebuildable. Node and Edge IDs are stable across revisions.

```text
Graph
  revision     advances when topology or total capacity changes
```

```text
Node
  id
  kind
  attrs
  capacity     dimension → total units
```

```text
Edge
  from
  to
  kind         Contains | SameNuma | SamePcie | Connected
  attrs        bandwidth, latency, or other edge data
```

v0 `Node.kind` values:

```text
Machine, Rack, PowerDomain, Socket, Numa, Cpu, Memory,
PcieRoot, Gpu, Nic, Nvme
```

Region and Datacenter are valid kinds when a Graph needs them. They are not
required for the first synthetic 3–10 machine Graphs.

Not v0 kinds: `Cell`, `AcceleratorPartition`, `DataObject`, `Switch`, `Fabric`,
`StoragePool`, `CXLDevice`. Later kinds extend this enum. They do not add a
second vertex type.

A machine Node contains sockets, memory, devices, and local storage. An Agent
runs on a machine Node. A GPU Node is not a machine and does not run an Agent.
This resolves the two common meanings of “node” without a second generic
resource type: Graph code uses Node; operators can say machine, GPU, or rack.

Default capacity:

- `Cpu`, `Gpu`, `Nic`, `Nvme`: `{count: 1}` each. Model each core or device as
  its own Node.
- `Memory`: `{bytes: N}` on the NUMA-local Memory Node.
- Topology Nodes (`Machine`, `Rack`, `Socket`, `Numa`, `PcieRoot`,
  `PowerDomain`) constrain placement through Edges. They are not occupied
  unless a Request claims exclusive use of that Node.

v0 occupancy is exclusive. Shared capacity, oversubscription, and GPU
partitioning are later.

The Graph has materialized indexes for:

- ID and kind lookup;
- parent and children through `Contains`;
- remaining exclusive capacity per Node;
- adjacency by Edge kind;
- attributes used in hard constraints.

Rebuild every index by folding the log from empty. Indexes never grant or
revoke a Lease. Health, temperature, and contention may influence an
Allocation, but are not authoritative ownership state.

## Request, Need, and Allocation

A **Request** is abstract intent. It does not name concrete Node IDs.

```text
Request
  class         Service | Batch | Gang
  needs         list of Need
  topology      required Edge relationships among selected Nodes
  preferences   scoring only; never a hard filter
  lifetime
  priority
```

```text
Need
  kind          Node.kind to consume
  quantity      dimension → units
  filters       hard attribute and topology predicates
```

A **Claim** is one concrete consumption. An **Allocation** is the concrete
result of matching a Request against a Graph revision:

```text
Claim
  node
  quantity      dimension → units; omitted means the Node's full capacity
```

```text
Allocation
  claims
  graph_revision
  explanation
```

The claimed Node set is derived from `claims`. Allocation is a value, not
authority and not a lifecycle. The scheduler computes it; the Cluster discards
it if occupancy changed, `graph_revision` no longer matches, or provider
preparation fails.

This is **Allocation**, not `Placement` or `Plan`:

- placement describes the act or policy of selecting;
- plan describes workflow and may include unrelated changes;
- allocation names the concrete resources assigned to a workload in Slurm and
  Flux.

The distinction from Lease is direct:

- **Allocation:** which Node units;
- **Lease:** who may use them, until when, and under which authority.

A gang Request prepares every member before commit. There is no silent partial
Lease. Every refusal and Allocation includes an explanation.

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

A child Lease may use only claims already covered by its parent. Two active
exclusive Leases cannot cover the same Node units.

The lifecycle is ordinary and explicit:

- it **expires** when `expires_at` passes;
- it is **released** when its owner gives it back;
- it is **revoked** when the Cluster takes it back;
- it **fails** when required provider preparation cannot complete.

`expires_at` is the deadline. `Expired` is the resulting state. Expiration is
not fencing by itself; every Binding must be closed or fenced before its claims
are available to another Lease.

Renewal, fence increment, and partition recovery are in
[`lease-fencing.md`](lease-fencing.md). Those operations must preserve
exclusive occupancy and Binding closure.

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
owns Lease intent and Binding records; the Provider owns endpoint-specific
enforcement.

There is no `FenceToken` type. `Binding.fence` is a scalar carried in a
provider request. The endpoint remembers its latest fence for that Node and
rejects an older one. The protocol is [`lease-fencing.md`](lease-fencing.md).

A Lease ID or expiration time cannot stop a delayed old operation. The endpoint
must reject an older fence. Closing or fencing all Bindings happens before
making their claims available to a new Lease.

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

IDs are opaque newtypes. v0 may use integers.

`Cluster.now` is the clock used for `expires_at`. The simulator injects it.

## Ownership

| Object | Owner |
|---|---|
| Command log, Graph, lease table, Binding records | Cluster |
| Endpoint enforcement | Provider, through a Binding |
| Discovery, Binding apply, health report | Agent |
| Scoring weights and preferences | Replaceable policy; not authority |

Telemetry is not authoritative allocation state.

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

Selection is a pure function of `(Graph, active claims, Request)`. It does not
mutate Cluster state.

Commit is accepted only when:

- `Allocation.graph_revision` equals the current `Graph.revision`;
- the claims are still free of overlapping exclusive Leases;
- every required Binding is prepared.

Otherwise the Allocation is discarded and selection may run again. A later
lease commit can invalidate occupancy without advancing `Graph.revision`.

When a Lease expires, is released, or is revoked, the Cluster closes or fences
every Binding before making its claims available to another Lease. Revoking a
parent fences its children.

## Command log and replay

Authoritative commands:

```text
ApplyGraph              register or update Nodes, Edges, and total capacity
OpenLease               Preparing
ActivateLease           Active, with its Bindings
FailLease
ReleaseLease
RevokeLease
ExpireLease
RenewLease
OpenBinding
ActivateBinding
FenceBinding
FailBinding
ReleaseBinding
SetAgentSession
QuarantineNode
UnquarantineNode
```

Replay input is the command log plus injected clock, health snapshot, and
faults. Production and the simulator use the same state-transition function.
The same input must produce the same Graph revision, Allocations, Leases,
Binding states, and digest.

`ApplyGraph` advances `Graph.revision`. Lease and Binding commands change
occupancy and Binding state; they do not by themselves advance
`Graph.revision`.

## v0 proof

1. Register machine Nodes with CPU, memory, NUMA, GPU, PCIe, NIC, and NVMe
   Nodes.
2. Build Graph indexes and replay them from the Cluster log.
3. Produce explainable Allocations for service, batch, and gang Requests.
4. Activate a Lease and prove exclusive claims do not overlap.
5. Activate a child Lease and prove it stays within its parent.
6. Expire, release, and revoke Leases; prove Bindings close or fence.
7. Restart an Agent; reject messages from its old session.
8. Replay the trace and match the state digest.
9. Fail a machine Node or Provider and produce an explicit recovery decision.

## Deferred

Fencing protocol: [`lease-fencing.md`](lease-fencing.md).

Not in this kernel contract:

- dynamic contention or health edges as authority;
- shared capacity and accelerator partitions;
- data objects and cache edges;
- virtual-cluster network, storage, and identity;
- consensus implementation and crate layout;
- runtime, device, network, and storage providers.

Runtime integrations, a Cargo workspace, and production provider protocols
follow this contract and `tk-l8xd`.
