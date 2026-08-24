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
/// One versioned snapshot generation: the cluster's decisions plus the
/// controller-side state that outlives restarts.
#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotEnvelope {
    version: u32,
    cluster: archon_kernel::Cluster,
    state: archon_node::service::ServiceState,
}

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
    /// Commands appended since the last compaction; shared with the sink
    /// closure that persists them.
    commands_since_compaction: std::sync::Arc<std::sync::atomic::AtomicU64>,
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
        // replays only what happened since. One envelope file holds both
        // parts, published atomically, so a crash mid-compaction leaves
        // either the old complete generation or the new one.
        let snapshot_path = Self::snapshot_path(&log_path);
        let restored = snapshot_path.exists();
        if restored {
            let envelope: SnapshotEnvelope =
                serde_json::from_reader(std::fs::File::open(&snapshot_path)?)?;
            service.restore(envelope.cluster, envelope.state);
        }

        let self_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let commands = CommandLog::read(&log_path)?;
        let first_boot = !restored && commands.is_empty();

        // The link registers at most once per boot: a remote agent serves
        // one controller connection, so registering twice would deadlock
        // the second handshake behind the still-open first.
        let mut registered = false;
        if first_boot && !matches!(link, AgentLink::None) {
            // The graph of record enters the log on first boot so restarts
            // replay it instead of rediscovering.
            service.set_command_sink(Some(Self::make_sink(&log_path, self_counter.clone())));
            match &link {
                AgentLink::Local { cgroup_root } => {
                    service.register_local(cgroup_root.clone())?;
                }
                AgentLink::Remote { addr } => {
                    service.register_remote(addr)?;
                }
                AgentLink::None => {}
            }
            registered = true;
            service.set_command_sink(None);
        }

        let replayed = commands.len();
        service.replay(commands)?;

        // Recovery policy: live leases do not survive a controller restart.
        // A fresh controller holds no processes, so work is revoked, never
        // silently re-executed.
        service.set_command_sink(Some(Self::make_sink(&log_path, self_counter.clone())));
        let recovered = service.revoke_live_leases()?;

        // Register this process's own execution path (local dev or the
        // legacy connect-out agent) unless first boot already did; dial-in
        // agents arrive via serve().
        if !registered && !matches!(link, AgentLink::None) {
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
            commands_since_compaction: self_counter,
            compact_every,
            next_request,
        })
    }

    fn snapshot_path(log_path: &std::path::Path) -> PathBuf {
        let mut path = log_path.as_os_str().to_owned();
        path.push(".snapshot");
        PathBuf::from(path)
    }

    fn make_sink(
        log_path: &std::path::Path,
        counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Box<dyn FnMut(&Command) + Send> {
        // One handle for the sink's lifetime: append-mode writes still land
        // at end-of-file after compaction truncates the file, so the
        // per-command flush keeps its crash-safety without reopening per
        // applied command.
        let mut log = match CommandLog::open(log_path) {
            Ok(log) => log,
            Err(err) => {
                eprintln!(
                    "archon: cannot open command log {}: {err}",
                    log_path.display()
                );
                std::process::exit(2);
            }
        };
        Box::new(move |command: &Command| {
            if let Err(err) = log.append(command) {
                eprintln!("archon: cannot append command log: {err}");
                std::process::exit(2);
            }
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    }

    /// Write one versioned snapshot envelope (cluster + controller state),
    /// then truncate the command log. Restarts load the snapshot and replay
    /// only what came after it.
    pub fn compact(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let envelope = SnapshotEnvelope {
            version: 1,
            cluster: self.service.cluster.clone(),
            state: self.service.state_snapshot(),
        };
        let path = Self::snapshot_path(&self.log_path);
        let tmp = path.with_extension("snapshot.tmp");
        serde_json::to_writer(std::fs::File::create(&tmp)?, &envelope)?;
        let file = std::fs::File::open(&tmp)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        if let Some(dir) = path.parent()
            && let Ok(dir) = std::fs::File::open(dir)
        {
            let _ = dir.sync_all();
        }
        std::fs::write(&self.log_path, b"")?;
        self.commands_since_compaction
            .store(0, std::sync::atomic::Ordering::Relaxed);
        eprintln!("archon: compacted command log into snapshot");
        Ok(())
    }

    /// Require a shared token on every connection.
    pub fn require_token(&mut self, token: String) {
        self.token = Some(token);
    }

    /// Periodic maintenance: expire due leases, collect workload exits,
    /// re-admit queued work, quarantine machines whose last probe failed,
    /// issue fresh probes, restart keep-alive workloads, and compact the
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
        // Quarantine on the previous round's probe results, then issue the
        // next round; answers arrive asynchronously.
        for machine in self.service.take_unreachable() {
            if self.service.mark_machine_unhealthy(machine).is_err() {
                eprintln!("archon: failed to mark machine {machine} unhealthy");
            }
        }
        self.service.probe_agents();
        for (request, owner) in self.service.take_restarts() {
            eprintln!("archon: restarting keep-alive request {}", request.id);
            self.service.submit(request, owner);
            match self.service.admit_one() {
                Ok(Some(id)) => eprintln!("archon: restarted as request {id}"),
                Ok(None) => {} // queued until capacity returns
                Err(err) => eprintln!("archon: restart admission failed: {err}"),
            }
        }
        let since = self
            .commands_since_compaction
            .load(std::sync::atomic::Ordering::Relaxed);
        if self.compact_every > 0
            && since >= self.compact_every
            && let Err(err) = self.compact()
        {
            eprintln!("archon: compaction failed: {err}");
        }
    }

    /// Accept clients and dial-in agents. The first frame on a connection
    /// decides its role: `Hello` registers an agent (its machine joins the
    /// graph, and re-registration with the same machine name reconciles);
    /// anything else is a client. A driver thread absorbs agent completions
    /// as they arrive, so no client handler or registration ever waits on
    /// network I/O while holding the plane lock.
    pub fn serve(this: &std::sync::Arc<std::sync::Mutex<Self>>, listener: TcpListener) {
        let inbox = this.lock().unwrap().service.inbox_handle();
        {
            let this = this.clone();
            std::thread::spawn(move || {
                loop {
                    inbox.wait_timeout(std::time::Duration::from_millis(500));
                    if let Ok(mut plane) = this.lock() {
                        plane.advance();
                    }
                }
            });
        }
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let plane = this.clone();
            std::thread::spawn(move || Self::accept(&plane, stream));
        }
    }

    /// Absorb completed agent work without waiting: completions already in
    /// the inbox apply under the lock; round trips happen on workers.
    pub fn advance(&mut self) {
        if let Err(err) = self.service.drive(std::time::Duration::ZERO) {
            eprintln!("archon: advancing agent work failed: {err}");
        }
    }

    /// True when no agent round trip or effect delivery is outstanding.
    pub fn settled(&self) -> bool {
        self.service.is_quiescent()
    }

    fn accept(this: &std::sync::Arc<std::sync::Mutex<Self>>, mut stream: TcpStream) {
        crate::api::set_stream_limits(&stream);
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
            ClientRequest::Logs { lease } => {
                // Resolve the agent call under the lock, then wait for the
                // reply outside it so a slow agent cannot stall the plane.
                let reply = self.service.request_logs(LeaseId::from_u64(lease));
                match reply {
                    Some(receiver) => {
                        match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
                            Ok(Ok(archon_node::protocol::AgentResponse::Logs {
                                output, ..
                            })) => ServerResponse::Logs { lease, output },
                            Ok(Ok(other)) => ServerResponse::Error {
                                reason: format!("expected Logs, got {other:?}"),
                            },
                            Ok(Err(reason)) => ServerResponse::Error { reason },
                            Err(_) => ServerResponse::Error {
                                reason: "timed out waiting for agent logs".into(),
                            },
                        }
                    }
                    None => match self.service.lease_logs(LeaseId::from_u64(lease)) {
                        Ok(output) => ServerResponse::Logs { lease, output },
                        Err(err) => ServerResponse::Error {
                            reason: err.to_string(),
                        },
                    },
                }
            }
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
            let bytes = memory_mib.checked_mul(1 << 20);
            let Some(bytes) = bytes else {
                return ServerResponse::Error {
                    reason: "memory_mib overflows".into(),
                };
            };
            needs.push(Need {
                kind: NodeKind::Memory,
                quantity: qty(Dimension::Bytes, bytes),
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
            ports: match ports
                .iter()
                .map(|spec| match spec.split_once(':') {
                    Some((host, container)) => {
                        let container_port: u16 = container.parse().map_err(|_| {
                            "invalid port spec (expected [host:]container)".to_string()
                        })?;
                        let host_port: u16 = host.parse().map_err(|_| {
                            "invalid port spec (expected [host:]container)".to_string()
                        })?;
                        Ok(archon_kernel::PortPublish {
                            container_port,
                            host_port: Some(host_port),
                        })
                    }
                    None => spec
                        .parse::<u16>()
                        .map(|container_port| archon_kernel::PortPublish {
                            container_port,
                            host_port: None,
                        })
                        .map_err(|_| "invalid port spec (expected [host:]container)".to_string()),
                })
                .collect::<Result<Vec<_>, String>>()
            {
                Ok(ports) => ports,
                Err(reason) => {
                    return ServerResponse::Error { reason };
                }
            },
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
                exit_code: lease.exit_code,
                command: self.service.lease_command_of(lease.id).unwrap_or_default(),
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
