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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// Helper: initialize a git repo with one empty commit in the given directory.
    fn init_git_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@test.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@test.com")
                .env("GIT_CONFIG_GLOBAL", "")
                .env("GIT_CONFIG_SYSTEM", "")
                .output()
                .expect("failed to run git");
            assert!(
                status.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&status.stderr)
            );
        };
        run(&["init"]);
        run(&["-c", "commit.gpgsign=false", "commit", "--allow-empty", "-m", "init"]);
    }

    #[test]
    fn detect_finds_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());

        let info = detect(tmp.path()).expect("detect should succeed in a git repo");

        // Root should be the canonical tempdir path
        assert_eq!(info.root, tmp.path().canonicalize().unwrap());
        assert!(!info.id.is_empty(), "project id should be non-empty");
    }

    #[test]
    fn detect_walks_up_from_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());

        let sub = tmp.path().join("sub").join("dir");
        fs::create_dir_all(&sub).unwrap();

        let info = detect(&sub).expect("detect should find git root from subdirectory");

        assert_eq!(info.root, tmp.path().canonicalize().unwrap());
        assert!(!info.id.is_empty());
    }

    #[test]
    fn detect_no_git_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        // No git init — plain directory
        let result = detect(tmp.path());
        assert!(result.is_err(), "detect should fail when there is no .git");
    }

    #[test]
    fn detect_or_cwd_returns_project_info() {
        // detect_or_cwd always succeeds (falls back to CWD hash)
        let info = detect_or_cwd();
        assert!(!info.id.is_empty(), "project id should be non-empty");
    }

    #[test]
    fn hash_path_same_input_matches() {
        // Note: DefaultHasher is not guaranteed stable across Rust versions or
        // processes. This test verifies intra-process consistency only. If
        // cross-process determinism matters, switch to a stable hasher.
        let path = Path::new("/some/test/path");
        let h1 = hash_path(path);
        let h2 = hash_path(path);
        assert_eq!(h1, h2, "hash_path should return the same value for the same input");
        // Different paths produce different hashes
        let h3 = hash_path(Path::new("/other/path"));
        assert_ne!(h1, h3, "different paths should produce different hashes");
    }
}
