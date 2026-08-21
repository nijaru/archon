//! Persistence tests: the command log reproduces cluster state exactly, and
//! recovery expires live work instead of re-executing it.

use std::path::{Path, PathBuf};

use archon_kernel::{
    Command, Dimension, LeaseId, LeaseState, Need, NodeKind, OwnerId, Request, RequestClass,
    RequestId, qty,
};
use archon_node::service::NodeService;

fn temp_log(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("archon-test-{name}-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn logged_service(path: &Path) -> NodeService {
    let path = path.to_path_buf();
    let mut service = NodeService::new();
    let log_path = path.clone();
    service.set_command_sink(Some(Box::new(move |command: &Command| {
        let mut log = archon_control::log::CommandLog::open(&log_path).expect("open log");
        log.append(command).expect("append log");
    })));
    service
        .register_local(None)
        .expect("register local machine");
    service
}

fn submit_sleep(service: &mut NodeService, id: u64, lifetime: u64) {
    let request = Request {
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
        command: vec!["sleep".into(), "30".into()],
        lifetime,
        priority: 1,
        machine_local: true,
        image: None,
        keep_alive: false,
    };
    service.submit(
        request,
        OwnerId::from_u64(1),
        vec!["sleep".into(), "30".into()],
    );
    service.cluster.set_now(1);
    service.admit_one().expect("admit");
}

#[test]
fn replay_reproduces_cluster_state_exactly() {
    let path = temp_log("replay");
    let mut service = logged_service(&path);
    submit_sleep(&mut service, 1, 3_600);
    let before = format!("{:?}", service.cluster.leases);

    let commands = archon_control::log::CommandLog::read(&path).expect("read log");
    assert!(commands.len() > 5, "log must capture the full lifecycle");

    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    let after = format!("{:?}", recovered.cluster.leases);
    assert_eq!(before, after, "replay must reproduce state exactly");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn recovery_revokes_live_leases_without_reexecution() {
    let path = temp_log("recover");
    let mut service = logged_service(&path);
    submit_sleep(&mut service, 1, 3_600);
    let lease = LeaseId::from_u64(1);
    assert!(service.is_running(lease));

    // A restart replays the log into a fresh agent that holds no processes.
    let commands = archon_control::log::CommandLog::read(&path).expect("read log");
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    assert_eq!(
        recovered.cluster.leases[&lease].state,
        LeaseState::Active,
        "replay alone restores the pre-crash state"
    );
    let revoked = recovered.revoke_live_leases().expect("revoke");
    assert_eq!(revoked, 1);
    assert_eq!(recovered.cluster.leases[&lease].state, LeaseState::Revoked);
    let _ = std::fs::remove_file(&path);
}
