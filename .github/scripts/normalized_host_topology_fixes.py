from pathlib import Path

path = Path("crates/node/src/service.rs")
text = path.read_text()
old = '''        let greeting = crate::protocol::Greeting::Agent {
            instance_id: String::new(),
            name: String::new(),
            cpus: 0,
            memory_bytes: 0,
            devices: Vec::new(),
        };
'''
new = '''        let greeting = crate::protocol::Greeting::Agent {
            instance_id: String::new(),
            name: String::new(),
            cpus: 0,
            memory_bytes: 0,
            topology: Vec::new(),
            devices: Vec::new(),
        };
'''
if text.count(old) != 1:
    raise RuntimeError("role-only Agent greeting changed")
path.write_text(text.replace(old, new, 1))
