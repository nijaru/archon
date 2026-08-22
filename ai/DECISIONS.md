# Archon Decisions

**Updated:** 2026-08-20

## Current direction

Archon is a distributed resource operating system. The product owns resource
control, leases, scheduling, node execution, and lifecycle across heterogeneous
compute. Inference and private accelerator fleets are first workloads, not the
definition of Archon. Existing schedulers and runtimes are optional integrations
or nested workloads; Archon is not merely a substrate that delegates control to
them.

The complete architecture is [`design/DISTRIBUTED_RESOURCE_OS.md`](design/DISTRIBUTED_RESOURCE_OS.md).
The accepted v0 kernel contract is [`design/kernel-primitives.md`](design/kernel-primitives.md).
The accepted fencing protocol is [`design/lease-fencing.md`](design/lease-fencing.md).

## Principles

- **Resource graph, not scalar node capacity.** Topology, locality, bandwidth,
  health, capability, cost, and failure domains are first-class.
- **Lease-first ownership.** Services, jobs, devices, VMs, and nested schedulers
  consume enforceable leases.
- **Mechanism separated from policy.** Discovery, state, fencing, and execution
  are core; placement and optimization are modular.
- **Hierarchical scheduling.** Global planning, Cluster allocation, node resource
  management, and workload-local scheduling have separate responsibilities.
- **Runtime and isolation neutrality.** Process, OCI, microVM, VM, and WASM are
  execution choices under one workload model.
- **AI/HPC are native policy domains.** Co-scheduled placement, reservations, goodput,
  communication topology, checkpoint cost, and elasticity are not afterthoughts.
- **Reuse mature infrastructure.** Linux, KVM, eBPF, OCI, CDI, storage,
  networking, accelerator drivers, and workload runtimes remain dependencies.
- **Compatibility without architectural capture.** Kubernetes, Slurm, Flux,
  Ray, MPI, and other systems are optional integrations or nested schedulers;
  Archon remains the native scheduler and resource authority for its workload
  classes.
- **Small deployments stay small.** A single VPS or small cell should not need a
  mandatory multi-service stack.
- **Explainability is a contract.** Placement, refusal, preemption, and
  recovery decisions include reasons and relevant resource state.

## Locked direction

| Decision | Choice | Rationale |
|---|---|---|
| Product category | Distributed resource operating system | One substrate for services, batch, HPC, AI, VMs, and WASM |
| Core primitive | Typed resource graph plus enforceable lease | Unifies heterogeneous capacity and ownership |
| Scheduler shape | Global planner → Cluster allocator → node manager → native Archon workload scheduler, with optional nested schedulers | Keeps policy and timing domains separate without delegating Archon's control authority |
| Resource scope | CPU, memory, accelerators, networks, storage, data, health, failure domains | Placement depends on more than node CPU/RAM |
| Runtime modes | Process, OCI, sandbox, microVM, VM, WASM | Isolation is a workload property |
| Implementation direction | Rust-first; no Go compatibility shell | Privileged infrastructure, concurrency, provider boundaries, WASM integration; the deleted Go scaffold encoded the wrong product |
| First workload | Accelerator-aware inference and services | Exercises topology, cache, health, cost, and runtime adapters |
| Compatibility | OCI, CDI, OpenTelemetry, Linux/KVM, standard protocols; optional integrations for K8s/Slurm/Flux/Ray/MPI | Adoption and composition without delegating native control |
| Node model | Minimal immutable Linux with an agent | Clear ownership, rollback, and hardware access |
| Extension model | Capability-limited WASM where practical | Policy extensibility without ambient host authority |
| License direction | Planned AGPL-3.0-or-later core; Apache 2.0 SDKs, schemas, examples, adapter interfaces | Preserve the resource authority as open source while keeping integrations easy |

## Reused systems

Archon should not rewrite:

- Linux, KVM, cgroups, namespaces, eBPF, WireGuard;
- OCI runtimes, containerd, Cloud Hypervisor, Firecracker, Wasmtime;
- Ceph, NVMe-oF, SPDK, NFS, object storage, and cloud block storage;
- GPU/accelerator drivers, CDI, VFIO, SR-IOV, NCCL/RCCL, MPI, and RDMA;
- vLLM, SGLang, Ray, PyTorch distributed, JAX, Slurm, or Flux.

## Superseded framing

The earlier model-first GPU-serving plan is retained only as a workload and UX
reference. Archon is not limited to:

- GPU-only scheduling;
- online inference;
- model registries and endpoints;
- direct-node deployments;
- Kubernetes avoidance;
- a gateway or runtime-specific control plane.

The direct node agent and vLLM path remain useful first adapters. They do not
constrain the resource, lease, or scheduler model.

## Decision log

| Date | Decision | Rationale |
|---|---|---|
| 2026-08-20 | Topology must be a forest; root-only Bindings; health is non-revision state | `Contains` cycles or multi-parent would hang ancestor/descendant walks or corrupt the parent index, so `ApplyGraph` validates staged state and rejects them atomically. Bindings belong to root Leases only: one endpoint per `(provider, node)` cannot hold two Leases' enforcement, and a child Binding would take over the endpoint and orphan the parent's enforcement on reconcile. `SetNodeHealth` changes attrs without advancing `Graph.revision` — health is scoring input, never authority, so updates never strand in-flight Allocations. `ReserveLease` retries are idempotent like `OpenLease`; the simulator dequeues an admitted request only after its lease opens. |
| 2026-08-20 | Health is a scoring input via a node attr, never authority | `health=degraded` on any ancestry node penalizes candidate score (-20,000) and is noted in the explanation, but never refuses placement — degraded resources remain usable for lower-priority workloads. Hard failure stays quarantine. Attrs ride `ApplyGraph`, so health updates advance the graph revision that Allocations pin. Superseded in part: health now rides `SetNodeHealth` and does not advance the revision. |
| 2026-08-20 | No Gang request class — co-scheduling is structural | Every Request is all-or-nothing: `select` is atomic per Request, one Lease commits atomically, and activation requires every Binding prepared. The Gang class had zero behavioral distinction, and the term is overloaded — in Slurm, gang scheduling means preemptive time-slicing. The class is removed (Service | Batch remain); a class returns only when a policy need (elasticity, checkpoint/restart, goodput) differentiates it, named after the guarantee. Pinned by `multi_member_request_places_atomically_or_not_at_all`. |
| 2026-08-20 | Harden the kernel against adverse command orderings (gpt-5.6-sol review) | Root activation now requires prepared Bindings over every enforced claim plus deadline, expiry, and quarantine checks. Releasing a lease with live descendants is refused; expiry and failure cascade to descendants and fence their Bindings; sibling accounting counts whole descendant subtrees. Duplicate `OpenBinding` is state-aware (Prepare only while Preparing). Binding result commands carry the fence and are rejected on mismatch; a failed Binding fails its Lease. `SetAgentSession` and endpoint handshakes are monotonic. `ApplyGraph` validates staged state before mutating. Backfill treats an unproven shadow as unbounded (disjoint-claims jobs only). Fair-share usage always comes from the Cluster's own lease table; count-based budget charges are at least one unit. The simulator holds a failed machine's fence acks until a restarted Agent reconciles. |
| 2026-08-20 | Backfill is EASY-style over the priority queue, shadow from lease expiry | `admit_backfill` walks the queue in admission order; a request that cannot select becomes a blocked head with shadow = earliest expiry event time at which it selects. A later request starts only if it finishes by every blocked head's shadow or claims none of that head's shadow claims — it can delay nothing. Unsatisfiable requests are dead and block nobody. The model is optimistic: renewals may push real starts later, never earlier. |
| 2026-08-20 | A reservation is a Lease in `Reserved` state, not a second ownership type | Lease-first ownership: committed capacity with no Bindings, reusing lease expiry, release, revoke, occupancy, preemption, fair-share usage, and replay. `ReserveLease` opens root-only; `PromoteLease` moves Reserved → Preparing and re-checks graph revision and overlap before the ordinary prepare path. Reservations occupy, so lower-priority reservations preempt like running work. |
| 2026-08-20 | Define the v0-to-v1 transition trigger | v0 shipped as decision logic without node execution; the execution gates move to v1 entry. v1 starts on a named real workload, a needed provider adapter, or a policy requiring real-node timing/enforcement. Simulator-first stays the default for policies the simulator can validate fairly. |
| 2026-08-21 | Link auth: Greeting frame with optional shared token | Connections now open with a typed Greeting (role + token) instead of first-frame role sniffing — one dispatch path, and the natural place for credentials. Server-enforced: --token-file/ARCHON_TOKEN set means mismatch closes the connection pre-work, compared constant-time; unset means open mode with a loud warning (dev ergonomics preserved). Chose connection-level shared token over per-frame HMAC or per-agent credentials: v0 threat model is accidental connections and casual tampering on trusted networks; TLS/per-agent creds are the upgrade path. |
| 2026-08-21 | Health: controller-side probes; keep-alive restarts capped, not infinite | Health detection is controller-driven (probe each registered agent per tick) rather than agent heartbeats — v0's lockstep framing makes controller-initiated probes free, and dead connections surface on the next probe. Unhealthy = quarantine + fail live leases (drop-on-no-agent already covers the dead side). Keep-alive restarts re-queue as fresh requests so admission/backfill naturally rate-limit placement, but a per-request cap (5) prevents a permanently-broken workload from spinning through the cluster forever — bounded failure is a resource-OS invariant. Run-once stays dead; the distinction is the workload contract. |
| 2026-08-21 | Storage/network v0: workload intent on Request, enforced by the container adapter | Volumes and ports are workload intent (like command/image), not graph resources yet — bind mounts and port publishing are enforced by the container adapter; the process adapter ignores them since bare processes share the host fs/network. Modeling storage as claimable graph resources (managed volumes) is the deeper design and stays future; the adapter hook is the part that had to exist first. Also: NodeService.submit dropped its duplicate command parameter — request.command is the single source of truth. |
| 2026-08-21 | Container execution via engine CLI, not a Rust OCI runtime | Request.image routes a lease to ContainerRuntime, which shells out to Docker/Podman (shared CLI surface) instead of embedding crun/youki. Rationale: the engine already solves image transport, rootfs assembly, and namespace wiring; embedding an OCI runtime would rebuild that for no enforcement gain at this stage. Lease claims map to --cpus/--memory; termination force-removes the container. The engine CLI boundary is swappable if a native runtime becomes necessary. |
| 2026-08-21 | Machine-local claims by default; explicit opt-out for spread | Heterogeneous testing exposed that select_need satisfied N-unit needs with claims from multiple machines — an allocation no host can run. Decision: one need's claims share a single Machine ancestor unless Request.machine_local is false (explicit multi-member groups like atomic co-scheduling). This is the semantics processes and containers require; spreading was implicit and unsafe as a default. Also fixed en route: read-only Status probes were adopting u64::MAX as an agent session generation, poisoning later effects — fencing protects mutations only, so reads now bypass sessions entirely. |
| 2026-08-21 | Stable agent identity: persistent instance id in machine attrs | Hostname-as-identity flapped when two agents claimed one name. The agent persists a random instance id; the controller stamps it into the Machine node's attrs at ApplyGraph so replay recovers it. Registration matches by id; same name with a different id is a new machine, never a takeover. Two invariants fell out and are pinned: replay resumes sessions above the logged high-water mark (a restarted controller must not hand out old generations), and Reconcile re-drives only Active leases (revoked/expired work stays dead across reconciliation). |
| 2026-08-21 | Multi-node: dial-in registration, drop-on-no-agent, reconcile-by-name | Agents dial the control plane (NAT-friendly, reconnection natural) and announce their machine; effects route by graph.machine_of. Three explicit semantics: (1) an effect whose agent is missing is dropped, not errored — kernel state proceeds and Reconcile re-drives when the agent returns, which is honest because a dead agent holds no processes; (2) re-registration under the same machine name matches the existing machine and re-drives via SetAgentSession -> Reconcile -> RebindSession -> ActivateBinding, all idempotent kernel ops; (3) machine identity is currently the hostname (--name overrides) — same-name agents flap by design until stable instance identity lands. |
| 2026-08-21 | Rename the project to Archon; one `archon` binary | The working name 'fleet' was temporary. Archon is simple: one word, one binary, role subcommands. Repo renamed to `omendb/archon`. |
| 2026-08-21 | Control plane: JSONL command log, replay recovery revokes live work | Cluster state must outlive processes. Every applied command (kernel + agent records) is appended to a log; replay is exact because the kernel is deterministic. Live leases at crash are revoked, not re-executed — a fresh agent holds no processes, so re-execution would silently respawn user work; revocation is the honest state. Kernel serde is an opt-in feature so the kernel stays dependency-free. The graph of record enters the log on first boot; restarts replay it rather than rediscovering. |
| 2026-08-21 | Remote agent protocol: length-prefixed JSON over TCP | The Effect/Record seam was already protocol-shaped, so remoteness only needed a wire format plus an agent core shared by local and daemon paths (one implementation, two transports). serde_json chosen over hand-rolled framing as the first new dependencies. Session+fence ride on every frame; the agent rejects stale generations exactly like kernel endpoints. Multi-node scheduling (routing Effects to per-machine agents) is deliberately deferred to the control plane workstream. Known gap recorded: spawn-before-cgroup-attach leaves a brief escape window; fix via clone3 when the runtime seam is next touched. |
| 2026-08-21 | cgroups v2 adapter: one cgroup per lease, claims as kernel limits | Resource isolation was the gap between lifecycle enforcement and real enforcement. A lease's CPU/memory claims map directly to `cpu.max`/`memory.max` in a per-lease group under a configurable root; termination uses `cgroup.kill` and removes the group; `ProcessRuntime::drop` kills survivors so a crashed agent cannot leak processes. No new dependencies — plain cgroupfs writes. Tests probe cgroupfs writability and skip where unavailable (CI), run for real on enforcement hosts. |
| 2026-08-21 | Fire the v1 trigger by declaration; build the walking skeleton | Waiting for an external workload left Archon a verified design forever. The skeleton — one machine, real processes under leases — is the concrete need. `Request.command` carries the workload payload (execution intent, not resource state); commands live outside the kernel log, keyed by request and lease. The node agent consumes kernel `Effect`s through the simulator's seam, so the decision/enforcement boundary is unchanged. Enforcement is lifecycle-only (spawn/kill) on macOS; cgroups isolation on Linux is the next adapter, then a remote agent protocol. |
| 2026-08-20 | Second review (gpt-5.6-sol) fixes: expiry validates first, occupancy sums roots, shadows respect fencing and quarantine, promotion revalidates | A rejected `ExpireLease` no longer mutates descendants (validation precedes the cascade). Root occupancy sums independent root trees instead of taking the max — partial memory claims can no longer overcommit — and `subtree_used` counts only direct children, charging each delegation lineage once (grandchild claims are subsets of their parent's). Backfill projections exclude only live leases at expiry (fence acks have no provable time) and treat quarantine-blocked heads as temporarily blocked (shadow unbounded, disjoint-only backfill) instead of dead. `PromoteLease` revalidates committed claims against the current graph and refreshes the revision, so an unrelated `ApplyGraph` cannot strand a reservation. Also: `Admission` carries the owner (fair share cannot be charged to a different owner); same-tuple `ApplyGraph` edges update attrs in place; digests cover capacities and edges; memory fragmentation scoring is bounded below the locality/health tiers; equal-session handshakes are idempotent and re-emit reconcile; `DataObject` is a locality hint and cannot be claimed. |
| 2026-08-20 | Fair-share ceilings are per owner per node kind (TRES-style) | An aggregate Count budget conflated 1 GPU with 1 CPU and could not express "2 GPUs and 4 CPUs per owner". `KindUsage` keys ceilings and usage by `NodeKind` — the fair-share analogue of Slurm's trackable resources. Kinds without a ceiling are unconstrained; an empty map disables the budget. Charges match selection: count-based needs claim at least one unit of their kind, memory claims bytes. |
| 2026-08-17 | Fair-share admission is a per-owner budget ceiling, not a reservation | An owner under budget is never blocked by another owner's consumption. An empty ceiling disables the budget and matches plain `admit`. Ceiling dimensionality resolved per kind in 2026-08-20. |
| 2026-08-17 | All nine required v0 scenarios pass | Registration, service/batch/gang, hard-filter refusal, exclusive claims, nested leases, revoke/fence/stale-agent, release-and-replace, deterministic replay, and machine failure with quarantine recovery. |
| 2026-08-17 | First workspace is kernel plus simulator | `crates/kernel` owns types and `Cluster::apply`. `crates/sim` owns delivery, clock, faults, and v0 proofs. Do not add the later `research/stack.md` crate map yet. |
| 2026-08-17 | Accept v0 fencing protocol | Occupancy lasts until Binding close/fence ack or Node quarantine. `Binding.fence` increases per `(provider, node)`. `Release` is cooperative; `Fence` is forced; both close the generation. Uncertain apply is never success. `RenewLease` extends `expires_at` only. Stale `Agent.session` and `Cluster.epoch` are rejected. |
| 2026-08-17 | Accept v0 kernel contract | `ai/design/kernel-primitives.md` is the kernel vocabulary and model. `Cluster` is the linearizable authority. `Allocation` is claims against a Graph revision. `Lease` is committed authority. Occupancy is exclusive Node-unit claims; Memory is quantified, devices/cores are discrete Nodes. Graph and indexes rebuild from the command log. `Cell`, `Host`, `Placement`, `Plan`, `FenceToken`, and `NodeIncarnation` are not kernel types. |
| 2026-08-17 | Working kernel vocabulary | Superseded by the accepted v0 kernel contract. The vocabulary itself did not change. |
| 2026-08-17 | Delete the Go model-serving scaffold | The stubs encoded models, endpoints, replicas, GPU telemetry, Postgres, and NATS; keeping them would define a second, wrong product |
| 2026-08-16 | Reframe Archon as a distributed resource OS | The resource graph, lease, hierarchy, and nested-scheduler model is the actual long-term product idea |
| 2026-06-07 | Use a workload-agnostic node core | The agent should receive a workload spec and pass workload-specific metadata to runtimes |
| 2026-06-07 | Use direct node execution as an initial path | Host-level device and lifecycle control should not depend on a pod abstraction |
| 2026-06-07 | Use CDI as the initial device attachment boundary | Vendor-neutral device specification for OCI workloads |
| 2026-06-06 | Keep NATS optional for the first local proof | Event durability and fanout should follow an actual control-plane need |
| 2026-06-06 | Keep Archon runtime-neutral | Archon owns lifecycle and allocation, not inference-engine internals |
| 2026-05-29 | Keep source-available core and Apache SDK/schema boundary | Historical scaffold decision; superseded by the AGPL-first 2026-08-16 release plan |

## Open decisions

- authority replication and global federation beyond one Cluster log;
- static versus dynamic topology and contention edges;
- dynamic topology, contention, and health representation;
- provider contracts for accelerator partitioning and preemption;
- virtual-cluster network, storage, and identity semantics;
- device-level preemption/reset contracts across vendors;
- security model for hostile multi-tenancy and confidential workloads.
