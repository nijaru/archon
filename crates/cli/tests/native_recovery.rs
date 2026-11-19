#![cfg(target_os = "linux")]

use std::fs;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use archon_control::api::{self, ClientRequest, Greeting, ServerResponse};
use archon_node::transport::{SecureStream, establish_initiator};

const TOKEN: &str = "archon-test-native-recovery";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct TestDirs {
    cgroups: PathBuf,
    state: PathBuf,
}

impl Drop for TestDirs {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.cgroups) {
            for scope in entries
                .flatten()
                .filter(|e| e.file_name().as_encoded_bytes().starts_with(b"execution-"))
            {
                let path = scope.path();
                let _ = fs::write(path.join("cgroup.kill"), "1");
                for _ in 0..50 {
                    if fs::remove_dir(&path).is_ok() || !path.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        let _ = fs::remove_dir(&self.cgroups);
        let _ = fs::remove_dir_all(&self.state);
    }
}

fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn connect(addr: SocketAddr) -> io::Result<SecureStream> {
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_millis(200))?;
    tcp.set_read_timeout(Some(Duration::from_secs(2)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut stream = establish_initiator(tcp, Some(TOKEN))?;
    api::write_frame(&mut stream, &Greeting::Client)?;
    Ok(stream)
}

fn request(client: &mut SecureStream, request: ClientRequest) -> ServerResponse {
    api::write_frame(client, &request).unwrap();
    api::read_response(client).unwrap()
}

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_archon"));
    command.env("ARCHON_TOKEN", TOKEN).env("ARCHON_DEVICES", "");
    command
}

fn agent(addr: &str, dirs: &TestDirs) -> ChildGuard {
    ChildGuard(
        command()
            .args(["agent", "--register", addr, "--id", "native-recovery"])
            .arg("--cgroup-root")
            .arg(&dirs.cgroups)
            .env("ARCHON_EXECUTION_STATE_DIR", &dirs.state)
            .env("ARCHON_LOG_DIR", dirs.state.join("execution-logs"))
            .spawn()
            .unwrap(),
    )
}

fn controller(addr: &str, dirs: &TestDirs) -> ChildGuard {
    ChildGuard(
        command()
            .args([
                "serve",
                "--no-local",
                "--listen",
                addr,
                "--probe-secs",
                "1",
                "--log",
            ])
            .arg(dirs.state.join("journal"))
            .spawn()
            .unwrap(),
    )
}

fn submit(client: &mut SecureStream, owner: u64, cpus: u64, command: Vec<String>) -> u64 {
    match request(
        client,
        ClientRequest::Submit {
            owner,
            cpus,
            memory_mib: 0,
            lifetime_secs: 120,
            command,
            keep_alive: false,
            volumes: vec![],
            ports: vec![],
            grace_secs: 0,
            image: None,
            gpus: 0,
        },
    ) {
        ServerResponse::Submitted { lease, .. } => lease,
        other => panic!("submit failed: {other:?}"),
    }
}

fn status(client: &mut SecureStream) -> (usize, Vec<api::LeaseInfo>) {
    match request(client, ClientRequest::Status) {
        ServerResponse::Status { queue_len, leases } => (queue_len, leases),
        other => panic!("status failed: {other:?}"),
    }
}

fn populated_scope(root: &Path) -> Option<PathBuf> {
    fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|scope| {
            fs::read_to_string(scope.join("cgroup.procs"))
                .is_ok_and(|pids| pids.lines().count() >= 2)
        })
}

#[test]
fn queued_work_waits_for_native_recovery_after_agent_crash() {
    recovery_with_queued_work(false);
}

#[test]
fn queued_work_waits_for_native_recovery_after_controller_and_agent_crash() {
    recovery_with_queued_work(true);
}

fn recovery_with_queued_work(restart_controller: bool) {
    let name = format!(
        "archon-native-recovery-{}-{restart_controller}",
        std::process::id()
    );
    let cgroups = Path::new("/sys/fs/cgroup").join(&name);
    if let Err(err) = fs::create_dir(&cgroups) {
        assert!(
            std::env::var_os("ARCHON_REQUIRE_PRIVILEGED_CGROUP").is_none(),
            "cannot create {}: {err}",
            cgroups.display()
        );
        eprintln!("skipping: writable cgroup v2 required ({err})");
        return;
    }
    let dirs = TestDirs {
        cgroups,
        state: std::env::temp_dir().join(name),
    };
    fs::create_dir(&dirs.state).unwrap();
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let addr_arg = addr.to_string();
    let mut plane = controller(&addr_arg, &dirs);
    let mut client = until("controller", || connect(addr).ok());
    let mut original = agent(&addr_arg, &dirs);
    // The first workload consumes the published CPU inventory. Submitting
    // before registration completes is fine: maintenance will place it.
    let cpus = archon_node::discover::describe().cpus;
    submit(
        &mut client,
        1,
        cpus,
        vec!["sh".into(), "-c".into(), "sleep 120 & wait".into()],
    );
    let old_scope = until("first workload and its descendant", || {
        populated_scope(&dirs.cgroups)
    });
    let (_, leases) = status(&mut client);
    let first = leases.iter().find(|lease| lease.owner == 1).unwrap().id;

    let marker = dirs.state.join("successor");
    let competitor = [
        "sh",
        "-c",
        "test ! -e \"$1\" && touch \"$2\"; exec sleep 120",
        "sh",
        old_scope.to_str().unwrap(),
        marker.to_str().unwrap(),
    ];
    assert_eq!(
        submit(&mut client, 2, 1, competitor.map(str::to_string).to_vec()),
        0
    );
    original.0.kill().unwrap();
    original.0.wait().unwrap();
    assert!(
        fs::read_to_string(old_scope.join("cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 1")
    );
    assert!(matches!(
        request(&mut client, ClientRequest::Revoke { lease: first }),
        ServerResponse::Revoked
    ));
    if restart_controller {
        // The API has acknowledged both the queued submission and the revoke.
        // Replay must restore them, but must not treat the revoke as closure.
        plane.0.kill().unwrap();
        plane.0.wait().unwrap();
        drop(client);
        plane = controller(&addr_arg, &dirs);
        client = until("controller journal replay", || connect(addr).ok());
    }
    let (queued, leases) = status(&mut client);
    assert_eq!(queued, 1);
    assert!(leases.iter().any(|lease| lease.id == first));
    assert!(!leases.iter().any(|lease| lease.owner == 2));
    assert!(old_scope.is_dir());
    assert!(!marker.exists());

    let _replacement = agent(&addr_arg, &dirs);
    // The competitor checks for the old scope in its first command. This
    // proves the order even if both changes happen between status polls.
    until(
        "automatic successor start after exact scope removal",
        || marker.exists().then_some(()),
    );
    assert!(!old_scope.exists());
    let (queued, leases) = status(&mut client);
    assert_eq!(queued, 0);
    assert!(leases.iter().any(|lease| lease.owner == 2));
    drop(plane);
}
