//! Archon kernel types and the Cluster transition function.
//!
//! Production and the simulator share this crate. Delivery, clocks, and faults
//! stay outside `Cluster::apply`.

mod admit;
mod cluster;
mod command;
mod endpoint;
mod error;
mod graph;
mod ids;
mod occupancy;
mod preempt;
mod select;
mod types;

pub use admit::{
    Admission, AdmissionPolicy, BackfillCtx, ClassUsage, RequestExclusions, admit, admit_backfill,
    admit_backfill_with_exclusions, admit_backfill_with_policy, admit_with_ceiling,
    admit_with_policy, owner_usage, refuse_reason,
};
pub use cluster::{BindingDigest, Cluster, Digest, LeaseDigest};
pub use command::{Command, Effect};
pub use endpoint::{EndpointError, EndpointOp};
pub use error::Error;
pub use graph::Graph;
pub use ids::{BindingId, FactWriterId, LeaseId, NodeId, OwnerId, ProviderId, RequestId};
pub use occupancy::{Occupancy, occupancy_from_leases};
pub use preempt::preempt_victims;
pub use select::select;
pub use types::{
    Allocation, Attrs, Binding, BindingScope, BindingState, CapacityDimension, Claim, ClaimBinding,
    ClaimBindingUpdate, Edge, EdgeKind, Endpoint, EndpointPhase, FactEdge, FactWriterAssignment,
    Filter, IdentifierError, Lease, LeaseState, Need, Node, NodeState, PortPublish, Preference,
    ProviderFactBatch, Quantity, Queued, Request, RequestClass, ResourceClass, StorageMount,
    TopologyConstraint, TopologyRelation, qty, quantity_get,
};

impl Cluster {
    pub fn occupancy(&self) -> Occupancy {
        self.occupancy_except(&std::collections::BTreeSet::new())
    }

    pub fn allocate(&self, request: &Request) -> Result<Allocation, Error> {
        let blocked = self.placement_blocked();
        select(&self.graph, &self.occupancy(), request, &blocked)
    }

    pub fn admit(&self, queue: &[Queued]) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit(&self.graph, &self.occupancy(), queue, &blocked)
    }

    /// Admission with an explicit per-owner, per-kind resource ceiling.
    /// Usage is always computed from this Cluster's own lease table.
    pub fn admit_with_ceiling(
        &self,
        queue: &[Queued],
        owner_ceiling: &ClassUsage,
    ) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit_with_ceiling(
            &self.graph,
            &self.occupancy(),
            &blocked,
            owner_ceiling,
            queue,
            &self.leases,
        )
    }

    /// Admission under an explicit scheduler policy. Policy can enable
    /// weighted dominant-share ordering without changing Lease authority.
    pub fn admit_with_policy(
        &self,
        queue: &[Queued],
        policy: &AdmissionPolicy,
    ) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit_with_policy(
            &self.graph,
            &self.occupancy(),
            &blocked,
            policy,
            queue,
            &self.leases,
        )
    }

    /// EASY-style backfill over the priority queue with per-owner, per-kind
    /// resource ceilings. Ceiling enforcement does not alter queue ordering.
    pub fn admit_backfill(
        &self,
        queue: &[Queued],
        owner_ceiling: &ClassUsage,
    ) -> Option<Admission> {
        self.admit_backfill_with_exclusions(queue, owner_ceiling, &RequestExclusions::new())
    }

    /// EASY-style backfill with request-specific hard node exclusions. The
    /// execution/provider layer can use this without mutating resource facts
    /// or schedulability state in the Graph.
    pub fn admit_backfill_with_exclusions(
        &self,
        queue: &[Queued],
        owner_ceiling: &ClassUsage,
        exclusions: &RequestExclusions,
    ) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit_backfill_with_exclusions(
            &self.graph,
            &self.occupancy(),
            &blocked,
            exclusions,
            owner_ceiling,
            queue,
            &crate::admit::BackfillCtx {
                now: self.now,
                leases: &self.leases,
                open_bindings: &self.open_binding_leases(),
            },
        )
    }

    /// EASY-style backfill under an explicit scheduler policy.
    pub fn admit_backfill_with_policy(
        &self,
        queue: &[Queued],
        policy: &AdmissionPolicy,
        exclusions: &RequestExclusions,
    ) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit_backfill_with_policy(
            &self.graph,
            &self.occupancy(),
            &blocked,
            exclusions,
            policy,
            queue,
            &crate::admit::BackfillCtx {
                now: self.now,
                leases: &self.leases,
                open_bindings: &self.open_binding_leases(),
            },
        )
    }
}
