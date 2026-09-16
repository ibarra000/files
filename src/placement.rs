//! Where the user put the panel.
//!
//! The panel is summoned and dismissed dozens of times an hour, and until now
//! it landed in the same computed spot every time: centred horizontally, top
//! edge twelve per cent down the work area. That is the right *default* and the
//! wrong *rule* - the one place a search panel must not cover is the drawing
//! somebody summoned it to look for, and only they know where that is.
//!
//! So a drag moves it, and this module is the half that makes the move outlive
//! the process.
//!
//! # Why this is not in the configuration file
//!
//! `config::file` treats an unknown key as a hard startup error, which is the
//! right policy for a file somebody hand-edits and the wrong one for a value a
//! mouse writes sixty times a second. More to the point, `Settings` is cloned
//! into every worker, and its own module note says so: a field that changes at
//! run time would be one truth with several stale copies of it.
//!
//! This is the same shape as [`crate::history`] instead - a small, best-effort,
//! atomically-rewritten runtime file beside the config rather than in it, with
//! its own error type and its own coalescing writer thread.
//!
//! # Why `%APPDATA%` and not `%LOCALAPPDATA%`
//!
//! A position is arguably machine-specific, because a monitor layout is. But
//! `%LOCALAPPDATA%\files` is `cache_dir`, which the user may redirect and which
//! `index::persist::gc_orphans` sweeps, and neither of those should be able to
//! reach this. Roaming it is safe because nothing here is trusted on the way
//! back in: [`crate::hotkey::geometry::place_at`] clamps a restored point into
//! the work area of whatever monitor it lands nearest, so the worst a position
//! from another machine can do is put the panel against an edge.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::util::latest_slot::LatestSlot;

/// Longest line this will read before deciding the file is not one of ours.
///
/// Two signed integers and a space is nineteen characters at the very most.
/// Anything longer is a damaged file or a different file, and either way the
/// honest answer is the default placement.
const MAX_LEN: usize = 64;

/// Why a position could not be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    /// No `%APPDATA%`, so there is nowhere to put it.
    NoLocation,
    Io(String),
}

impl PlacementError {
    pub fn detail(&self) -> String {
        match self {
            Self::NoLocation => "no profile directory to store the position in".into(),
            Self::Io(e) => e.clone(),
        }
    }
}

impl std::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail())
    }
}

impl std::error::Error for PlacementError {}

/// `%APPDATA%\files\window.txt`.
///
/// Derived the same way as the config path rather than shared with it, for the
/// reason [`crate::history::default_path`] gives: the two must not become
/// silently coupled if either moves.
pub fn default_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(appdata);
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(dir.join("files").join("window.txt"))
}

/// Reads the remembered top-left corner, in device pixels.
///
/// Never fails. A missing file is the ordinary case - it means nobody has moved
/// the panel yet - and a damaged one is treated the same way, because the cost
/// of being wrong is a panel in the default place and the cost of reporting it
/// is a message about a file the user has never heard of.
pub fn load(path: &Path) -> Option<(i32, i32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() || line.len() > MAX_LEN {
        return None;
    }
    parse(line)
}

/// One line, two integers. Separate from [`load`] so the parsing is testable
/// without a filesystem.
fn parse(line: &str) -> Option<(i32, i32)> {
    let mut parts = line.split_ascii_whitespace();
    let left = parts.next()?.parse::<i32>().ok()?;
    let top = parts.next()?.parse::<i32>().ok()?;
    // A third field means this is not the file this program writes, and
    // guessing at what the first two meant is how a panel ends up somewhere
    // nobody asked for.
    if parts.next().is_some() {
        return None;
    }
    Some((left, top))
}

/// Writes the top-left corner, in device pixels.
///
/// Temp file, `sync_all`, then rename over the destination - the same shape as
/// [`crate::history::save`] and for the same reasons.
pub fn save(path: &Path, at: (i32, i32)) -> Result<(), PlacementError> {
    let parent = path.parent().ok_or(PlacementError::NoLocation)?;
    std::fs::create_dir_all(parent).map_err(|e| PlacementError::Io(e.to_string()))?;

    let tmp = parent.join(format!(
        "window-{:x}-{:x}.tmp",
        std::process::id(),
        crate::util::once::now_nanos()
    ));

    {
        let mut file = File::create(&tmp).map_err(|e| PlacementError::Io(e.to_string()))?;
        (|| -> std::io::Result<()> {
            writeln!(file, "{} {}", at.0, at.1)?;
            // Without this a power loss can leave a renamed but empty file,
            // which reads back as "never moved" rather than as a failure.
            file.sync_all()
        })()
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            PlacementError::Io(e.to_string())
        })?;
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        PlacementError::Io(e.to_string())
    })
}

/// Removes the remembered position, so the panel goes back to being placed.
///
/// A missing file is success: the caller asked for there to be no remembered
/// position, and there is none.
pub fn forget(path: &Path) -> Result<(), PlacementError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PlacementError::Io(e.to_string())),
    }
}

/// What the writer thread is being asked to do.
///
/// An enum rather than an `Option<(i32, i32)>` because the two are not the same
/// instruction: `None` would have to mean either "forget it" or "nothing to
/// say", and a writer that cannot tell them apart deletes the file every time
/// the queue is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    At(i32, i32),
    Forget,
}

/// The thread that owns the position file.
///
/// Fed through a [`LatestSlot`] because a drag produces one position per frame
/// and the only one worth a round trip to `%APPDATA%` - which often roams to an
/// SMB path - is the last one. Coalescing is what makes it safe to call
/// [`Writer::store`] from the drawing thread on every frame of a drag rather
/// than having the caller debounce.
pub struct Writer {
    slot: Arc<LatestSlot<Job>>,
    handle: Option<JoinHandle<()>>,
}

impl Writer {
    /// Queues a position, replacing any not yet written.
    pub fn store(&self, at: (i32, i32)) {
        self.slot.put(Job::At(at.0, at.1));
    }

    /// Queues the removal of the remembered position.
    pub fn forget(&self) {
        self.slot.put(Job::Forget);
    }

    /// Stops the thread, waiting for a write in progress.
    ///
    /// The wait is unbounded, exactly as [`crate::history::Writer::shutdown`]
    /// is: this is one short line to a local disk, and the alternative is
    /// losing the move the user just made.
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
    let slot = Arc::new(LatestSlot::<Job>::new());
    let worker = Arc::clone(&slot);
    let handle = std::thread::Builder::new()
        .name("files-placement".into())
        .spawn(move || {
            while let Some(job) = worker.take_blocking() {
                // Best effort, like the index cache and the history: a profile
                // that cannot be written to costs the panel its position next
                // session and nothing else.
                let _ = match job {
                    Job::At(left, top) => save(&path, (left, top)),
                    Job::Forget => forget(&path),
                };
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

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    #[test]
    fn a_saved_position_reads_back_unchanged() {
        let dir = temp();
        let path = dir.path().join("files").join("window.txt");
        save(&path, (1234, -56)).expect("the write succeeded");
        assert_eq!(load(&path), Some((1234, -56)));
    }

    /// Negative coordinates are the ordinary case for a second monitor placed
    /// to the left of the primary one, which is exactly the layout a remembered
    /// position exists to serve.
    #[test]
    fn a_position_on_a_monitor_left_of_the_primary_one_survives() {
        let dir = temp();
        let path = dir.path().join("window.txt");
        save(&path, (-1920, 200)).expect("the write succeeded");
        assert_eq!(load(&path), Some((-1920, 200)));
    }

    #[test]
    fn a_file_that_is_not_there_is_no_position_rather_than_an_error() {
        let dir = temp();
        assert_eq!(load(&dir.path().join("window.txt")), None);
    }

    /// Everything a damaged or foreign file can look like. Each of these must
    /// read as "no remembered position", because the alternative is a panel
    /// placed from a number that meant something else.
    #[test]
    fn a_file_that_is_not_ours_is_no_position() {
        for bad in [
            "",
            "   ",
            "hello",
            "12",
            "12 34 56",
            "12.5 34",
            "99999999999999999999 0",
            "12,34",
        ] {
            assert_eq!(parse(bad), None, "{bad:?} was accepted");
        }
    }

    #[test]
    fn the_temporary_file_does_not_survive_a_successful_save() {
        let dir = temp();
        let path = dir.path().join("window.txt");
        save(&path, (10, 20)).expect("the write succeeded");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the directory is readable")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a temp file was left behind: {leftovers:?}"
        );
    }

    /// Writing twice must leave the second position, not append to the first.
    #[test]
    fn a_second_save_replaces_the_first() {
        let dir = temp();
        let path = dir.path().join("window.txt");
        save(&path, (1, 2)).expect("the first write succeeded");
        save(&path, (3, 4)).expect("the second write succeeded");
        assert_eq!(load(&path), Some((3, 4)));
    }

    #[test]
    fn forgetting_a_position_that_was_never_saved_is_not_a_failure() {
        let dir = temp();
        let path = dir.path().join("window.txt");
        assert_eq!(forget(&path), Ok(()));

        save(&path, (5, 6)).expect("the write succeeded");
        assert_eq!(forget(&path), Ok(()));
        assert_eq!(load(&path), None);
    }

    /// The writer coalesces, so the file holds the last position rather than
    /// one of the sixty a drag produced.
    #[test]
    fn the_writer_stores_the_last_position_it_was_given() {
        let dir = temp();
        let path = dir.path().join("window.txt");
        let mut writer = spawn_writer(path.clone()).expect("the thread started");
        for x in 0..50 {
            writer.store((x, x * 2));
        }
        writer.shutdown();
        assert_eq!(load(&path), Some((49, 98)));
    }

    #[test]
    fn the_writer_can_be_asked_to_forget() {
        let dir = temp();
        let path = dir.path().join("window.txt");
        let mut writer = spawn_writer(path.clone()).expect("the thread started");
        writer.store((7, 8));
        writer.forget();
        writer.shutdown();
        assert_eq!(load(&path), None);
    }
}
