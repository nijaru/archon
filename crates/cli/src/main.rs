//! `fleet` — one binary, three roles.
//!
//! - `fleet serve --listen ADDR --log FILE [--remote ADDR]` — control plane:
//!   persistent command log, admission, the Cluster.
//! - `fleet agent --listen ADDR [--cgroup-root PATH]` — node agent: execute
//!   leases as real processes on this machine.
//! - `fleet demo [--remote ADDR]` — walking-skeleton demo.
//! - `fleet -c ADDR submit|status|revoke` — client.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

use fleet_control::api::{ClientRequest, ServerResponse, read_response, write_frame};
use fleet_node::protocol::{read_request, write_response};
use fleet_node::service::NodeService;

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

fn usage() -> ! {
    eprintln!(
        "usage:\n  fleet serve --listen ADDR --log FILE [--remote ADDR] [--cgroup-root PATH]\n  fleet agent --listen ADDR [--cgroup-root PATH]\n  fleet demo [--remote ADDR]\n  fleet -c ADDR submit [--owner N] [--cpus N] [--mem-mib N] [--lifetime SECS] -- CMD...\n  fleet -c ADDR status\n  fleet -c ADDR revoke LEASE"
    );
    exit(2);
}

// --- control plane -------------------------------------------------------

fn serve(args: &[String]) {
    let mut listen = None;
    let mut log = None;
    let mut remote = None;
    let mut cgroup_root = std::env::var("FLEET_CGROUP_ROOT").ok();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let Some(value) = args.get(index + 1) else {
            usage()
        };
        match flag {
            "--listen" => listen = Some(value.clone()),
            "--log" => log = Some(PathBuf::from(value)),
            "--remote" => remote = Some(value.clone()),
            "--cgroup-root" => cgroup_root = Some(value.clone()),
            _ => usage(),
        }
        index += 2;
    }
    let (Some(listen), Some(log)) = (listen, log) else {
        usage();
    };
    let link = match &remote {
        Some(addr) => fleet_control::server::AgentLink::Remote { addr: addr.clone() },
        None => fleet_control::server::AgentLink::Local { cgroup_root },
    };
    let mut plane = fleet_control::server::ControlPlane::boot(link, log).expect("boot");
    let listener = TcpListener::bind(&listen).expect("bind");
    eprintln!("fleet: control plane serving on {listen}");
    plane.serve(listener).expect("serve");
}

// --- node agent ----------------------------------------------------------

fn agent(args: &[String]) {
    let mut listen = None;
    let mut cgroup_root = std::env::var("FLEET_CGROUP_ROOT").ok();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let Some(value) = args.get(index + 1) else {
            usage()
        };
        match flag {
            "--listen" => listen = Some(value.clone()),
            "--cgroup-root" => cgroup_root = Some(value.clone()),
            _ => usage(),
        }
        index += 2;
    }
    let Some(listen) = listen else {
        usage();
    };
    let listener = TcpListener::bind(&listen).expect("bind");
    eprintln!("fleet: agent listening on {listen}");
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let peer = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_default();
        eprintln!("fleet: controller connected from {peer}");
        let runtime = build_runtime(&cgroup_root);
        let mut agent = fleet_node::agent::LeaseAgent::new(runtime);
        while let Ok(request) = read_request(&mut stream) {
            let response = agent.handle(request);
            if write_response(&mut stream, &response).is_err() {
                break;
            }
        }
        eprintln!("fleet: controller {peer} disconnected");
    }
}

fn build_runtime(cgroup_root: &Option<String>) -> fleet_node::runtime::ProcessRuntime {
    #[cfg(target_os = "linux")]
    if let Some(root) = cgroup_root {
        eprintln!("fleet: cgroup v2 enforcement enabled at {root}");
        return fleet_node::runtime::ProcessRuntime::new().with_cgroup_root(root.clone());
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cgroup_root;
    fleet_node::runtime::ProcessRuntime::new()
}

// --- demo ----------------------------------------------------------------

fn demo(remote: Option<String>) {
    let mut service = match &remote {
        Some(addr) => {
            let service = NodeService::connect(addr).expect("connect to agent");
            eprintln!("fleet: connected to remote agent at {addr}");
            service
        }
        None => {
            let mut service = NodeService::new();
            #[cfg(target_os = "linux")]
            if let Ok(root) = std::env::var("FLEET_CGROUP_ROOT") {
                service = NodeService::local_with_cgroups(root);
                eprintln!("fleet: cgroup v2 enforcement enabled");
            }
            let (local, nodes, edges) = fleet_node::discover::discover();
            service.boot(nodes, edges).expect("boot cluster");
            let _ = local;
            service
        }
    };
    print_machine(&service);

    // Submit a real workload: sleep 5 under a 30-second lease.
    let request = fleet_kernel::Request {
        id: fleet_kernel::RequestId::from_u64(1),
        class: fleet_kernel::RequestClass::Batch,
        needs: vec![fleet_kernel::Need {
            kind: fleet_kernel::NodeKind::Cpu,
            quantity: fleet_kernel::qty(fleet_kernel::Dimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        command: vec!["sleep".into(), "5".into()],
        lifetime: 30,
        priority: 1,
    };
    service.submit(
        request,
        fleet_kernel::OwnerId::from_u64(1),
        vec!["sleep".into(), "5".into()],
    );
    service.tick().expect("tick");
    let admitted = service.admit_one().expect("admit");
    assert_eq!(admitted, Some(fleet_kernel::RequestId::from_u64(1)));
    let lease = fleet_kernel::LeaseId::from_u64(1);
    assert!(
        service.is_running(lease),
        "sleep must be running under the lease"
    );
    let where_ = remote.as_ref().map_or("locally", |addr| addr.as_str());
    println!("fleet: lease 1 active — `sleep 5` is running as a real process on {where_}");

    // Revoke: the lease fences and the process dies immediately.
    std::thread::sleep(Duration::from_millis(500));
    service.revoke(lease).expect("revoke");
    assert!(
        !service.is_running(lease),
        "process must die with the lease"
    );
    println!("fleet: lease 1 revoked — process terminated");
    println!("fleet: walking skeleton complete");
}

fn print_machine(service: &NodeService) {
    let machine = service
        .cluster
        .graph
        .nodes_of_kind(fleet_kernel::NodeKind::Machine)
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
            .nodes_of_kind(fleet_kernel::NodeKind::Memory)
            .first()
            .and_then(|node| service.cluster.graph.node(*node))
            .and_then(|node| {
                node.capacity
                    .iter()
                    .find(|(dimension, _)| **dimension == fleet_kernel::Dimension::Bytes)
                    .map(|(_, amount)| amount / (1 << 30))
            })
            .unwrap_or(0);
        let cpus = service
            .cluster
            .graph
            .nodes_of_kind(fleet_kernel::NodeKind::Cpu)
            .len();
        println!("fleet: discovered {name} ({cpus} cpus, {memory} GiB)");
    }
}

// --- client --------------------------------------------------------------

fn client(connect: Option<String>, args: &[String]) {
    let rest: Vec<String> = args.to_vec();
    let Some(addr) = connect else { usage() };
    let mut stream = TcpStream::connect(&addr).expect("connect to control plane");
    let response = match rest[0].as_str() {
        "submit" => submit_request(&rest[1..]),
        "status" => ClientRequest::Status,
        "revoke" => ClientRequest::Revoke {
            lease: rest.get(1).expect("lease id").parse().expect("lease id"),
        },
        _ => usage(),
    };
    write_frame(&mut stream, &response).expect("send");
    let reply = read_response(&mut stream).expect("read response");
    print_response(reply);
}

fn submit_request(args: &[String]) -> ClientRequest {
    let mut owner = 1;
    let mut cpus = 1;
    let mut mem_mib = 0;
    let mut lifetime = 60;
    let mut rest = args;
    while !rest.is_empty() && rest[0].starts_with("--") && rest[0] != "--" {
        let (flag, value) = (rest[0].as_str(), rest.get(1).expect("flag value"));
        match flag {
            "--owner" => owner = value.parse().expect("owner"),
            "--cpus" => cpus = value.parse().expect("cpus"),
            "--mem-mib" => mem_mib = value.parse().expect("mem-mib"),
            "--lifetime" => lifetime = value.parse().expect("lifetime"),
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
