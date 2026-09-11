//! Codes typed before, recalled with the Up arrow.
//!
//! Two halves that deliberately do not know about each other: [`History`] is a
//! pure in-memory list with a browsing cursor, driven from the state machine
//! and testable without a filesystem; the free functions at the bottom load
//! and store it.
//!
//! # Why it is written eagerly
//!
//! There is no keyboard quit - the window's close button is how the program
//! ends - so there is no clean shutdown to flush anything at. The process is
//! simply terminated. A history saved on exit would therefore be a history
//! that is never saved, so every commit writes the whole (tiny) file.
//!
//! # Where it lives
//!
//! `%APPDATA%\files\history.txt`, next to the config rather than beside the
//! index in `%LOCALAPPDATA%`. The index is a rebuildable cache of one
//! machine's drives and must not roam; the codes someone works on are theirs
//! and should follow them, exactly like their settings.
//!
//! # Failure
//!
//! Best effort throughout, matching the index cache: if the file cannot be
//! read or written the program runs normally and simply forgets between
//! sessions. Losing a convenience must never cost someone a search.

use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::util::latest_slot::LatestSlot;

/// How many codes are remembered.
///
/// Recall is a list someone scrolls, not an archive: past a couple of hundred
/// the Up arrow is the wrong tool and the file is still under 4 KB.
pub const MAX_ENTRIES: usize = 200;

/// The longest line accepted from disk, so a corrupted file cannot be loaded
/// into the search box.
const MAX_ENTRY_LEN: usize = 512;

/// Previously used codes, newest first, plus the browsing cursor.
///
/// `draft` holds whatever was half-typed when browsing started, so Esc can put
/// it back. Without it, reaching for history would silently destroy the code
/// someone was in the middle of entering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    entries: Vec<String>,
    cursor: Option<usize>,
    draft: Option<String>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds from loaded lines, applying the same dedupe and cap as `record`
    /// so a hand-edited file cannot produce a list the program would never
    /// have written.
    pub fn from_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut history = Self::new();
        // Reversed: the file is newest-first, and `record` pushes to the
        // front, so replaying it in order would invert the list.
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

    /// Which entry is highlighted, while browsing.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn is_browsing(&self) -> bool {
        self.cursor.is_some()
    }

    pub fn draft(&self) -> Option<&str> {
        self.draft.as_deref()
    }

    /// Remembers a code. Returns whether the list changed.
    ///
    /// Matching is case-insensitive because the shares are: `11-D-0704` and
    /// `11-d-0704` are the same job, and keeping both would fill the list with
    /// the same code in different clothes. The newest spelling wins, since
    /// that is the one just confirmed to work.
    pub fn record(&mut self, entry: &str) -> bool {
        let entry = entry.trim();
        if entry.is_empty() || entry.len() > MAX_ENTRY_LEN {
            return false;
        }

        let already_newest = self
            .entries
            .first()
            .is_some_and(|e| e.eq_ignore_ascii_case(entry));
        if already_newest {
            return false;
        }

        self.entries.retain(|e| !e.eq_ignore_ascii_case(entry));
        self.entries.insert(0, entry.to_string());
        self.entries.truncate(MAX_ENTRIES);

        // A list that just moved under the cursor would leave the highlight
        // pointing at a different code than the one being looked at.
        self.cursor = None;
        self.draft = None;
        true
    }

    /// Starts browsing, remembering `draft` so Esc can restore it.
    ///
    /// Returns the entry to show, or `None` when there is no history at all.
    pub fn begin(&mut self, draft: &str) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        if self.cursor.is_none() {
            self.draft = Some(draft.to_string());
        }
        self.cursor = Some(0);
        self.entries.first().map(String::as_str)
    }

    /// Steps one entry further back. Stops at the oldest rather than wrapping:
    /// wrapping in a recall list means silently jumping from the code someone
    /// wanted to one from last week.
    pub fn older(&mut self) -> Option<&str> {
        let cursor = self.cursor?;
        let next = (cursor + 1).min(self.entries.len().saturating_sub(1));
        self.cursor = Some(next);
        self.entries.get(next).map(String::as_str)
    }

    /// Steps one entry towards the newest.
    ///
    /// Past the newest it returns `None` and stops browsing, which the caller
    /// turns into "put the draft back" - the same thing a shell does.
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

    /// Jumps straight to an entry, as clicking one in the panel does.
    pub fn select(&mut self, index: usize) -> Option<&str> {
        if index >= self.entries.len() {
            return None;
        }
        self.cursor = Some(index);
        self.entries.get(index).map(String::as_str)
    }

    /// The entry currently highlighted.
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.cursor?).map(String::as_str)
    }

    /// Stops browsing, keeping whatever was recalled.
    pub fn accept(&mut self) {
        self.cursor = None;
        self.draft = None;
    }

    /// Stops browsing and hands back the half-typed code to restore.
    pub fn cancel(&mut self) -> Option<String> {
        self.cursor = None;
        self.draft.take()
    }

    /// A copy for the writer thread. Cheap: a few hundred short strings, built
    /// at most once per confirmed search.
    pub fn snapshot(&self) -> Vec<String> {
        self.entries.clone()
    }
}

/// Why history could not be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    /// No `%APPDATA%`, so there is nowhere to put it.
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

/// `%APPDATA%\files\history.txt`.
///
/// Derived the same way as the config path rather than shared with it, so the
/// two cannot be silently coupled if either moves.
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
/// Never fails: a missing, unreadable, or partly corrupt file yields the
/// entries that could be read, which is empty in the worst case.
pub fn load(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && line.len() <= MAX_ENTRY_LEN
                // A control character here means the file was damaged, or was
                // never a history file. Dropping the line is better than
                // loading something that would corrupt the display.
                && !line.chars().any(char::is_control)
        })
        .map(str::to_string)
        .take(MAX_ENTRIES)
        .collect()
}

/// Writes the codes, newest first.
///
/// Temp file, `sync_all`, then rename over the destination - the same shape as
/// the index's pointer file. Renaming over it is safe precisely because this
/// file is never memory-mapped, which is the constraint that forces the index
/// itself to use versioned names instead.
pub fn save(path: &Path, entries: &[String]) -> Result<(), HistoryError> {
    let parent = path.parent().ok_or(HistoryError::NoLocation)?;
    std::fs::create_dir_all(parent).map_err(|e| HistoryError::Io(e.to_string()))?;

    let tmp = parent.join(format!(
        "history-{:x}-{:x}.tmp",
        std::process::id(),
        now_nanos()
    ));

    {
        let mut file = File::create(&tmp).map_err(|e| HistoryError::Io(e.to_string()))?;
        (|| -> std::io::Result<()> {
            for entry in entries.iter().take(MAX_ENTRIES) {
                file.write_all(entry.as_bytes())?;
                file.write_all(b"\n")?;
            }
            // Without this a power loss can leave a renamed but empty file,
            // which reads back as "no history" rather than as a failure.
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

/// The thread that owns the history file.
///
/// Fed through a [`LatestSlot`] for the same reason the search worker is: two
/// commits in quick succession should produce one write of the newer list, not
/// two writes racing to rename over each other. Whole snapshots make that safe
/// - the loser is simply a list that is one entry out of date, and it never
/// lands after the winner because there is only one writer.
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
    /// The wait is unbounded, unlike the network-facing workers: this is a few
    /// kilobytes to a local disk, and the alternative is losing the very code
    /// that was just used.
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
                // Best effort, exactly like the index cache: a profile that
                // cannot be written to costs recall next session and nothing
                // else.
                let _ = save(&path, &entries);
            }
        })?;
    Ok(Writer {
        slot,
        handle: Some(handle),
    })
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
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
        // The shares are case-insensitive, so keeping both spellings would
        // fill the list with duplicates of one job.
        let mut h = history(&["11-D-0704"]);
        h.record("11-d-0704");
        assert_eq!(h.entries(), ["11-d-0704"], "newest spelling wins");
        assert_eq!(h.len(), 1);
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
        assert_eq!(h.begin("draft"), Some("c"));
        assert_eq!(h.older(), Some("b"));
        assert_eq!(h.older(), Some("a"));
        assert_eq!(h.older(), Some("a"), "stops at the oldest, never wraps");
    }

    #[test]
    fn stepping_past_the_newest_ends_browsing_so_the_draft_comes_back() {
        let mut h = history(&["b", "a"]);
        h.begin("11-D");
        h.older();
        assert_eq!(h.newer(), Some("b"));
        assert_eq!(h.newer(), None, "past the newest");
        assert!(!h.is_browsing());
    }

    #[test]
    fn cancelling_hands_back_what_was_being_typed() {
        let mut h = history(&["a"]);
        h.begin("11-D");
        h.older();
        assert_eq!(h.cancel(), Some("11-D".to_string()));
        assert!(!h.is_browsing());
    }

    #[test]
    fn accepting_keeps_the_recalled_code_and_forgets_the_draft() {
        let mut h = history(&["a"]);
        h.begin("11-D");
        h.accept();
        assert!(!h.is_browsing());
        assert_eq!(h.draft(), None);
    }

    #[test]
    fn an_entry_can_be_jumped_to_directly() {
        let mut h = history(&["c", "b", "a"]);
        h.begin("draft");
        assert_eq!(h.select(2), Some("a"));
        assert_eq!(h.cursor(), Some(2));
        assert_eq!(h.select(9), None, "out of range changes nothing");
        assert_eq!(h.cursor(), Some(2));
    }

    #[test]
    fn browsing_an_empty_history_does_nothing() {
        let mut h = History::new();
        assert_eq!(h.begin("11-D"), None);
        assert!(!h.is_browsing());
        assert_eq!(h.older(), None);
        assert_eq!(h.newer(), None);
    }

    #[test]
    fn recording_while_browsing_drops_the_cursor() {
        // The list is about to shift underneath it, so a kept cursor would
        // point at a different code than the one on screen.
        let mut h = history(&["b", "a"]);
        h.begin("draft");
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
