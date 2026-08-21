//! End-to-end tests against real processes: a lease runs a real command and
//! lease termination kills it. These exercise the same seam production
//! enforcement will use.

use archon_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_node::service::NodeService;

fn boot() -> NodeService {
    let mut service = NodeService::new();
    let (local, nodes, edges) = archon_node::discover::discover();
    service.boot(nodes, edges).expect("boot");
    let _ = local;
    service
}

fn request(id: u64, command: Vec<String>) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: command.clone(),
        lifetime: 3_600,
        priority: 1,
    }
}

#[test]
fn lease_runs_a_real_process() {
    let mut service = boot();
    service.submit(
        request(1, vec!["sleep".into(), "10".into()]),
        OwnerId::from_u64(1),
        vec!["sleep".into(), "10".into()],
    );
    service.tick().unwrap();
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let lease = LeaseId::from_u64(1);
    assert!(service.is_running(lease), "sleep must run under the lease");
    service.revoke(lease).unwrap();
    assert!(!service.is_running(lease), "revoke must kill the process");
}

#[test]
fn lease_expiry_kills_the_process() {
    let mut service = boot();
    service.submit(
        request(1, vec!["sleep".into(), "10".into()]),
        OwnerId::from_u64(1),
        vec!["sleep".into(), "10".into()],
    );
    // Shorten the lease so expiry is due immediately: manual clock, since
    // tick() would jump to real wall-clock time.
    service.cluster.set_now(1);
    service.admit_one().unwrap();
    let lease = LeaseId::from_u64(1);
    assert!(service.is_running(lease));
    service.cluster.set_now(3_601);
    let expired = service.expire_due().unwrap();
    assert_eq!(expired, vec![lease]);
    assert!(!service.is_running(lease), "expiry must kill the process");
}
