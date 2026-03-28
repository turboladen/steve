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
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Some(stem) = path.file_stem() {
                    names.push(stem.to_string_lossy().to_string());
                }
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
            if let Some(name) = path.file_name() {
                if name.to_string_lossy().ends_with(".json.tmp") {
                    tmp_files.push(path);
                }
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
