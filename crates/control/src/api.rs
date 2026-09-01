//! Control plane API: length-prefixed JSON frames over TCP, same framing as
//! the agent protocol. Clients submit work, inspect state, and revoke.
//! The framing primitives live in the node protocol crate so both wire
//! surfaces share one implementation.

use serde::{Deserialize, Serialize};

pub use archon_node::protocol::{
    Greeting, MAX_FRAME, read_payload, set_stream_limits, write_frame,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ClientRequest {
    Submit {
        owner: u64,
        cpus: u64,
        memory_mib: u64,
        lifetime_secs: u64,
        command: Vec<String>,
        #[serde(default)]
        keep_alive: bool,
        /// "host_path:mount_path" bind mounts.
        #[serde(default)]
        volumes: Vec<String>,
        /// Ports to publish; "container" or "host:container".
        #[serde(default)]
        ports: Vec<String>,
        /// Seconds between SIGTERM and SIGKILL on teardown (drain).
        #[serde(default)]
        grace_secs: u32,
        /// OCI image reference; the command runs inside a container.
        #[serde(default)]
        image: Option<String>,
        /// Device-count request (claims against declared Gpu nodes).
        #[serde(default)]
        gpus: u64,
    },
    /// Register a desired service group: one member template plus a
    /// cardinality policy, maintained by controller reconciliation.
    /// Re-submitting the same id replaces the template/cardinality.
    /// Absent cardinality fields mean fixed desired count.
    SubmitService {
        id: String,
        owner: u64,
        desired: u32,
        /// Upper bound for bounded-elastic groups; requires `--min` too.
        #[serde(default)]
        max: Option<u32>,
        /// Lower bound for bounded-elastic groups; `desired` is the target.
        #[serde(default)]
        min: Option<u32>,
        /// One member per eligible machine instead of a fixed count.
        #[serde(default)]
        per_machine: bool,
        cpus: u64,
        memory_mib: u64,
        lifetime_secs: u64,
        command: Vec<String>,
        /// "host_path:mount_path" bind mounts.
        #[serde(default)]
        volumes: Vec<String>,
        /// Ports to publish; "container" or "host:container".
        #[serde(default)]
        ports: Vec<String>,
        /// Seconds between SIGTERM and SIGKILL on teardown (drain).
        #[serde(default)]
        grace_secs: u32,
        /// OCI image reference; the command runs inside a container.
        #[serde(default)]
        image: Option<String>,
    },
    /// Steer a bounded-elastic group's target member count within bounds.
    ScaleService {
        id: String,
        target: u32,
    },
    Status,
    Revoke {
        lease: u64,
    },
    /// Fetch a lease's captured output.
    Logs {
        lease: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaseInfo {
    pub id: u64,
    pub owner: u64,
    pub state: String,
    pub expires_at: u64,
    /// Outcome of a workload that finished on its own.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// What ran under this lease.
    #[serde(default)]
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerResponse {
    Submitted {
        request: u64,
        lease: u64,
    },
    /// The group's durable desired state was registered; reconciliation
    /// runs with maintenance.
    ServiceRegistered {
        id: String,
        /// Current target member count, or 0 for per-machine groups.
        desired: u32,
    },
    Scaled {
        id: String,
        target: u32,
    },
    Status {
        queue_len: usize,
        leases: Vec<LeaseInfo>,
    },
    Revoked,
    Logs {
        lease: u64,
        output: String,
    },
    Error {
        reason: String,
    },
}

pub fn read_greeting(stream: &mut impl std::io::Read) -> std::io::Result<Greeting> {
    read_payload(stream)
        .map(|payload| serde_json::from_slice(&payload).map_err(std::io::Error::other))?
}

pub fn read_request(stream: &mut impl std::io::Read) -> std::io::Result<ClientRequest> {
    read_payload(stream)
        .map(|payload| serde_json::from_slice(&payload).map_err(std::io::Error::other))?
}

pub fn write_response(
    stream: &mut impl std::io::Write,
    response: &ServerResponse,
) -> std::io::Result<()> {
    write_frame(stream, response)
}

pub fn read_response(stream: &mut impl std::io::Read) -> std::io::Result<ServerResponse> {
    read_payload(stream)
        .map(|payload| serde_json::from_slice(&payload).map_err(std::io::Error::other))?
}
