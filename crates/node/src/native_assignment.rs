//! Durable, single-use native execution scopes for a dedicated local cgroup root.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use archon_kernel::LeaseId;

use crate::cgroup::CgroupGroup;
use crate::protocol::ExecutionId;

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Owner {
    lease: LeaseId,
    boot: String,
    root_device: u64,
    root_inode: u64,
}

/// The record is both the scope's provenance and the single-use start gate.
/// Losing the start reply cannot make the same assignment launch again. Never
/// unlink records while their assignments can still be presented to this host.
pub(crate) struct NativeAssignment {
    _lock: File,
    pub(crate) name: String,
    scope: PathBuf,
}

impl NativeAssignment {
    fn owner(root: &Path, lease: LeaseId) -> Result<Owner, String> {
        let metadata =
            fs::metadata(root).map_err(|err| format!("inspect {}: {err}", root.display()))?;
        if !metadata.is_dir() {
            return Err(format!("cgroup root {} is not a directory", root.display()));
        }
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|err| format!("read host boot identity: {err}"))?;
        Ok(Owner {
            lease,
            boot,
            root_device: metadata.dev(),
            root_inode: metadata.ino(),
        })
    }

    pub(crate) fn begin(
        state: &Path,
        root: &Path,
        lease: LeaseId,
        id: ExecutionId,
    ) -> Result<Self, String> {
        let name = id.native_scope_name();
        let scope = root.join(&name);
        let owner = Self::owner(root, lease)?;
        if CgroupGroup::recover(scope.clone())?.is_some() {
            return Err(format!(
                "execution scope {} already exists; ownership unproven",
                scope.display()
            ));
        }
        let path = state.join(&name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| format!("reserve single-use execution {}: {err}", path.display()))?;
        file.try_lock()
            .map_err(|err| format!("lock {}: {err}", path.display()))?;
        let bytes = serde_json::to_vec(&owner).map_err(|err| err.to_string())?;
        file.write_all(&bytes)
            .map_err(|err| format!("write {}: {err}", path.display()))?;
        Self::sync(&file, state, &path)?;
        Ok(Self {
            _lock: file,
            name,
            scope,
        })
    }

    pub(crate) fn open(
        state: &Path,
        root: &Path,
        lease: LeaseId,
        id: ExecutionId,
    ) -> Result<Option<Self>, String> {
        let name = id.native_scope_name();
        let path = state.join(&name);
        let mut file = match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("open {}: {err}", path.display())),
        };
        file.try_lock()
            .map_err(|err| format!("lock {}: {err}", path.display()))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(4096)
            .read_to_end(&mut bytes)
            .map_err(|err| format!("read {}: {err}", path.display()))?;
        let persisted: Owner = serde_json::from_slice(&bytes)
            .map_err(|err| format!("read {}: {err}", path.display()))?;
        if persisted != Self::owner(root, lease)? {
            return Err(format!(
                "execution {} belongs to a different lease, boot or cgroup root",
                path.display()
            ));
        }
        // A prior process might have died after writing but before syncing.
        // Do not acknowledge closure until the start gate is durable.
        Self::sync(&file, state, &path)?;
        Ok(Some(Self {
            _lock: file,
            scope: root.join(&name),
            name,
        }))
    }

    fn sync(file: &File, state: &Path, path: &Path) -> Result<(), String> {
        file.sync_all()
            .map_err(|err| format!("sync {}: {err}", path.display()))?;
        File::open(state)
            .and_then(|dir| dir.sync_all())
            .map_err(|err| format!("sync {}: {err}", state.display()))
    }

    pub(crate) fn close(self) -> Result<(), String> {
        if let Some(group) = CgroupGroup::recover(self.scope.clone())? {
            group.kill()?;
            group.remove_after_kill()?;
        }
        Ok(())
    }
}
