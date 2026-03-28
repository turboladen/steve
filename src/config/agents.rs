use std::path::{Path, PathBuf};

/// An AGENTS.md file discovered during walk-up discovery.
#[derive(Debug, Clone)]
pub struct AgentsFile {
    /// Absolute path to the AGENTS.md file.
    pub path: PathBuf,
    /// File content.
    pub content: String,
}

/// Load the AGENTS.md file from the project root, if it exists.
pub fn load_agents_md(project_root: &Path) -> Option<String> {
    let path = project_root.join("AGENTS.md");
    std::fs::read_to_string(path).ok()
}

/// Walk from `cwd` up to `project_root` (inclusive), collecting AGENTS.md files.
/// Returns them root-first (outermost to innermost / highest to lowest priority).
pub fn load_agents_md_chain(project_root: &Path, cwd: &Path) -> Vec<AgentsFile> {
    let mut files = Vec::new();
    // Guard: if cwd is not under project_root, fall back to project_root
    let effective_cwd = if cwd.starts_with(project_root) {
        cwd
    } else {
        project_root
    };
    let mut dir = effective_cwd.to_path_buf();
    loop {
        let agents_path = dir.join("AGENTS.md");
        if let Ok(content) = std::fs::read_to_string(&agents_path) {
            files.push(AgentsFile {
                path: agents_path,
                content,
            });
        }
        if dir == project_root {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    files.reverse(); // root-first order
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_md_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# My Agent\nHello").unwrap();
        let content = load_agents_md(dir.path());
        assert_eq!(content, Some("# My Agent\nHello".into()));
    }

    #[test]
    fn agents_md_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_agents_md(dir.path()).is_none());
    }

    #[test]
    fn agents_md_chain_single_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Root").unwrap();
        let chain = load_agents_md_chain(dir.path(), dir.path());
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].content, "# Root");
    }

    #[test]
    fn agents_md_chain_nested() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("AGENTS.md"), "# Root").unwrap();
        let sub = root.join("sub").join("dir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "# Sub").unwrap();

        let chain = load_agents_md_chain(root, &sub);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].content, "# Root", "root-first order");
        assert_eq!(chain[1].content, "# Sub", "subdirectory last");
    }

    #[test]
    fn agents_md_chain_middle_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("AGENTS.md"), "# Root").unwrap();
        let deep = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("AGENTS.md"), "# Deep").unwrap();

        let chain = load_agents_md_chain(root, &deep);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].content, "# Root");
        assert_eq!(chain[1].content, "# Deep");
    }

    #[test]
    fn agents_md_chain_none() {
        let dir = tempfile::tempdir().unwrap();
        let chain = load_agents_md_chain(dir.path(), dir.path());
        assert!(chain.is_empty());
    }

    #[test]
    fn agents_md_chain_cwd_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sub = root.join("pkg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "# Pkg only").unwrap();

        let chain = load_agents_md_chain(root, &sub);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].content, "# Pkg only");
    }

    #[test]
    fn agents_md_chain_cwd_outside_root_falls_back() {
        let root_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        std::fs::write(root_dir.path().join("AGENTS.md"), "# Root").unwrap();
        std::fs::write(other_dir.path().join("AGENTS.md"), "# Other").unwrap();

        // cwd is not under project_root — should fall back to project_root
        let chain = load_agents_md_chain(root_dir.path(), other_dir.path());
        assert_eq!(chain.len(), 1);
        assert_eq!(
            chain[0].content, "# Root",
            "should only find root, not the unrelated dir"
        );
    }
}
