//! Append-only controller journal: one fsynced JSON record per line.
//! Commands and their matching desired state share a record; metadata-only
//! records persist acceptance without introducing workload semantics in the kernel.
//! Legacy command-only logs remain readable.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::Path;

use archon_kernel::Command;

pub(crate) fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(parent)?.sync_all()
}

/// One atomic controller boundary. Metadata-only records accept desired state;
/// command records pair it with the corresponding kernel transition.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct JournalRecord {
    pub sequence: u64,
    pub command: Option<Command>,
    pub state: archon_node::service::ServiceState,
}

pub enum LogRecord {
    Controller(Box<JournalRecord>),
    Legacy(Command),
}

/// An exclusively owned journal. The OS lock lasts until all owned handles
/// close, including after process termination; replay and repair require it too.
pub struct CommandLog {
    file: File,
}

impl CommandLog {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        file.try_lock().map_err(|error| {
            let error = std::io::Error::from(error);
            std::io::Error::new(
                error.kind(),
                format!("cannot lock journal {}: {error}", path.display()),
            )
        })?;
        file.sync_all()?;
        sync_parent(path)?;
        Ok(Self { file })
    }

    /// Share this ownership with the controller's compaction path. Never
    /// re-lock the duplicate: it already shares the original handle's lock.
    pub(crate) fn try_clone(&self) -> std::io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }

    pub(crate) fn truncate(&self) -> std::io::Result<()> {
        self.file.set_len(0)?;
        self.file.sync_all()
    }

    pub fn append(&mut self, command: &Command) -> std::io::Result<()> {
        self.append_value(command)
    }

    pub fn append_record(&mut self, record: &JournalRecord) -> std::io::Result<()> {
        self.append_value(record)
    }

    fn append_value(&mut self, value: &impl serde::Serialize) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()?;
        // Durability is the point of the log: a command the plane acted on
        // (and may have acknowledged) must survive an OS crash, not just a
        // clean shutdown. Command rates are low; fsync every append.
        self.file.sync_all()
    }

    /// Read every logged command in order. A torn final line (crash
    /// mid-append) is truncated instead of failing boot: its fsync never
    /// completed, so no response could have acknowledged it. Corruption
    /// anywhere else stays a hard error.
    pub fn read(&mut self) -> std::io::Result<Vec<Command>> {
        Ok(self
            .read_records()?
            .into_iter()
            .filter_map(|record| match record {
                LogRecord::Controller(record) => record.command,
                LogRecord::Legacy(command) => Some(command),
            })
            .collect())
    }

    pub fn read_records(&mut self) -> std::io::Result<Vec<LogRecord>> {
        self.file.rewind()?;
        let mut bytes = Vec::new();
        self.file.read_to_end(&mut bytes)?;
        let mut commands = Vec::new();
        let mut line_start = 0usize;
        let mut line_no = 1usize;
        loop {
            let newline = bytes[line_start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| line_start + index);
            let Some(end) = newline else {
                // Newline is the record boundary, even if the JSON itself
                // happens to be complete. Remove the tail before appending.
                if line_start < bytes.len() {
                    self.file.set_len(line_start as u64)?;
                    self.file.sync_all()?;
                }
                break;
            };
            let line = &bytes[line_start..end];
            if !line.iter().all(|byte| byte.is_ascii_whitespace()) {
                // Deserialize directly: serde's untagged intermediate form
                // cannot represent the kernel's u128 resource quantities.
                let record =
                    serde_json::from_slice::<JournalRecord>(line)
                        .map(|record| LogRecord::Controller(Box::new(record)))
                        .or_else(|journal_error| {
                            serde_json::from_slice::<Command>(line)
                            .map(LogRecord::Legacy)
                            .map_err(|legacy_error| std::io::Error::other(format!(
                                "corrupt log entry {line_no}: {journal_error}; {legacy_error}"
                            )))
                        })?;
                commands.push(record);
            }
            line_start = end + 1;
            line_no += 1;
        }
        Ok(commands)
    }
}
