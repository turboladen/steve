use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Information about the detected project.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    /// Absolute path to the project root (git worktree root).
    pub root: PathBuf,
    /// Deterministic project ID derived from the git root commit hash.
    pub id: String,
}

/// Detect the project root by walking up from `start_dir` looking for `.git/`.
/// Uses the git root commit hash as the project ID (deterministic across clones).
pub fn detect(start_dir: &Path) -> Result<ProjectInfo> {
    let start = start_dir
        .canonicalize()
        .with_context(|| format!("cannot resolve path: {}", start_dir.display()))?;

    // Walk up from start_dir looking for .git
    let mut dir = start.as_path();
    loop {
        let git_dir = dir.join(".git");
        if git_dir.exists() {
            let root = dir.to_path_buf();
            let id = root_commit_hash(&root)?;
            return Ok(ProjectInfo { root, id });
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => bail!(
                "no git repository found from {}",
                start_dir.display()
            ),
        }
    }
}

/// Detect project from current directory, falling back to CWD with a hash-based ID.
pub fn detect_or_cwd() -> ProjectInfo {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match detect(&cwd) {
        Ok(info) => info,
        Err(_) => {
            // Fallback: use CWD as root, hash of path as ID
            let id = format!("{:x}", hash_path(&cwd));
            ProjectInfo { root: cwd, id }
        }
    }
}

/// Get the root commit hash(es) of the repository, sorted, take the first.
fn root_commit_hash(repo_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-list", "--max-parents=0", "--all"])
        .current_dir(repo_root)
        .output()
        .context("failed to run git rev-list")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git rev-list failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut hashes: Vec<&str> = stdout.lines().collect();
    hashes.sort();

    hashes
        .first()
        .map(|h| h.to_string())
        .context("no root commits found in repository")
}

/// Simple hash of a path for fallback project IDs.
fn hash_path(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}
