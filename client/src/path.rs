//! Path comparisons.

use std::path::Path;

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
    #[case::identical("/nonexistent/a", "/nonexistent/a", true)]
    #[case::different("/nonexistent/a", "/nonexistent/b", false)]
    fn missing_paths_compare_as_written(#[case] a: &str, #[case] b: &str, #[case] expected: bool) {
        assert_eq!(is_same(Path::new(a), Path::new(b)), expected);
    }
}
