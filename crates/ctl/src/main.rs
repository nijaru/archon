//! fleet-ctl: control plane server and CLI client.
//!
//! Server:  fleet-ctl serve --listen ADDR --log FILE [--remote ADDR]
//!          [--cgroup-root PATH]
//! Client:  fleet-ctl -c ADDR submit --cpus N --mem-mib N --lifetime SECS -- CMD...
//!          fleet-ctl -c ADDR status
//!          fleet-ctl -c ADDR revoke LEASE

use std::net::TcpListener;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::exit;

use fleet_ctl::api::{ClientRequest, ServerResponse, read_response, write_frame};
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
        (Some("submit"), connect) | (Some("status"), connect) | (Some("revoke"), connect) => {
            client(connect, &args)
        }
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  fleet-ctl serve --listen ADDR --log FILE [--remote ADDR] [--cgroup-root PATH]\n  fleet-ctl -c ADDR submit --cpus N --mem-mib N --lifetime SECS -- CMD...\n  fleet-ctl -c ADDR status\n  fleet-ctl -c ADDR revoke LEASE"
    );
    exit(2);
}

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
        Some(addr) => fleet_ctl::server::AgentLink::Remote { addr: addr.clone() },
        None => fleet_ctl::server::AgentLink::Local { cgroup_root },
    };
    let mut plane = fleet_ctl::server::ControlPlane::boot(link, log).expect("boot");
    let listener = TcpListener::bind(&listen).expect("bind");
    eprintln!("fleet-ctl: serving on {listen}");
    plane.serve(listener).expect("serve");
}

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
