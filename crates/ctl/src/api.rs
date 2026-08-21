//! Control plane API: length-prefixed JSON frames over TCP, same framing as
//! the agent protocol. Clients submit work, inspect state, and revoke.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ClientRequest {
    Submit {
        owner: u64,
        cpus: u64,
        memory_mib: u64,
        lifetime_secs: u64,
        command: Vec<String>,
    },
    Status,
    Revoke {
        lease: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaseInfo {
    pub id: u64,
    pub owner: u64,
    pub state: String,
    pub expires_at: u64,
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
    Error {
        reason: String,
    },
}

pub fn write_frame(stream: &mut impl Write, message: &impl Serialize) -> std::io::Result<()> {
    let payload = serde_json::to_vec(message).expect("serialize frame");
    stream.write_all(&(payload.len() as u32).to_le_bytes())?;
    stream.write_all(&payload)
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

fn read_payload(stream: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}
