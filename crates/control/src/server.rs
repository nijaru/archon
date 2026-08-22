//! The control plane server: one [`NodeService`], one command log, clients
//! over TCP. Expiry is checked as requests arrive; recovery replays the log
//! and expires work that was live at shutdown.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

use archon_kernel::{
    Command, Dimension, LeaseId, Need, NodeKind, OwnerId, Request, RequestClass, RequestId, qty,
};
use archon_node::service::NodeService;

/// A parsed client submission.
struct SubmitSpec {
    owner: u64,
    cpus: u64,
    memory_mib: u64,
    lifetime_secs: u64,
    command: Vec<String>,
    keep_alive: bool,
    volumes: Vec<String>,
    ports: Vec<String>,
    grace_secs: u32,
    image: Option<String>,
    gpus: u64,
}

/// How the control plane reaches its execution agents.
pub enum AgentLink {
    /// Execute on this machine; cgroup root enables kernel enforcement.
    Local { cgroup_root: Option<String> },
    /// Connect out to one remote `archon agent` daemon at boot.
    Remote { addr: String },
    /// Pure control plane: no built-in machine; agents dial in.
    None,
}

use crate::api::{ClientRequest, LeaseInfo, ServerResponse};
use crate::log::CommandLog;

pub struct ControlPlane {
    service: NodeService,
    log_path: PathBuf,
    /// Commands appended since the last compaction.
    commands_since_compaction: u64,
    /// Compact when the log exceeds this many commands (0 = never).
    compact_every: u64,
    next_request: u64,
    /// When set, every connection must present this token in its Greeting.
    token: Option<String>,
}

impl ControlPlane {
    /// Boot from a log. Replay restores state first — on a restart the
    /// graph of record comes from the log, not from rediscovery. Only a
    /// first boot (empty log) discovers the machine. Live leases are then
    /// revoked: a fresh agent holds no processes, so recovery restores
    /// decisions, it never re-executes work.
    pub fn boot(
        link: AgentLink,
        log_path: PathBuf,
        compact_every: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut service = NodeService::new();

        // A snapshot restores everything at compaction time; the log then
        // replays only what happened since.
        let snapshot_path = Self::snapshot_path(&log_path);
        let state_path = Self::state_path(&log_path);
        let restored = snapshot_path.exists() && state_path.exists();
        if restored {
            let cluster: archon_kernel::Cluster =
                serde_json::from_reader(std::fs::File::open(&snapshot_path)?)?;
            let state: archon_node::service::ServiceState =
                serde_json::from_reader(std::fs::File::open(&state_path)?)?;
            service.restore(cluster, state);
        }

        let commands = CommandLog::read(&log_path)?;
        let first_boot = !restored && commands.is_empty();

        if first_boot && !matches!(link, AgentLink::None) {
            // The graph of record enters the log on first boot so restarts
            // replay it instead of rediscovering.
            service.set_command_sink(Some(Self::make_sink(&log_path)));
            match &link {
                AgentLink::Local { cgroup_root } => {
                    service.register_local(cgroup_root.clone())?;
                }
                AgentLink::Remote { addr } => {
                    service.register_remote(addr)?;
                }
                AgentLink::None => {}
            }
            service.set_command_sink(None);
        }

        let replayed = commands.len();
        service.replay(commands)?;

        // Recovery policy: live leases do not survive a controller restart.
        // A fresh controller holds no processes, so work is revoked, never
        // silently re-executed.
        service.set_command_sink(Some(Self::make_sink(&log_path)));
        let recovered = service.revoke_live_leases()?;

        // Register this process's own execution path (local dev or the
        // legacy connect-out agent); dial-in agents arrive via serve().
        if !matches!(link, AgentLink::None) {
            match &link {
                AgentLink::Local { cgroup_root } => {
                    service.register_local(cgroup_root.clone())?;
                }
                AgentLink::Remote { addr } => {
                    service.register_remote(addr)?;
                }
                AgentLink::None => {}
            }
        }

        let next_request = service
            .cluster
            .leases
            .keys()
            .map(|id| id.as_u64() + 1)
            .max()
            .unwrap_or(1);
        eprintln!(
            "archon: {}boot, replayed {replayed} commands, revoked {recovered} live leases",
            if restored {
                "snapshot "
            } else if first_boot {
                "first "
            } else {
                ""
            }
        );
        Ok(Self {
            service,
            token: None,
            log_path,
            commands_since_compaction: 0,
            compact_every,
            next_request,
        })
    }

    fn snapshot_path(log_path: &std::path::Path) -> PathBuf {
        let mut path = log_path.as_os_str().to_owned();
        path.push(".snapshot");
        PathBuf::from(path)
    }

    fn state_path(log_path: &std::path::Path) -> PathBuf {
        let mut path = log_path.as_os_str().to_owned();
        path.push(".state");
        PathBuf::from(path)
    }

    fn make_sink(log_path: &std::path::Path) -> Box<dyn FnMut(&Command) + Send> {
        let log_path = log_path.to_path_buf();
        Box::new(move |command: &Command| {
            let mut log = CommandLog::open(&log_path).expect("open command log");
            log.append(command).expect("append command log");
        })
    }

    /// Write a snapshot of the cluster plus controller state, then truncate
    /// the command log. Restarts load the snapshot and replay only what
    /// came after it.
    pub fn compact(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        fn write_atomic<T: serde::Serialize>(
            path: &std::path::Path,
            value: &T,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let tmp = path.with_extension("tmp");
            serde_json::to_writer(std::fs::File::create(&tmp)?, value)?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        }
        write_atomic(&Self::snapshot_path(&self.log_path), &self.service.cluster)?;
        write_atomic(
            &Self::state_path(&self.log_path),
            &self.service.state_snapshot(),
        )?;
        std::fs::write(&self.log_path, b"")?;
        self.commands_since_compaction = 0;
        eprintln!("archon: compacted command log into snapshot");
        Ok(())
    }

    /// Require a shared token on every connection.
    pub fn require_token(&mut self, token: String) {
        self.token = Some(token);
    }

    /// Periodic maintenance: expire due leases, probe agents, quarantine
    /// unreachable machines, restart keep-alive workloads, and compact the
    /// log when it grows past the threshold.
    pub fn maintain(&mut self) {
        self.service.tick().ok();
        if let Err(err) = self.service.collect_completions() {
            eprintln!("archon: completion collection failed: {err}");
        }
        // Freed capacity re-opens admission: drain the queue until nothing
        // more places.
        loop {
            match self.service.admit_one() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(err) => {
                    eprintln!("archon: admission failed: {err}");
                    break;
                }
            }
        }
        let unreachable = self.service.probe_agents();
        for machine in unreachable {
            if self.service.mark_machine_unhealthy(machine).is_err() {
                eprintln!("archon: failed to mark machine {machine} unhealthy");
            }
        }
        for (request, owner) in self.service.take_restarts() {
            eprintln!("archon: restarting keep-alive request {}", request.id);
            self.service.submit(request, owner);
            match self.service.admit_one() {
                Ok(Some(id)) => eprintln!("archon: restarted as request {id}"),
                Ok(None) => {} // queued until capacity returns
                Err(err) => eprintln!("archon: restart admission failed: {err}"),
            }
        }
        self.commands_since_compaction = self.service.cluster.log.len() as u64;
        if self.compact_every > 0
            && self.commands_since_compaction >= self.compact_every
            && let Err(err) = self.compact()
        {
            eprintln!("archon: compaction failed: {err}");
        }
    }

    /// Accept clients and dial-in agents. The first frame on a connection
    /// decides its role: `Hello` registers an agent (its machine joins the
    /// graph, and re-registration with the same machine name reconciles);
    /// anything else is a client. Agents are then driven lockstep by
    /// whichever thread routes their effects.
    pub fn serve(this: &std::sync::Arc<std::sync::Mutex<Self>>, listener: TcpListener) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let plane = this.clone();
            std::thread::spawn(move || Self::accept(&plane, stream));
        }
    }

    fn accept(this: &std::sync::Arc<std::sync::Mutex<Self>>, mut stream: TcpStream) {
        let peer = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_default();
        // The first frame is always a Greeting: role declaration plus token
        // when the plane requires one. A bad token ends the connection
        // before any other work happens.
        let greeting = match crate::api::read_payload(&mut stream)
            .ok()
            .and_then(|payload| serde_json::from_slice::<crate::api::Greeting>(&payload).ok())
        {
            Some(greeting) => greeting,
            None => {
                eprintln!("archon: {peer} sent no valid greeting");
                return;
            }
        };
        let authorized = {
            let plane = this.lock().unwrap();
            match &plane.token {
                Some(expected) => greeting
                    .token()
                    .is_some_and(|presented| crate::api::token_matches(expected, presented)),
                None => true,
            }
        };
        if !authorized {
            eprintln!("archon: {peer} failed authentication");
            return;
        }
        match greeting {
            crate::api::Greeting::Agent {
                instance_id,
                name,
                cpus,
                memory_bytes,
                devices,
                ..
            } => {
                if let Err(err) = this.lock().unwrap().register_dial_in(
                    stream,
                    instance_id,
                    name,
                    cpus,
                    memory_bytes,
                    devices,
                ) {
                    eprintln!("archon: agent {peer} registration failed: {err}");
                } else {
                    eprintln!("archon: agent {peer} disconnected");
                }
            }
            crate::api::Greeting::Client { .. } => {
                eprintln!("archon: client connected from {peer}");
                while let Ok(request) = crate::api::read_request(&mut stream) {
                    let response = this.lock().unwrap().tick_and_handle(request);
                    if crate::api::write_response(&mut stream, &response).is_err() {
                        break;
                    }
                }
                eprintln!("archon: client {peer} disconnected");
            }
        }
    }

    fn register_dial_in(
        &mut self,
        stream: TcpStream,
        instance_id: String,
        name: String,
        cpus: u64,
        memory_bytes: u64,
        devices: Vec<(archon_kernel::NodeKind, String)>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let description = archon_node::discover::MachineDescription {
            instance_id,
            name,
            cpus,
            memory_bytes,
            devices,
        };
        let executor = archon_node::service::RemoteExecutor::from_stream(stream);
        let machine = self
            .service
            .register_agent(description, Box::new(executor))?;
        let name = self
            .service
            .cluster
            .graph
            .node(machine)
            .and_then(|node| node.attrs.get("name").cloned())
            .unwrap_or_default();
        eprintln!("archon: agent registered as machine {machine} ({name})");
        Ok(())
    }

    fn tick_and_handle(
        &mut self,
        request: crate::api::ClientRequest,
    ) -> crate::api::ServerResponse {
        self.service.tick().ok();
        self.handle(request)
    }

    pub fn handle(&mut self, request: ClientRequest) -> ServerResponse {
        match request {
            ClientRequest::Submit {
                owner,
                cpus,
                memory_mib,
                lifetime_secs,
                command,
                keep_alive,
                volumes,
                ports,
                grace_secs,
                image,
                gpus,
            } => self.submit(SubmitSpec {
                owner,
                cpus,
                memory_mib,
                lifetime_secs,
                command,
                keep_alive,
                volumes,
                ports,
                grace_secs,
                image,
                gpus,
            }),
            ClientRequest::Status => self.status(),
            ClientRequest::Revoke { lease } => self.revoke(lease),
        }
    }

    fn submit(&mut self, spec: SubmitSpec) -> ServerResponse {
        let SubmitSpec {
            owner,
            cpus,
            memory_mib,
            lifetime_secs,
            command,
            keep_alive,
            volumes,
            ports,
            grace_secs,
            image,
            gpus,
        } = spec;
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
        if gpus > 0 {
            needs.push(Need {
                kind: NodeKind::Gpu,
                quantity: qty(Dimension::Count, gpus),
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
            keep_alive,
            priority: 1,
            machine_local: true,
            grace_secs,
            image,
            storage: volumes
                .iter()
                .filter_map(|spec| spec.split_once(':'))
                .map(|(host_path, mount_path)| archon_kernel::StorageMount {
                    host_path: host_path.into(),
                    mount_path: mount_path.into(),
                })
                .collect(),
            ports: ports
                .iter()
                .map(|spec| match spec.split_once(':') {
                    Some((host, container)) => archon_kernel::PortPublish {
                        container_port: container.parse().expect("port"),
                        host_port: Some(host.parse().expect("host port")),
                    },
                    None => archon_kernel::PortPublish {
                        container_port: spec.parse().expect("port"),
                        host_port: None,
                    },
                })
                .collect(),
        };
        self.service.submit(request, OwnerId::from_u64(owner));
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
