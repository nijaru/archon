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
    // Extract `-c ADDR` from the argument list, stopping at `--`: beyond
    // it everything belongs to the workload's command, including any
    // flags of its own.
    let mut connect = None;
    let mut index = 0;
    while index < args.len() && args[index] != "--" {
        if args[index] == "-c" {
            connect = Some(
                args.get(index + 1)
                    .unwrap_or_else(|| fail("-c requires an address"))
                    .clone(),
            );
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
        (Some("submit"), connect)
        | (Some("status"), connect)
        | (Some("revoke"), connect)
        | (Some("logs"), connect) => client(connect, &args),
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

/// Report an operational failure and exit; the CLI never panics at users.
fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("archon: {message}");
    exit(1);
}

/// Parse a user-supplied value or report which flag was invalid.
fn parse_flag<T>(value: &str, flag: &str) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .unwrap_or_else(|err| fail(format!("invalid value for {flag}: '{value}' ({err})")))
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  archon serve --listen ADDR --log FILE [--remote ADDR | --no-local] [--cgroup-root PATH]\n  archon agent --listen ADDR | --register ADDR [--cgroup-root PATH]\n  archon demo [--remote ADDR]\n  archon -c ADDR submit [--owner N] [--cpus N] [--mem-mib N] [--lifetime SECS] -- CMD...\n  archon -c ADDR status | logs LEASE\n  archon -c ADDR revoke LEASE"
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
    let mut compact_every: u64 = 10_000;
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
            "--probe-secs" => probe_secs = parse_flag(value, "--probe-secs"),
            "--compact-every" => compact_every = parse_flag(value, "--compact-every"),
            "--cgroup-root" => cgroup_root = Some(value.clone()),
            _ => usage(),
        }
        index += 2;
    }
    let (Some(listen), Some(log)) = (listen, log) else {
        usage();
    };
    let token = load_token(&token_file);
    let link = match (&remote, no_local) {
        (Some(addr), _) => archon_control::server::AgentLink::Remote {
            addr: addr.clone(),
            token: token.clone(),
        },
        (None, false) => archon_control::server::AgentLink::Local { cgroup_root },
        (None, true) => archon_control::server::AgentLink::None,
    };
    let mut plane = match archon_control::server::ControlPlane::boot(link, log, compact_every) {
        Ok(plane) => plane,
        Err(err) => fail(format!("boot failed: {err}")),
    };
    if let Some(token) = token {
        plane.require_token(token);
        eprintln!("archon: link auth enabled");
    } else {
        eprintln!("archon: warning: no auth token configured; links are open");
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
    let listener = match TcpListener::bind(&listen) {
        Ok(listener) => listener,
        Err(err) => fail(format!("cannot listen on {listen}: {err}")),
    };
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
    let token = load_token(&token_file);
    if token.is_none() {
        eprintln!(
            "archon: warning: no auth token configured; this agent executes              commands for anyone who can reach {listen} (--token-file to secure)"
        );
    }
    let listener = match TcpListener::bind(&listen) {
        Ok(listener) => listener,
        Err(err) => fail(format!("cannot listen on {listen}: {err}")),
    };
    eprintln!("archon: agent listening on {listen}");
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        archon_control::api::set_stream_limits(&stream);
        let peer = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_default();
        // Every controller link runs a Noise XXpsk3 handshake first: the
        // token is the PSK, proven on both sides without crossing the wire.
        // A wrong token fails before any frame is read.
        let mut stream = match archon_node::transport::establish_responder(stream, token.as_deref())
        {
            Ok(stream) => stream,
            Err(_) => {
                eprintln!("archon: {peer} failed agent authentication");
                continue;
            }
        };
        // The first framed message declares the role; anything else is
        // refused before a single request is served.
        let role_ok = matches!(
            archon_control::api::read_greeting(&mut stream),
            Ok(archon_control::api::Greeting::Agent { .. })
        );
        if !role_ok {
            continue;
        }
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
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        fail(format!(
            "cannot create state directory {}: {err}",
            parent.display()
        ));
    }
    if let Err(err) = std::fs::write(&path, &id) {
        fail(format!(
            "cannot persist agent id to {}: {err}",
            path.display()
        ));
    }
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
            Ok(stream) => {
                archon_control::api::set_stream_limits(&stream);
                let mut stream =
                    match archon_node::transport::establish_initiator(stream, token.as_deref()) {
                        Ok(stream) => stream,
                        Err(err) => {
                            eprintln!("archon: handshake with control plane failed: {err}");
                            std::thread::sleep(Duration::from_secs(2));
                            continue;
                        }
                    };
                let description = match archon_node::discover::try_describe() {
                    Ok(description) => description,
                    Err(err) => {
                        eprintln!(
                            "archon: device discovery incomplete; registration deferred: {err}"
                        );
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                };
                let greeting = archon_control::api::Greeting::Agent {
                    instance_id: instance_id.clone(),
                    name: name.clone().unwrap_or(description.name),
                    cpus: description.cpus,
                    memory_bytes: description.memory_bytes,
                    devices: description.devices.clone(),
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
            if let Err(err) = service.register_remote(addr) {
                fail(format!("cannot connect to remote agent at {addr}: {err}"));
            }
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
            kind: archon_kernel::ResourceClass::Cpu,
            quantity: archon_kernel::qty(archon_kernel::CapacityDimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec!["sleep".into(), "5".into()],
        machine_local: true,
        grace_secs: 0,
        image: None,
        storage: vec![],
        ports: vec![],
        lifetime: 30,
        keep_alive: false,
        priority: 1,
    };
    service.submit(request, archon_kernel::OwnerId::from_u64(1));
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
        .nodes_of_class(archon_kernel::ResourceClass::Machine)
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
            .nodes_of_class(archon_kernel::ResourceClass::Memory)
            .first()
            .and_then(|node| service.cluster.graph.node(*node))
            .and_then(|node| {
                node.capacity
                    .iter()
                    .find(|(dimension, _)| **dimension == archon_kernel::CapacityDimension::Bytes)
                    .map(|(_, amount)| amount / (1 << 30))
            })
            .unwrap_or(0);
        let cpus = service
            .cluster
            .graph
            .nodes_of_class(archon_kernel::ResourceClass::Cpu)
            .len();
        println!("archon: discovered {name} ({cpus} cpus, {memory} GiB)");
    }
}

// --- client --------------------------------------------------------------

fn client(connect: Option<String>, args: &[String]) {
    let rest: Vec<String> = args.to_vec();
    let Some(addr) = connect else { usage() };
    let mut stream = match TcpStream::connect(&addr).and_then(|stream| {
        archon_control::api::set_stream_limits(&stream);
        let token = std::env::var("ARCHON_TOKEN").ok().filter(|t| !t.is_empty());
        archon_node::transport::establish_initiator(stream, token.as_deref())
    }) {
        Ok(stream) => stream,
        Err(err) => fail(format!("cannot reach control plane at {addr}: {err}")),
    };
    let greeting = archon_control::api::Greeting::Client;
    if let Err(err) = archon_node::protocol::write_frame(&mut stream, &greeting) {
        fail(format!("cannot greet control plane at {addr}: {err}"));
    }
    // A failed handshake or greeting is silent (connection closed); surface
    // it on the first request failing instead.
    let response = match rest[0].as_str() {
        "submit" => submit_request(&rest[1..]),
        "status" => ClientRequest::Status,
        "revoke" | "logs" => {
            let Some(value) = rest.get(1) else {
                fail("lease id required (archon -c ADDR revoke|logs LEASE)");
            };
            let lease: u64 = parse_flag(value, "lease id");
            if rest[0] == "revoke" {
                ClientRequest::Revoke { lease }
            } else {
                ClientRequest::Logs { lease }
            }
        }
        _ => usage(),
    };
    if let Err(err) = write_frame(&mut stream, &response) {
        fail(format!("cannot send request: {err}"));
    }
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
    let mut volumes: Vec<String> = Vec::new();
    let mut ports: Vec<String> = Vec::new();
    let mut grace_secs: u32 = 0;
    let mut image: Option<String> = None;
    let mut gpus: u64 = 0;
    let mut rest = args;
    while !rest.is_empty() && rest[0].starts_with("--") && rest[0] != "--" {
        let (flag, value) = (
            rest[0].as_str(),
            match rest.get(1) {
                Some(value) => value,
                None => fail(format!("flag {} needs a value", rest[0])),
            },
        );
        match flag {
            "--owner" => owner = parse_flag(value, flag),
            "--cpus" => cpus = parse_flag(value, flag),
            "--mem-mib" => mem_mib = parse_flag(value, flag),
            "--lifetime" => lifetime = parse_flag(value, flag),
            "--keep-alive" => {
                keep_alive = true;
                rest = &rest[1..];
                continue;
            }
            "--grace-secs" => grace_secs = parse_flag(value, flag),
            "--image" => image = Some(value.clone()),
            "--gpu" => gpus = parse_flag(value, flag),
            "--volume" => {
                volumes.push(value.clone());
                rest = &rest[2..];
                continue;
            }
            "--publish" => {
                ports.push(value.clone());
                rest = &rest[2..];
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
        volumes,
        ports,
        grace_secs,
        image,
        gpus,
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
                let exit = lease
                    .exit_code
                    .map(|code| format!(" exit={code}"))
                    .unwrap_or_default();
                let command = lease.command.join(" ");
                println!(
                    "lease {} owner={} state={}{} [{}]",
                    lease.id, lease.owner, lease.state, exit, command
                );
            }
        }
        ServerResponse::Revoked => println!("revoked"),
        ServerResponse::Logs { output, .. } => print!("{output}"),
        ServerResponse::Error { reason } => {
            eprintln!("error: {reason}");
            exit(1);
        }
    }
}
