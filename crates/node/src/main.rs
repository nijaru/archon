//! Fleet node: one machine, real processes under leases, local or remote.
//!
//! - `fleet-node` — local demo: boot a Cluster over this machine, run a
//!   real process under a lease, revoke it, watch it die.
//! - `fleet-node serve --listen ADDR [--cgroup-root PATH]` — agent daemon:
//!   execute a controller's leases on this machine over the agent protocol.
//! - `fleet-node demo --remote ADDR` — run the demo against a remote agent.

use std::time::Duration;

use fleet_kernel::{
    Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};

use fleet_node::protocol::{read_request, write_response};
use fleet_node::service::NodeService;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("demo") => {
            let remote = args
                .get(1)
                .and_then(|arg| arg.strip_prefix("--remote="))
                .map(String::from);
            demo(remote)
        }
        _ => demo(None),
    }
}

/// Agent daemon: serve one controller connection at a time; a new
/// connection replaces a dead one. Sessions fence stale controllers.
fn serve(args: &[String]) {
    let mut listen = None;
    let mut cgroup_root = std::env::var("FLEET_CGROUP_ROOT").ok();
    let mut rest = args;
    while let [flag, value, tail @ ..] = rest {
        match flag.as_str() {
            "--listen" => listen = Some(value.clone()),
            "--cgroup-root" => cgroup_root = Some(value.clone()),
            other => {
                eprintln!("unknown flag {other}");
                std::process::exit(2);
            }
        }
        rest = tail;
    }
    let Some(listen) = listen else {
        eprintln!("usage: fleet-node serve --listen ADDR [--cgroup-root PATH]");
        std::process::exit(2);
    };
    let listener = std::net::TcpListener::bind(&listen).expect("bind");
    println!("fleet: agent listening on {listen}");
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let peer = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_default();
        println!("fleet: controller connected from {peer}");
        let runtime = build_runtime(&cgroup_root);
        let mut agent = fleet_node::agent::LeaseAgent::new(runtime);
        while let Ok(request) = read_request(&mut stream) {
            let response = agent.handle(request);
            if write_response(&mut stream, &response).is_err() {
                break;
            }
        }
        println!("fleet: controller {peer} disconnected");
    }
}

fn build_runtime(cgroup_root: &Option<String>) -> fleet_node::runtime::ProcessRuntime {
    #[cfg(target_os = "linux")]
    if let Some(root) = cgroup_root {
        println!("fleet: cgroup v2 enforcement enabled at {root}");
        return fleet_node::runtime::ProcessRuntime::new().with_cgroup_root(root.clone());
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cgroup_root;
    fleet_node::runtime::ProcessRuntime::new()
}

/// The walking-skeleton demo, local or against a remote agent.
fn demo(remote: Option<String>) {
    let mut service = match &remote {
        Some(addr) => {
            let service = NodeService::connect(addr).expect("connect to agent");
            println!("fleet: connected to remote agent at {addr}");
            service
        }
        None => {
            let mut service = NodeService::new();
            #[cfg(target_os = "linux")]
            if let Ok(root) = std::env::var("FLEET_CGROUP_ROOT") {
                service = NodeService::local_with_cgroups(root);
                println!("fleet: cgroup v2 enforcement enabled");
            }
            let (local, nodes, edges) = fleet_node::discover::discover();
            service.boot(nodes, edges).expect("boot cluster");
            let _ = local;
            service
        }
    };
    print_machine(&service);

    // Submit a real workload: sleep 5 under a 30-second lease.
    let request = Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, 1),
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
        OwnerId::from_u64(1),
        vec!["sleep".into(), "5".into()],
    );
    service.tick().expect("tick");
    let admitted = service.admit_one().expect("admit");
    assert_eq!(admitted, Some(RequestId::from_u64(1)));
    let lease = LeaseId::from_u64(1);
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
        .nodes_of_kind(NodeKind::Machine)
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
            .nodes_of_kind(NodeKind::Memory)
            .first()
            .and_then(|node| service.cluster.graph.node(*node))
            .and_then(|node| {
                node.capacity
                    .iter()
                    .find(|(dimension, _)| **dimension == Dimension::Bytes)
                    .map(|(_, amount)| amount / (1 << 30))
            })
            .unwrap_or(0);
        let cpus = service.cluster.graph.nodes_of_kind(NodeKind::Cpu).len();
        println!("fleet: discovered {name} ({cpus} cpus, {memory} GiB)");
    }
}
