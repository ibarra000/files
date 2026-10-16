//! Persists the panel's dragged position in `%APPDATA%\files\window.txt`.
//!
//! Kept out of `config.toml` because the config rejects unknown keys and
//! `Settings` is cloned into every worker, so it must not change at run time.
//! Like [`crate::history`], this is a best-effort, atomically rewritten file
//! with its own coalescing writer thread.
//!
//! Stored under `%APPDATA%` rather than `%LOCALAPPDATA%\files`, which is the
//! cache directory that `index::persist::gc_orphans` sweeps. A roamed position
//! is safe because [`crate::hotkey::geometry::place_at`] clamps it into the
//! nearest monitor's work area.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::util::latest_slot::LatestSlot;

/// Maximum accepted line length. Two `i32`s and a space need at most 22
/// characters; anything longer is treated as a foreign or damaged file.
const MAX_LEN: usize = 64;

/// An error storing or removing the position file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    /// The path has no parent directory.
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

/// Returns `%APPDATA%\files\window.txt`, or `None` if `APPDATA` is unset.
///
/// Derived independently of the config path, as in
/// [`crate::history::default_path`].
pub fn default_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(appdata);
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(dir.join("files").join("window.txt"))
}

/// Reads the saved top-left corner, in device pixels.
///
/// Returns `None` for a missing or malformed file; the panel then uses its
/// default placement.
pub fn load(path: &Path) -> Option<(i32, i32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() || line.len() > MAX_LEN {
        return None;
    }
    parse(line)
}

/// Parses `"<left> <top>"`.
fn parse(line: &str) -> Option<(i32, i32)> {
    let mut parts = line.split_ascii_whitespace();
    let left = parts.next()?.parse::<i32>().ok()?;
    let top = parts.next()?.parse::<i32>().ok()?;
    // Reject extra fields rather than guess at their meaning.
    if parts.next().is_some() {
        return None;
    }
    Some((left, top))
}

/// Writes the top-left corner, in device pixels.
///
/// Writes a temp file, syncs it, then renames it over `path`, as
/// [`crate::history::save`] does.
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
            // Without this a power loss can leave a renamed but empty file.
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

/// Deletes the saved position. A missing file is success.
pub fn forget(path: &Path) -> Result<(), PlacementError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PlacementError::Io(e.to_string())),
    }
}

/// A request to the writer thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Task {
    At(i32, i32),
    Forget,
}

/// Handle to the thread that owns the position file.
///
/// Requests go through a [`LatestSlot`], so only the newest is written. This
/// makes [`Writer::store`] cheap enough to call on every frame of a drag,
/// even when `%APPDATA%` is on a network share.
pub struct Writer {
    slot: Arc<LatestSlot<Task>>,
    handle: Option<JoinHandle<()>>,
}

impl Writer {
    /// Queues a position, replacing any not yet written.
    pub fn store(&self, at: (i32, i32)) {
        self.slot.put(Task::At(at.0, at.1));
    }

    /// Queues the removal of the remembered position.
    pub fn forget(&self) {
        self.slot.put(Task::Forget);
    }

    /// Stops the thread, waiting without a timeout for any pending write, as
    /// [`crate::history::Writer::shutdown`] does.
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
    let slot = Arc::new(LatestSlot::<Task>::new());
    let worker = Arc::clone(&slot);
    let handle = std::thread::Builder::new()
        .name("files-placement".into())
        .spawn(move || {
            while let Some(task) = worker.take_blocking() {
                // Best effort: a failed write only loses the position.
                let _ = match task {
                    Task::At(left, top) => save(&path, (left, top)),
                    Task::Forget => forget(&path),
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

    /// Monitors left of the primary have negative coordinates.
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

    /// The writer coalesces queued positions and writes the last one.
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
