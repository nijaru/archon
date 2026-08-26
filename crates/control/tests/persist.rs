//! Persistence tests: the command log reproduces cluster state exactly, and
//! recovery expires live work instead of re-executing it.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use archon_control::api::{ClientRequest, ServerResponse, read_response, write_frame};
use archon_control::server::ControlPlane;
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
    let mut log = archon_control::log::CommandLog::open(&log_path).expect("open log");
    service.set_command_sink(Some(Box::new(move |command: &Command| {
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
        grace_secs: 0,
        image: None,
        storage: vec![],
        ports: vec![],
        keep_alive: false,
    };
    service.submit(request, OwnerId::from_u64(1));
    service.cluster.set_now(1);
    service.admit_one().expect("admit");
    // Activation completes as agent acks arrive; settle before callers
    // inspect the log or lease state.
    service.drive(Duration::from_secs(5)).expect("settle");
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
fn restart_recovery_revokes_unprovable_work_through_reconciliation() {
    let path = temp_log("recover");
    let mut service = logged_service(&path);
    submit_sleep(&mut service, 1, 3_600);
    let lease = LeaseId::from_u64(1);
    assert!(service.is_running(lease));

    // A restart replays the log into a fresh controller whose new local
    // agent holds no processes: reconciliation must prove ownership against
    // the endpoint, fail it as unprovable, and fence through the protocol —
    // not blanket-revoke at boot.
    let commands = archon_control::log::CommandLog::read(&path).expect("read log");
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    assert_eq!(
        recovered.cluster.leases[&lease].state,
        LeaseState::Active,
        "replay alone restores the pre-crash state"
    );
    assert_eq!(recovered.live_lease_count(), 1);
    recovered
        .register_local(None)
        .expect("register local agent");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline
        && !matches!(
            recovered.cluster.leases.get(&lease).map(|l| l.state),
            Some(LeaseState::Revoked | LeaseState::Failed)
        )
    {
        let _ = recovered.drive(Duration::from_millis(20));
    }
    assert_ne!(
        recovered.cluster.leases[&lease].state,
        LeaseState::Active,
        "unprovable work must not survive a restart"
    );
    assert!(
        !recovered.cluster.occupies(lease),
        "claims free only after every binding fenced"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn snapshot_compaction_preserves_state_across_restarts() {
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    let dir = std::env::temp_dir().join(format!("archon-snap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let log = dir.join("cluster.jsonl");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let link = archon_control::server::AgentLink::Local { cgroup_root: None };
    let plane = Arc::new(Mutex::new(
        ControlPlane::boot(link, log.clone(), 0).expect("boot"),
    ));
    {
        let plane = plane.clone();
        std::thread::spawn(move || ControlPlane::serve(&plane, listener));
    }

    // Operate: two workloads live, one already finished (revoked).
    // Connections secure the link with the Noise handshake, then greet.
    let stream = TcpStream::connect(&addr).expect("connect");
    let mut stream = archon_node::transport::establish_initiator(stream, None).unwrap();
    write_frame(&mut stream, &archon_control::api::Greeting::Client).unwrap();
    for _id in [1u64, 2] {
        write_frame(
            &mut stream,
            &ClientRequest::Submit {
                owner: 1,
                cpus: 1,
                memory_mib: 0,
                lifetime_secs: 3_600,
                command: vec!["sleep".into(), "30".into()],
                keep_alive: false,
                volumes: vec![],
                ports: vec![],
                grace_secs: 0,
                image: None,
                gpus: 0,
            },
        )
        .unwrap();
        read_response(&mut stream).unwrap();
    }
    write_frame(&mut stream, &ClientRequest::Revoke { lease: 1 }).unwrap();
    read_response(&mut stream).unwrap();

    // Agent acks (activation of lease 2, teardown of lease 1) complete
    // asynchronously; let the plane drain before freezing state.
    assert!(
        wait_until(Duration::from_secs(5), || plane.lock().unwrap().settled()),
        "plane must settle before compaction"
    );

    // Compact: snapshot + truncated log.
    plane.lock().unwrap().compact().unwrap();
    eprintln!(
        "debug: dir={} exists={} entries={:?}",
        dir.display(),
        dir.exists(),
        std::fs::read_dir(&dir)
            .map(|entries| entries.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );
    let commands_after_compact = log_len(&log);
    assert_eq!(
        commands_after_compact, 0,
        "compaction must truncate the log"
    );
    assert!(log.with_added_extension("snapshot").exists());

    // Restart from the snapshot alone.
    drop(stream);
    let link2 = archon_control::server::AgentLink::Local { cgroup_root: None };
    let mut plane2 = ControlPlane::boot(link2, log.clone(), 0).expect("reboot");
    // The fresh local agent holds no processes: reconciliation proves the
    // live workload gone, revokes its lease, and fences its bindings.
    let settled = wait_until(Duration::from_secs(5), || {
        plane2.advance();
        let response = plane2.handle(ClientRequest::Status);
        let ServerResponse::Status { leases, .. } = response else {
            return false;
        };
        leases
            .iter()
            .find(|l| l.id == 2)
            .is_some_and(|survivor| survivor.state != "Active")
    });
    assert!(settled, "restart reconciliation must settle the live lease");
    let status = plane2.handle(ClientRequest::Status);
    let ServerResponse::Status { leases, .. } = status else {
        panic!("expected Status");
    };
    assert_eq!(leases.len(), 2, "both leases survive compaction");
    let revoked = leases.iter().find(|l| l.id == 1).expect("lease 1");
    assert_eq!(revoked.state, "Revoked");
    // Recovery policy: work is preserved only when provably current; the
    // fresh local agent cannot prove it, so the survivor is torn down.
    let survivor = leases.iter().find(|l| l.id == 2).expect("lease 2");
    assert_ne!(survivor.state, "Active");

    // The restored controller still operates: fresh work places normally.
    let response = plane2.handle(ClientRequest::Submit {
        owner: 1,
        cpus: 1,
        memory_mib: 0,
        lifetime_secs: 3_600,
        command: vec!["sleep".into(), "30".into()],
        keep_alive: false,
        volumes: vec![],
        ports: vec![],
        grace_secs: 0,
        image: None,
        gpus: 0,
    });
    assert!(
        matches!(response, ServerResponse::Submitted { lease: 3, .. }),
        "fresh work must place after restore, got {response:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn log_len(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|content| content.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

fn wait_until(deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    check()
}

#[test]
fn replay_recovers_id_high_water_marks() {
    let path = temp_log("highwater");
    {
        let mut service = logged_service(&path);
        submit_sleep(&mut service, 1, 60);
        service.admit_one().expect("first admission opens bindings");
        // Lease 1 ran; its log holds OpenLease(1) and OpenBinding(1).
    }

    // A fresh controller replays and admits new work: ids must continue,
    // not restart at 1.
    let commands = archon_control::log::CommandLog::read(&path).expect("read log");
    let mut recovered = NodeService::new();
    recovered.replay(commands).expect("replay");
    recovered.register_local(None).expect("register");
    submit_sleep(&mut recovered, 2, 60);
    recovered
        .admit_one()
        .expect("post-replay admission must not collide with reused binding ids");
}
