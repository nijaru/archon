from pathlib import Path

# --- discover.rs ---------------------------------------------------------
path = Path("crates/node/src/discover.rs")
text = path.read_text()
old = '''pub fn describe() -> MachineDescription {
    MachineDescription {
        instance_id: String::new(),
        name: hostname(),
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()) as u64,
        memory_bytes: total_memory_bytes(),
        devices: discovered_devices(),
    }
}

fn discovered_devices() -> Vec<DeviceSpec> {
    if std::env::var_os("ARCHON_DEVICES").is_some() {
        return declared_devices();
    }
    nvidia_devices()
}

/// Discover NVIDIA devices through the vendor's stable management query.
/// Failure to run the optional provider leaves the host with no automatic
/// device claims; explicit `ARCHON_DEVICES` declarations remain available.
fn nvidia_devices() -> Vec<DeviceSpec> {
    let output = match Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,uuid,name,pci.bus_id,memory.total,compute_cap,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            eprintln!(
                "archon: nvidia-smi discovery failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return Vec::new();
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            eprintln!("archon: cannot run nvidia-smi discovery: {err}");
            return Vec::new();
        }
    };
    parse_nvidia_devices(&String::from_utf8_lossy(&output.stdout))
}
'''
new = '''fn description_with_devices(devices: Vec<DeviceSpec>) -> MachineDescription {
    MachineDescription {
        instance_id: String::new(),
        name: hostname(),
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()) as u64,
        memory_bytes: total_memory_bytes(),
        devices,
    }
}

/// Discover this machine with an authoritative device inventory. Registration
/// paths use this form so an uncertain provider query can never masquerade as
/// an authoritative empty inventory and retire/fail previously known devices.
pub fn try_describe() -> Result<MachineDescription, String> {
    discovered_devices().map(description_with_devices)
}

/// Best-effort one-shot discovery for callers that do not reconcile an
/// existing machine. Agent registration must use `try_describe` instead.
pub fn describe() -> MachineDescription {
    match try_describe() {
        Ok(description) => description,
        Err(err) => {
            eprintln!("archon: device discovery incomplete: {err}");
            description_with_devices(Vec::new())
        }
    }
}

fn discovered_devices() -> Result<Vec<DeviceSpec>, String> {
    if std::env::var_os("ARCHON_DEVICES").is_some() {
        return Ok(declared_devices());
    }
    nvidia_devices()
}

/// Discover NVIDIA devices through the vendor's stable management query.
/// An absent `nvidia-smi` means the optional provider is not installed. Once
/// the provider is present, query failure is uncertainty rather than proof
/// that all previously known GPUs disappeared.
fn nvidia_devices() -> Result<Vec<DeviceSpec>, String> {
    let output = match Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,uuid,name,pci.bus_id,memory.total,compute_cap,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("cannot run nvidia-smi discovery: {err}")),
    };
    interpret_nvidia_query(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

fn interpret_nvidia_query(success: bool, stdout: &str, stderr: &str) -> Result<Vec<DeviceSpec>, String> {
    if !success {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(if detail.is_empty() {
            "nvidia-smi discovery failed".into()
        } else {
            format!("nvidia-smi discovery failed: {detail}")
        });
    }

    let devices = parse_nvidia_devices(stdout);
    if devices.is_empty() && !stdout.trim().is_empty() {
        return Err(format!(
            "nvidia-smi returned no valid GPU inventory rows: {}",
            stdout.trim()
        ));
    }
    Ok(devices)
}
'''
if text.count(old) != 1:
    raise RuntimeError("discover discovery block changed")
text = text.replace(old, new, 1)
old_test = '''    #[test]
    fn malformed_nvidia_rows_are_ignored() {
        assert!(parse_nvidia_devices("not,a,gpu\\n").is_empty());
        assert!(
            parse_nvidia_devices(
                "0, GPU-test, NVIDIA Test GPU, 00000000:01:00.0, not-a-number, 8.9, 610.57.04\\n"
            )
            .is_empty()
        );
    }
'''
new_test = old_test + '''
    #[test]
    fn uncertain_nvidia_query_is_not_an_authoritative_empty_inventory() {
        let failed = interpret_nvidia_query(false, "", "driver unavailable").unwrap_err();
        assert!(failed.contains("driver unavailable"));

        let malformed = interpret_nvidia_query(true, "No devices were found\\n", "").unwrap_err();
        assert!(malformed.contains("no valid GPU inventory rows"));

        assert!(interpret_nvidia_query(true, "", "").unwrap().is_empty());
    }
'''
if text.count(old_test) != 1:
    raise RuntimeError("discover tests changed")
path.write_text(text.replace(old_test, new_test, 1))

# --- service.rs ----------------------------------------------------------
path = Path("crates/node/src/service.rs")
text = path.read_text()
old = '''    pub fn register_local(&mut self, cgroup_root: Option<String>) -> Result<NodeId, Error> {
        let description = crate::discover::describe();
'''
new = '''    pub fn register_local(&mut self, cgroup_root: Option<String>) -> Result<NodeId, Error> {
        let description = crate::discover::try_describe().map_err(|reason| Error::Refused {
            explanation: format!("local device discovery incomplete: {reason}"),
        })?;
'''
if text.count(old) != 1:
    raise RuntimeError("register_local block changed")
path.write_text(text.replace(old, new, 1))

# --- agent.rs ------------------------------------------------------------
path = Path("crates/node/src/agent.rs")
text = path.read_text()
old = '''    fn hello(&self) -> AgentResponse {
        let description = crate::discover::describe();
        AgentResponse::Welcome {
            name: description.name,
            cpus: description.cpus,
            memory_bytes: description.memory_bytes,
            devices: description.devices,
        }
    }
'''
new = '''    fn hello(&self) -> AgentResponse {
        let description = match crate::discover::try_describe() {
            Ok(description) => description,
            Err(reason) => {
                return self.failed_none(&format!("device discovery incomplete: {reason}"));
            }
        };
        AgentResponse::Welcome {
            name: description.name,
            cpus: description.cpus,
            memory_bytes: description.memory_bytes,
            devices: description.devices,
        }
    }
'''
if text.count(old) != 1:
    raise RuntimeError("agent hello block changed")
path.write_text(text.replace(old, new, 1))

# --- cli main.rs ---------------------------------------------------------
path = Path("crates/cli/src/main.rs")
text = path.read_text()
old = '''                let description = archon_node::discover::describe();
                let greeting = archon_control::api::Greeting::Agent {
'''
new = '''                let description = match archon_node::discover::try_describe() {
                    Ok(description) => description,
                    Err(err) => {
                        eprintln!(
                            "archon: device discovery incomplete; registration deferred: {err}"
                        );
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                };
                let greeting = archon_control::api::Greeting::Agent {
'''
if text.count(old) != 1:
    raise RuntimeError("dial-in discovery block changed")
path.write_text(text.replace(old, new, 1))
