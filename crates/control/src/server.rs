//! The control plane server: one [`NodeService`], one command log, clients
//! over TCP. Expiry is checked as requests arrive; recovery replays the log
//! and expires work that was live at shutdown.

use std::net::TcpListener;
use std::path::PathBuf;

use fleet_kernel::{
    Command, Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use fleet_node::discover;
use fleet_node::service::NodeService;

/// How the control plane reaches its execution agent.
pub enum AgentLink {
    /// Execute on this machine; cgroup root enables kernel enforcement.
    Local { cgroup_root: Option<String> },
    /// Execute on a remote `fleet-node serve` agent.
    Remote { addr: String },
}

use crate::api::{ClientRequest, LeaseInfo, ServerResponse};
use crate::log::CommandLog;

pub struct ControlPlane {
    service: NodeService,
    _log_path: PathBuf,
    next_request: u64,
}

impl ControlPlane {
    /// Boot from a log. Replay restores state first — on a restart the
    /// graph of record comes from the log, not from rediscovery. Only a
    /// first boot (empty log) discovers the machine. Live leases are then
    /// revoked: a fresh agent holds no processes, so recovery restores
    /// decisions, it never re-executes work.
    pub fn boot(link: AgentLink, log_path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let mut service = match &link {
            AgentLink::Local { cgroup_root } => NodeService::local(cgroup_root.clone()),
            AgentLink::Remote { addr } => NodeService::connect(addr)?,
        };
        let commands = CommandLog::read(&log_path)?;
        let first_boot = commands.is_empty();
        fn make_sink(log_path: &std::path::Path) -> Box<dyn FnMut(&Command) + Send> {
            let log_path = log_path.to_path_buf();
            Box::new(move |command: &Command| {
                let mut log = CommandLog::open(&log_path).expect("open command log");
                log.append(command).expect("append command log");
            })
        }
        if first_boot && matches!(link, AgentLink::Local { .. }) {
            // The graph of record enters the log on first boot so restarts
            // replay it instead of rediscovering.
            service.set_command_sink(Some(make_sink(&log_path)));
            let (_local, nodes, edges) = discover::discover();
            service.boot(nodes, edges)?;
            service.set_command_sink(None);
        }
        let replayed = commands.len();
        service.replay(commands)?;
        service.set_command_sink(Some(make_sink(&log_path)));
        let recovered = service.revoke_live_leases()?;
        let next_request = service
            .cluster
            .leases
            .keys()
            .max()
            .map(|id| id.as_u64() + 1)
            .unwrap_or(1);
        eprintln!(
            "fleet: {}boot, recovered {replayed} commands, revoked {recovered} live leases",
            if first_boot { "first " } else { "" }
        );
        Ok(Self {
            service,
            _log_path: log_path,
            next_request,
        })
    }

    /// Serve clients sequentially; a new connection replaces a dead one.
    pub fn serve(&mut self, listener: TcpListener) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let peer = stream
                .peer_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_default();
            eprintln!("fleet: client connected from {peer}");
            while let Ok(request) = crate::api::read_request(&mut stream) {
                self.service.tick().ok();
                let response = self.handle(request);
                if crate::api::write_response(&mut stream, &response).is_err() {
                    break;
                }
            }
            eprintln!("fleet: client {peer} disconnected");
        }
        Ok(())
    }

    fn handle(&mut self, request: ClientRequest) -> ServerResponse {
        match request {
            ClientRequest::Submit {
                owner,
                cpus,
                memory_mib,
                lifetime_secs,
                command,
            } => self.submit(owner, cpus, memory_mib, lifetime_secs, command),
            ClientRequest::Status => self.status(),
            ClientRequest::Revoke { lease } => self.revoke(lease),
        }
    }

    fn submit(
        &mut self,
        owner: u64,
        cpus: u64,
        memory_mib: u64,
        lifetime_secs: u64,
        command: Vec<String>,
    ) -> ServerResponse {
        if command.is_empty() {
            return ServerResponse::Error {
                reason: "empty command".into(),
            };
        }
        let id = RequestId::from_u64(self.next_request);
        self.next_request += 1;
        let mut needs = vec![Need {
            kind: NodeKind::Cpu,
            quantity: qty(Dimension::Count, cpus.max(1)),
            filters: vec![],
        }];
        if memory_mib > 0 {
            needs.push(Need {
                kind: NodeKind::Memory,
                quantity: qty(Dimension::Bytes, memory_mib * (1 << 20)),
                filters: vec![],
            });
        }
        let request = Request {
            id,
            class: RequestClass::Batch,
            needs,
            topology: vec![],
            preferences: vec![],
            data: vec![],
            command: command.clone(),
            lifetime: lifetime_secs.max(1),
            priority: 1,
        };
        self.service
            .submit(request, OwnerId::from_u64(owner), command);
        match self.service.admit_one() {
            Ok(Some(_)) => ServerResponse::Submitted {
                request: id.as_u64(),
                lease: self.lease_of_request(),
            },
            Ok(None) => ServerResponse::Submitted {
                request: id.as_u64(),
                lease: 0,
            },
            Err(err) => ServerResponse::Error {
                reason: err.to_string(),
            },
        }
    }

    fn lease_of_request(&self) -> u64 {
        // v0 admits one request per submit; the newest lease is its lease.
        self.service
            .cluster
            .leases
            .keys()
            .max()
            .map(|id| id.as_u64())
            .unwrap_or(0)
    }

    fn status(&self) -> ServerResponse {
        let leases = self
            .service
            .cluster
            .leases
            .values()
            .map(|lease| LeaseInfo {
                id: lease.id.as_u64(),
                owner: lease.owner.as_u64(),
                state: format!("{:?}", lease.state),
                expires_at: lease.expires_at,
            })
            .collect();
        ServerResponse::Status {
            queue_len: self.service.queue_len(),
            leases,
        }
    }

    fn revoke(&mut self, lease: u64) -> ServerResponse {
        match self.service.revoke(LeaseId::from_u64(lease)) {
            Ok(()) => ServerResponse::Revoked,
            Err(err) => ServerResponse::Error {
                reason: err.to_string(),
            },
        }
    }
}
