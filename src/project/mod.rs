use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

/// Information about the detected project.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    /// Absolute path to the project root (git worktree root).
    pub root: PathBuf,
    /// Deterministic project ID derived from the git root commit hash.
    pub id: String,
    /// The user's actual working directory (may be a subdirectory of `root`).
    pub cwd: PathBuf,
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
            return Ok(ProjectInfo {
                root,
                id,
                cwd: start.to_path_buf(),
            });
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => bail!("no git repository found from {}", start_dir.display()),
        }
    }
}

/// Detect project from current directory, falling back to CWD with a hash-based ID.
pub fn detect_or_cwd() -> ProjectInfo {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
    match detect(&cwd) {
        Ok(mut info) => {
            info.cwd = canonical_cwd;
            info
        }
        Err(_) => {
            // Fallback: use CWD as root, hash of path as ID
            let id = format!("{:x}", hash_path(&canonical_cwd));
            ProjectInfo {
                root: canonical_cwd.clone(),
                id,
                cwd: canonical_cwd,
            }
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

/// Get the current git branch name for the repo at `repo_root`.
/// Returns `None` if not a git repo or the command fails.
pub fn git_branch(repo_root: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Check whether the repo at `repo_root` has uncommitted changes.
/// Returns `Some(true)` if dirty, `Some(false)` if clean, `None` on failure.
pub fn git_is_dirty(repo_root: &Path) -> Option<bool> {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
}

/// Extract the repository name from the repo root path (last component).
pub fn git_repo_name(repo_root: &Path) -> Option<String> {
    repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};

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
        run(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ]);
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
        assert_eq!(
            info.cwd,
            sub.canonicalize().unwrap(),
            "cwd should be the start directory, not root"
        );
        assert!(!info.id.is_empty());
    }

    #[test]
    fn detect_sets_cwd_to_root_when_started_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());

        let info = detect(tmp.path()).expect("detect should succeed");
        let canonical = tmp.path().canonicalize().unwrap();
        assert_eq!(info.root, canonical);
        assert_eq!(
            info.cwd, canonical,
            "cwd should equal root when started from root"
        );
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
        assert_eq!(
            h1, h2,
            "hash_path should return the same value for the same input"
        );
        // Different paths produce different hashes
        let h3 = hash_path(Path::new("/other/path"));
        assert_ne!(h1, h3, "different paths should produce different hashes");
    }

    // -- git_branch tests --

    #[test]
    fn git_branch_in_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let branch = git_branch(tmp.path());
        // After `git init` + commit, should be on a default branch
        assert!(branch.is_some(), "should detect branch in git repo");
        let name = branch.unwrap();
        assert!(!name.is_empty());
        // Typically "main" or "master" depending on git config
        assert!(
            name == "main" || name == "master",
            "expected default branch, got: {name}"
        );
    }

    #[test]
    fn git_branch_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(git_branch(tmp.path()), None);
    }

    // -- git_is_dirty tests --

    #[test]
    fn git_is_dirty_clean_repo() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        assert_eq!(git_is_dirty(tmp.path()), Some(false));
    }

    #[test]
    fn git_is_dirty_with_untracked_file() {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("new_file.txt"), "content").unwrap();
        assert_eq!(git_is_dirty(tmp.path()), Some(true));
    }

    #[test]
    fn git_is_dirty_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(git_is_dirty(tmp.path()), None);
    }

    // -- git_repo_name tests --

    #[test]
    fn git_repo_name_extracts_last_component() {
        let path = Path::new("/home/user/projects/my-app");
        assert_eq!(git_repo_name(path), Some("my-app".to_string()));
    }

    #[test]
    fn git_repo_name_root_path() {
        // Root path has no file_name
        let path = Path::new("/");
        assert_eq!(git_repo_name(path), None);
    }

    #[test]
    fn git_repo_name_single_component() {
        let path = Path::new("my-repo");
        assert_eq!(git_repo_name(path), Some("my-repo".to_string()));
    }
}
