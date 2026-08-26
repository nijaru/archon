# Deployment and Integration

Archon's current deployment target is ordinary Linux. Archon runs above Linux
and existing hardware/runtime stacks; it does not require a custom host OS.

The authoritative design is maintained in the private project context at
[`design/deployment-and-adoption.md`](https://github.com/nijaru/agent-context/blob/main/projects/github.com/omendb/archon/ai/design/deployment-and-adoption.md).
This file records the implementation-facing constraints that must remain true in
this repository.

## Host model

A normal deployment has one Archon Cluster authority and one Archon agent per
managed Linux machine. The agent discovers the capabilities it can actually
enforce and registers them with the Cluster.

Archon reuses the host mechanisms appropriate to a workload, including cgroup
v2, namespaces, eBPF, OCI runtimes, KVM/VMMs, CDI/VFIO/SR-IOV, vendor drivers,
NCCL/RCCL, RDMA, Linux networking, and existing storage systems.

The current native-process path relies on Linux cgroup v2 and, where used,
`clone3(CLONE_INTO_CGROUP)`, `cgroup.kill`, and cgroup-device BPF. Unsupported
capabilities must be reported or rejected; do not silently weaken an accepted
resource or isolation contract.

## Provisioning boundary

Archon does not currently own machine provisioning. PXE/imaging, Terraform,
cloud APIs, MAAS, Tinkerbell, or another provisioning system may create and
prepare a machine. Archon's responsibility begins when an agent joins with a
usable Linux/runtime/driver environment and publishes its enforceable resources.

Bare-metal provisioning may be added later behind a real deployment need. It is
not a prerequisite for the resource manager.

## Resource authority

A schedulable resource must have one unambiguous authority boundary at a time.

Do not design a path where Archon and Kubernetes, Slurm, Flux, Nomad, or another
scheduler independently believe they own the same exclusive CPU/device/storage
resource.

Valid coexistence includes:

- different machines or explicitly disjoint resource sets under different
  authorities;
- an outer scheduler delegating a bounded allocation to Archon for evaluation;
- Archon granting a bounded parent lease to a nested scheduler/runtime.

The normal initial migration unit should be a whole machine or another clearly
enforced resource set. Fine-grained same-host dual ownership is not an initial
requirement.

## Datacenter adoption

Archon must not require an all-at-once migration.

A typical adoption path is:

1. drain a bounded set of machines/resources from the incumbent authority;
2. prove the old authority has released/fenced them;
3. start/register Archon agents;
4. let Archon become resource authority for that set;
5. run existing binaries/images/runtimes through Archon providers;
6. expand only after the deployment is proven.

Removing a resource from Archon reverses the ownership order: stop new
placement, drain or revoke active leases, close/fence bindings, remove the
resource from Archon authority, then hand it to another system.

## Hardware providers

Hardware-specific behavior stays behind discovery/provider boundaries:

```text
hardware
-> Linux/vendor discovery
-> stable identity + capabilities + topology
-> Archon resource graph
-> allocation + lease
-> provider prepare/attach
-> workload-visible resource
-> health/reconcile/detach/reset
```

The resource kernel should model placement/ownership facts, not vendor APIs.
CDI, VFIO, SR-IOV, MIG/MPS, vendor libraries, and similar mechanisms belong in
providers.

## Network and storage

Archon may own scheduling-relevant topology/capacity, attachment/detachment,
policy intent, QoS constraints, health, and fencing. Existing Linux/network
fabrics and storage systems remain the data plane.

Do not turn the core into a service mesh, L7 routing platform, packet-processing
stack, distributed filesystem, or storage engine merely to make deployment
self-contained.

## Existing workloads and nested schedulers

Existing OCI images, native binaries, vLLM/SGLang, PyTorch/JAX, MPI, and normal
Linux tooling should work through adapters/providers rather than requiring
application rewrites.

Kubernetes, Slurm, Flux, Ray, MPI, and similar systems may be compatibility
integrations or nested inside a bounded Archon lease. A nested system may
suballocate only the authority delegated to it.

During evaluation the relationship may be reversed: Archon can operate inside a
bounded allocation granted by an incumbent scheduler. That is an adoption mode,
not the target ownership model for a directly managed Archon Cluster.

## Simplicity

The useful small deployment remains:

```text
one Archon binary with role subcommands
one Cluster authority
one agent per managed machine
no mandatory external database/message bus/coordinator/service mesh
```

Do not add a mandatory service or deployment dependency unless a measured
correctness, scale, security, or workload requirement justifies it.
