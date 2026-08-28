from pathlib import Path

path = Path("crates/node/src/discover.rs")
text = path.read_text()
old = '''fn discovered_devices() -> Result<Vec<DeviceSpec>, String> {
    if std::env::var_os("ARCHON_DEVICES").is_some() {
        return Ok(declared_devices());
    }
    nvidia_devices()
}
'''
new = '''fn discovered_devices() -> Result<Vec<DeviceSpec>, String> {
    if std::env::var_os("ARCHON_DEVICES").is_some() {
        let spec = std::env::var("ARCHON_DEVICES")
            .map_err(|_| "ARCHON_DEVICES is not valid UTF-8".to_string())?;
        validate_declared_devices(&spec)?;
        return Ok(declared_devices());
    }
    nvidia_devices()
}

fn validate_declared_devices(spec: &str) -> Result<(), String> {
    for raw in spec.split(',').filter(|entry| !entry.trim().is_empty()) {
        let entry = raw.trim();
        let (kind, id, dev) = match entry.split_once('=') {
            Some((kind, rest)) => {
                let (id, dev) = rest.split_once(':').ok_or_else(|| {
                    format!("invalid ARCHON_DEVICES entry {entry:?}: expected kind=id:path")
                })?;
                (kind, id, dev)
            }
            None => {
                let (kind, dev) = entry.split_once(':').ok_or_else(|| {
                    format!("invalid ARCHON_DEVICES entry {entry:?}: expected kind:path")
                })?;
                (kind, dev, dev)
            }
        };
        if id.trim().is_empty() || dev.trim().is_empty() {
            return Err(format!("invalid ARCHON_DEVICES entry {entry:?}: empty id or path"));
        }
        match kind.trim().to_ascii_lowercase().as_str() {
            "gpu" | "nic" | "nvme" => {}
            other => return Err(format!("invalid ARCHON_DEVICES device kind {other:?}")),
        }
    }
    Ok(())
}
'''
assert text.count(old) == 1
text = text.replace(old, new, 1)
old = '''    let devices = parse_nvidia_devices(stdout);
    if devices.is_empty() && !stdout.trim().is_empty() {
        return Err(format!(
            "nvidia-smi returned no valid GPU inventory rows: {}",
            stdout.trim()
        ));
    }
    Ok(devices)
'''
new = '''    let rows = stdout.lines().filter(|line| !line.trim().is_empty()).count();
    let devices = parse_nvidia_devices(stdout);
    if devices.len() != rows {
        return Err(format!(
            "nvidia-smi returned an incomplete GPU inventory: parsed {} of {rows} rows",
            devices.len()
        ));
    }
    Ok(devices)
'''
assert text.count(old) == 1
text = text.replace(old, new, 1)
old = '''        let malformed = interpret_nvidia_query(true, "No devices were found\\n", "").unwrap_err();
        assert!(malformed.contains("no valid GPU inventory rows"));

        assert!(interpret_nvidia_query(true, "", "").unwrap().is_empty());
    }
'''
new = '''        let malformed = interpret_nvidia_query(true, "No devices were found\\n", "").unwrap_err();
        assert!(malformed.contains("incomplete GPU inventory"));

        let partial = interpret_nvidia_query(
            true,
            "0, GPU-test, NVIDIA Test GPU, 00000000:01:00.0, 24564, 8.9, 610.57.04\\nmalformed\\n",
            "",
        )
        .unwrap_err();
        assert!(partial.contains("parsed 1 of 2 rows"));

        assert!(interpret_nvidia_query(true, "", "").unwrap().is_empty());
    }

    #[test]
    fn explicit_device_inventory_must_parse_completely() {
        assert!(validate_declared_devices("gpu=g0:/dev/nvidia0,nic=n0:/dev/net0").is_ok());
        assert!(validate_declared_devices("").is_ok());
        assert!(validate_declared_devices("gpu=g0:/dev/nvidia0,broken").is_err());
        assert!(validate_declared_devices("unknown=x:/dev/x").is_err());
        assert!(validate_declared_devices("gpu=:/dev/nvidia0").is_err());
    }
'''
assert text.count(old) == 1
path.write_text(text.replace(old, new, 1))
