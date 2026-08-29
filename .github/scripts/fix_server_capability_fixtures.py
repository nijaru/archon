from pathlib import Path

path = Path("crates/control/tests/server.rs")
text = path.read_text()

text = text.replace(
    "use std::net::{TcpListener, TcpStream};\n",
    "use std::collections::BTreeSet;\nuse std::net::{TcpListener, TcpStream};\n",
    1,
)
text = text.replace(
    "use archon_node::transport::{SecureStream, establish_initiator};\n",
    "use archon_node::protocol::{\n    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_greeting,\n    read_request, write_response,\n};\nuse archon_node::service::LeaseExecutor;\nuse archon_node::transport::{SecureStream, establish_initiator};\n",
    1,
)

anchor = '''fn temp_log(name: &str) -> PathBuf {
'''
helpers = r'''#[derive(Default)]
struct EnforcingTestExecutor {
    running: BTreeSet<u64>,
}

impl LeaseExecutor for EnforcingTestExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
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
                .execute(request)
                .unwrap_or_else(|reason| AgentResponse::Failed { binding: 0, reason });
            if write_response(&mut stream, &response).is_err() {
                break;
            }
        }
    });
    addr
}

'''
if anchor not in text:
    raise SystemExit("temp_log anchor not found")
text = text.replace(anchor, helpers + anchor, 1)

old_spawn = '''fn spawn_server(name: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let log = temp_log(name);
    std::thread::spawn(move || {
        let link = archon_control::server::AgentLink::Local { cgroup_root: None };
        let plane = std::sync::Arc::new(std::sync::Mutex::new(
            ControlPlane::boot(link, log, 0).expect("boot"),
        ));
        ControlPlane::serve(&plane, listener);
    });
    addr
}
'''
new_spawn = '''fn spawn_server(name: &str) -> String {
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
'''
if old_spawn not in text:
    raise SystemExit("spawn_server not found")
text = text.replace(old_spawn, new_spawn, 1)

old_comment = '''    // The agent registers and then goes silent; it never answers Prepare.
'''
new_comment = '''    // The agent announces itself and then goes silent before proving runtime
    // capabilities. Its connection handler may wait for that proof, but it
    // must never hold the shared ControlPlane lock while waiting.
'''
text = text.replace(old_comment, new_comment, 1)

old_submit_tail = '''    let ServerResponse::Submitted { lease, .. } = response else {
        panic!("expected Submitted, got {response:?}");
    };
    assert!(lease > 0);

    // While the agent stays silent, another client must be served promptly.
'''
new_submit_tail = '''    let ServerResponse::Submitted { lease, .. } = response else {
        panic!("expected Submitted, got {response:?}");
    };
    assert_eq!(
        lease, 0,
        "an agent that cannot prove capabilities must not receive Lease authority"
    );

    // While the agent's capability proof stays silent, another client must
    // still be served promptly.
'''
if old_submit_tail not in text:
    raise SystemExit("silent submit assertion not found")
text = text.replace(old_submit_tail, new_submit_tail, 1)

old_admin = '''    // Administrative requests stay responsive too.
    let start = Instant::now();
    let response = roundtrip(&mut client, ClientRequest::Revoke { lease });
    assert!(matches!(response, ServerResponse::Revoked));
    assert!(start.elapsed() < Duration::from_secs(2));
'''
new_admin = '''    // Administrative requests stay responsive too. There is deliberately no
    // lease to revoke because the silent agent never became execution-capable.
    let start = Instant::now();
    let response = roundtrip(&mut client, ClientRequest::Revoke { lease: 1 });
    assert!(matches!(response, ServerResponse::Error { .. }));
    assert!(start.elapsed() < Duration::from_secs(2));
'''
if old_admin not in text:
    raise SystemExit("silent admin assertion not found")
text = text.replace(old_admin, new_admin, 1)

path.write_text(text)
