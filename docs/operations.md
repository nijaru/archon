# Operating Archon in a lab

Start with the [README quickstart](../README.md#quickstart). This guide covers
what happens during restart, recovery and capacity reuse. It does not describe
a production deployment or high-availability setup.

## Controller persistence

The `--log` journal synchronously persists accepted submissions, service
registration/updates and scale targets, queued payloads, and admitted workload
lineage before acknowledging them or dispatching their execution effects.
A journal write failure terminates the controller rather than accepting volatile
work. Restart restores desired state; live leases still require agent
reconciliation, not blind re-execution.

`--compact-every N` sets the compaction interval in journal records (default
10,000); `0` disables compaction, not durability. Keep the log and its adjacent
`.snapshot` file together and restart with the same `--log` path.
Snapshot replay cursors make an interrupted log truncation safe, and recovery
truncates an incomplete final journal record before appending again. Malformed
complete records and gaps in journal sequence numbers fail boot.

This single-controller R&D format copies full controller metadata per journal
record and is intended for small workloads, not high submission rates or
unbounded retained history. Older kernel-only logs and version-1 snapshots can
be read, but cannot recover desired state that the old controller never wrote
or resolve an old version-1 compaction overlap. A legacy lease admission after a
version-1 snapshot is refused because its workload lineage cannot be proven;
preserve those files for operator reconciliation rather than discarding the log.
New snapshots use version 2; older binaries cannot read them.

The controller acquires a nonblocking exclusive OS lock on the journal before
snapshot loading, replay or tail repair, and holds it through compaction until
shutdown. A second controller using the same journal fails boot; process exit
releases the lock without stale-lock cleanup. This is local writer exclusion,
not distributed fencing or HA. Use a local filesystem with working file locks
and fsync; unsupported locking fails boot. The lock is advisory on Unix: do not
modify, delete, replace or externally rotate either durable file while the
controller runs, and do not run older binaries that bypass locking.

## Retrying a job submission

Choose a unique key before the first send, for example `archon -c ADDR submit
--key report-2026-11-20-01 -- sleep 30`. If the reply is lost, retry with the same
key and job arguments. The controller returns the same request ID and, once
admitted, its original lease ID. A retry does not start or replace a job;
reusing the key for different intent is rejected. Without `--key`, each submit
is a new request.

Use `archon -c ADDR queue` to list waiting request IDs, owners, submission
times (Unix seconds), and service groups where applicable. The list is in
submission order, not scheduler priority order. Use
`archon -c ADDR cancel-queued REQUEST` with a request ID from `queue` or
`submit` to cancel a standalone job before admission. If the job was already admitted,
the error gives its lease ID; use `revoke LEASE` for admitted work. To remove
service members, scale the group instead. Retrying a canceled job's original
submission key reports cancellation and does not enqueue new work.

Keys are journal-wide, 1–128 bytes, and retained with the submission history.
They are not credentials or scoped by an authenticated user. Retry against the
same journal, including after normal compaction or restart. A fresh journal,
an older backup that predates the submission, or running an older controller
against the same state does not preserve this guarantee.

## Closing a machine to new work

Use `archon -c ADDR machines` to find the numeric machine ID and see both its
resource state and whether it is cordoned. `archon -c ADDR cordon MACHINE`
blocks new allocations on the machine, including plans made before the cordon.
It does not evict running workloads or interrupt leases already preparing.

`archon -c ADDR uncordon MACHINE` removes only the operator's cordon; any
quarantine or other placement restriction still applies. Cordons survive agent
re-registration, controller restart, and journal compaction. A cordon alone
does not make the machine safe to shut down. In `machines`, `holding-leases`
lists the leases still charged to the machine or with open bindings there,
including terminal leases. Check those leases in `status` before maintenance.
`holding-leases=none` means the controller reports no such holds; it does not
check for unmanaged work or prove that the host can be shut down safely.

## Agent restart and recovery

`archon status` reports capacity separately from lease state. A `Revoked` lease
with `capacity=held` still counts against admission; revoke alone does not prove
teardown. For a terminal lease, `teardown=execution-stop` means the controller
still needs proof that execution stopped. `teardown=binding-closure` means it
is waiting for open resource bindings to close. `capacity=released` means this
lease no longer blocks reuse, though other leases or machine health can still
prevent placement.
`capacity=unknown` means the controller did not report occupancy.

Adoption requires member status and successful endpoint enumeration from the
returning agent's current session. Every expected open binding must match its
node, provider, scope, binding ID, fence, and active phase. A running process alone
is not ownership proof. Missing or inconsistent evidence blocks adoption; it
does not release occupied capacity. Stray endpoint closes must each be
acknowledged. Successful empty enumeration completes recovery for an idle machine.

Adoption does not reactivate resources or restart execution. Re-register the
agent to collect fresh recovery evidence; enumeration is not automatically
retried. A report from a new session is not proof that an older execution has
stopped.

New program leases have a journaled execution ID distinct from the lease
number. The controller records that the attempt may have been exposed before
sending the first start, and sends the same ID on status and stop after restart.
A lease proven never dispatched can close its bindings without an execution-stop
reply. A missing start reply or an older record without an assignment does not
prove that no start was sent.

Drain before upgrading across this execution protocol change. Controllers and
agents must use matching versions; old agents must not ignore the execution ID.
Do not downgrade with live leases.

A reported root process exit does not bypass execution teardown. For a tracked
native Linux cgroup, the agent must remove the execution scope before it
acknowledges stop.

For same-boot cleanup after agent replacement, set `ARCHON_EXECUTION_STATE_DIR`
on the remote agent (or controller for a local agent). Use an existing, private
local directory on a filesystem with working locks and fsync, and an existing
cgroup root dedicated to that agent. Enable this before launching workloads.
For native executions with cgroup limits and no device grants, the agent records
ownership before launch. A replacement using the same directory and cgroup root
can stop that exact scope and answer repeated stop requests. It does not adopt
or relaunch the workload. Replacement cleanup does not honor the old grace
period because it has no child handle. If the controller also restarts, replay
restores queued submissions and pending revocations. A terminal binding omitted
by the replacement agent is fenced again rather than treated as closed. Queued
work can use that capacity only after the agent proves execution stop and the
binding closes.

Keep the state directory and its records intact while a controller could replay
those attempts. Do not share them with workloads, move or replace either root,
or change configuration during recovery. Records are retained to reject stale
starts; automatic pruning is not implemented. Missing or invalid records and
changed boot or cgroup-root identity do not prove closure. This recovery path
does not cover OCI, devices, or earlier executions launched without records.

For OCI jobs, the agent obtains the engine's full container ID before asking
it to start. If the start reply fails, the same agent can still remove that
exact container before acknowledging teardown. If a removal reply is lost,
a successful engine query that confirms the exact ID is absent also proves
closure.

With Docker and `ARCHON_EXECUTION_STATE_DIR` configured, the agent also
persists the execution ID's single-use record, lease, daemon ID, and full
container ID before sending start. A replacement agent using the same state
directory and Docker daemon can remove that container without a grace period.
This is stop recovery, not adoption or automatic restart. Keep the engine
context fixed while an agent is running, and don't roll back, clone, or
replace the daemon state or execution records while assignments can be live.

The agent records a unique label before creating a Docker container. If the
create reply is lost, it can recover the container ID from exactly one
matching container on the recorded daemon. No match is not proof of closure:
the create request may still be in flight. The controller periodically retries
unconfirmed stops and binding closure on registered agents. Capacity stays
held when identity or removal cannot be proven, including when a record is
missing, unreadable, or refers to a different daemon. Podman does not use
durable OCI records.

Keep a cgroup root dedicated to one controller's journal. A start that finds an
existing native scope at the requested lease number is refused without changing
that scope. Lease numbers can recur in independent journals; neither a directory
name nor an empty scope proves ownership. Reconcile old scopes before reusing a
root rather than deleting them to unblock admission.

## Stateful workloads

Host-path mounts are raw attachments, not storage placement claims or exclusive
volume ownership. The operator must establish data locality and writer fencing;
service replica count does not provide database replication or failover safety.
Database transaction, recovery and query-admission semantics remain owned by the
database.

Agent execution/recovery proof and storage-writer fencing remain qualification
work before dependable stateful hosting. The local journal lock does not fence
agents, storage writers or controllers using different journals. Do not infer
storage HA from resource leases or generic service restart.

## Resource graph updates

Graph updates cannot change a resource's machine ancestor while a lease holds
its capacity. This includes reservations, preparation, and terminal leases with
unclosed bindings. Reclassifying an ancestor or introducing a nearer machine
must not redirect enforcement or let another machine acknowledge teardown.
Unsafe updates are rejected atomically. Capacity increases and unrelated facts
can still be refreshed; the machine restriction ends once the lease is terminal
and all its bindings are closed. This is not workload migration or validation of
physical device identity.
