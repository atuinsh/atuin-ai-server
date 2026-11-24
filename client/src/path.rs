//! Path comparisons and display.

use std::path::Path;

/// `path` for display, with `~` for the home directory it is in.
#[must_use]
pub fn display_from_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(relative) => Path::new("~").join(relative).display().to_string(),
        None => path.display().to_string(),
    }
}

/// Whether `a` and `b` resolve to the same path, symlinks and all.
///
/// Both have to exist to be resolved; when either doesn't, only identical paths are the same.
#[must_use]
pub fn is_same(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    fn a_symlink_is_the_same_as_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(is_same(&link, &target));
    }

    #[rstest]
    #[case::in_home("/home/me/.atuin/ai.toml", Some("/home/me"), "~/.atuin/ai.toml")]
    #[case::elsewhere("/etc/atuin/ai.toml", Some("/home/me"), "/etc/atuin/ai.toml")]
    #[case::sibling_with_a_shared_prefix(
        "/home/meg/ai.toml",
        Some("/home/me"),
        "/home/meg/ai.toml"
    )]
    #[case::no_home("/home/me/ai.toml", None, "/home/me/ai.toml")]
    fn display_from_home_abbreviates(
        #[case] path: &str,
        #[case] home: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(display_from_home(Path::new(path), home.map(Path::new)), expected);
    }

    #[rstest]
    #[case::identical("/nonexistent/a", "/nonexistent/a", true)]
    #[case::different("/nonexistent/a", "/nonexistent/b", false)]
    fn missing_paths_compare_as_written(#[case] a: &str, #[case] b: &str, #[case] expected: bool) {
        assert_eq!(is_same(Path::new(a), Path::new(b)), expected);
    }
}
