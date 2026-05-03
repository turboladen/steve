//! Tempdir workspace for one scenario run.
//!
//! Sets up a fresh tempdir, copies fixtures from the scenario directory into
//! it, runs setup shell commands, and snapshots the final post-setup state
//! so Phase 3 `FileUnchanged` assertions can detect mutations.
//!
//! Path semantics: a fixture path like `foo/bar.txt` in `setup.copy_fixtures`
//! is read from `<scenario_dir>/foo/bar.txt` and written to
//! `<workspace_root>/foo/bar.txt` literally. Path validation in
//! `Scenario::validate` rejects absolute paths and `..` traversal at parse
//! time, so this module trusts that those constraints hold.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::eval::scenario::Setup;

#[derive(Debug)]
pub struct ScenarioWorkspace {
    pub root: PathBuf,
    pub baseline: WorkspaceSnapshot,
    /// RAII handle — `Drop` cleans up the tempdir.
    _tmp: tempfile::TempDir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    /// Workspace-relative path → SHA-256 of file content. Used by
    /// `FileUnchanged` to detect any mutation without re-reading every fixture.
    pub files: BTreeMap<PathBuf, [u8; 32]>,
}

impl ScenarioWorkspace {
    pub fn build(scenario_dir: &Path, setup: &Setup) -> Result<Self> {
        let tmp = tempfile::tempdir().context("creating scenario tempdir")?;
        let root = tmp.path().to_path_buf();

        for fixture in &setup.copy_fixtures {
            let src = scenario_dir.join(fixture);
            let dst = root.join(fixture);
            // Reject symlink fixtures BEFORE copying — `std::fs::copy` follows
            // symlinks, so a scenario could commit a symlink to /etc/passwd
            // or CI secrets and have those contents copied into the workspace
            // for the LLM to read/exfiltrate.
            let src_meta = std::fs::symlink_metadata(&src)
                .with_context(|| format!("statting fixture source {}", src.display()))?;
            if src_meta.file_type().is_symlink() {
                anyhow::bail!(
                    "fixture {} is a symlink — refusing to copy (would let the workspace exfiltrate file content from outside the scenario dir)",
                    src.display()
                );
            }
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("creating parent dir for fixture {}", dst.display())
                })?;
            }
            std::fs::copy(&src, &dst).with_context(|| {
                format!("copying fixture {} → {}", src.display(), dst.display())
            })?;
        }

        for cmd in &setup.shell {
            let status = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&root)
                .status()
                .with_context(|| format!("spawning setup shell command {cmd:?}"))?;
            if !status.success() {
                anyhow::bail!(
                    "setup shell command {cmd:?} exited with {status} in scenario tempdir"
                );
            }
        }

        let baseline = snapshot(&root)?;

        Ok(Self {
            root,
            baseline,
            _tmp: tmp,
        })
    }
}

/// Walk the workspace recursively and SHA-256 every regular file. Symlinks
/// are NOT followed (would risk escaping the workspace). The returned map is
/// keyed by workspace-relative path and ordered (BTreeMap) so equality
/// comparisons are deterministic.
fn snapshot(root: &Path) -> Result<WorkspaceSnapshot> {
    let mut files = BTreeMap::new();
    walk(root, root, &mut files)?;
    Ok(WorkspaceSnapshot { files })
}

fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, [u8; 32]>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry under {}", dir.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("statting {}", entry.path().display()))?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            walk(root, &path, files)?;
        } else if file_type.is_file() {
            let content =
                std::fs::read(&path).with_context(|| format!("reading file {}", path.display()))?;
            let hash: [u8; 32] = Sha256::digest(&content).into();
            let rel = path
                .strip_prefix(root)
                .with_context(|| format!("stripping {} from {}", root.display(), path.display()))?
                .to_path_buf();
            files.insert(rel, hash);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn build_copies_fixtures_preserving_paths() {
        let scenario_dir = tempfile::tempdir().unwrap();
        write_file(&scenario_dir.path().join(".teller.yml"), "providers: []\n");
        write_file(
            &scenario_dir.path().join("sub/AGENTS.md"),
            "# project doc\n",
        );

        let setup = Setup {
            copy_fixtures: vec![PathBuf::from(".teller.yml"), PathBuf::from("sub/AGENTS.md")],
            shell: vec![],
        };
        let ws = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap();

        let teller = std::fs::read_to_string(ws.root.join(".teller.yml")).unwrap();
        assert_eq!(teller, "providers: []\n");
        let agents = std::fs::read_to_string(ws.root.join("sub/AGENTS.md")).unwrap();
        assert_eq!(agents, "# project doc\n");

        // Both files appear in the snapshot, keyed by workspace-relative path.
        assert!(ws.baseline.files.contains_key(Path::new(".teller.yml")));
        assert!(ws.baseline.files.contains_key(Path::new("sub/AGENTS.md")));
        assert_eq!(ws.baseline.files.len(), 2);
    }

    #[test]
    fn build_runs_shell_commands_in_workspace() {
        let scenario_dir = tempfile::tempdir().unwrap();
        let setup = Setup {
            copy_fixtures: vec![],
            shell: vec!["echo first > a.txt".into(), "echo second > b.txt".into()],
        };
        let ws = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap();

        assert_eq!(
            std::fs::read_to_string(ws.root.join("a.txt"))
                .unwrap()
                .trim(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(ws.root.join("b.txt"))
                .unwrap()
                .trim(),
            "second"
        );
    }

    #[test]
    fn build_runs_shell_after_fixtures() {
        // The shell command depends on the fixture being in place — confirms
        // the documented "fixtures first, then shell" ordering.
        let scenario_dir = tempfile::tempdir().unwrap();
        write_file(&scenario_dir.path().join("data.txt"), "original\n");
        let setup = Setup {
            copy_fixtures: vec![PathBuf::from("data.txt")],
            shell: vec!["echo overwritten > data.txt".into()],
        };
        let ws = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap();

        assert_eq!(
            std::fs::read_to_string(ws.root.join("data.txt"))
                .unwrap()
                .trim(),
            "overwritten"
        );
    }

    #[test]
    fn build_aborts_on_failed_shell_command() {
        let scenario_dir = tempfile::tempdir().unwrap();
        let setup = Setup {
            copy_fixtures: vec![],
            shell: vec!["exit 7".into()],
        };
        let err = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("exit 7") || chain.contains("exited"),
            "expected non-zero exit propagated: {chain}"
        );
    }

    #[test]
    fn build_aborts_on_missing_fixture() {
        let scenario_dir = tempfile::tempdir().unwrap();
        let setup = Setup {
            copy_fixtures: vec![PathBuf::from("does-not-exist.txt")],
            shell: vec![],
        };
        let err = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap_err();
        let chain = format!("{err:#}");
        // After the symlink-rejection check, missing-fixture surfaces as a
        // stat failure on the source path. Either the operation context
        // ("statting fixture source" or the legacy "copying fixture") plus
        // the missing path name is sufficient to confirm we got the right error.
        assert!(
            chain.contains("does-not-exist.txt"),
            "expected missing-fixture error mentioning the path: {chain}"
        );
        assert!(
            chain.contains("statting fixture source") || chain.contains("copying fixture"),
            "expected fixture-stage error: {chain}"
        );
    }

    #[test]
    fn snapshot_hashes_are_content_addressed() {
        let scenario_dir = tempfile::tempdir().unwrap();
        write_file(&scenario_dir.path().join("a.txt"), "hello\n");
        write_file(&scenario_dir.path().join("b.txt"), "hello\n");
        write_file(&scenario_dir.path().join("c.txt"), "different\n");
        let setup = Setup {
            copy_fixtures: vec![
                PathBuf::from("a.txt"),
                PathBuf::from("b.txt"),
                PathBuf::from("c.txt"),
            ],
            shell: vec![],
        };
        let ws = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap();

        let h_a = ws.baseline.files.get(Path::new("a.txt")).unwrap();
        let h_b = ws.baseline.files.get(Path::new("b.txt")).unwrap();
        let h_c = ws.baseline.files.get(Path::new("c.txt")).unwrap();
        assert_eq!(h_a, h_b, "identical content must produce identical hashes");
        assert_ne!(h_a, h_c, "different content must produce different hashes");
    }

    #[test]
    fn build_rejects_symlink_fixture() {
        // Security: fixture symlinks would be followed by std::fs::copy,
        // letting a scenario exfiltrate file content from outside its dir.
        #[cfg(unix)]
        {
            let scenario_dir = tempfile::tempdir().unwrap();
            // Create a real file that a malicious symlink would point at.
            let secret = scenario_dir.path().join("secret-target.txt");
            write_file(&secret, "sensitive\n");
            // Symlink the fixture to the secret.
            let fixture_link = scenario_dir.path().join("evil.txt");
            std::os::unix::fs::symlink(&secret, &fixture_link).unwrap();

            let setup = Setup {
                copy_fixtures: vec![PathBuf::from("evil.txt")],
                shell: vec![],
            };
            let err = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap_err();
            let chain = format!("{err:#}");
            assert!(
                chain.contains("symlink") && chain.contains("refusing to copy"),
                "expected symlink rejection: {chain}"
            );
        }
    }

    #[test]
    fn snapshot_walks_recursively_and_skips_symlinks() {
        // Symlinks are skipped to prevent escaping the workspace.
        #[cfg(unix)]
        {
            let scenario_dir = tempfile::tempdir().unwrap();
            write_file(&scenario_dir.path().join("nested/deep/file.txt"), "x\n");
            let setup = Setup {
                copy_fixtures: vec![PathBuf::from("nested/deep/file.txt")],
                shell: vec!["ln -s /etc/hosts symlink".into()],
            };
            let ws = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap();

            assert!(
                ws.baseline
                    .files
                    .contains_key(Path::new("nested/deep/file.txt"))
            );
            assert!(
                !ws.baseline.files.contains_key(Path::new("symlink")),
                "symlinks must NOT appear in snapshot"
            );
        }
    }

    #[test]
    fn workspace_root_is_dropped_with_tempdir() {
        let scenario_dir = tempfile::tempdir().unwrap();
        let setup = Setup {
            copy_fixtures: vec![],
            shell: vec!["echo x > marker".into()],
        };
        let root_path = {
            let ws = ScenarioWorkspace::build(scenario_dir.path(), &setup).unwrap();
            assert!(ws.root.join("marker").exists());
            ws.root.clone()
        };
        // After `ws` is dropped, the tempdir should be gone.
        assert!(
            !root_path.exists(),
            "tempdir should be cleaned up when ScenarioWorkspace is dropped"
        );
    }
}
