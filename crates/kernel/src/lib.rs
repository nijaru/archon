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
    Admission, BackfillCtx, ClassUsage, admit, admit_backfill, admit_fair, owner_usage,
    refuse_reason,
};
pub use cluster::{BindingDigest, Cluster, Digest, LeaseDigest};
pub use command::{Command, Effect};
pub use endpoint::{EndpointError, EndpointOp};
pub use error::Error;
pub use graph::Graph;
pub use ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId, RequestId};
pub use occupancy::{Occupancy, occupancy_from_leases};
pub use preempt::preempt_victims;
pub use select::select;
pub use types::{
    Allocation, Attrs, Binding, BindingState, CapacityDimension, Claim, Edge, EdgeKind, Endpoint,
    EndpointPhase, Filter, IdentifierError, Lease, LeaseState, Need, Node, NodeState, PortPublish,
    Preference, Quantity, Queued, Request, RequestClass, ResourceClass, StorageMount,
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

    /// Budget-aware admission with per-owner, per-kind fair-share ceilings.
    /// Usage is always computed from this Cluster's own lease table.
    pub fn admit_fair(&self, queue: &[Queued], fair_share: &ClassUsage) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit_fair(
            &self.graph,
            &self.occupancy(),
            &blocked,
            fair_share,
            queue,
            &self.leases,
        )
    }

    /// EASY-style backfill over the priority queue with per-owner, per-kind
    /// fair-share ceilings.
    pub fn admit_backfill(&self, queue: &[Queued], fair_share: &ClassUsage) -> Option<Admission> {
        let blocked = self.placement_blocked();
        admit_backfill(
            &self.graph,
            &self.occupancy(),
            &blocked,
            fair_share,
            queue,
            &crate::admit::BackfillCtx {
                now: self.now,
                leases: &self.leases,
                open_bindings: &self.open_binding_leases(),
            },
        )
    }
}
