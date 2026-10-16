//! Search history recalled with the Up arrow.
//!
//! [`History`] is a pure in-memory list with a browsing cursor; the free
//! functions below load and store it.
//!
//! The process has no clean shutdown (it is terminated), so every commit
//! rewrites the whole file rather than saving on exit. The file lives in
//! `%APPDATA%\files\history.txt` so it roams with the config, unlike the
//! machine-local index. All I/O is best effort: on failure, history is simply
//! not persisted.

use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::util::latest_slot::LatestSlot;

/// Maximum number of remembered codes.
pub const MAX_ENTRIES: usize = 200;

/// Longest entry accepted, in bytes, so a corrupt file cannot flood the
/// search box.
const MAX_ENTRY_LEN: usize = 512;

/// Previously used codes, newest first, plus the browsing cursor.
///
/// The cursor is `Some` only while the user is browsing; nothing on screen may
/// show these codes while it is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    entries: Vec<String>,
    cursor: Option<usize>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds from loaded lines, applying the same dedupe and cap as `record`.
    pub fn from_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut history = Self::new();
        // Reversed: the file is newest-first and `record` pushes to the front.
        let loaded: Vec<String> = entries.into_iter().map(Into::into).collect();
        for entry in loaded.into_iter().rev() {
            history.record(&entry);
        }
        history
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Index of the highlighted entry while browsing.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn is_browsing(&self) -> bool {
        self.cursor.is_some()
    }

    /// Records a code at the front of the list. Returns whether the list
    /// changed.
    ///
    /// Duplicates are removed case-insensitively, matching the shares; the
    /// newest spelling is kept.
    pub fn record(&mut self, entry: &str) -> bool {
        let entry = entry.trim();
        if entry.is_empty() || entry.len() > MAX_ENTRY_LEN {
            return false;
        }

        // Exact comparison, so a re-entry in different case still replaces the
        // stored spelling below.
        if self.entries.first().is_some_and(|e| e == entry) {
            return false;
        }

        // A longer code extending the head replaces it, so `11-D` followed by
        // `11-D-0704` leaves one entry. Only the head is checked, since only
        // the previous entry can be part of the same typing sequence.
        // Shortening a code is intentional and keeps both entries.
        if self.supersedes_head(entry) {
            self.entries[0] = entry.to_string();
            self.cursor = None;
            return true;
        }

        self.entries.retain(|e| !e.eq_ignore_ascii_case(entry));
        self.entries.insert(0, entry.to_string());
        self.entries.truncate(MAX_ENTRIES);

        // The list shifted, so the cursor would point at the wrong entry.
        self.cursor = None;
        true
    }

    /// Whether `entry` extends the head entry (ASCII case-insensitive).
    fn supersedes_head(&self, entry: &str) -> bool {
        let Some(head) = self.entries.first() else {
            return false;
        };
        entry.len() > head.len()
            && entry.as_bytes()[..head.len()].eq_ignore_ascii_case(head.as_bytes())
    }

    /// Starts browsing at the newest code. Returns `None` when history is
    /// empty.
    ///
    /// Browsing only starts from an empty field, so no draft is saved.
    pub fn begin(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        self.cursor = Some(0);
        self.entries.first().map(String::as_str)
    }

    /// Steps one entry older, stopping at the oldest without wrapping.
    pub fn older(&mut self) -> Option<&str> {
        let cursor = self.cursor?;
        let next = (cursor + 1).min(self.entries.len().saturating_sub(1));
        self.cursor = Some(next);
        self.entries.get(next).map(String::as_str)
    }

    /// Steps one entry newer. Past the newest, returns `None` and stops
    /// browsing, as a shell does.
    pub fn newer(&mut self) -> Option<&str> {
        let cursor = self.cursor?;
        if cursor == 0 {
            self.cursor = None;
            return None;
        }
        let next = cursor - 1;
        self.cursor = Some(next);
        self.entries.get(next).map(String::as_str)
    }

    /// Moves the cursor to `index`, as clicking an entry does.
    pub fn select(&mut self, index: usize) -> Option<&str> {
        if index >= self.entries.len() {
            return None;
        }
        self.cursor = Some(index);
        self.entries.get(index).map(String::as_str)
    }

    /// Returns the highlighted entry.
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.cursor?).map(String::as_str)
    }

    /// Stops browsing, keeping the recalled code.
    pub fn accept(&mut self) {
        self.cursor = None;
    }

    /// Returns a copy of the entries for the writer thread.
    pub fn snapshot(&self) -> Vec<String> {
        self.entries.clone()
    }
}

/// Why history could not be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    /// `%APPDATA%` is unset or empty.
    NoLocation,
    Io(String),
}

impl HistoryError {
    pub fn detail(&self) -> String {
        match self {
            Self::NoLocation => "no profile directory to store history in".into(),
            Self::Io(e) => e.clone(),
        }
    }
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail())
    }
}

impl std::error::Error for HistoryError {}

/// Returns `%APPDATA%\files\history.txt`.
///
/// Derived independently of the config path so the two are not coupled.
pub fn default_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(appdata);
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(dir.join("files").join("history.txt"))
}

/// Reads the stored codes, newest first.
///
/// Never fails: an unreadable file yields nothing and invalid lines are
/// skipped.
pub fn load(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && line.len() <= MAX_ENTRY_LEN
                // Control characters indicate a damaged or foreign file.
                && !line.chars().any(char::is_control)
        })
        .map(str::to_string)
        .take(MAX_ENTRIES)
        .collect()
}

/// Writes the codes, newest first.
///
/// Writes a temp file, calls `sync_all`, then renames over the destination.
/// Renaming over is safe because this file is never memory-mapped (unlike the
/// index, which uses versioned names).
pub fn save(path: &Path, entries: &[String]) -> Result<(), HistoryError> {
    let parent = path.parent().ok_or(HistoryError::NoLocation)?;
    std::fs::create_dir_all(parent).map_err(|e| HistoryError::Io(e.to_string()))?;

    let tmp = parent.join(format!(
        "history-{:x}-{:x}.tmp",
        std::process::id(),
        crate::util::once::now_nanos()
    ));

    {
        let mut file = File::create(&tmp).map_err(|e| HistoryError::Io(e.to_string()))?;
        (|| -> std::io::Result<()> {
            for entry in entries.iter().take(MAX_ENTRIES) {
                file.write_all(entry.as_bytes())?;
                file.write_all(b"\n")?;
            }
            // Without this, a power loss can leave a renamed but empty file.
            file.sync_all()
        })()
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            HistoryError::Io(e.to_string())
        })?;
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        HistoryError::Io(e.to_string())
    })
}

/// Handle to the thread that owns the history file.
///
/// Fed through a [`LatestSlot`] so rapid commits coalesce into one write of
/// the newest snapshot; a single writer keeps writes in order.
pub struct Writer {
    slot: Arc<LatestSlot<Arc<Vec<String>>>>,
    handle: Option<JoinHandle<()>>,
}

impl Writer {
    /// Queues a list to be written, replacing any not yet written.
    pub fn store(&self, entries: Arc<Vec<String>>) {
        self.slot.put(entries);
    }

    /// Stops the thread, waiting for a write in progress.
    ///
    /// The wait is unbounded: the write is small and local, and abandoning it
    /// would lose the latest entry.
    pub fn shutdown(&mut self) {
        self.slot.close();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts the writer for `path`.
pub fn spawn_writer(path: PathBuf) -> std::io::Result<Writer> {
    let slot = Arc::new(LatestSlot::<Arc<Vec<String>>>::new());
    let worker = Arc::clone(&slot);
    let handle = std::thread::Builder::new()
        .name("files-history".into())
        .spawn(move || {
            while let Some(entries) = worker.take_blocking() {
                // Best effort: a failed write only loses recall next session.
                let _ = save(&path, &entries);
            }
        })?;
    Ok(Writer {
        slot,
        handle: Some(handle),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(entries: &[&str]) -> History {
        History::from_entries(entries.iter().copied())
    }

    #[test]
    fn the_newest_code_is_first() {
        let mut h = History::new();
        h.record("11-D-0704");
        h.record("P12345-001");
        assert_eq!(h.entries(), ["P12345-001", "11-D-0704"]);
    }

    #[test]
    fn repeating_a_code_moves_it_to_the_front_rather_than_duplicating_it() {
        let mut h = history(&["c", "b", "a"]);
        assert!(h.record("a"));
        assert_eq!(h.entries(), ["a", "c", "b"]);
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn the_same_code_in_different_case_is_the_same_job() {
        let mut h = history(&["11-D-0704"]);
        h.record("11-d-0704");
        assert_eq!(h.entries(), ["11-d-0704"], "newest spelling wins");
        assert_eq!(h.len(), 1);
    }

    /// Covers prefixes recorded when typing pauses past the quiet period.
    #[test]
    fn a_code_typed_on_the_way_to_a_longer_one_is_superseded_by_it() {
        let mut h = History::new();
        for step in ["11", "11-D", "11-D-07", "11-D-0704"] {
            h.record(step);
        }
        assert_eq!(h.entries(), ["11-D-0704"]);
        assert_eq!(h.len(), 1, "the list kept the walk as well as the code");
    }

    #[test]
    fn only_the_entry_just_recorded_can_be_superseded() {
        let mut h = history(&["P12345-001", "11-D"]);
        h.record("11-D-0704");
        assert_eq!(h.entries(), ["11-D-0704", "P12345-001", "11-D"]);
    }

    #[test]
    fn a_prefix_in_the_other_case_is_still_a_prefix() {
        let mut h = history(&["11-d"]);
        h.record("11-D-0704");
        assert_eq!(h.entries(), ["11-D-0704"]);
    }

    #[test]
    fn shortening_a_code_keeps_both() {
        let mut h = history(&["11-D-0704"]);
        h.record("11-D");
        assert_eq!(h.entries(), ["11-D", "11-D-0704"]);
    }

    /// Known trade-off: a distinct code that extends the head still replaces it.
    #[test]
    fn a_longer_code_sharing_a_prefix_replaces_it() {
        let mut h = history(&["11-D-07"]);
        h.record("11-D-0704");
        assert_eq!(h.entries(), ["11-D-0704"]);
    }

    #[test]
    fn recording_the_code_already_at_the_front_changes_nothing() {
        let mut h = history(&["a"]);
        assert!(!h.record("a"));
    }

    #[test]
    fn blank_and_oversized_entries_are_refused() {
        let mut h = History::new();
        assert!(!h.record("   "));
        assert!(!h.record(""));
        assert!(!h.record(&"x".repeat(MAX_ENTRY_LEN + 1)));
        assert!(h.is_empty());
    }

    #[test]
    fn the_list_is_capped() {
        let mut h = History::new();
        for i in 0..(MAX_ENTRIES + 50) {
            h.record(&format!("code-{i}"));
        }
        assert_eq!(h.len(), MAX_ENTRIES);
        assert_eq!(h.entries()[0], format!("code-{}", MAX_ENTRIES + 49));
    }

    #[test]
    fn browsing_walks_from_newest_to_oldest_and_stops() {
        let mut h = history(&["c", "b", "a"]);
        assert_eq!(h.begin(), Some("c"));
        assert_eq!(h.older(), Some("b"));
        assert_eq!(h.older(), Some("a"));
        assert_eq!(h.older(), Some("a"), "stops at the oldest, never wraps");
    }

    #[test]
    fn stepping_past_the_newest_ends_browsing() {
        let mut h = history(&["b", "a"]);
        h.begin();
        h.older();
        assert_eq!(h.newer(), Some("b"));
        assert_eq!(h.newer(), None, "past the newest");
        assert!(!h.is_browsing());
    }

    #[test]
    fn accepting_keeps_the_recalled_code() {
        let mut h = history(&["a"]);
        h.begin();
        h.accept();
        assert!(!h.is_browsing());
    }

    #[test]
    fn an_entry_can_be_jumped_to_directly() {
        let mut h = history(&["c", "b", "a"]);
        h.begin();
        assert_eq!(h.select(2), Some("a"));
        assert_eq!(h.cursor(), Some(2));
        assert_eq!(h.select(9), None, "out of range changes nothing");
        assert_eq!(h.cursor(), Some(2));
    }

    #[test]
    fn browsing_an_empty_history_does_nothing() {
        let mut h = History::new();
        assert_eq!(h.begin(), None);
        assert!(!h.is_browsing());
        assert_eq!(h.older(), None);
        assert_eq!(h.newer(), None);
    }

    #[test]
    fn recording_while_browsing_drops_the_cursor() {
        let mut h = history(&["b", "a"]);
        h.begin();
        h.record("new");
        assert!(!h.is_browsing());
    }

    #[test]
    fn a_saved_history_loads_back_in_the_same_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.txt");
        let entries = vec!["c".to_string(), "b".to_string(), "a".to_string()];

        save(&path, &entries).unwrap();
        assert_eq!(load(&path), entries);

        let reloaded = History::from_entries(load(&path));
        assert_eq!(reloaded.entries(), ["c", "b", "a"]);
    }

    #[test]
    fn no_temporary_files_are_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.txt");
        save(&path, &["a".to_string()]).unwrap();
        save(&path, &["b".to_string(), "a".to_string()]).unwrap();

        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn saving_creates_the_directory_when_it_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("history.txt");
        save(&path, &["a".to_string()]).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_missing_file_reads_as_no_history_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("absent.txt")).is_empty());
    }

    #[test]
    fn damaged_lines_are_skipped_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.txt");
        std::fs::write(&path, "good\n\n\u{7}bad\nalso-good\n").unwrap();
        assert_eq!(load(&path), ["good", "also-good"]);
    }

    #[test]
    fn the_writer_stores_what_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.txt");

        let mut writer = spawn_writer(path.clone()).unwrap();
        writer.store(Arc::new(vec!["11-D-0704".to_string()]));
        writer.shutdown();

        assert_eq!(load(&path), ["11-D-0704"]);
    }

    #[test]
    fn every_error_carries_a_usable_detail() {
        assert!(!HistoryError::NoLocation.detail().is_empty());
        assert_eq!(HistoryError::Io("boom".into()).detail(), "boom");
    }
}
