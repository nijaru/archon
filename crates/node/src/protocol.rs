//! Remote agent protocol: length-prefixed JSON frames over TCP.
//!
//! The controller sends `AgentRequest`s derived from kernel Effects; the
//! agent answers with `AgentResponse`s the controller turns into Record
//! commands. Every request carries the binding's session and fence, so the
//! seam between decision and enforcement survives the network unchanged.

use std::io::{Read, Write};

use archon_kernel::{BindingScope, PortPublish, StorageMount};
use serde::{Deserialize, Serialize};

/// Connection greeting: role declaration, sent as the first framed
/// message after the Noise handshake authenticates and encrypts the link.
/// One claimed device handed to an execution adapter: its stable `id`
/// (survives re-registration), the host path it is currently reachable
/// through, and an optional provider-native attachment name. Adapters enforce
/// access per runtime — containers via `--device` (a CDI name when opted in),
/// processes via their own device provider (e.g. cgroup-device eBPF filters)
/// keyed on the same id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAccess {
    pub id: String,
    pub dev: String,
    #[serde(default)]
    pub paths: Vec<String>,
    /// Provider-native container attachment, such as
    /// `nvidia.com/gpu=GPU-...`.
    #[serde(default)]
    pub cdi: Option<String>,
}

impl DeviceAccess {
    /// Build from a device node's graph attributes.
    pub fn from_attrs(attrs: &archon_kernel::Attrs) -> Option<Self> {
        let dev = attrs.get("dev")?;
        if dev.is_empty() {
            return None;
        }
        let paths = attrs
            .get("access")
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default();
        Some(DeviceAccess {
            id: attrs.get("id").cloned().unwrap_or_else(|| dev.clone()),
            dev: dev.clone(),
            paths,
            cdi: attrs.get("cdi").cloned(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Greeting {
    /// A dial-in agent announcing its machine.
    Agent {
        instance_id: String,
        name: String,
        cpus: u64,
        memory_bytes: u64,
        #[serde(default)]
        host_nodes: Vec<crate::discover::HostNodeSpec>,
        /// Devices this machine exposes.
        #[serde(default)]
        devices: Vec<crate::discover::DeviceSpec>,
    },
    /// A CLI client.
    Client,
}

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
        #[serde(default)]
        host_nodes: Vec<crate::discover::HostNodeSpec>,
        /// Devices this machine exposes.
        #[serde(default)]
        devices: Vec<crate::discover::DeviceSpec>,
    },
    Prepare {
        binding: u64,
        lease: u64,
        node: u64,
        provider: u64,
        scope: BindingScope,
        session: u64,
        fence: u64,
        epoch: u64,
    },
    Activate {
        binding: u64,
        lease: u64,
        node: u64,
        provider: u64,
        scope: BindingScope,
        session: u64,
        fence: u64,
        epoch: u64,
        command: Vec<String>,
        limits: LeaseLimits,
        /// OCI image reference; empty runs the command as a plain process.
        #[serde(default)]
        image: String,
        /// Host directories bound into the container.
        #[serde(default)]
        storage: Vec<StorageMount>,
        /// Container ports published to the host.
        #[serde(default)]
        ports: Vec<PortPublish>,
        /// Seconds between SIGTERM and SIGKILL on teardown.
        #[serde(default)]
        grace_secs: u32,
        /// Devices the lease's claims bound.
        #[serde(default)]
        devices: Vec<DeviceAccess>,
    },
    Release {
        binding: u64,
        lease: u64,
        node: u64,
        provider: u64,
        scope: BindingScope,
        session: u64,
        fence: u64,
        epoch: u64,
    },
    Fence {
        binding: u64,
        lease: u64,
        node: u64,
        provider: u64,
        scope: BindingScope,
        session: u64,
        fence: u64,
        epoch: u64,
    },
    /// Liveness probe for a lease's process. Read-only: carries no
    /// session, so probing never touches fencing generations.
    Status { lease: u64 },
    /// A lease's captured output. Read-only like Status.
    Logs { lease: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AgentResponse {
    Welcome {
        /// Stable agent identity. Older agents decode as empty and are
        /// refused by controller-initiated registration rather than silently
        /// aliasing every legacy endpoint to the same machine.
        #[serde(default)]
        instance_id: String,
        name: String,
        cpus: u64,
        memory_bytes: u64,
        #[serde(default)]
        host_nodes: Vec<crate::discover::HostNodeSpec>,
        #[serde(default)]
        devices: Vec<crate::discover::DeviceSpec>,
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
        /// Set once the workload has exited; None while running or unknown.
        exit_code: Option<i32>,
    },
    Logs {
        lease: u64,
        output: String,
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

/// Upper bound on one frame; refused before any allocation.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Bound how long one frame read/write may stall. Without this a silent
/// peer blocks its handler thread forever.
pub fn set_stream_limits(stream: &std::net::TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(60)));
}

/// Read one raw framed payload. Public because the control plane must
/// parse the Greeting before it knows which message type follows.
pub fn read_payload(stream: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds maximum size",
        ));
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

pub fn read_frame(stream: &mut impl Read) -> std::io::Result<AgentResponse> {
    let payload = read_payload(stream)?;
    serde_json::from_slice(&payload).map_err(std::io::Error::other)
}

pub fn write_response(stream: &mut impl Write, response: &AgentResponse) -> std::io::Result<()> {
    write_frame(stream, response)
}

pub fn read_greeting(stream: &mut impl Read) -> std::io::Result<Greeting> {
    let payload = read_payload(stream)?;
    serde_json::from_slice(&payload).map_err(std::io::Error::other)
}

pub fn read_request(stream: &mut impl Read) -> std::io::Result<AgentRequest> {
    let payload = read_payload(stream)?;
    serde_json::from_slice(&payload).map_err(std::io::Error::other)
}
