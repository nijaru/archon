//! Append-only command log: one JSON-encoded [`Command`] per line.
//! Replay applies commands in order; the kernel's determinism does the rest.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use archon_kernel::Command;

pub struct CommandLog {
    file: File,
}

impl CommandLog {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    pub fn append(&mut self, command: &Command) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(command).expect("serialize command");
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()
    }

    /// Read every logged command in order.
    pub fn read(path: &Path) -> std::io::Result<Vec<Command>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let reader = BufReader::new(File::open(path)?);
        let mut commands = Vec::new();
        for (index, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            commands.push(serde_json::from_str(&line).map_err(|err| {
                std::io::Error::other(format!("corrupt log entry {}: {err}", index + 1))
            })?);
        }
        Ok(commands)
    }
}
