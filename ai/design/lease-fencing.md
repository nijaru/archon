# Fleet Lease and Binding Fencing

**Status:** accepted v0 protocol for `tk-l8xd`
**Updated:** 2026-08-17

This is the lease, Binding, and partition protocol. Types and the commit path
are in [`kernel-primitives.md`](kernel-primitives.md). Do not add `FenceToken`
or `NodeIncarnation`.

## Owners

| Guarantee | Owner |
|---|---|
| Lease intent, occupancy, Binding records, assigned fences | Cluster |
| Accepted fence and applied device state | Provider endpoint for that Node |
| Process identity and delivery to providers | Agent |
| Clock used for deadlines | `Cluster.now` |

The Cluster log is authoritative for what the Cluster has decided. The
endpoint is authoritative for what it has applied. Those copies can diverge.
Reconcile or fence; do not guess.

A Binding record is not enforcement. Claims stay occupied until every Binding
of that Lease is `Released` or `Fenced` in the Cluster log after an endpoint
ack, or the Node is quarantined.

## Occupancy

A Lease occupies its claims when:

- its state is `Preparing` or `Active`; or
- its state is `Released`, `Expired`, `Revoked`, or `Failed` and any Binding
  is not yet `Released` or `Fenced`.

Occupied claims are not available to another exclusive Lease. `expires_at`
passing does not free them. `ExpireLease` only starts the fence path.

`RenewLease` is allowed only on `Active`. It writes a later `expires_at` and
changes no claims, fences, Binding identities, or sessions.

## Versions

Every Cluster-to-Agent command carries `Cluster.epoch`. The Agent stores the
highest epoch it has accepted and rejects a lower one.

Every Agent-to-Cluster and Agent-to-Provider message carries `Agent.session`.
The Cluster rejects a session that is not the recorded session for that
machine Node. The Provider rejects a session that is not the session from the
current Agent handshake.

`Binding.fence` is a `u64` assigned by the Cluster, strictly increasing per
`(provider, node)`. It is not a type. The endpoint remembers the highest fence
it has accepted for that pair.

## Endpoint

```text
Endpoint
  provider
  node
  accepted_fence     highest fence accepted
  open               this generation may still mutate
  binding            Binding.id when open
  phase              Idle | Prepared | Active
  session            last accepted Agent.session
```

```text
Prepare(binding, fence, session)
  reject stale session
  reject fence < accepted_fence
  reject fence == accepted_fence unless open, same binding, phase Prepared
  if fence > accepted_fence:
      accepted_fence = fence
      open = true
      binding = id
      phase = Prepared

Activate(binding, fence, session)
  reject stale session
  require open, same binding, fence == accepted_fence
  require phase Prepared or Active
  phase = Active

Release(binding, fence, session)
Fence(binding, fence, session)
  reject stale session
  if fence < accepted_fence: already done
  accepted_fence = fence
  open = false
  binding = none
  phase = Idle
```

`Release` is cooperative teardown then the same generation close as `Fence`.
`Fence` stops the generation immediately. Both make `Prepare` at that fence
illegal. The next `OpenBinding` on the pair receives `accepted_fence + 1`.

The Cluster assigns that next fence when it opens the Binding. It never reuses
a fence for the same `(provider, node)`.

## Close versus fence

| Path | Cluster command | Endpoint op | Binding state |
|---|---|---|---|
| Owner gives the Lease back | `ReleaseLease` then `ReleaseBinding` | `Release` | `Released` |
| Cluster takes it, time runs out, prepare fails, or an effect is uncertain | `RevokeLease` / `ExpireLease` / `FailLease` then `FenceBinding` | `Fence` | `Fenced` |

If `Release` times out or the endpoint is unreachable, escalate to `Fence`.
`FailBinding` is a Cluster reason record. The endpoint action on a failed or
uncertain Binding is `Fence`.

Revoking a parent fences child Leases first, then the parent. A child Lease
may have its own Bindings. A child with no Bindings is only a claim record;
revoking it does not free parent claims.

## Messages

Cluster → Agent, all with `epoch` and the Binding's `fence` and
`expected_session`:

```text
PrepareBinding
ActivateBinding
ReleaseBinding
FenceBinding
ReconcileBindings     expected Preparing/Active Bindings for this machine
```

Agent → Cluster, all with `session` and `fence`:

```text
BindingPrepared       includes opaque provider_handle
BindingActive
BindingReleased
BindingFenced
BindingFailed         reason
AgentHello            machine, session, endpoint accepted_fence map
```

The Agent rejects `epoch` lower than its stored epoch. It rejects
`expected_session` that is not its current session. Provider calls carry the
same `session` and `fence`.

Commands are idempotent on
`(kind, binding, fence, session, epoch)`. A lower fence, old session, or old
epoch is a no-op rejection.

## Prepare and commit

```text
OpenLease(Preparing, prepare_deadline)
  for each enforced claim:
      OpenBinding(Preparing, fence = last + 1, session = current)
      send PrepareBinding
  wait until every Binding is Prepared, or any fails, or prepare_deadline
  if all Prepared:
      ActivateLease
      ActivateBinding on each
      send ActivateBinding
  else:
      FailLease
      FenceBinding on every opened Binding
```

A gang Request is one Lease. Every member Binding is prepared before
`ActivateLease`. There is no Active Lease with a subset of Bindings.

`prepare_deadline` is separate from `expires_at`. A late prepare fails the
Lease; it does not become Active and then immediately expire.

Activate is accepted only when the kernel commit checks still hold: matching
`graph_revision`, free exclusive claims, and every required Binding Prepared.

## Uncertain effects

If the Cluster does not have an ack, the prepare, release, or fence may have
been applied. That is not success and not a clean failure.

On timeout or lost ack:

1. send `Fence` for that Binding generation;
2. keep the claims occupied;
3. do not `ActivateLease`;
4. record `FailLease` or escalate `Release` to `Fence`;
5. free claims only after `BindingFenced` / `BindingReleased`, or quarantine
   the Node.

Never reuse exclusive claims because a deadline passed or a message was lost.

## Renewal and expiry

```text
RenewLease
  require Active
  require new_expires_at > lease.expires_at
  require new_expires_at > Cluster.now
  write expires_at
```

Renewal is a Cluster log command. It is not an endpoint fence change. Agents
may be told the new deadline; they cannot extend authority.

When `Cluster.now >= expires_at` and the Lease is `Active`, the Cluster
appends `ExpireLease` and fences every Binding. Occupancy remains until those
Bindings are `Fenced` or `Released`.

Without Cluster agreement, `RenewLease` is refused. The Lease then expires
when a recovered Cluster can apply `ExpireLease`. Endpoints keep the last
accepted Active generation until fenced.

## Session and reconcile

An Agent start or restart creates a new `session` and sends `AgentHello`.
The Cluster appends `SetAgentSession` and rejects the previous session.

Then:

1. Cluster sends `ReconcileBindings` for Preparing and Active Bindings on
   Nodes contained in that machine.
2. Agent reports each endpoint's `accepted_fence`, `open` binding, and phase.
3. Expected Binding present at the same fence: record the new session and
   continue (`Prepare` or `Activate` as needed).
4. Expected Binding missing or behind: send `Prepare` or `Fence` according to
   the Lease state.
5. Unexpected open endpoint generation: `Fence` it.
6. Quarantined Nodes stay quarantined until their fences are acked.

A delayed message from the old session is rejected even if its fence is
current.

## Partitions

**Cluster has no agreement.** Refuse `OpenLease`, `ActivateLease`,
`RenewLease`, and occupancy-granting `ApplyGraph`. Do not fail over to a
replica that cannot prove it owns the current epoch. Existing endpoint
generations keep running. Occupancy-changing expiry, revoke, and fence wait
for agreement unless this is the single v0 in-memory Cluster applying its
own clock.

**Cluster recovers.** `Cluster.epoch` advances. Agents accept the new epoch
and reject the old one. Reconcile as above. Commands tagged with the previous
epoch are rejected.

**Agent cannot reach Cluster.** The Agent keeps enforcing Active Bindings. It
does not invent prepares or renewals. On reconnect, same session replays
pending commands; a restart takes the session path.

**Provider or machine unreachable.** Any in-flight prepare fails the Lease and
fences what can be reached. Active claims on the unreachable Node are
quarantined: the Lease may become `Revoked`, `Expired`, or `Failed`, but those
claims are not available until a later Agent session acks the fence. The
recovery decision is explicit.

**Stale Cluster replica.** Agents reject its lower epoch. They do not apply
its prepares, activates, or releases.

## Commands

Add to the kernel log:

```text
RenewLease
QuarantineNode
UnquarantineNode      only after fence ack on the quarantined claims
```

`ExpireLease`, `ReleaseLease`, `RevokeLease`, `FailLease`, `FenceBinding`,
`ReleaseBinding`, and `SetAgentSession` stay as defined in the kernel
contract. Their occupancy effect is this protocol.

## Invariants

1. Exclusive occupying claims do not overlap.
2. Claims become free only after every Binding is `Released` or `Fenced`, or
   remain quarantined.
3. An endpoint rejects `fence < accepted_fence` and any mutate after close.
4. The Cluster rejects a stale `Agent.session`.
5. An Agent rejects a stale `Cluster.epoch`.
6. `ActivateLease` requires every required Binding `Prepared`.
7. A parent revoke fences children before the parent is freed.
8. `RenewLease` does not change fences, claims, or Binding ids.
9. Uncertain apply never grants a new exclusive Lease.
10. The same replay input yields the same Graph revision, Leases, Binding
    states, quarantine set, and digest.

## v0 proof

The kernel proof still holds. These cases are required with it:

1. Renew an Active Lease; fences and claims stay; `expires_at` moves.
2. Expire an Active Lease; claims stay occupied until fence ack.
3. Deliver `Prepare` with an old fence; endpoint rejects it.
4. Deliver any Binding message with an old session; Cluster and Provider
   reject it.
5. Miss a prepare ack; FailLease, fence, no Active subset.
6. Hit `prepare_deadline` with some Bindings Prepared; FailLease and fence
   them.
7. Restart an Agent under an Active Lease; new session reconciles; old
   session is rejected; claims stay occupied.
8. Advance `Cluster.epoch`; old-epoch commands are rejected.
9. Revoke a parent; child Bindings fence first.
10. Mark a machine unreachable; claims quarantine; a new exclusive Lease on
    them is refused until unquarantine.

## Deferred

Not in this protocol:

- shared occupancy and accelerator partitions;
- provider-specific reset beyond the opaque handle;
- consensus implementation;
- runtime attach/detach beyond Binding prepare/activate/release/fence;
- production network, storage, or device fencing internals.

The simulator and the production transition function implement this protocol
and the kernel contract.
