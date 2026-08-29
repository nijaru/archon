from pathlib import Path

path = Path("crates/control/tests/persist.rs")
text = path.read_text()

old_import = '''use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
'''
new_import = '''use std::collections::BTreeSet;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
'''
if old_import not in text:
    raise SystemExit("std imports not found")
text = text.replace(old_import, new_import, 1)

old_node_import = '''use archon_node::service::NodeService;
'''
new_node_import = '''use archon_node::discover::MachineDescription;
use archon_node::protocol::{
    AgentRequest, AgentResponse, ExecutionCapabilities, RuntimeCapabilities, read_greeting,
    read_request, write_response,
};
use archon_node::service::{LeaseExecutor, NodeService};
'''
if old_node_import not in text:
    raise SystemExit("node import not found")
text = text.replace(old_node_import, new_node_import, 1)

anchor = '''fn temp_log(name: &str) -> PathBuf {
'''
helpers = r'''#[derive(Default)]
struct EnforcingTestExecutor {
    running: BTreeSet<u64>,
}

impl EnforcingTestExecutor {
    fn capabilities() -> ExecutionCapabilities {
        ExecutionCapabilities {
            process: RuntimeCapabilities {
                available: true,
                cpu_limit: true,
                memory_limit: true,
                device_isolation: true,
                physical_cpu_placement: false,
                numa_memory_placement: false,
            },
            container: RuntimeCapabilities::default(),
        }
    }
}

impl LeaseExecutor for EnforcingTestExecutor {
    fn execute(&mut self, request: AgentRequest) -> Result<AgentResponse, String> {
        Ok(match request {
            AgentRequest::Capabilities => AgentResponse::Capabilities {
                capabilities: Self::capabilities(),
            },
            AgentRequest::Hello => AgentResponse::Welcome {
                instance_id: "persist-agent".into(),
                name: "persist-agent".into(),
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

fn test_machine() -> MachineDescription {
    MachineDescription {
        instance_id: "persist-agent".into(),
        name: "persist-agent".into(),
        cpus: 4,
        memory_bytes: 8 * (1 << 30),
        host_nodes: Vec::new(),
        devices: Vec::new(),
    }
}

fn register_test_agent(service: &mut NodeService) {
    service
        .register_agent(test_machine(), Box::new(EnforcingTestExecutor::default()))
        .expect("register enforcing test agent");
}

/// Serve one controller-initiated remote connection with the same explicit
/// enforcement double used by the direct persistence tests. The transport,
/// protocol, sessions, command log, and recovery paths remain production code;
/// only workload execution itself is simulated because hosted CI has no
/// delegated cgroup subtree.
fn spawn_test_agent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test agent");
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

old_logged = '''    service
        .register_local(None)
        .expect("register local machine");
    service
'''
new_logged = '''    register_test_agent(&mut service);
    service
'''
if old_logged not in text:
    raise SystemExit("logged_service registration not found")
text = text.replace(old_logged, new_logged, 1)

old_recovery = '''    recovered
        .register_local(None)
        .expect("register local agent");
'''
new_recovery = '''    register_test_agent(&mut recovered);
'''
if old_recovery not in text:
    raise SystemExit("recovery registration not found")
text = text.replace(old_recovery, new_recovery, 1)

# Snapshot compaction uses a remote test agent so ControlPlane itself remains
# unmodified and the encrypted controller↔agent seam is still exercised.
text = text.replace(
    '''    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
''',
    '''    use std::sync::{Arc, Mutex};
''',
    1,
)
old_link = '''    let link = archon_control::server::AgentLink::Local { cgroup_root: None };
'''
new_link = '''    let agent_addr = spawn_test_agent();
    let link = archon_control::server::AgentLink::Remote {
        addr: agent_addr,
        token: None,
    };
'''
if old_link not in text:
    raise SystemExit("initial snapshot link not found")
text = text.replace(old_link, new_link, 1)
old_link2 = '''    let link2 = archon_control::server::AgentLink::Local { cgroup_root: None };
'''
new_link2 = '''    let restarted_agent_addr = spawn_test_agent();
    let link2 = archon_control::server::AgentLink::Remote {
        addr: restarted_agent_addr,
        token: None,
    };
'''
if old_link2 not in text:
    raise SystemExit("restart snapshot link not found")
text = text.replace(old_link2, new_link2, 1)

# High-water recovery must also prove capability before admitting fresh work.
old_highwater = '''    recovered.register_local(None).expect("register");
'''
new_highwater = '''    register_test_agent(&mut recovered);
'''
if old_highwater not in text:
    raise SystemExit("highwater registration not found")
text = text.replace(old_highwater, new_highwater, 1)

path.write_text(text)
