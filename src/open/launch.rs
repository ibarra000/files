//! Handing a finished file to a program.
//!
//! Three routes, and the choice between them is not arbitrary:
//!
//! * **`avwin.exe`** by name on `PATH`, as this program has always done.
//! * **A configured `pdf_viewer`**, spawned directly. Only ever given a
//!   `.pdf`, because the file the user picked is not always one - see
//!   [`open_pdf`].
//! * **The system's `.pdf` association**, via `ShellExecuteW`. The default,
//!   because which PDF reader someone wants is their decision and Windows
//!   already records it.
//!
//! # Why `ShellExecuteW` and not `cmd /C start`
//!
//! `start` reads a first quoted argument as a *window title*, so
//! `start "C:\path with spaces\doc.pdf"` opens an empty console window named
//! after the document. Working around that means an empty `""` title argument,
//! and even then `&` in a path still breaks the command line. `ShellExecuteW`
//! takes the path as a parameter rather than as text to be re-parsed, so there
//! is nothing to quote and nothing to escape.

use std::path::Path;
use std::process::{Command, Stdio};

/// The legacy viewer, looked up on `PATH`.
pub const AVWIN: &str = "avwin.exe";

/// Why a launch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// The viewer itself is not installed, named so the message can say which.
    ViewerNotFound(String),
    /// Windows has no program registered for this kind of file. Carries the
    /// extension, because the degrade path opens whatever the cursor was on -
    /// which is not always a PDF, and blaming PDFs for an unopenable `.dwg`
    /// sends the reader looking in the wrong place.
    NoHandler(String),
    Io(String),
}

impl LaunchError {
    pub fn detail(&self) -> String {
        match self {
            Self::ViewerNotFound(name) => format!("{name} not found on PATH"),
            Self::NoHandler(kind) if kind.is_empty() => {
                "no program is registered to open this kind of file".into()
            }
            Self::NoHandler(kind) => {
                format!("no program is registered to open {kind} files on this machine")
            }
            Self::Io(e) => e.clone(),
        }
    }
}

/// Opens `path` with `avwin.exe`.
pub fn open_avwin(path: &str) -> Result<(), LaunchError> {
    spawn(AVWIN, path)
}

/// Opens `path` for the PDF viewer.
///
/// `configured` is used only when `path` really is a PDF. Under the rule that
/// a selected row outside the code's page group opens on its own, the target
/// can be a `.doc` or a `.dwg`, and handing that to a program chosen for its
/// PDF rendering would be worse than useless. Those go to the shell, which
/// knows what to do with them.
pub fn open_pdf(path: &str, configured: Option<&Path>) -> Result<(), LaunchError> {
    match configured {
        Some(exe) if is_pdf(path) => spawn(&exe.to_string_lossy(), path),
        _ => shell_open(path),
    }
}

/// The extension, upper-cased, for an error message. Empty when there is none.
fn extension_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_uppercase())
        .unwrap_or_default()
}

fn is_pdf(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Runs a program with one argument, detached.
///
/// Stdio is nulled so a chatty viewer cannot write over the terminal this
/// program is drawing in, and the child handle is dropped rather than waited
/// on - the viewer outliving the search is the point.
fn spawn(program: &str, path: &str) -> Result<(), LaunchError> {
    Command::new(program)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LaunchError::ViewerNotFound(program.to_string()),
            _ => LaunchError::Io(e.to_string()),
        })
}

/// Opens a file with whatever is registered for its type.
#[cfg(windows)]
fn shell_open(path: &str) -> Result<(), LaunchError> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    /// `ShellExecuteW` returns a fake `HINSTANCE`. Anything above this is
    /// success; at or below it the value is an error code. A genuinely
    /// terrible interface, and the threshold is the documented contract.
    const SUCCESS_THRESHOLD: isize = 32;
    /// No application is associated with this file type.
    const SE_ERR_NOASSOC: isize = 31;
    const SE_ERR_FNF: isize = 2;

    let file = crate::index::win_util::wide_path(Path::new(path), false);
    let verb: [u16; 5] = [b'o' as u16, b'p' as u16, b'e' as u16, b'n' as u16, 0];

    // The shell wants COM on the calling thread. This runs on the dedicated
    // open worker, which does nothing else, so initialising it here is both
    // correct and harmless; an already-initialised thread returns S_FALSE,
    // which is not an error.
    //
    // SAFETY: both arguments are the documented "no aggregate, default
    // options" values, and this thread makes no other COM calls.
    unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
    }

    // SAFETY: `file` and `verb` are NUL-terminated UTF-16 buffers that outlive
    // the call; the remaining pointers are null, which the API documents as
    // "no parameters", "no working directory" and "no owner window".
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;

    match result {
        r if r > SUCCESS_THRESHOLD => Ok(()),
        SE_ERR_NOASSOC => Err(LaunchError::NoHandler(extension_of(path))),
        SE_ERR_FNF => Err(LaunchError::Io("the file no longer exists".into())),
        other => Err(LaunchError::Io(format!(
            "the shell refused to open it (code {other})"
        ))),
    }
}

/// Off Windows there is no association to consult, so this exists to keep the
/// module compiling and testable on a development machine.
#[cfg(not(windows))]
fn shell_open(path: &str) -> Result<(), LaunchError> {
    spawn("xdg-open", path)
}

/// Whether `avwin.exe` can be found, for the startup warning.
///
/// Asks `where` rather than spawning the viewer: the point is to warn someone
/// before they need it, not to open a window they did not ask for.
pub fn avwin_available() -> bool {
    #[cfg(windows)]
    let mut probe = Command::new("where");
    #[cfg(not(windows))]
    let mut probe = Command::new("which");

    probe
        .arg(AVWIN)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_carries_a_usable_detail() {
        assert!(
            LaunchError::ViewerNotFound(AVWIN.into())
                .detail()
                .contains("avwin.exe")
        );
        assert!(
            LaunchError::NoHandler("PDF".into())
                .detail()
                .contains("PDF files")
        );
        assert!(
            LaunchError::NoHandler(String::new())
                .detail()
                .contains("this kind of file")
        );
        assert_eq!(LaunchError::Io("boom".into()).detail(), "boom");
    }

    /// A missing viewer has to name itself: "not found on PATH" alone leaves
    /// the reader guessing which program the program means.
    #[test]
    fn a_missing_viewer_names_itself() {
        let err = spawn("definitely-not-installed-4a91.exe", r"C:\x.pdf").unwrap_err();
        assert_eq!(
            err,
            LaunchError::ViewerNotFound("definitely-not-installed-4a91.exe".into())
        );
        assert!(err.detail().contains("definitely-not-installed-4a91.exe"));
    }

    #[test]
    fn only_a_pdf_is_sent_to_the_configured_viewer() {
        assert!(is_pdf(r"V:\Documents\custpro\11-d-0704.pdf"));
        assert!(is_pdf(r"V:\a.PDF"), "the extension folds");
        assert!(!is_pdf(r"R:\11d\11-D-0704 notes.doc"));
        assert!(!is_pdf(r"R:\11d\drawing.dwg"));
        assert!(!is_pdf(r"R:\11d\no-extension"));
    }

    /// The degrade path opens the row the cursor was on, which is not always a
    /// PDF. Blaming PDFs for an unopenable `.dwg` sends the reader looking in
    /// entirely the wrong place.
    #[test]
    fn a_missing_association_names_the_kind_of_file_it_could_not_open() {
        assert_eq!(extension_of(r"R:	d\drawing.dwg"), "DWG");
        assert_eq!(extension_of(r"R:	d.PdF"), "PDF");
        assert_eq!(
            extension_of(
                r"R:	d
o-extension"
            ),
            ""
        );
        assert!(
            LaunchError::NoHandler(extension_of(r"R:	d\drawing.dwg"))
                .detail()
                .contains("DWG")
        );
    }

    /// The answer depends on the machine; only the absence of a panic and of a
    /// spawned viewer window is being asserted.
    #[test]
    fn probing_for_avwin_does_not_panic() {
        let _ = avwin_available();
    }
}
