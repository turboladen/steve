use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Serialize, de::DeserializeOwned};

/// JSON file storage under `~/.local/share/steve/storage/`.
///
/// Keys are path segments that map to the filesystem:
///   `["sessions", "abc123"]` → `{base}/sessions/abc123.json`
///
/// File locking via `std::fs::File` ensures safe concurrent access.
#[derive(Debug, Clone)]
pub struct Storage {
    base_dir: PathBuf,
}

impl Storage {
    /// Create storage for a specific project.
    pub fn new(project_id: &str) -> Result<Self> {
        let base = data_dir()?.join(project_id);
        fs::create_dir_all(&base)
            .with_context(|| format!("failed to create storage dir: {}", base.display()))?;
        Ok(Self { base_dir: base })
    }

    /// Create storage at an explicit base directory (for tests).
    #[cfg(test)]
    pub fn with_base(base_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base_dir)
            .with_context(|| format!("failed to create storage dir: {}", base_dir.display()))?;
        Ok(Self { base_dir })
    }

    /// Get the base directory for this storage instance.
    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    /// Read a JSON value at the given key path.
    pub fn read<T: DeserializeOwned>(&self, key: &[&str]) -> Result<T> {
        let path = self.key_to_path(key);
        let file =
            fs::File::open(&path).with_context(|| format!("failed to open: {}", path.display()))?;

        // Shared (read) lock
        file.lock_shared()
            .with_context(|| format!("failed to lock: {}", path.display()))?;

        let reader = std::io::BufReader::new(&file);
        let value = serde_json::from_reader(reader)
            .with_context(|| format!("failed to parse: {}", path.display()))?;

        file.unlock()?;
        Ok(value)
    }

    /// Write a JSON value at the given key path. Uses atomic write (tmp + rename).
    pub fn write<T: Serialize>(&self, key: &[&str], value: &T) -> Result<()> {
        let path = self.key_to_path(key);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write to a temp file first, then rename for atomicity
        let tmp_path = path.with_extension("json.tmp");
        let file = fs::File::create(&tmp_path)
            .with_context(|| format!("failed to create: {}", tmp_path.display()))?;

        // Exclusive (write) lock
        file.lock()
            .with_context(|| format!("failed to lock: {}", tmp_path.display()))?;

        serde_json::to_writer_pretty(&file, value)
            .with_context(|| format!("failed to write: {}", tmp_path.display()))?;

        file.unlock()?;
        drop(file);

        // Atomic rename
        fs::rename(&tmp_path, &path).with_context(|| {
            format!(
                "failed to rename {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;

        Ok(())
    }

    /// List all JSON files under a key prefix, returning their stem names.
    pub fn list(&self, prefix: &[&str]) -> Result<Vec<String>> {
        let dir = self.key_to_dir(prefix);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut names = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json")
                && let Some(stem) = path.file_stem()
            {
                names.push(stem.to_string_lossy().to_string());
            }
        }
        Ok(names)
    }

    /// Delete a JSON file at the given key path.
    pub fn delete(&self, key: &[&str]) -> Result<()> {
        let path = self.key_to_path(key);
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to delete: {}", path.display()))?;
        }
        Ok(())
    }

    /// Check if a key exists.
    pub fn exists(&self, key: &[&str]) -> bool {
        self.key_to_path(key).exists()
    }

    fn key_to_path(&self, key: &[&str]) -> PathBuf {
        let mut path = self.base_dir.clone();
        for (i, segment) in key.iter().enumerate() {
            if i == key.len() - 1 {
                // Last segment gets .json extension
                path.push(format!("{segment}.json"));
            } else {
                path.push(segment);
            }
        }
        path
    }

    fn key_to_dir(&self, prefix: &[&str]) -> PathBuf {
        let mut path = self.base_dir.clone();
        for segment in prefix {
            path.push(segment);
        }
        path
    }
}

/// Delete orphan `memory.md` files from the removed memory tool. Idempotent.
pub fn sweep_legacy_memory_files() -> usize {
    let storage_root = match data_dir() {
        Ok(dir) => dir,
        Err(err) => {
            tracing::debug!(error = %err, "sweep skipped: could not determine data dir");
            return 0;
        }
    };
    sweep_legacy_memory_files_in(&storage_root)
}

fn sweep_legacy_memory_files_in(storage_root: &std::path::Path) -> usize {
    let entries = match std::fs::read_dir(storage_root) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(err) => {
            tracing::warn!(
                path = %storage_root.display(),
                error = %err,
                "sweep skipped: could not read storage root",
            );
            return 0;
        }
    };

    let mut removed = 0;
    for result in entries {
        let entry = match result {
            Ok(e) => e,
            Err(err) => {
                tracing::debug!(error = %err, "skipping unreadable directory entry");
                continue;
            }
        };

        let is_dir = match entry.file_type() {
            Ok(ft) => ft.is_dir(),
            Err(err) => {
                tracing::debug!(
                    path = %entry.path().display(),
                    error = %err,
                    "could not determine file type, skipping",
                );
                continue;
            }
        };
        if !is_dir {
            continue;
        }

        let memory_file = entry.path().join("memory.md");
        match std::fs::remove_file(&memory_file) {
            Ok(()) => {
                tracing::info!(
                    path = %memory_file.display(),
                    "removed legacy memory.md",
                );
                removed += 1;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(
                    path = %memory_file.display(),
                    error = %err,
                    "failed to remove legacy memory.md",
                );
            }
        }
    }
    removed
}

/// Get the base data directory: `~/.local/share/steve/storage/`
fn data_dir() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "steve").context("failed to determine data directory")?;
    let storage_dir = dirs.data_dir().join("storage");
    Ok(storage_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn test_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempdir().expect("failed to create temp dir");
        let storage =
            Storage::with_base(dir.path().to_path_buf()).expect("failed to create storage");
        (storage, dir)
    }

    #[test]
    fn write_read_roundtrip() {
        let (storage, _dir) = test_storage();
        let value = json!({"key": "value"});
        storage.write(&["test"], &value).expect("write failed");
        let result: serde_json::Value = storage.read(&["test"]).expect("read failed");
        assert_eq!(result, value);
    }

    #[test]
    fn write_creates_parent_directories() {
        let (storage, _dir) = test_storage();
        let value = json!({"key": "value"});
        storage
            .write(&["a", "b", "c"], &value)
            .expect("write with nested key path failed");
        let result: serde_json::Value = storage.read(&["a", "b", "c"]).expect("read failed");
        assert_eq!(result, value);
    }

    #[test]
    fn read_nonexistent_returns_error() {
        let (storage, _dir) = test_storage();
        let result = storage.read::<serde_json::Value>(&["nonexistent"]);
        assert!(result.is_err());
    }

    #[test]
    fn list_empty_directory() {
        let (storage, _dir) = test_storage();
        let items = storage.list(&["empty"]).expect("list failed");
        assert!(items.is_empty());
    }

    #[test]
    fn list_returns_file_stems() {
        let (storage, _dir) = test_storage();
        let value = json!({"key": "value"});
        storage
            .write(&["items", "alpha"], &value)
            .expect("write alpha failed");
        storage
            .write(&["items", "beta"], &value)
            .expect("write beta failed");
        storage
            .write(&["items", "gamma"], &value)
            .expect("write gamma failed");

        let mut items = storage.list(&["items"]).expect("list failed");
        items.sort();
        assert_eq!(items, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn delete_removes_file() {
        let (storage, _dir) = test_storage();
        let value = json!({"key": "value"});
        storage.write(&["to_delete"], &value).expect("write failed");
        assert!(storage.exists(&["to_delete"]));

        storage.delete(&["to_delete"]).expect("delete failed");
        assert!(!storage.exists(&["to_delete"]));
    }

    #[test]
    fn delete_nonexistent_no_error() {
        let (storage, _dir) = test_storage();
        let result = storage.delete(&["does_not_exist"]);
        assert!(result.is_ok());
    }

    #[test]
    fn exists_true_false() {
        let (storage, _dir) = test_storage();
        assert!(!storage.exists(&["check_me"]));

        let value = json!({"key": "value"});
        storage.write(&["check_me"], &value).expect("write failed");
        assert!(storage.exists(&["check_me"]));
    }

    #[test]
    fn atomic_write_no_tmp_residue() {
        let (storage, _dir) = test_storage();
        let value = json!({"key": "value"});
        storage.write(&["clean"], &value).expect("write failed");

        // Check that no .json.tmp files remain in the base directory
        let mut tmp_files = Vec::new();
        for entry in fs::read_dir(storage.base_dir()).expect("read_dir failed") {
            let entry = entry.expect("entry failed");
            let path = entry.path();
            if let Some(name) = path.file_name()
                && name.to_string_lossy().ends_with(".json.tmp")
            {
                tmp_files.push(path);
            }
        }
        assert!(
            tmp_files.is_empty(),
            "found residual tmp files: {tmp_files:?}"
        );
    }

    #[test]
    fn key_to_path_single_segment() {
        let (storage, _dir) = test_storage();
        let value = json!({"key": "value"});
        storage.write(&["foo"], &value).expect("write failed");

        let expected = storage.base_dir().join("foo.json");
        assert!(expected.exists(), "expected {expected:?} to exist");
    }

    #[test]
    fn key_to_path_multi_segment() {
        let (storage, _dir) = test_storage();
        let value = json!({"key": "value"});
        storage
            .write(&["a", "b", "c"], &value)
            .expect("write failed");

        let expected = storage.base_dir().join("a").join("b").join("c.json");
        assert!(expected.exists(), "expected {expected:?} to exist");
    }

    #[test]
    fn sweep_removes_memory_files_across_projects() {
        let dir = tempdir().expect("failed to create temp dir");
        let storage_root = dir.path();

        // Simulate two project storage dirs, each with a memory.md + an unrelated file.
        for proj in ["proj-alpha", "proj-beta"] {
            let p = storage_root.join(proj);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("memory.md"), "stale content").unwrap();
            std::fs::write(p.join("keep.txt"), "should remain").unwrap();
        }

        let removed = sweep_legacy_memory_files_in(storage_root);
        assert_eq!(removed, 2, "should remove both memory.md files");

        for proj in ["proj-alpha", "proj-beta"] {
            let p = storage_root.join(proj);
            assert!(!p.join("memory.md").exists(), "memory.md should be gone");
            assert!(p.join("keep.txt").exists(), "unrelated files must stay");
        }

        // Second call is idempotent.
        let removed_again = sweep_legacy_memory_files_in(storage_root);
        assert_eq!(removed_again, 0, "second sweep finds nothing");
    }

    #[test]
    fn sweep_handles_missing_storage_dir() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let removed = sweep_legacy_memory_files_in(&missing);
        assert_eq!(removed, 0);
    }

    #[test]
    fn sweep_skips_files_at_storage_root() {
        // A `memory.md` directly at the storage root (not inside a project dir)
        // must NOT be removed — the sweep only targets `{project}/memory.md`.
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("memory.md"), "stray file at root").unwrap();

        let removed = sweep_legacy_memory_files_in(root);
        assert_eq!(removed, 0, "file at storage root must not be swept");
        assert!(root.join("memory.md").exists(), "stray file must remain");
    }

    #[test]
    fn sweep_continues_after_remove_failure() {
        // If one project's memory.md can't be removed (simulated by making it
        // a directory, which triggers EISDIR on remove_file), the sweep must
        // log and continue — other projects still get cleaned.
        let dir = tempdir().unwrap();
        let root = dir.path();

        // proj-a: memory.md is a DIRECTORY — remove_file fails with EISDIR
        let proj_a_memory = root.join("proj-a").join("memory.md");
        std::fs::create_dir_all(&proj_a_memory).unwrap();

        // proj-b: memory.md is a regular file — normal success path
        let proj_b = root.join("proj-b");
        std::fs::create_dir_all(&proj_b).unwrap();
        std::fs::write(proj_b.join("memory.md"), "stale").unwrap();

        let removed = sweep_legacy_memory_files_in(root);
        assert_eq!(removed, 1, "only proj-b's file should count as removed");
        assert!(proj_a_memory.is_dir(), "proj-a's directory must survive");
        assert!(
            !proj_b.join("memory.md").exists(),
            "proj-b file must be gone"
        );
    }

    #[test]
    fn concurrent_reads_dont_block() {
        let (storage, _dir) = test_storage();
        let value = json!({"key": "value"});
        storage.write(&["shared"], &value).expect("write failed");

        let storage1 = storage.clone();
        let storage2 = storage.clone();

        let handle1 = std::thread::spawn(move || {
            storage1
                .read::<serde_json::Value>(&["shared"])
                .expect("read 1 failed")
        });
        let handle2 = std::thread::spawn(move || {
            storage2
                .read::<serde_json::Value>(&["shared"])
                .expect("read 2 failed")
        });

        let result1 = handle1.join().expect("thread 1 panicked");
        let result2 = handle2.join().expect("thread 2 panicked");
        assert_eq!(result1, value);
        assert_eq!(result2, value);
    }
}
