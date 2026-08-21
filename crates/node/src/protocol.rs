//! Remote agent protocol: length-prefixed JSON frames over TCP.
//!
//! The controller sends `AgentRequest`s derived from kernel Effects; the
//! agent answers with `AgentResponse`s the controller turns into Record
//! commands. Every request carries the binding's session and fence, so the
//! seam between decision and enforcement survives the network unchanged.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// Resource limits derived from a lease's claims. Zero means unlimited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseLimits {
    pub cpu_count: u64,
    pub memory_bytes: u64,
}

impl LeaseLimits {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.cpu_count == 0 && self.memory_bytes == 0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AgentRequest {
    /// Ask the agent to describe its machine; the controller builds the
    /// graph from the answer.
    Hello,
    /// Dial-in registration: the agent connects to the control plane and
    /// announces its machine up front. `instance_id` is the agent's stable
    /// identity across reconnects; `name` is display-only.
    Register {
        instance_id: String,
        name: String,
        cpus: u64,
        memory_bytes: u64,
    },
    Prepare {
        binding: u64,
        lease: u64,
        session: u64,
        fence: u64,
    },
    Activate {
        binding: u64,
        lease: u64,
        session: u64,
        fence: u64,
        command: Vec<String>,
        limits: LeaseLimits,
    },
    Release {
        binding: u64,
        lease: u64,
        session: u64,
        fence: u64,
    },
    Fence {
        binding: u64,
        lease: u64,
        session: u64,
        fence: u64,
    },
    /// Liveness probe for a lease's process. Read-only: carries no
    /// session, so probing never touches fencing generations.
    Status { lease: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AgentResponse {
    Welcome {
        name: String,
        cpus: u64,
        memory_bytes: u64,
    },
    Prepared {
        binding: u64,
        handle: u64,
    },
    Activated {
        binding: u64,
    },
    Released {
        binding: u64,
    },
    Fenced {
        binding: u64,
    },
    /// The agent refused or failed the operation; the controller turns this
    /// into RecordBindingFailed.
    Failed {
        binding: u64,
        reason: String,
    },
    Running {
        lease: u64,
        running: bool,
    },
}

pub fn write_frame<T: serde::Serialize>(
    stream: &mut impl Write,
    message: &T,
) -> std::io::Result<()> {
    let payload = serde_json::to_vec(message).expect("serialize frame");
    stream.write_all(&(payload.len() as u32).to_le_bytes())?;
    stream.write_all(&payload)
}

pub fn read_frame(stream: &mut impl Read) -> std::io::Result<AgentResponse> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(std::io::Error::other)
}

pub fn write_response(stream: &mut impl Write, response: &AgentResponse) -> std::io::Result<()> {
    let payload = serde_json::to_vec(response).expect("serialize response");
    stream.write_all(&(payload.len() as u32).to_le_bytes())?;
    stream.write_all(&payload)
}

pub fn read_request(stream: &mut impl Read) -> std::io::Result<AgentRequest> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(std::io::Error::other)
}
