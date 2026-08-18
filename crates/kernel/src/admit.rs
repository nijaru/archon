use crate::error::Error;
use crate::graph::Graph;
use crate::occupancy::Occupancy;
use crate::select::select;
use crate::types::{Allocation, Request};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub request: Request,
    pub submitted_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub request: Request,
    pub allocation: Allocation,
}

pub fn admit(
    graph: &Graph,
    occupancy: &Occupancy,
    queue: &[Queued],
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
) -> Option<Admission> {
    let mut order: Vec<usize> = (0..queue.len()).collect();
    order.sort_by(|&left, &right| {
        queue[right]
            .request
            .priority
            .cmp(&queue[left].request.priority)
            .then(queue[left].submitted_at.cmp(&queue[right].submitted_at))
            .then(queue[left].request.id.cmp(&queue[right].request.id))
    });
    for index in order {
        if let Ok(allocation) = select(graph, occupancy, &queue[index].request, quarantine) {
            return Some(Admission {
                request: queue[index].request.clone(),
                allocation,
            });
        }
    }
    None
}

pub fn refuse_reason(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
) -> Option<Error> {
    select(graph, occupancy, request, quarantine).err()
}
