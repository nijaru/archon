use std::collections::BTreeSet;

use crate::Cluster;
use crate::ids::LeaseId;
use crate::select::select;
use crate::types::Request;

pub fn preempt_victims(cluster: &Cluster, request: &Request) -> Option<Vec<LeaseId>> {
    let blocked = cluster.placement_blocked();
    if select(&cluster.graph, &cluster.occupancy(), request, &blocked).is_ok() {
        return Some(Vec::new());
    }
    let mut candidates: Vec<LeaseId> = cluster
        .leases
        .values()
        .filter(|lease| {
            lease.parent.is_none()
                && lease.priority < request.priority
                && cluster.occupies(lease.id)
        })
        .map(|lease| lease.id)
        .collect();
    candidates.sort_by(|left, right| {
        let left_lease = &cluster.leases[left];
        let right_lease = &cluster.leases[right];
        left_lease
            .priority
            .cmp(&right_lease.priority)
            .then(right.cmp(left))
    });
    let mut victims = Vec::new();
    for candidate in candidates {
        victims.push(candidate);
        let mut except: BTreeSet<LeaseId> = victims.iter().copied().collect();
        for victim in &victims {
            except.extend(cluster.descendants_postorder(*victim));
        }
        let occupancy = cluster.occupancy_except(&except);
        let blocked = cluster.placement_blocked();
        if select(&cluster.graph, &occupancy, request, &blocked).is_ok() {
            return Some(victims);
        }
    }
    None
}
