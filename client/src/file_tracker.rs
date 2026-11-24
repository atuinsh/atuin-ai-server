//! Tracks which files have been read in the current session, for freshness
//! checking before edits.
//!
//! The tracker records the content hash and mtime of each file at the time
//! it was last read. Before an edit, the tracker verifies the file hasn't
//! changed since the last read — catching both external modifications and
//! concurrent tool calls.
//!
//! Persisted as JSON in session metadata so it survives across CLI
//! invocations within the same logical session.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eyre::Result;
use serde::{Deserialize, Serialize};

/// Metadata key used for session_metadata persistence.
pub const METADATA_KEY: &str = "file_read_tracker";

/// State recorded for a single file read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadState {
    /// Hash of the file contents at the time of the last read.
    pub content_hash: u64,
    /// File mtime (as milliseconds since epoch) at the time of the last read.
    /// Millisecond precision ensures sub-second modifications are detected.
    pub mtime_ms: i64,
}

impl FileReadState {
    /// The file at `path` as it is now. Hashed in chunks, so a huge file costs no more memory than
    /// a small one.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let mtime_ms = unix_millis(std::fs::metadata(path)?.modified()?);
        Ok(Self {
            content_hash: hash_file(path)?,
            mtime_ms,
        })
    }
}

/// [`FileReadTracker::hash_content`] of the file at `path`, read in chunks.
fn hash_file(path: &Path) -> std::io::Result<u64> {
    const CHUNK_BYTES: usize = 64 * 1024;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut chunk = vec![0; CHUNK_BYTES];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => return Ok(hasher.digest()),
            Ok(n) => hasher.update(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Tracks file read state for freshness checking.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FileReadTracker {
    reads: HashMap<PathBuf, FileReadState>,
}

/// Result of a freshness check.
pub enum FreshnessCheck {
    /// File is fresh — the content hasn't changed since the last read.
    Fresh,
    /// File has never been read in this session.
    NotRead,
    /// File has been modified since the last read.
    Stale,
}

impl FileReadTracker {
    /// Record what a `read_file` saw of the file at `path`, which should be canonical.
    pub fn record(&mut self, path: PathBuf, state: FileReadState) {
        self.reads.insert(path, state);
    }

    /// Record that a file was read, with `content` and `mtime`. The `path` should be canonical
    /// (absolute, tilde-expanded).
    #[cfg(test)]
    pub fn record_read(&mut self, path: PathBuf, content: &[u8], mtime: SystemTime) {
        let content_hash = Self::hash_content(content);
        let mtime_ms = unix_millis(mtime);

        self.reads.insert(path, FileReadState {
            content_hash,
            mtime_ms,
        });
    }

    /// Check whether a file is fresh (unchanged since last read).
    ///
    /// Uses mtime as a fast path — only re-hashes if mtime differs.
    pub fn check_freshness(&self, path: &Path) -> Result<FreshnessCheck> {
        let Some(state) = self.reads.get(path) else {
            return Ok(FreshnessCheck::NotRead);
        };

        // Stat the file
        // file deleted or inaccessible
        let Ok(metadata) = std::fs::metadata(path) else {
            return Ok(FreshnessCheck::Stale);
        };

        let current_mtime_ms = unix_millis(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH));

        // Fast path: mtime unchanged → fresh
        if current_mtime_ms == state.mtime_ms {
            return Ok(FreshnessCheck::Fresh);
        }

        // Mtime changed — re-hash to confirm
        let current_hash = hash_file(path)?;

        if current_hash == state.content_hash {
            Ok(FreshnessCheck::Fresh)
        } else {
            Ok(FreshnessCheck::Stale)
        }
    }

    /// Update the tracker entry after a successful edit (new content written).
    pub fn update_after_edit(&mut self, path: &Path, new_content: &[u8], new_mtime: SystemTime) {
        let content_hash = Self::hash_content(new_content);
        let mtime_ms = unix_millis(new_mtime);

        self.reads.insert(path.to_path_buf(), FileReadState {
            content_hash,
            mtime_ms,
        });
    }

    /// Serialize to JSON for session metadata persistence.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Deserialize from JSON session metadata.
    pub fn from_json(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    fn hash_content(content: &[u8]) -> u64 {
        xxhash_rust::xxh3::xxh3_64(content)
    }
}

/// Milliseconds since the Unix epoch: `0` before it, `i64::MAX` past what fits.
fn unix_millis(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    #[rstest::rstest]
    fn a_file_hashed_in_chunks_hashes_as_a_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.log");
        let content: Vec<u8> = (0..200_000u32).flat_map(u32::to_le_bytes).collect();
        std::fs::write(&path, &content).unwrap();

        let state = FileReadState::of(&path).unwrap();
        assert_eq!(state.content_hash, FileReadTracker::hash_content(&content));
    }

    use std::io::Write;

    use rstest::*;
    use tempfile::NamedTempFile;

    use super::*;

    #[fixture]
    fn recorded() -> (FileReadTracker, NamedTempFile) {
        let mut tracker = FileReadTracker::default();
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "original").unwrap();

        let path = tmp.path().to_path_buf();
        let content = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        tracker.record_read(path, &content, mtime);

        (tracker, tmp)
    }

    #[rstest]
    fn record_and_check_fresh(#[from(recorded)] (tracker, tmp): (FileReadTracker, NamedTempFile)) {
        assert!(matches!(tracker.check_freshness(tmp.path()).unwrap(), FreshnessCheck::Fresh));
    }

    #[test]
    fn check_not_read() {
        let tracker = FileReadTracker::default();
        let path = PathBuf::from("/nonexistent/file.txt");
        assert!(matches!(tracker.check_freshness(&path).unwrap(), FreshnessCheck::NotRead));
    }

    #[rstest]
    fn check_stale_after_modification(
        #[from(recorded)] (tracker, tmp): (FileReadTracker, NamedTempFile),
    ) {
        // Small delay to ensure the filesystem mtime advances
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Modify the file
        std::fs::write(tmp.path(), "modified").unwrap();

        assert!(matches!(tracker.check_freshness(tmp.path()).unwrap(), FreshnessCheck::Stale));
    }

    #[rstest]
    fn update_after_edit_makes_fresh(
        #[from(recorded)] (mut tracker, tmp): (FileReadTracker, NamedTempFile),
    ) {
        // Simulate an edit
        let new_content = b"edited content";
        std::fs::write(tmp.path(), new_content).unwrap();
        let new_mtime = std::fs::metadata(tmp.path()).unwrap().modified().unwrap();
        tracker.update_after_edit(tmp.path(), new_content, new_mtime);

        assert!(matches!(tracker.check_freshness(tmp.path()).unwrap(), FreshnessCheck::Fresh));
    }

    #[test]
    fn roundtrip_json() {
        let mut tracker = FileReadTracker::default();
        tracker.reads.insert(PathBuf::from("/some/file.toml"), FileReadState {
            content_hash: 12345,
            mtime_ms: 1_700_000_000_000,
        });

        let json = tracker.to_json().unwrap();
        let restored = FileReadTracker::from_json(&json).unwrap();
        assert_eq!(restored.reads.len(), 1);
        assert_eq!(restored.reads[&PathBuf::from("/some/file.toml")].content_hash, 12345);
    }
}
