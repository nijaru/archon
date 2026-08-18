//! Fleet kernel types and the Cluster transition function.
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
mod select;
mod types;

pub use admit::{Admission, Queued, admit, refuse_reason};
pub use cluster::{BindingDigest, Cluster, Digest, LeaseDigest};
pub use command::{Command, Effect};
pub use endpoint::{EndpointError, EndpointOp};
pub use error::Error;
pub use graph::Graph;
pub use ids::{BindingId, LeaseId, NodeId, OwnerId, ProviderId, RequestId};
pub use occupancy::{Occupancy, occupancy_from_leases};
pub use select::select;
pub use types::{
    Allocation, Attrs, Binding, BindingState, Claim, Dimension, Edge, EdgeKind, Endpoint,
    EndpointPhase, Filter, Lease, LeaseState, Need, Node, NodeKind, Preference, Quantity, Request,
    RequestClass, TopologyConstraint, qty,
};

impl Cluster {
    pub fn occupancy(&self) -> Occupancy {
        occupancy_from_leases(
            self.leases.values(),
            &self
                .bindings
                .values()
                .filter(|binding| !binding.state.is_closed())
                .map(|binding| binding.lease)
                .collect(),
            &std::collections::BTreeSet::new(),
        )
    }

    pub fn allocate(&self, request: &Request) -> Result<Allocation, Error> {
        select(&self.graph, &self.occupancy(), request)
    }

    pub fn admit(&self, queue: &[Queued]) -> Option<Admission> {
        admit(&self.graph, &self.occupancy(), queue)
    }
}
