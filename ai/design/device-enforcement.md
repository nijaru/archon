# Device enforcement

How device claims (Gpu, Nic, Nvme) become real hardware access, not just
scheduler bookkeeping.

## Identity

Device nodes carry their host identity in the graph attr `dev` (a device
path, e.g. `/dev/nvidia0`). Agents declare devices at registration via
`ARCHON_DEVICES` (`kind:path[,kind:path]`, kind in `gpu|nic|nvme`); the
graph fragment gains one node per device: capacity `Count=1`, attr `dev`,
child of the Machine. Real discovery (lspci/nvidia-smi, CDI specs) is a
later upgrade behind the same attrs.

## Exclusivity

Claims already pin specific nodes: claiming `Count=1` of a capacity-1
device node is exclusive at the scheduler level — the claim cannot be
duplicated, and capacity shrink/refusals apply. Kernel-level exclusivity
follows from enforcement below: a lease's processes only get the devices
they claimed.

## Flow

1. Request declares a need of device kind (`--gpu N` on the CLI).
2. Admission claims specific device nodes (existing machinery).
3. At activation the controller resolves the allocation's device-kind
   claims through the graph and sends the `dev` paths in
   `AgentRequest::Activate.devices`.
4. Enforcement by executor:
   - **Container**: `--device <path>:<path>` (rw). CDI / `--gpus` and
     NVIDIA-specific handling are later upgrades on the same seam.
   - **Process**: cgroup v2 has no devices controller; kernel filtering
     needs an eBPF cgroup-device program (rootful). v0: processes receive
     their devices implicitly (they see the host's /dev) and enforcement
     is documented as container-only.

## Proof without GPU hardware

Desktop: declare `ARCHON_DEVICES=gpu:/dev/net/tun`, submit
`--gpu 1 --image busybox -- ls /dev/net/tun`. The device must exist
inside the lease's container and the lease must complete — exercising
declaration, placement against a device claim, resolution, and
passthrough end to end. GPU exclusivity proof (container toolkit, MPS
off, MIG) stays hardware-gated.
