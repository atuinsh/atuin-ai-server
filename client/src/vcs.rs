//! Finding the repository a directory is in.

use std::path::{Path, PathBuf};

/// The nearest repository at or above `path`, whichever VCS owns it, if any.
///
/// A git worktree resolves to its main repository, so all worktrees share one scope; a jj
/// workspace keeps its own root.
#[must_use]
pub fn root(path: &Path) -> Option<PathBuf> {
    for dir in path.ancestors().take_while(|dir| !dir.as_os_str().is_empty()) {
        if dir.join(".jj").join("repo").exists() {
            return Some(dir.to_path_buf());
        }

        if dir.join(".git").exists() {
            return Some(main_worktree(dir).unwrap_or_else(|| dir.to_path_buf()));
        }
    }

    None
}

/// The main repository of the linked git worktree checked out at `path`, if it is one.
///
/// A linked worktree's `.git` is a file holding `gitdir: <main>/.git/worktrees/<name>`, whose
/// `commondir` points on to the main `.git`. Like git, a relative pointer resolves from the real
/// location of its file, so `..` applies after symlinks.
fn main_worktree(path: &Path) -> Option<PathBuf> {
    let git_path = path.join(".git");
    if !git_path.is_file() {
        return None;
    }

    let contents = std::fs::read_to_string(&git_path).ok()?;
    let gitdir = contents.strip_prefix("gitdir: ")?.trim();
    let gitdir = std::fs::canonicalize(path.join(gitdir)).ok()?;

    // A submodule's gitdir is a repository of its own, with no `commondir`.
    let commondir = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let commondir = std::fs::canonicalize(gitdir.join(commondir.trim())).ok()?;

    // A bare repository has no main worktree to share.
    if commondir.file_name()? != ".git" {
        return None;
    }
    commondir.parent().map(ToOwned::to_owned)
}

#[cfg(all(test, not(windows)))]
mod tests {
    use rstest::{fixture, rstest};
    use tempfile::TempDir;

    use super::*;

    #[fixture]
    fn tmp() -> TempDir {
        tempfile::tempdir().expect("could not create a temp dir")
    }

    /// `<root>/.git` as a directory: an ordinary clone.
    fn make_git_repo(root: &Path) {
        std::fs::create_dir_all(root.join(".git")).unwrap();
    }

    /// `<root>/.git` as a file pointing into `<main>/.git/worktrees/<name>`.
    fn make_git_worktree(root: &Path, main: &Path, name: &str) {
        let git_dir = main.join(".git").join("worktrees").join(name);
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(".git"), format!("gitdir: {}", git_dir.display())).unwrap();
    }

    /// `<root>/.jj/repo` as a directory: a main jj workspace.
    fn make_jj_workspace(root: &Path) {
        std::fs::create_dir_all(root.join(".jj").join("repo")).unwrap();
    }

    /// A directory well below `root`.
    fn subdir(root: &Path) -> PathBuf {
        let deep = root.join("src").join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        deep
    }

    /// Canonical, as worktree resolution returns it: macOS's temp dir is behind a symlink.
    fn canonical(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap()
    }

    #[rstest]
    fn git_repo_resolves_to_its_root(tmp: TempDir) {
        make_git_repo(tmp.path());
        assert_eq!(root(&subdir(tmp.path())), Some(tmp.path().to_path_buf()));
    }

    #[rstest]
    fn git_worktree_resolves_to_the_main_repo(tmp: TempDir) {
        let main = tmp.path().join("main");
        make_git_repo(&main);
        let worktree = tmp.path().join("worktree");
        make_git_worktree(&worktree, &main, "worktree");
        assert_eq!(root(&subdir(&worktree)), Some(canonical(&main)));
    }

    #[rstest]
    fn jj_workspace_resolves_to_its_root(tmp: TempDir) {
        make_jj_workspace(tmp.path());
        assert_eq!(root(&subdir(tmp.path())), Some(tmp.path().to_path_buf()));
    }

    #[rstest]
    fn the_nearest_repository_wins(tmp: TempDir) {
        make_git_repo(tmp.path());
        let inner = tmp.path().join("vendor").join("dep");
        make_git_repo(&inner);
        assert_eq!(root(&subdir(&inner)), Some(inner));
    }

    #[rstest]
    fn outside_a_repository_there_is_none(tmp: TempDir) {
        assert_eq!(root(&subdir(tmp.path())), None);
    }
}
