//! Control plane API: length-prefixed JSON frames over TCP, same framing as
//! the agent protocol. Clients submit work, inspect state, and revoke.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// The first frame on any control-plane connection: role and optional
/// shared token. Defined in the node protocol so both directions of the
/// wire share one type.
pub use archon_node::protocol::Greeting;

/// Constant-time equality; a length mismatch leaks only the length.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let (a, b) = (expected.as_bytes(), presented.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

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

pub fn write_frame(stream: &mut impl Write, message: &impl Serialize) -> std::io::Result<()> {
    let payload = serde_json::to_vec(message).expect("serialize frame");
    stream.write_all(&(payload.len() as u32).to_le_bytes())?;
    stream.write_all(&payload)
}

pub fn read_greeting(stream: &mut impl Read) -> std::io::Result<Greeting> {
    read_payload(stream)
        .map(|payload| serde_json::from_slice(&payload).map_err(std::io::Error::other))?
}

pub fn read_request(stream: &mut impl Read) -> std::io::Result<ClientRequest> {
    read_payload(stream)
        .map(|payload| serde_json::from_slice(&payload).map_err(std::io::Error::other))?
}

pub fn write_response(stream: &mut impl Write, response: &ServerResponse) -> std::io::Result<()> {
    write_frame(stream, response)
}

pub fn read_response(stream: &mut impl Read) -> std::io::Result<ServerResponse> {
    read_payload(stream)
        .map(|payload| serde_json::from_slice(&payload).map_err(std::io::Error::other))?
}

pub fn read_payload(stream: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}
