use anyhow::Context;
use fs4::TryLockError;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

pub struct WorkspaceDataLease {
    data_dir: PathBuf,
    _lock: File,
}

impl WorkspaceDataLease {
    pub fn acquire(cache_dir: &Path, server_id: &str, root: &Path) -> anyhow::Result<Self> {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root_hash = blake3::hash(canonical_root.as_os_str().as_encoded_bytes()).to_hex();
        let server_dir = format!(
            "{}-{}",
            sanitize_component(server_id),
            &blake3::hash(server_id.as_bytes()).to_hex()[..8]
        );
        let workspace_dir = cache_dir
            .join("lsp")
            .join(server_dir)
            .join(root_hash.as_str());
        std::fs::create_dir_all(&workspace_dir).with_context(|| {
            format!(
                "Failed to create LSP workspace data directory {}",
                workspace_dir.display()
            )
        })?;

        #[cfg(windows)]
        {
            // Windows cannot currently terminate a launcher's complete process tree. Never reuse
            // a slot that may still be owned by an orphaned JDTLS JVM after Ki exits.
            let slot = format!("session-{}", uuid::Uuid::new_v4());
            return Self::try_acquire_slot(&workspace_dir, &slot)?.ok_or_else(|| {
                anyhow::anyhow!("Newly allocated LSP data slot was unexpectedly locked")
            });
        }

        #[cfg(not(windows))]
        for slot in 0_u32.. {
            if let Some(lease) = Self::try_acquire_slot(&workspace_dir, &slot.to_string())? {
                return Ok(lease);
            }
        }

        #[cfg(not(windows))]
        unreachable!("u32 LSP data slots exhausted")
    }

    fn try_acquire_slot(workspace_dir: &Path, slot: &str) -> anyhow::Result<Option<Self>> {
        let slot_dir = workspace_dir.join(slot);
        std::fs::create_dir_all(&slot_dir)
            .with_context(|| format!("Failed to create LSP data slot {}", slot_dir.display()))?;
        let lock_path = slot_dir.join(".lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("Failed to open LSP data lock {}", lock_path.display()))?;

        match fs4::FileExt::try_lock(&lock) {
            Ok(()) => {
                let data_dir = slot_dir.join("data");
                std::fs::create_dir_all(&data_dir).with_context(|| {
                    format!("Failed to create LSP data directory {}", data_dir.display())
                })?;
                Ok(Some(Self {
                    data_dir,
                    _lock: lock,
                }))
            }
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error)
                .with_context(|| format!("Failed to lock LSP data slot {}", lock_path.display())),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

fn sanitize_component(value: &str) -> String {
    let value = value
        .chars()
        .take(48)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();

    match value.as_str() {
        "" | "." | ".." => "server".to_string(),
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(windows))]
    fn reuses_first_slot_after_release() -> anyhow::Result<()> {
        let cache = tempfile::tempdir()?;
        let root = tempfile::tempdir()?;
        let first_path = WorkspaceDataLease::acquire(cache.path(), "jdtls", root.path())?
            .data_dir()
            .to_path_buf();

        let second_path = WorkspaceDataLease::acquire(cache.path(), "jdtls", root.path())?
            .data_dir()
            .to_path_buf();

        assert_eq!(first_path, second_path);
        Ok(())
    }

    #[test]
    fn concurrent_leases_use_different_slots() -> anyhow::Result<()> {
        let cache = tempfile::tempdir()?;
        let root = tempfile::tempdir()?;
        let first = WorkspaceDataLease::acquire(cache.path(), "jdtls", root.path())?;
        let second = WorkspaceDataLease::acquire(cache.path(), "jdtls", root.path())?;

        assert_ne!(first.data_dir(), second.data_dir());
        assert_eq!(first.data_dir().file_name(), Some("data".as_ref()));
        assert_eq!(second.data_dir().file_name(), Some("data".as_ref()));
        Ok(())
    }

    #[test]
    fn different_roots_use_different_workspace_buckets() -> anyhow::Result<()> {
        let cache = tempfile::tempdir()?;
        let first_root = tempfile::tempdir()?;
        let second_root = tempfile::tempdir()?;
        let first = WorkspaceDataLease::acquire(cache.path(), "jdtls", first_root.path())?;
        let second = WorkspaceDataLease::acquire(cache.path(), "jdtls", second_root.path())?;

        assert_ne!(first.data_dir(), second.data_dir());
        Ok(())
    }

    #[test]
    fn server_id_cannot_escape_cache_directory() -> anyhow::Result<()> {
        let cache = tempfile::tempdir()?;
        let root = tempfile::tempdir()?;
        let lease = WorkspaceDataLease::acquire(cache.path(), "../../jdtls", root.path())?;

        assert!(lease.data_dir().starts_with(cache.path()));
        Ok(())
    }
}
