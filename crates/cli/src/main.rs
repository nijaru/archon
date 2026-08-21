//! `archon` — one binary, three roles.
//!
//! - `archon serve --listen ADDR --log FILE [--remote ADDR]` — control plane:
//!   persistent command log, admission, the Cluster.
//! - `archon agent --listen ADDR [--cgroup-root PATH]` — node agent: execute
//!   leases as real processes on this machine.
//! - `archon demo [--remote ADDR]` — walking-skeleton demo.
//! - `archon -c ADDR submit|status|revoke` — client.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

use archon_control::api::{ClientRequest, ServerResponse, read_response, write_frame};
use archon_node::protocol::{read_request, write_response};
use archon_node::service::NodeService;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Extract `-c ADDR` (client mode) from anywhere in the argument list.
    let mut connect = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "-c" {
            connect = Some(args.get(index + 1).expect("-c ADDR").clone());
            args.drain(index..=(index + 1).min(args.len() - 1));
        } else {
            index += 1;
        }
    }
    match (args.first().map(String::as_str), connect) {
        (Some("serve"), _) => serve(&args[1..]),
        (Some("agent"), _) => agent(&args[1..]),
        (Some("demo"), _) => demo(
            args.get(1)
                .and_then(|arg| arg.strip_prefix("--remote="))
                .map(String::from),
        ),
        (Some("submit"), connect) | (Some("status"), connect) | (Some("revoke"), connect) => {
            client(connect, &args)
        }
        _ => usage(),
    }
}

/// Shared secret for links: --token-file, then ARCHON_TOKEN. None means
/// open mode (the server warns; clients just omit the token).
fn load_token(token_file: &Option<String>) -> Option<String> {
    if let Some(path) = token_file {
        let content = std::fs::read_to_string(path).unwrap_or_else(|err| {
            eprintln!("archon: cannot read token file {path}: {err}");
            std::process::exit(2);
        });
        return Some(content.trim().to_string());
    }
    std::env::var("ARCHON_TOKEN").ok().filter(|t| !t.is_empty())
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  archon serve --listen ADDR --log FILE [--remote ADDR | --no-local] [--cgroup-root PATH]\n  archon agent --listen ADDR | --register ADDR [--cgroup-root PATH]\n  archon demo [--remote ADDR]\n  archon -c ADDR submit [--owner N] [--cpus N] [--mem-mib N] [--lifetime SECS] -- CMD...\n  archon -c ADDR status\n  archon -c ADDR revoke LEASE"
    );
    exit(2);
}

// --- control plane -------------------------------------------------------

fn serve(args: &[String]) {
    let mut listen = None;
    let mut log = None;
    let mut remote = None;
    let mut no_local = false;
    let mut token_file = None;
    let mut probe_secs: u64 = 5;
    let mut cgroup_root = std::env::var("ARCHON_CGROUP_ROOT").ok();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--no-local" {
            no_local = true;
            index += 1;
            continue;
        }
        let flag = args[index].as_str();
        let Some(value) = args.get(index + 1) else {
            usage()
        };
        match flag {
            "--listen" => listen = Some(value.clone()),
            "--log" => log = Some(PathBuf::from(value)),
            "--remote" => remote = Some(value.clone()),
            "--token-file" => token_file = Some(value.clone()),
            "--probe-secs" => probe_secs = value.parse().expect("probe-secs"),
            "--cgroup-root" => cgroup_root = Some(value.clone()),
            _ => usage(),
        }
        index += 2;
    }
    let (Some(listen), Some(log)) = (listen, log) else {
        usage();
    };
    let link = match (&remote, no_local) {
        (Some(addr), _) => archon_control::server::AgentLink::Remote { addr: addr.clone() },
        (None, false) => archon_control::server::AgentLink::Local { cgroup_root },
        (None, true) => archon_control::server::AgentLink::None,
    };
    let mut plane = archon_control::server::ControlPlane::boot(link, log).expect("boot");
    match load_token(&token_file) {
        Some(token) => {
            plane.require_token(token);
            eprintln!("archon: link auth enabled");
        }
        None => eprintln!("archon: warning: no auth token configured; links are open"),
    }
    let plane = std::sync::Arc::new(std::sync::Mutex::new(plane));
    {
        let plane = plane.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(probe_secs));
                plane.lock().expect("plane lock").maintain();
            }
        });
    }
    let listener = TcpListener::bind(&listen).expect("bind");
    eprintln!("archon: control plane serving on {listen}");
    archon_control::server::ControlPlane::serve(&plane, listener);
}

// --- node agent ----------------------------------------------------------

fn agent(args: &[String]) {
    let mut listen = None;
    let mut register = None;
    let mut name = None;
    let mut id = None;
    let mut token_file = None;
    let mut cgroup_root = std::env::var("ARCHON_CGROUP_ROOT").ok();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let Some(value) = args.get(index + 1) else {
            usage()
        };
        match flag {
            "--listen" => listen = Some(value.clone()),
            "--register" => register = Some(value.clone()),
            "--name" => name = Some(value.clone()),
            "--id" => id = Some(value.clone()),
            "--token-file" => token_file = Some(value.clone()),
            "--cgroup-root" => cgroup_root = Some(value.clone()),
            _ => usage(),
        }
        index += 2;
    }
    if let Some(addr) = register {
        dial_in(&addr, id, name, load_token(&token_file), cgroup_root);
        return;
    }
    let Some(listen) = listen else {
        usage();
    };
    let listener = TcpListener::bind(&listen).expect("bind");
    eprintln!("archon: agent listening on {listen}");
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let peer = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_default();
        eprintln!("archon: controller connected from {peer}");
        let runtime = build_runtime(&cgroup_root);
        let mut agent = archon_node::agent::LeaseAgent::new(runtime);
        while let Ok(request) = read_request(&mut stream) {
            let response = agent.handle(request);
            if write_response(&mut stream, &response).is_err() {
                break;
            }
        }
        eprintln!("archon: controller {peer} disconnected");
    }
}

/// Dial-in mode: connect to the control plane, announce this machine, then
/// execute its leases. Reconnects until the control plane answers.
/// The agent's stable identity: generated once, persisted to the state
/// directory, and presented on every registration.
fn load_instance_id(id: Option<String>) -> String {
    if let Some(id) = id {
        return id;
    }
    let path = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })
        .map(|state| state.join("archon/agent-id"));
    let Some(path) = path else {
        eprintln!("archon: no state directory for the agent id; pass --id");
        std::process::exit(2);
    };
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return existing.to_string();
        }
    }
    // 64 random bits from the OS; time+pid only if /dev/urandom is absent
    // (macOS has it, so this is effectively never).
    use std::io::Read;
    let mut bytes = [0u8; 8];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    let id = if ok {
        format!("{:016x}", u64::from_le_bytes(bytes))
    } else {
        format!(
            "{:016x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos() as u64
                ^ std::process::id() as u64
        )
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, &id).expect("persist agent id");
    id
}

fn dial_in(
    addr: &str,
    id: Option<String>,
    name: Option<String>,
    token: Option<String>,
    cgroup_root: Option<String>,
) {
    let instance_id = load_instance_id(id);
    loop {
        match TcpStream::connect(addr) {
            Ok(mut stream) => {
                let description = archon_node::discover::describe();
                let greeting = archon_control::api::Greeting::Agent {
                    token: token.clone(),
                    instance_id: instance_id.clone(),
                    name: name.clone().unwrap_or(description.name),
                    cpus: description.cpus,
                    memory_bytes: description.memory_bytes,
                };
                if archon_node::protocol::write_frame(&mut stream, &greeting).is_err() {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
                eprintln!("archon: registered with control plane at {addr}");
                let runtime = build_runtime(&cgroup_root);
                let mut lease_agent = archon_node::agent::LeaseAgent::new(runtime);
                while let Ok(request) = read_request(&mut stream) {
                    let response = lease_agent.handle(request);
                    if write_response(&mut stream, &response).is_err() {
                        break;
                    }
                }
                eprintln!("archon: lost the control plane; retrying");
            }
            Err(_) => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

fn build_runtime(cgroup_root: &Option<String>) -> archon_node::runtime::ProcessRuntime {
    #[cfg(target_os = "linux")]
    if let Some(root) = cgroup_root {
        eprintln!("archon: cgroup v2 enforcement enabled at {root}");
        return archon_node::runtime::ProcessRuntime::new().with_cgroup_root(root.clone());
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cgroup_root;
    archon_node::runtime::ProcessRuntime::new()
}

// --- demo ----------------------------------------------------------------

fn demo(remote: Option<String>) {
    let mut service = match &remote {
        Some(addr) => {
            let mut service = NodeService::new();
            service
                .register_remote(addr)
                .expect("register remote agent");
            eprintln!("archon: connected to remote agent at {addr}");
            service
        }
        None => {
            let cgroup_root = std::env::var("ARCHON_CGROUP_ROOT").ok();
            NodeService::local(cgroup_root)
        }
    };
    print_machine(&service);

    // Submit a real workload: sleep 5 under a 30-second lease.
    let request = archon_kernel::Request {
        id: archon_kernel::RequestId::from_u64(1),
        class: archon_kernel::RequestClass::Batch,
        needs: vec![archon_kernel::Need {
            kind: archon_kernel::NodeKind::Cpu,
            quantity: archon_kernel::qty(archon_kernel::Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec!["sleep".into(), "5".into()],
        machine_local: true,
        image: None,
        lifetime: 30,
        keep_alive: false,
        priority: 1,
    };
    service.submit(
        request,
        archon_kernel::OwnerId::from_u64(1),
        vec!["sleep".into(), "5".into()],
    );
    service.tick().expect("tick");
    let admitted = service.admit_one().expect("admit");
    assert_eq!(admitted, Some(archon_kernel::RequestId::from_u64(1)));
    let lease = archon_kernel::LeaseId::from_u64(1);
    assert!(
        service.is_running(lease),
        "sleep must be running under the lease"
    );
    let where_ = remote.as_ref().map_or("locally", |addr| addr.as_str());
    println!("archon: lease 1 active — `sleep 5` is running as a real process on {where_}");

    // Revoke: the lease fences and the process dies immediately.
    std::thread::sleep(Duration::from_millis(500));
    service.revoke(lease).expect("revoke");
    assert!(
        !service.is_running(lease),
        "process must die with the lease"
    );
    println!("archon: lease 1 revoked — process terminated");
    println!("archon: walking skeleton complete");
}

fn print_machine(service: &NodeService) {
    let machine = service
        .cluster
        .graph
        .nodes_of_kind(archon_kernel::NodeKind::Machine)
        .first()
        .copied();
    if let Some(machine) = machine {
        let name = service
            .cluster
            .graph
            .node(machine)
            .and_then(|node| node.attrs.get("name"))
            .cloned()
            .unwrap_or_else(|| "machine".into());
        let memory = service
            .cluster
            .graph
            .nodes_of_kind(archon_kernel::NodeKind::Memory)
            .first()
            .and_then(|node| service.cluster.graph.node(*node))
            .and_then(|node| {
                node.capacity
                    .iter()
                    .find(|(dimension, _)| **dimension == archon_kernel::Dimension::Bytes)
                    .map(|(_, amount)| amount / (1 << 30))
            })
            .unwrap_or(0);
        let cpus = service
            .cluster
            .graph
            .nodes_of_kind(archon_kernel::NodeKind::Cpu)
            .len();
        println!("archon: discovered {name} ({cpus} cpus, {memory} GiB)");
    }
}

// --- client --------------------------------------------------------------

fn client(connect: Option<String>, args: &[String]) {
    let rest: Vec<String> = args.to_vec();
    let Some(addr) = connect else { usage() };
    let mut stream = TcpStream::connect(&addr).expect("connect to control plane");
    let token = std::env::var("ARCHON_TOKEN").ok().filter(|t| !t.is_empty());
    let greeting = archon_control::api::Greeting::Client { token };
    archon_node::protocol::write_frame(&mut stream, &greeting).expect("send greeting");
    // The server's auth failure is silent (connection closed); surface it
    // on the first request failing instead.
    let response = match rest[0].as_str() {
        "submit" => submit_request(&rest[1..]),
        "status" => ClientRequest::Status,
        "revoke" => ClientRequest::Revoke {
            lease: rest.get(1).expect("lease id").parse().expect("lease id"),
        },
        _ => usage(),
    };
    write_frame(&mut stream, &response).expect("send");
    let reply = read_response(&mut stream).unwrap_or_else(|_| {
        eprintln!("error: connection closed by control plane (bad token?)");
        std::process::exit(1);
    });
    print_response(reply);
}

fn submit_request(args: &[String]) -> ClientRequest {
    let mut owner = 1;
    let mut cpus = 1;
    let mut mem_mib = 0;
    let mut lifetime = 60;
    let mut keep_alive = false;
    let mut rest = args;
    while !rest.is_empty() && rest[0].starts_with("--") && rest[0] != "--" {
        let (flag, value) = (rest[0].as_str(), rest.get(1).expect("flag value"));
        match flag {
            "--owner" => owner = value.parse().expect("owner"),
            "--cpus" => cpus = value.parse().expect("cpus"),
            "--mem-mib" => mem_mib = value.parse().expect("mem-mib"),
            "--lifetime" => lifetime = value.parse().expect("lifetime"),
            "--keep-alive" => {
                keep_alive = true;
                rest = &rest[1..];
                continue;
            }
            other => {
                eprintln!("unknown flag {other}");
                exit(2);
            }
        }
        rest = &rest[2..];
    }
    let command: Vec<String> = match rest.split_first() {
        Some((sep, command)) if sep == "--" => command.to_vec(),
        _ => {
            eprintln!("submit needs `-- CMD...`");
            exit(2);
        }
    };
    ClientRequest::Submit {
        owner,
        cpus,
        memory_mib: mem_mib,
        lifetime_secs: lifetime,
        command,
        keep_alive,
    }
}

fn print_response(response: ServerResponse) {
    match response {
        ServerResponse::Submitted { request, lease } => {
            if lease == 0 {
                println!("queued request {request} (nothing admitted yet)");
            } else {
                println!("request {request} admitted as lease {lease}");
            }
        }
        ServerResponse::Status { queue_len, leases } => {
            println!("queue: {queue_len}");
            if leases.is_empty() {
                println!("no leases");
            }
            for lease in leases {
                println!(
                    "lease {} owner={} state={} expires_at={}",
                    lease.id, lease.owner, lease.state, lease.expires_at
                );
            }
        }
        ServerResponse::Revoked => println!("revoked"),
        ServerResponse::Error { reason } => {
            eprintln!("error: {reason}");
            exit(1);
        }
    }
}
