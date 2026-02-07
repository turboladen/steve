use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};

/// JSON file storage under `~/.local/share/steve/storage/`.
///
/// Keys are path segments that map to the filesystem:
///   `["sessions", "abc123"]` → `{base}/sessions/abc123.json`
///
/// File locking via `fs2` ensures safe concurrent access.
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

    /// Get the base directory for this storage instance.
    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    /// Read a JSON value at the given key path.
    pub fn read<T: DeserializeOwned>(&self, key: &[&str]) -> Result<T> {
        let path = self.key_to_path(key);
        let file = fs::File::open(&path)
            .with_context(|| format!("failed to open: {}", path.display()))?;

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
        file.lock_exclusive()
            .with_context(|| format!("failed to lock: {}", tmp_path.display()))?;

        serde_json::to_writer_pretty(&file, value)
            .with_context(|| format!("failed to write: {}", tmp_path.display()))?;

        file.unlock()?;
        drop(file);

        // Atomic rename
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("failed to rename {} -> {}", tmp_path.display(), path.display()))?;

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
    let dirs = ProjectDirs::from("", "", "steve")
        .context("failed to determine data directory")?;
    let storage_dir = dirs.data_dir().join("storage");
    Ok(storage_dir)
}
