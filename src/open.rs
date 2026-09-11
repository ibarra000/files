//! Launching the viewer.
//!
//! The previous implementation was `let _ = Command::new("avwin.exe").arg(path).spawn();`,
//! so a missing viewer or a deleted file produced exactly nothing: the user
//! pressed Enter and the application appeared to ignore them.
//!
//! Here the launch is reported either way, and the viewer's presence is
//! probed once at startup so the user learns about a missing `avwin.exe`
//! before they need it rather than after.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use crossbeam_channel::Sender;

use crate::app::event::{AppEvent, OpenMsg};

/// The viewer this tool hands files to.
pub const VIEWER: &str = "avwin.exe";

/// Why a launch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    ViewerNotFound,
    FileMissing,
    Io(String),
}

impl OpenError {
    pub fn detail(&self) -> String {
        match self {
            Self::ViewerNotFound => format!("{VIEWER} not found on PATH"),
            Self::FileMissing => "the file no longer exists".into(),
            Self::Io(e) => e.clone(),
        }
    }
}

/// Launches the viewer for `path`.
pub fn open(path: &str) -> Result<(), OpenError> {
    // Checked first so a stale index produces a clear message rather than a
    // confusing viewer error.
    if !Path::new(path).exists() {
        return Err(OpenError::FileMissing);
    }
    Command::new(VIEWER)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => OpenError::ViewerNotFound,
            _ => OpenError::Io(e.to_string()),
        })
}

/// Whether the viewer can be found, for a startup warning.
///
/// Uses `where` on Windows rather than spawning the viewer itself.
pub fn viewer_available() -> bool {
    #[cfg(windows)]
    let probe = Command::new("where")
        .arg(VIEWER)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(not(windows))]
    let probe = Command::new("which")
        .arg(VIEWER)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    probe.map(|s| s.success()).unwrap_or(false)
}

/// Launches on a detached thread and reports the result.
///
/// Spawning a process can block briefly; the UI thread never should.
pub fn open_async(path: Arc<str>, events: Sender<AppEvent>) {
    let _ = std::thread::Builder::new()
        .name("files-open".into())
        .spawn(move || {
            let msg = match open(&path) {
                Ok(()) => OpenMsg::Launched { path },
                Err(err) => OpenMsg::Failed {
                    path,
                    detail: err.detail(),
                },
            };
            let _ = events.send(AppEvent::Open(msg));
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use std::time::Duration;

    #[test]
    fn a_missing_file_is_reported_before_the_viewer_is_involved() {
        let err = open("C:\\definitely-not-here-4a91\\nope.pdf").unwrap_err();
        assert_eq!(err, OpenError::FileMissing);
        assert!(err.detail().contains("no longer exists"));
    }

    #[test]
    fn a_missing_viewer_produces_an_actionable_message() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("real.pdf");
        std::fs::write(&file, b"x").unwrap();

        // avwin.exe is not present on a development machine, so this
        // exercises the real path.
        match open(file.to_str().unwrap()) {
            Err(OpenError::ViewerNotFound) => {
                assert!(OpenError::ViewerNotFound.detail().contains("avwin.exe"));
            }
            Err(other) => panic!("unexpected failure: {other:?}"),
            Ok(()) => {
                // A machine that genuinely has the viewer installed.
            }
        }
    }

    #[test]
    fn every_error_carries_a_usable_detail() {
        assert!(OpenError::ViewerNotFound.detail().contains(VIEWER));
        assert!(!OpenError::FileMissing.detail().is_empty());
        assert_eq!(OpenError::Io("boom".into()).detail(), "boom");
    }

    #[test]
    fn the_async_launcher_always_reports_back() {
        let (tx, rx) = bounded(4);
        open_async(Arc::from("C:\\definitely-not-here-4a91\\nope.pdf"), tx);

        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(AppEvent::Open(OpenMsg::Failed { detail, .. })) => {
                assert!(detail.contains("no longer exists"));
            }
            other => panic!("expected a failure report, got {other:?}"),
        }
    }

    #[test]
    fn probing_for_the_viewer_does_not_panic() {
        // The answer depends on the machine; only the absence of a panic is
        // being asserted.
        let _ = viewer_available();
    }
}
