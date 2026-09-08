//! The control plane server: one [`NodeService`], one command log, clients
//! over TCP. Expiry is checked as requests arrive; recovery replays the log
//! and expires work that was live at shutdown.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

use archon_kernel::{
    CapacityDimension, Command, LeaseId, Need, OwnerId, Request, RequestClass, RequestId,
    ResourceClass, qty,
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

/// Group fields for SubmitService: identity, owner, and cardinality policy
/// alongside the shared workload wire fields.
struct ServiceSpec {
    id: String,
    owner: u64,
    desired: u32,
    max: Option<u32>,
    min: Option<u32>,
    per_machine: bool,
    wire: WorkloadWire,
}

/// Resource/execution wire fields shared by single submits and service-group
/// member templates.
struct WorkloadWire {
    cpus: u64,
    memory_mib: u64,
    lifetime_secs: u64,
    command: Vec<String>,
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
    /// Connect out to one remote `archon agent` daemon at boot; the token
    /// secures that link the same way `require_token` secures inbound ones.
    Remote { addr: String, token: Option<String> },
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
            if envelope.version != 1 {
                return Err(format!("unsupported snapshot version {}", envelope.version).into());
            }
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
                AgentLink::Remote { addr, token } => {
                    if let Some(token) = token {
                        service.set_link_token(token.clone());
                    }
                    service.register_remote(addr)?;
                }
                AgentLink::None => {}
            }
            registered = true;
            service.set_command_sink(None);
        }

        let replayed = commands.len();
        service.replay(commands)?;

        // Recovery policy: replay restores authoritative state, but ownership
        // of live work is not assumed. Each agent re-registers with a fresh
        // monotonic session; registration queries the machine's actual
        // endpoint state and adopts only provably-current bindings, fencing
        // or revoking everything else before its claims can be reused.
        service.set_command_sink(Some(Self::make_sink(&log_path, self_counter.clone())));
        let live = service.live_lease_count();

        // Register this process's own execution path (local dev or the
        // legacy connect-out agent) unless first boot already did; dial-in
        // agents arrive via serve().
        if !registered && !matches!(link, AgentLink::None) {
            match &link {
                AgentLink::Local { cgroup_root } => {
                    service.register_local(cgroup_root.clone())?;
                }
                AgentLink::Remote { addr, .. } => {
                    service.register_remote(addr)?;
                }
                AgentLink::None => {}
            }
        }

        // Request ids must clear every id the restored state already
        // holds (leases, queue, and service-group members), not just the
        // leased ones: reusing a live queued id would admit one request
        // twice under two leases.
        let next_request = service
            .cluster
            .leases
            .keys()
            .map(|id| id.as_u64() + 1)
            .max()
            .unwrap_or(1)
            .max(service.next_request_id());
        eprintln!(
            "archon: {}boot, replayed {replayed} commands, {live} live leases await agent reconciliation",
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
        self.service.clear_history();
        self.commands_since_compaction
            .store(0, std::sync::atomic::Ordering::Relaxed);
        eprintln!("archon: compacted command log into snapshot");
        Ok(())
    }

    /// Young deployments have no snapshot yet: persist desired state on
    /// the first mutation so any later restart recovers the queue and
    /// service groups from the snapshot instead of only the kernel log.
    /// Submits are infrequent, so the synchronous write costs nothing;
    /// a failure degrades to today's behavior and never fails the submit.
    fn ensure_snapshot(&mut self) {
        if self.compact_every > 0
            && !Self::snapshot_path(&self.log_path).exists()
            && let Err(err) = self.compact()
        {
            eprintln!("archon: initial snapshot failed: {err}");
        }
    }

    /// Require token-authenticated links: connections complete a Noise
    /// XXpsk3 handshake whose PSK derives from this token. Without it, links
    /// stay encrypted but unauthenticated (open mode). Also secures the
    /// controller's own outbound agent connections.
    pub fn require_token(&mut self, token: String) {
        self.service.set_link_token(token.clone());
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
        // Service groups are the desired-state owner for their members:
        // reconcile after per-member restarts so groups replace dead or
        // missing members up to their desired count.
        for member in self.service.reconcile_service_groups() {
            match self.service.admit_one() {
                Ok(Some(admitted)) => {
                    eprintln!("archon: service member admitted as request {admitted}")
                }
                Ok(None) => {} // queued until capacity returns
                Err(err) => eprintln!("archon: service member admission failed: {err}"),
            }
            let _ = member;
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

    fn accept(this: &std::sync::Arc<std::sync::Mutex<Self>>, stream: TcpStream) {
        crate::api::set_stream_limits(&stream);
        let peer = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_default();
        // Every link runs a Noise XXpsk3 handshake first: the shared token
        // is the PSK, proven on both sides without ever crossing the wire.
        // A wrong token fails the handshake before any frame is read.
        let token = Self::lock_plane(this).token.clone();
        let mut stream = match archon_node::transport::establish_responder(stream, token.as_deref())
        {
            Ok(stream) => stream,
            Err(_) => {
                eprintln!("archon: {peer} failed the transport handshake");
                return;
            }
        };
        // The first framed message is always a Greeting declaring the role.
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
        match greeting {
            crate::api::Greeting::Agent {
                instance_id,
                name,
                cpus,
                memory_bytes,
                host_nodes,
                devices,
            } => {
                let description = archon_node::discover::MachineDescription {
                    instance_id,
                    name,
                    cpus,
                    memory_bytes,
                    host_nodes,
                    devices,
                };
                let mut executor = archon_node::service::RemoteAgentClient::from_secure(stream);
                let capabilities =
                    match archon_node::service::NodeService::query_execution_capabilities(
                        &mut executor,
                    ) {
                        Ok(capabilities) => capabilities,
                        Err(err) => {
                            eprintln!("archon: agent {peer} capability proof failed: {err}");
                            return;
                        }
                    };
                if let Err(err) =
                    Self::lock_plane(this).register_dial_in(executor, description, capabilities)
                {
                    eprintln!("archon: agent {peer} registration failed: {err}");
                } else {
                    eprintln!(
                        "archon: agent {peer} registered; connection handed to the agent worker"
                    );
                }
            }
            crate::api::Greeting::Client => {
                eprintln!("archon: client connected from {peer}");
                while let Ok(request) = crate::api::read_request(&mut stream) {
                    // A Logs request waits on an agent round trip: resolve
                    // it without holding the plane lock, so a slow or dead
                    // agent cannot stall every other client, registration,
                    // or the driver thread.
                    let response = if let ClientRequest::Logs { lease } = request {
                        Self::logs_without_lock(this, lease)
                    } else {
                        Self::lock_plane(this).tick_and_handle(request)
                    };
                    if crate::api::write_response(&mut stream, &response).is_err() {
                        break;
                    }
                }
                eprintln!("archon: client {peer} disconnected");
            }
        }
    }

    /// Resolve one Logs request off-lock: grab the agent reply channel
    /// under the lock, wait for the agent without it, and only re-lock
    /// for the local-file fallback when no agent holds the lease.
    fn logs_without_lock(
        plane: &std::sync::Arc<std::sync::Mutex<Self>>,
        lease: u64,
    ) -> ServerResponse {
        let receiver = Self::lock_plane(plane)
            .service
            .request_logs(LeaseId::from_u64(lease));
        match receiver {
            Some(receiver) => Self::logs_reply_to_response(
                lease,
                receiver.recv_timeout(std::time::Duration::from_secs(10)),
            ),
            None => Self::lock_plane(plane).handle(ClientRequest::Logs { lease }),
        }
    }

    /// Map one agent Logs round trip onto the client response. Shared by
    /// the off-lock path and the in-lock handler so the two cannot drift.
    fn logs_reply_to_response(
        lease: u64,
        reply: Result<
            Result<archon_node::protocol::AgentResponse, String>,
            std::sync::mpsc::RecvTimeoutError,
        >,
    ) -> ServerResponse {
        match reply {
            Ok(Ok(archon_node::protocol::AgentResponse::Logs { output, .. })) => {
                ServerResponse::Logs { lease, output }
            }
            Ok(Ok(other)) => ServerResponse::Error {
                reason: format!("expected Logs, got {other:?}"),
            },
            Ok(Err(reason)) => ServerResponse::Error { reason },
            Err(_) => ServerResponse::Error {
                reason: "timed out waiting for agent logs".into(),
            },
        }
    }

    /// Lock the plane for connection handling. A poisoned mutex means a
    /// previous holder panicked mid-mutation, so authority state may be
    /// half-applied: fail-stop rather than serve from it.
    fn lock_plane(
        this: &std::sync::Arc<std::sync::Mutex<Self>>,
    ) -> std::sync::MutexGuard<'_, Self> {
        match this.lock() {
            Ok(guard) => guard,
            Err(_) => {
                eprintln!("archon: control-plane lock poisoned; fail-stop");
                std::process::exit(1);
            }
        }
    }

    fn register_dial_in(
        &mut self,
        executor: archon_node::service::RemoteAgentClient,
        description: archon_node::discover::MachineDescription,
        capabilities: archon_node::protocol::ExecutionCapabilities,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine = self.service.register_agent_with_capabilities(
            description,
            Box::new(executor),
            capabilities,
        )?;
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
        // Drain completed agent answers (recovery proofs, activations) so
        // single-threaded callers observe settled state without serve().
        let _ = self.service.drive(std::time::Duration::ZERO);
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
            ClientRequest::SubmitService {
                id,
                owner,
                desired,
                max,
                min,
                per_machine,
                cpus,
                memory_mib,
                lifetime_secs,
                command,
                volumes,
                ports,
                grace_secs,
                image,
            } => self.submit_service(ServiceSpec {
                id,
                owner,
                desired,
                max,
                min,
                per_machine,
                wire: WorkloadWire {
                    cpus,
                    memory_mib,
                    lifetime_secs,
                    command,
                    volumes,
                    ports,
                    grace_secs,
                    image,
                    gpus: 0,
                },
            }),
            ClientRequest::ScaleService { id, target } => self.scale_service(id, target),
            ClientRequest::Status => self.status(),
            ClientRequest::Revoke { lease } => self.revoke(lease),
            ClientRequest::Logs { lease } => {
                // Resolve the agent call under the lock, then wait for the
                // reply outside it so a slow agent cannot stall the plane.
                // (The connection handler keeps the lock-free path in
                // `logs_without_lock`; this arm covers direct callers.)
                let reply = self.service.request_logs(LeaseId::from_u64(lease));
                match reply {
                    Some(receiver) => Self::logs_reply_to_response(
                        lease,
                        receiver.recv_timeout(std::time::Duration::from_secs(10)),
                    ),
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
        let mut request = match self.compile_workload(
            id,
            WorkloadWire {
                cpus,
                memory_mib,
                lifetime_secs,
                command,
                volumes,
                ports,
                grace_secs,
                image,
                gpus,
            },
        ) {
            Ok(workload) => workload,
            Err(reason) => return ServerResponse::Error { reason },
        };
        request.keep_alive = keep_alive;
        self.service.submit(request, OwnerId::from_u64(owner));
        self.ensure_snapshot();
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

    /// Build a member WorkloadSpec from wire fields. Shared by single submits
    /// and service-group member templates.
    fn compile_workload(
        &self,
        id: archon_kernel::RequestId,
        wire: WorkloadWire,
    ) -> Result<archon_node::workload::WorkloadSpec, String> {
        let WorkloadWire {
            cpus,
            memory_mib,
            lifetime_secs,
            command,
            volumes,
            ports,
            grace_secs,
            image,
            gpus,
        } = wire;
        if command.is_empty() && image.is_none() {
            return Err("empty command".into());
        }
        let mut needs = vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, cpus.max(1)),
            filters: vec![],
        }];
        if memory_mib > 0 {
            let bytes = memory_mib
                .checked_mul(1 << 20)
                .ok_or_else(|| "memory_mib overflows".to_string())?;
            needs.push(Need {
                kind: ResourceClass::Memory,
                quantity: qty(CapacityDimension::Bytes, bytes),
                filters: vec![],
            });
        }
        if gpus > 0 {
            needs.push(Need {
                kind: ResourceClass::Gpu,
                quantity: qty(CapacityDimension::Count, gpus),
                filters: vec![],
            });
        }
        let execution = archon_node::workload::ExecutionSpec {
            command,
            image,
            storage: volumes
                .iter()
                .map(|spec| {
                    let (host_path, mount_path) = spec.split_once(':').ok_or_else(|| {
                        "invalid volume spec (expected host_path:mount_path)".to_string()
                    })?;
                    if host_path.is_empty() || mount_path.is_empty() {
                        return Err(
                            "invalid volume spec (expected host_path:mount_path)".to_string()
                        );
                    }
                    Ok(archon_node::workload::StorageMount {
                        host_path: host_path.into(),
                        mount_path: mount_path.into(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
            ports: ports
                .iter()
                .map(|spec| match spec.split_once(':') {
                    Some((host, container)) => {
                        let container_port: u16 = container.parse().map_err(|_| {
                            "invalid port spec (expected [host:]container)".to_string()
                        })?;
                        let host_port: u16 = host.parse().map_err(|_| {
                            "invalid port spec (expected [host:]container)".to_string()
                        })?;
                        Ok(archon_node::workload::PortPublish {
                            container_port,
                            host_port: Some(host_port),
                        })
                    }
                    None => spec
                        .parse::<u16>()
                        .map(|container_port| archon_node::workload::PortPublish {
                            container_port,
                            host_port: None,
                        })
                        .map_err(|_| "invalid port spec (expected [host:]container)".to_string()),
                })
                .collect::<Result<Vec<_>, String>>()?,
            grace_secs,
        };
        Ok(archon_node::workload::WorkloadSpec {
            resources: Request {
                id,
                class: RequestClass::Batch,
                needs,
                topology: vec![],
                preferences: vec![],
                data: vec![],
                lifetime: lifetime_secs.max(1),
                priority: 1,
                machine_local: true,
            },
            execution,
            keep_alive: false,
        })
    }

    /// Register a desired service group and immediately run one
    /// reconciliation round so the first members are submitted now rather
    /// than at the next maintenance tick. Later rounds run in `maintain`.
    #[allow(clippy::too_many_arguments)]
    fn submit_service(&mut self, spec: ServiceSpec) -> ServerResponse {
        let ServiceSpec {
            id,
            owner,
            desired,
            max,
            min,
            per_machine,
            wire,
        } = spec;
        let cardinality = if per_machine {
            archon_node::workload::Cardinality::PerMachine
        } else if let (Some(min), Some(max)) = (min, max) {
            archon_node::workload::Cardinality::Elastic {
                min: min as usize,
                target: desired as usize,
                max: max as usize,
            }
        } else {
            archon_node::workload::Cardinality::Fixed(desired as usize)
        };
        if let Err(reason) = cardinality.validate() {
            return ServerResponse::Error { reason };
        }
        // Reserve no member ids here: reconciliation compiles members with
        // the service's own fresh ids so replacement lineage stays coherent.
        let template = match self.compile_workload(archon_kernel::RequestId::from_u64(0), wire) {
            Ok(workload) => workload,
            Err(reason) => return ServerResponse::Error { reason },
        };
        if let Err(reason) =
            self.service
                .register_service_group(archon_node::workload::ServiceGroup {
                    id: id.clone(),
                    owner: OwnerId::from_u64(owner),
                    cardinality,
                    template,
                })
        {
            return ServerResponse::Error { reason };
        }
        self.ensure_snapshot();
        for member in self.service.reconcile_service_groups() {
            match self.service.admit_one() {
                Ok(Some(admitted)) if admitted != member => continue,
                Ok(_) => {}
                Err(err) => {
                    eprintln!("archon: service member admission failed: {err}");
                }
            }
        }
        // Per-machine groups have no fixed target: report 0 (not a stale
        // machine count) per the API contract.
        let desired = match cardinality {
            archon_node::workload::Cardinality::PerMachine => 0,
            _ => desired,
        };
        ServerResponse::ServiceRegistered { id, desired }
    }

    /// Steer a bounded-elastic group's target within its bounds. The
    /// mutation is desired-state only; reconciliation applies it.
    fn scale_service(&mut self, id: String, target: u32) -> ServerResponse {
        match self.service.scale_service_group(&id, target as usize) {
            Ok(applied) => {
                for member in self.service.reconcile_service_groups() {
                    match self.service.admit_one() {
                        Ok(Some(admitted)) if admitted != member => continue,
                        Ok(_) => {}
                        Err(err) => {
                            eprintln!("archon: service member admission failed: {err}");
                        }
                    }
                }
                ServerResponse::Scaled {
                    id,
                    target: applied as u32,
                }
            }
            Err(reason) => ServerResponse::Error { reason },
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
