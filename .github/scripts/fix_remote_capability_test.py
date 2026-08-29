from pathlib import Path

path = Path("crates/node/tests/remote.rs")
text = path.read_text()
text = text.replace("use std::time::{Duration, Instant};\n\n", "", 1)
text = text.replace(
    "    CapacityDimension, LeaseId, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,\n",
    "    CapacityDimension, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,\n",
    1,
)
old_wait = '''fn wait_until(deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    check()
}

'''
if old_wait not in text:
    raise SystemExit("wait_until helper not found")
text = text.replace(old_wait, "", 1)
old_test = '''#[test]
fn controller_runs_and_kills_a_process_on_a_remote_agent() {
    let addr = spawn_agent("remote-a");
    let mut service = NodeService::new();
    service
        .register_remote(&addr)
        .expect("register remote agent");
    service.submit(
        request(1, vec!["sleep".into(), "30".into()]),
        OwnerId::from_u64(1),
    );
    service.tick().unwrap();
    assert_eq!(service.admit_one().unwrap(), Some(RequestId::from_u64(1)));
    let lease = LeaseId::from_u64(1);
    // The process lives in the agent thread's process tree — this same OS,
    // proving the round trip executed something real.
    assert!(
        wait_until(Duration::from_secs(5), || service.is_running(lease)),
        "sleep must run on the remote agent"
    );
    service.revoke(lease).unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || !service.is_running(lease)),
        "revoke must kill the remote process"
    );
}
'''
new_test = '''#[test]
fn controller_excludes_unenforced_cpu_work_on_a_remote_agent() {
    let addr = spawn_agent("remote-a");
    let mut service = NodeService::new();
    service
        .register_remote(&addr)
        .expect("register remote agent");
    service.submit(
        request(1, vec!["sleep".into(), "30".into()]),
        OwnerId::from_u64(1),
    );
    service.tick().unwrap();
    assert_eq!(
        service.admit_one().unwrap(),
        None,
        "a remote lifecycle-only process runtime must not receive a CPU Claim"
    );
    assert!(
        service.cluster.leases.is_empty(),
        "remote capability refusal must happen before Lease authority"
    );
}
'''
if old_test not in text:
    raise SystemExit("remote execution test not found")
path.write_text(text.replace(old_test, new_test, 1))
