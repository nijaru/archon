use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use archon_control::api::{ClientRequest, ServerResponse};
use archon_control::log::CommandLog;
use archon_control::server::{AgentLink, ControlPlane};

struct JournalDir(PathBuf);

impl JournalDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("archon-{name}-{}", std::process::id()));
        std::fs::create_dir(&path).expect("create isolated journal directory");
        Self(path)
    }

    fn log(&self) -> PathBuf {
        self.0.join("controller.jsonl")
    }
}

impl Drop for JournalDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_owned(path: &Path) {
    let error = ControlPlane::boot(AgentLink::None, path.to_owned(), 0)
        .err()
        .expect("a second controller must not boot");
    assert_eq!(
        error.downcast_ref::<std::io::Error>().map(|e| e.kind()),
        Some(std::io::ErrorKind::WouldBlock),
        "boot must fail on ownership before reading durable state: {error}"
    );
}

#[test]
fn competing_boot_cannot_read_snapshot_or_repair_an_owned_journal() {
    let dir = JournalDir::new("journal-exclusion");
    let path = dir.log();
    let tail = b"{\"sequence\":";
    std::fs::write(&path, tail).unwrap();
    let owner = CommandLog::open(&path).unwrap();
    assert_owned(&path);
    assert_eq!(std::fs::read(&path).unwrap(), tail);

    // Even a bad snapshot must not be consulted before obtaining ownership.
    let snapshot = path.with_added_extension("snapshot");
    std::fs::write(&snapshot, b"not a snapshot").unwrap();
    assert_owned(&path);
    let alias = dir.0.join("alias.jsonl");
    std::fs::hard_link(&path, &alias).unwrap();
    assert_owned(&alias);
    assert_eq!(std::fs::read(&path).unwrap(), tail);
    assert_eq!(std::fs::read(&snapshot).unwrap(), b"not a snapshot");

    drop(owner);
    std::fs::remove_file(snapshot).unwrap();
    let plane = ControlPlane::boot(AgentLink::None, path.clone(), 0)
        .expect("dropping the owner must release the journal");
    assert!(
        std::fs::read(&path).unwrap().is_empty(),
        "new owner repairs the tail"
    );
    assert_owned(&path);
    drop(plane);
    CommandLog::open(&path).expect("dropping the controller must release both handles");
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn submit(plane: &mut ControlPlane, expected: u64) {
    assert!(matches!(
        plane.handle(ClientRequest::Submit {
            owner: 1,
            cpus: 1,
            memory_mib: 0,
            lifetime_secs: 3600,
            command: vec!["sleep".into(), "30".into()],
            keep_alive: false,
            volumes: vec![],
            ports: vec![],
            grace_secs: 0,
            image: None,
            gpus: 0,
        }),
        ServerResponse::Submitted { request, lease: 0 } if request == expected
    ));
}

#[test]
fn journal_lock_survives_compaction_and_process_exit() {
    const CHILD_PATH: &str = "ARCHON_JOURNAL_LOCK_CHILD";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let mut plane = ControlPlane::boot(AgentLink::None, path.into(), 0).unwrap();
        submit(&mut plane, 1);
        plane.compact().unwrap();
        submit(&mut plane, 2);
        println!("journal-lock-ready");
        std::io::stdout().flush().unwrap();
        // Parent kills the process without running Rust destructors.
        std::io::stdin().read_exact(&mut [0]).unwrap();
        panic!("parent must terminate the child, not request a clean shutdown");
    }

    let dir = JournalDir::new("journal-process");
    let path = dir.log();
    let mut child = KillOnDrop(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "journal_lock_survives_compaction_and_process_exit",
                "--nocapture",
            ])
            .env(CHILD_PATH, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (ready, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let found = std::io::BufReader::new(stdout)
            .lines()
            .any(|line| line.is_ok_and(|line| line.contains("journal-lock-ready")));
        let _ = ready.send(found);
    });
    assert_eq!(received.recv_timeout(Duration::from_secs(10)), Ok(true));
    reader.join().unwrap();

    let before = std::fs::read(&path).unwrap();
    let snapshot = path.with_added_extension("snapshot");
    let snapshot_before = std::fs::read(&snapshot).unwrap();
    assert_owned(&path);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&snapshot).unwrap(), snapshot_before);

    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    let mut recovered = ControlPlane::boot(AgentLink::None, path, 0)
        .expect("OS must release the killed owner's lock without stale-lock cleanup");
    assert!(matches!(
        recovered.handle(ClientRequest::Status),
        ServerResponse::Status { queue_len: 2, .. }
    ));
    submit(&mut recovered, 3);
}
