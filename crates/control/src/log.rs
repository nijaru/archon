//! Append-only command log: one JSON-encoded [`Command`] per line.
//! Replay applies commands in order; the kernel's determinism does the rest.

use std::fs::{File, OpenOptions};
use std::io::Write;
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
        self.file.flush()?;
        // Durability is the point of the log: a command the plane acted on
        // (and may have acknowledged) must survive an OS crash, not just a
        // clean shutdown. Command rates are low; fsync every append.
        self.file.sync_all()
    }

    /// Read every logged command in order. A torn final line (crash
    /// mid-append) is truncated with a warning instead of failing boot:
    /// its fsync never completed, so no response could have acknowledged
    /// it. Corruption anywhere else stays a hard error.
    pub fn read(path: &Path) -> std::io::Result<Vec<Command>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = std::fs::read(path)?;
        let mut commands = Vec::new();
        let mut line_start = 0usize;
        let mut line_no = 1usize;
        loop {
            let newline = bytes[line_start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| line_start + index);
            let Some(end) = newline else {
                let tail = &bytes[line_start..];
                if tail.iter().all(|byte| byte.is_ascii_whitespace()) {
                    break;
                }
                match serde_json::from_slice::<Command>(tail) {
                    Ok(command) => {
                        // Complete final line that just lost its newline;
                        // unobservable in practice since append writes it.
                        commands.push(command);
                        break;
                    }
                    Err(_) => {
                        eprintln!(
                            "archon: truncating torn log tail at line {line_no}; \
                             its append never completed"
                        );
                        OpenOptions::new()
                            .write(true)
                            .open(path)?
                            .set_len(line_start as u64)?;
                        break;
                    }
                }
            };
            let line = &bytes[line_start..end];
            if !line.iter().all(|byte| byte.is_ascii_whitespace()) {
                commands.push(serde_json::from_slice(line).map_err(|err| {
                    std::io::Error::other(format!("corrupt log entry {line_no}: {err}"))
                })?);
            }
            line_start = end + 1;
            line_no += 1;
        }
        Ok(commands)
    }
}
