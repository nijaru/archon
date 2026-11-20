#![cfg(target_os = "linux")]

use std::fs;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use archon_control::api::{self, ClientRequest, Greeting, ServerResponse, Submission};
use archon_node::agent::LeaseAgent;
use archon_node::protocol::{self, AgentRequest, AgentResponse};
use archon_node::runtime::ProcessRuntime;
use archon_node::transport::{SecureStream, establish_initiator, establish_responder};

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
        let lost_create = fs::read_to_string(self.state.join("created-id"))
            .ok()
            .filter(|id| full_container_id(id.trim()));
        for id in recorded_containers(&self.state)
            .into_iter()
            .chain(lost_create.map(|id| id.trim().to_owned()))
        {
            let _ = Command::new("docker").args(["rm", "-f", &id]).output();
        }
        if std::thread::panicking()
            && let Ok(log) = fs::read_to_string(self.state.join("controller.stderr"))
        {
            eprintln!("controller stderr:\n{log}");
        }
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
    ChildGuard(agent_command(addr, dirs).spawn().unwrap())
}

fn agent_command(addr: &str, dirs: &TestDirs) -> Command {
    let mut agent = command();
    agent.args(["agent", "--register", addr, "--id", "recovery-test"]);
    if dirs.cgroups.is_dir() {
        agent.arg("--cgroup-root").arg(&dirs.cgroups);
    } else {
        agent.env_remove("ARCHON_CGROUP_ROOT");
    }
    agent
        .env("ARCHON_EXECUTION_STATE_DIR", &dirs.state)
        .env("ARCHON_LOG_DIR", dirs.state.join("execution-logs"));
    agent
}

fn controller(addr: &str, dirs: &TestDirs) -> ChildGuard {
    let stderr = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dirs.state.join("controller.stderr"))
        .unwrap();
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
            .stderr(stderr)
            .spawn()
            .unwrap(),
    )
}

fn submission(owner: u64, cpus: u64, command: Vec<String>) -> Submission {
    Submission {
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
    }
}

fn submit(client: &mut SecureStream, owner: u64, cpus: u64, command: Vec<String>) -> u64 {
    match request(
        client,
        ClientRequest::Submit(submission(owner, cpus, command)),
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

fn recorded_containers(state: &Path) -> Vec<String> {
    fs::read_dir(state)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().as_encoded_bytes().starts_with(b"oci-"))
        .filter_map(|entry| {
            let text = fs::read_to_string(entry.path()).ok()?;
            let (_, id) = text.split_once('\n')?;
            full_container_id(id).then(|| id.to_owned())
        })
        .collect()
}

fn full_container_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn assert_docker_removed(id: &str) {
    assert!(full_container_id(id));
    let output = Command::new("docker")
        .args([
            "container",
            "ls",
            "--all",
            "--quiet",
            "--no-trunc",
            "--filter",
            &format!("id={id}"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout.is_empty(),
        "container not proven removed: {output:?}"
    );
}

fn docker_running(id: &str) -> bool {
    Command::new("docker")
        .args(["inspect", "--format", "{{.State.Running}}", id])
        .output()
        .is_ok_and(|out| out.status.success() && out.stdout == b"true\n")
}

#[test]
fn queued_work_waits_for_exact_docker_agent_recovery() {
    // Opt in explicitly: the test runs two containers in the local daemon.
    let Some(image) = std::env::var_os("ARCHON_TEST_DOCKER_IMAGE") else {
        return;
    };
    let image = image.into_string().unwrap();
    let state = std::env::temp_dir().join(format!("archon-docker-recovery-{}", std::process::id()));
    fs::create_dir(&state).unwrap();
    let dirs = TestDirs {
        cgroups: state.join("unused-cgroups"),
        state,
    };
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let addr_arg = addr.to_string();
    let mut plane = controller(&addr_arg, &dirs);
    let mut client = until("controller", || connect(addr).ok());
    let docker_agent = |state: &Path| {
        ChildGuard(
            agent_command(&addr_arg, &dirs)
                .env("ARCHON_CONTAINER_ENGINE", "docker")
                .env("ARCHON_EXECUTION_STATE_DIR", state)
                .spawn()
                .unwrap(),
        )
    };
    let mut original = docker_agent(&dirs.state);
    let job = |owner, cpus| {
        let mut intent = submission(owner, cpus, vec!["sleep".into(), "120".into()]);
        intent.image = Some(image.clone());
        ClientRequest::Submit(intent)
    };
    assert!(matches!(
        request(&mut client, job(1, archon_node::discover::describe().cpus)),
        ServerResponse::Submitted { .. }
    ));
    let old = until("first Docker workload", || {
        recorded_containers(&dirs.state)
            .into_iter()
            .find(|id| docker_running(id))
    });
    let (_, leases) = status(&mut client);
    let first = leases.iter().find(|lease| lease.owner == 1).unwrap().id;
    assert!(matches!(
        request(&mut client, job(2, 1)),
        ServerResponse::Submitted { lease: 0, .. }
    ));
    original.0.kill().unwrap();
    original.0.wait().unwrap();
    assert!(docker_running(&old));
    assert!(matches!(
        request(&mut client, ClientRequest::Revoke { lease: first }),
        ServerResponse::Revoked
    ));
    plane.0.kill().unwrap();
    plane.0.wait().unwrap();
    drop(client);
    plane = controller(&addr_arg, &dirs);
    client = until("controller journal replay", || connect(addr).ok());
    let empty_state = dirs.state.join("empty-state");
    fs::create_dir(&empty_state).unwrap();
    let without_record = docker_agent(&empty_state);
    until("replacement to refuse stop without the record", || {
        fs::read_to_string(dirs.state.join("controller.stderr"))
            .ok()?
            .lines()
            .any(|line| {
                line.contains(&format!("execution stop for lease LeaseId({first})"))
                    && line.contains("is untracked")
            })
            .then_some(())
    });
    assert!(docker_running(&old));
    let (queued, leases) = status(&mut client);
    assert_eq!(queued, 1);
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].occupies_capacity, Some(true));
    drop(without_record);

    let _replacement = docker_agent(&dirs.state);
    until("queued Docker workload to start", || {
        recorded_containers(&dirs.state)
            .into_iter()
            .find(|id| *id != old && docker_running(id))
    });
    assert_docker_removed(&old);
    let (queued, leases) = status(&mut client);
    assert_eq!(queued, 0);
    assert!(
        leases
            .iter()
            .any(|lease| lease.id == first && lease.occupies_capacity == Some(false))
    );
    assert!(
        leases
            .iter()
            .any(|lease| lease.owner == 2 && lease.occupies_capacity == Some(true))
    );
    drop(plane);
}

#[test]
fn docker_agent_recovers_after_a_lost_create_reply() {
    use std::os::unix::fs::PermissionsExt;

    let Ok(image) = std::env::var("ARCHON_TEST_DOCKER_IMAGE") else {
        return;
    };
    let state =
        std::env::temp_dir().join(format!("archon-docker-lost-create-{}", std::process::id()));
    fs::create_dir(&state).unwrap();
    let dirs = TestDirs {
        cgroups: state.join("unused-cgroups"),
        state,
    };
    let wrapper = dirs.state.join("docker-fault");
    fs::write(
        &wrapper,
        r#"#!/bin/sh
set -eu
case "$1" in
    create)
        docker "$@" > "$ARCHON_EXECUTION_STATE_DIR/created-id"
        echo 'injected lost create reply' >&2
        exit 1;;
    start)
        touch "$ARCHON_EXECUTION_STATE_DIR/start-attempted"
        exit 1;;
    *) exec docker "$@";;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let addr_arg = addr.to_string();
    let _plane = controller(&addr_arg, &dirs);
    let mut client = until("controller", || connect(addr).ok());
    let _agent = ChildGuard(
        agent_command(&addr_arg, &dirs)
            .env("ARCHON_CONTAINER_ENGINE", wrapper)
            .spawn()
            .unwrap(),
    );
    let mut job = submission(1, 1, vec!["sleep".into(), "120".into()]);
    job.image = Some(image);
    assert!(matches!(
        request(&mut client, ClientRequest::Submit(job)),
        ServerResponse::Submitted { .. }
    ));
    until("failed create to release capacity", || {
        let (queue, leases) = status(&mut client);
        (queue == 0
            && leases.len() == 1
            && leases[0].state == "Failed"
            && leases[0].occupies_capacity == Some(false))
        .then_some(())
    });
    let created = fs::read_to_string(dirs.state.join("created-id")).unwrap();
    let id = created.trim();
    assert!(!dirs.state.join("start-attempted").exists());
    assert_eq!(recorded_containers(&dirs.state), [id]);
    assert_docker_removed(id);
}

#[test]
fn queued_work_waits_for_native_recovery_after_agent_crash() {
    recovery_with_queued_work(Crash::Agent);
}

#[test]
fn queued_work_waits_for_native_recovery_after_controller_and_agent_crash() {
    recovery_with_queued_work(Crash::BothAfterRevoke);
}

#[test]
fn unproven_active_work_is_reclaimed_without_operator_revoke() {
    recovery_with_queued_work(Crash::BothWhileActive);
}

#[test]
fn queued_work_waits_for_a_fresh_stop_reply_after_native_scope_removal() {
    recovery_with_queued_work(Crash::LostStopReply);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Crash {
    Agent,
    BothAfterRevoke,
    BothWhileActive,
    LostStopReply,
}

// Relay the real agent's protocol so teardown runs normally. Disconnect only
// after the agent has returned a successful stop, without forwarding its reply.
fn lose_stop_reply(listener: TcpListener, controller: SocketAddr) -> io::Result<AgentRequest> {
    listener.set_nonblocking(true)?;
    let agent = until("agent to connect to the relay", || {
        match listener.accept() {
            Ok((socket, _)) => Some(socket),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => None,
            Err(err) => panic!("agent accept: {err}"),
        }
    });
    let upstream = TcpStream::connect_timeout(&controller, Duration::from_secs(2))?;
    for socket in [&agent, &upstream] {
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(Duration::from_secs(20)))?;
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    }
    let mut agent = establish_responder(agent, Some(TOKEN))?;
    let mut upstream = establish_initiator(upstream, Some(TOKEN))?;
    api::write_frame(&mut upstream, &api::read_greeting(&mut agent)?)?;
    let mut start = None;
    loop {
        let request = protocol::read_request(&mut upstream)?;
        protocol::write_frame(&mut agent, &request)?;
        let reply = protocol::read_frame(&mut agent)?;
        match (&request, &reply) {
            (AgentRequest::StartExecution { .. }, AgentResponse::ExecutionStarted { .. }) => {
                start = Some(request.clone());
            }
            (
                AgentRequest::StopExecution {
                    lease, execution, ..
                },
                AgentResponse::ExecutionStopped { .. },
            ) => {
                let start = start.expect("stop must follow a successful start");
                assert!(matches!(&start, AgentRequest::StartExecution {
                    lease: started_lease, execution: Some(started_id), ..
                } if started_lease == lease && Some(started_id) == execution.as_ref()));
                return Ok(start);
            }
            _ => {}
        }
        protocol::write_response(&mut upstream, &reply)?;
    }
}

fn recovery_with_queued_work(crash: Crash) {
    let name = format!("archon-native-recovery-{}-{crash:?}", std::process::id());
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
    let (mut original, relay) = if crash == Crash::LostStopReply {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let original = agent(&listener.local_addr().unwrap().to_string(), &dirs);
        (
            original,
            Some(std::thread::spawn(move || lose_stop_reply(listener, addr))),
        )
    } else {
        (agent(&addr_arg, &dirs), None)
    };
    // The first workload consumes the published CPU inventory. Submitting
    // before registration completes is fine: maintenance will place it.
    let cpus = archon_node::discover::describe().cpus;
    let incumbent = submission(
        1,
        cpus,
        vec!["sh".into(), "-c".into(), "sleep 120 & wait".into()],
    );
    let first_request = if crash == Crash::LostStopReply {
        let keyed = ClientRequest::SubmitOnce {
            key: "incumbent".into(),
            submission: incumbent,
        };
        let mut abandoned = connect(addr).unwrap();
        api::write_frame(&mut abandoned, &keyed).unwrap();
        drop(abandoned); // Lose the submit reply independently of the agent link.
        Some(keyed)
    } else {
        assert!(matches!(
            request(&mut client, ClientRequest::Submit(incumbent)),
            ServerResponse::Submitted { .. }
        ));
        None
    };
    let old_scope = until("first workload and its descendant", || {
        populated_scope(&dirs.cgroups)
    });
    let (_, leases) = status(&mut client);
    let first = leases.iter().find(|lease| lease.owner == 1).unwrap().id;
    if let Some(keyed) = first_request {
        let receipt = request(&mut client, keyed);
        assert!(
            matches!(receipt, ServerResponse::Submitted { request: 1, lease } if lease == first),
            "{receipt:?}"
        );
        let (queued, leases) = status(&mut client);
        assert_eq!(queued, 0, "submit retry must not enqueue a second job");
        assert_eq!(leases.len(), 1);
    }

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
    if crash == Crash::BothWhileActive {
        // Freeze the agent before killing the controller: neither side may
        // observe the other's exit and perform its normal disconnect cleanup.
        let pid = original.0.id();
        assert!(
            Command::new("kill")
                .args(["-STOP", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        let proc_status = format!("/proc/{pid}/status");
        until("original agent to stop", || {
            fs::read_to_string(&proc_status)
                .ok()?
                .lines()
                .any(|line| line.starts_with("State:\tT"))
                .then_some(())
        });
        plane.0.kill().unwrap();
        plane.0.wait().unwrap();
    }
    if let Some(relay) = relay {
        assert!(matches!(
            request(&mut client, ClientRequest::Revoke { lease: first }),
            ServerResponse::Revoked
        ));
        let start = relay.join().unwrap().unwrap();
        assert!(!old_scope.exists(), "scope survived a successful stop");
        // Use a fresh runtime, so a rejection must come from durable state,
        // not from the original agent's cached stop receipt.
        let mut replay = LeaseAgent::new(
            ProcessRuntime::new()
                .with_cgroup_root(dirs.cgroups.to_str().unwrap().into())
                .with_execution_state(dirs.state.clone()),
        );
        let reply = replay.handle(start);
        assert!(
            matches!(&reply, AgentResponse::ExecutionFailed { reason, .. }
            if reason.contains("reserve single-use execution")),
            "{reply:?}"
        );
        assert!(!old_scope.exists());
    }
    original.0.kill().unwrap();
    original.0.wait().unwrap();
    if crash != Crash::LostStopReply {
        assert!(
            fs::read_to_string(old_scope.join("cgroup.events"))
                .unwrap()
                .lines()
                .any(|line| line == "populated 1")
        );
    }
    if matches!(crash, Crash::Agent | Crash::BothAfterRevoke) {
        assert!(matches!(
            request(&mut client, ClientRequest::Revoke { lease: first }),
            ServerResponse::Revoked
        ));
    }
    if matches!(crash, Crash::BothAfterRevoke | Crash::BothWhileActive) {
        // Replay must preserve the queue and the first lease's ownership,
        // regardless of whether revoke had already been committed.
        if crash == Crash::BothAfterRevoke {
            plane.0.kill().unwrap();
            plane.0.wait().unwrap();
        }
        drop(client);
        plane = controller(&addr_arg, &dirs);
        client = until("controller journal replay", || connect(addr).ok());
    }
    let (queued, leases) = status(&mut client);
    assert_eq!(queued, 1);
    let old = leases.iter().find(|lease| lease.id == first).unwrap();
    assert_eq!(old.occupies_capacity, Some(true));
    assert_eq!(
        old.state,
        if crash == Crash::BothWhileActive {
            "Active"
        } else {
            "Revoked"
        }
    );
    assert!(!leases.iter().any(|lease| lease.owner == 2));
    assert_eq!(old_scope.is_dir(), crash != Crash::LostStopReply);
    assert!(!marker.exists());

    if crash == Crash::BothWhileActive {
        assert_recovery_holds_without_record(&addr_arg, &dirs, &mut client, first, &old_scope);
        assert!(!marker.exists());
    }

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
    assert!(leases.iter().any(|lease| lease.owner == 2
        && lease.state == "Active"
        && lease.occupies_capacity == Some(true)));
    assert!(leases.iter().any(|lease| lease.id == first
        && lease.state == "Revoked"
        && lease.occupies_capacity == Some(false)));
    drop(plane);
}

fn assert_recovery_holds_without_record(
    addr: &str,
    dirs: &TestDirs,
    client: &mut SecureStream,
    lease: u64,
    scope: &Path,
) {
    let empty_state = dirs.state.join("empty-execution-state");
    fs::create_dir(&empty_state).unwrap();
    let _without_record = ChildGuard(
        agent_command(addr, dirs)
            .env("ARCHON_EXECUTION_STATE_DIR", empty_state)
            .spawn()
            .unwrap(),
    );
    // Wait for the controller to receive the refusal, not merely for the
    // replacement to connect. A refusal must leave occupancy intact.
    let rejection = format!("execution stop for lease LeaseId({lease})");
    until("controller to receive the stop refusal", || {
        fs::read_to_string(dirs.state.join("controller.stderr"))
            .ok()?
            .lines()
            .any(|line| line.contains(&rejection) && line.contains("is untracked"))
            .then_some(())
    });
    let (queued, leases) = status(client);
    assert_eq!(queued, 1);
    assert!(leases.iter().any(|entry| entry.id == lease
        && entry.state == "Revoked"
        && entry.occupies_capacity == Some(true)));
    assert!(!leases.iter().any(|entry| entry.owner == 2));
    let output = command().args(["-c", addr, "status"]).output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.lines().any(|line| {
            line.starts_with(&format!("lease {lease} "))
                && line.contains("state=Revoked capacity=held")
        }),
        "{stdout}"
    );
    assert!(
        fs::read_to_string(scope.join("cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 1")
    );
}
