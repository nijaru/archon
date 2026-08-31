//! Server integration tests: a real ControlPlane over loopback TCP, driven
//! by the same frames the CLI sends.

use std::collections::BTreeSet;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use std::io::{Read, Write};

use archon_control::api::{ClientRequest, ServerResponse, read_response, write_frame};
use archon_control::server::ControlPlane;
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_greeting,
    read_request, write_response,
};
use archon_node::service::AgentClient;
use archon_node::transport::{SecureStream, establish_initiator};

#[derive(Default)]
struct EnforcingTestExecutor {
    running: BTreeSet<u64>,
}

impl AgentClient for EnforcingTestExecutor {
    fn call(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        Ok(match request {
            AgentRequest::Capabilities => AgentResponse::Capabilities {
                capabilities: ExecutionCapabilities {
                    process: RuntimeCapabilities {
                        available: true,
                        cpu_limit: true,
                        memory_limit: true,
                        device_isolation: true,
                        physical_cpu_placement: false,
                        numa_memory_placement: false,
                    },
                    container: RuntimeCapabilities::default(),
                },
            },
            AgentRequest::Hello => AgentResponse::Welcome {
                instance_id: "server-test-agent".into(),
                name: "server-test-agent".into(),
                cpus: 4,
                memory_bytes: 8 * (1 << 30),
                host_nodes: Vec::new(),
                devices: Vec::new(),
            },
            AgentRequest::Prepare { binding, .. } => AgentResponse::Prepared {
                binding,
                handle: binding,
            },
            AgentRequest::Activate { binding, lease, .. } => {
                self.running.insert(lease);
                AgentResponse::Activated { binding }
            }
            AgentRequest::Release { binding, lease, .. } => {
                self.running.remove(&lease);
                AgentResponse::Released { binding }
            }
            AgentRequest::Fence { binding, lease, .. } => {
                self.running.remove(&lease);
                AgentResponse::Fenced { binding }
            }
            AgentRequest::Status { lease } => AgentResponse::Running {
                lease,
                running: self.running.contains(&lease),
                exit_code: None,
            },
            AgentRequest::Logs { lease } => AgentResponse::Logs {
                lease,
                output: String::new(),
            },
            AgentRequest::Register { .. } => AgentResponse::Failed {
                binding: 0,
                reason: "unexpected Register request".into(),
            },
        })
    }
}

fn spawn_enforcing_agent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind agent");
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let Ok(mut stream) = archon_node::transport::establish_responder(stream, None) else {
            return;
        };
        let _ = read_greeting(&mut stream);
        let mut executor = EnforcingTestExecutor::default();
        while let Ok(request) = read_request(&mut stream) {
            let response = executor
                .call(request)
                .unwrap_or_else(|reason| AgentResponse::Failed { binding: 0, reason });
            if write_response(&mut stream, &response).is_err() {
                break;
            }
        }
    });
    addr
}

fn temp_log(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("archon-server-{name}-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn spawn_server(name: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let log = temp_log(name);
    let agent_addr = spawn_enforcing_agent();
    std::thread::spawn(move || {
        let link = archon_control::server::AgentLink::Remote {
            addr: agent_addr,
            token: None,
        };
        let plane = std::sync::Arc::new(std::sync::Mutex::new(
            ControlPlane::boot(link, log, 0).expect("boot"),
        ));
        ControlPlane::serve(&plane, listener);
    });
    addr
}

fn roundtrip<S: Read + Write>(stream: &mut S, request: ClientRequest) -> ServerResponse {
    write_frame(stream, &request).expect("send");
    read_response(stream).expect("receive")
}

/// Connect, secure the link, and greet as a client.
fn open_client(addr: &str, token: Option<&str>) -> SecureStream {
    let stream = TcpStream::connect(addr).expect("connect");
    let mut stream = establish_initiator(stream, token).expect("handshake");
    write_frame(&mut stream, &archon_control::api::Greeting::Client).expect("send greeting");
    stream
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
fn submit_status_revoke_over_the_wire() {
    let addr = spawn_server("lifecycle");
    let mut stream = open_client(&addr, None);

    let response = roundtrip(
        &mut stream,
        ClientRequest::Submit {
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
    );
    let ServerResponse::Submitted { lease, .. } = response else {
        panic!("expected Submitted, got {response:?}");
    };
    assert!(lease > 0, "submit must admit on an empty cluster");

    let saw_running = wait_until(Duration::from_secs(5), || {
        matches!(
            roundtrip(&mut stream, ClientRequest::Status),
            ServerResponse::Status { .. }
        )
    });
    assert!(saw_running);

    let response = roundtrip(&mut stream, ClientRequest::Revoke { lease });
    assert!(matches!(response, ServerResponse::Revoked));

    let response = roundtrip(&mut stream, ClientRequest::Status);
    let ServerResponse::Status { leases, .. } = response else {
        panic!("expected Status, got {response:?}");
    };
    let lease_info = leases.iter().find(|info| info.id == lease).expect("lease");
    assert_eq!(lease_info.state, "Revoked");
}

#[test]
fn wrong_token_is_rejected_before_any_work() {
    use archon_control::api::Greeting;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let log = temp_log("auth");
    let token = "right-token".to_string();
    let server_token = token.clone();
    std::thread::spawn(move || {
        let link = archon_control::server::AgentLink::Local { cgroup_root: None };
        let mut plane = ControlPlane::boot(link, log, 0).expect("boot");
        plane.require_token(server_token);
        ControlPlane::serve(&std::sync::Arc::new(std::sync::Mutex::new(plane)), listener);
    });

    // Wrong token: the responder rejects it while decrypting message 3,
    // so the connection closes before any request is served.
    if let Ok(mut stream) =
        TcpStream::connect(&addr).and_then(|stream| establish_initiator(stream, Some("wrong")))
    {
        write_frame(&mut stream, &Greeting::Client).expect("send greeting");
        assert!(
            read_response(&mut stream).is_err(),
            "wrong token must close the connection"
        );
    }

    // Right token: requests are served.
    let mut stream = open_client(&addr, Some(&token));
    write_frame(&mut stream, &ClientRequest::Status).expect("send status");
    let response = read_response(&mut stream).expect("status response");
    assert!(matches!(response, ServerResponse::Status { .. }));
}

#[test]
fn empty_command_is_rejected() {
    let addr = spawn_server("reject");
    let mut stream = open_client(&addr, None);
    let response = roundtrip(
        &mut stream,
        ClientRequest::Submit {
            owner: 1,
            cpus: 1,
            memory_mib: 0,
            lifetime_secs: 60,
            command: vec![],
            keep_alive: false,
            volumes: vec![],
            ports: vec![],
            grace_secs: 0,
            image: None,
            gpus: 0,
        },
    );
    assert!(matches!(response, ServerResponse::Error { .. }));
}

/// A silent agent holds its worker's round trip, not the plane: other
/// clients keep getting fast responses while one machine never answers.
#[test]
fn a_silent_agent_does_not_block_other_clients() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let log = temp_log("silent-agent");
    std::thread::spawn(move || {
        let link = archon_control::server::AgentLink::None;
        let plane = std::sync::Arc::new(std::sync::Mutex::new(
            ControlPlane::boot(link, log, 0).expect("boot"),
        ));
        ControlPlane::serve(&plane, listener);
    });

    // The agent announces itself and then goes silent before proving runtime
    // capabilities. Its connection handler may wait for that proof, but it
    // must never hold the shared ControlPlane lock while waiting.
    let agent = TcpStream::connect(&addr).expect("agent connect");
    let mut agent =
        archon_node::transport::establish_initiator(agent, None).expect("agent handshake");
    write_frame(
        &mut agent,
        &archon_control::api::Greeting::Agent {
            instance_id: "inst-silent".into(),
            name: "silent".into(),
            cpus: 4,
            memory_bytes: 8 << 30,
            host_nodes: Vec::new(),
            devices: vec![],
        },
    )
    .expect("agent greeting");

    // Give registration a moment, then submit work that lands on it.
    std::thread::sleep(Duration::from_millis(200));
    let mut client = open_client(&addr, None);
    let response = roundtrip(
        &mut client,
        ClientRequest::Submit {
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
    );
    let ServerResponse::Submitted { lease, .. } = response else {
        panic!("expected Submitted, got {response:?}");
    };
    assert_eq!(
        lease, 0,
        "an agent that cannot prove capabilities must not receive Lease authority"
    );

    // While the agent's capability proof stays silent, another client must
    // still be served promptly.
    let mut watcher = open_client(&addr, None);
    for _ in 0..10 {
        let start = Instant::now();
        roundtrip(&mut watcher, ClientRequest::Status);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a silent agent must not stall other clients"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // Administrative requests stay responsive too. There is deliberately no
    // lease to revoke because the silent agent never became execution-capable.
    let start = Instant::now();
    let response = roundtrip(&mut client, ClientRequest::Revoke { lease: 1 });
    assert!(matches!(response, ServerResponse::Error { .. }));
    assert!(start.elapsed() < Duration::from_secs(2));
}
