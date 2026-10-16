//! Launching a program on a file.
//!
//! - A configured `[[viewer]]` program, by name on `PATH` or by full path.
//! - A configured `pdf_viewer`, given only `.pdf` files (see
//!   [`open_associated`]).
//! - The file's association via `ShellExecuteW`; the default, and the only
//!   route under `Auto`.
//!
//! `ShellExecuteW` is used instead of `cmd /C start` because `start` treats a
//! quoted first argument as a window title and `&` in a path breaks its
//! command line; `ShellExecuteW` takes the path as a parameter.

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Why a launch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// The named viewer is not installed.
    ViewerNotFound(String),
    /// No program is registered for the file type. Carries the extension, since
    /// the file is not always a PDF; the message points to F2 for a viewer.
    NoHandler(String),
    Io(String),
}

impl LaunchError {
    pub fn detail(&self) -> String {
        match self {
            Self::ViewerNotFound(name) => format!("{name} not found on PATH"),
            Self::NoHandler(kind) if kind.is_empty() => {
                "nothing is registered to open this kind of file \u{b7} F2 switches viewer".into()
            }
            Self::NoHandler(kind) => {
                format!("nothing is registered to open {kind} files \u{b7} F2 switches viewer")
            }
            Self::Io(e) => e.clone(),
        }
    }
}

/// Opens `path` with a configured viewer's `program`.
pub fn open_with(program: &str, path: &str) -> Result<(), LaunchError> {
    spawn(program, path)
}

/// Opens `path` with its associated program.
///
/// `configured` is used only when `path` is a PDF; any other file type goes to
/// the shell.
pub fn open_associated(path: &str, configured: Option<&Path>) -> Result<(), LaunchError> {
    match configured {
        Some(exe) if is_pdf(path) => spawn(&exe.to_string_lossy(), path),
        _ => shell_open(path),
    }
}

/// Returns the upper-cased extension for an error message, or empty.
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

/// Runs a program with one argument, detached and maximized.
///
/// The show state must be set at launch: it only applies to the child's first
/// `ShowWindow` call, and without it a viewer restores its last placement,
/// which may be minimized.
fn spawn(program: &str, path: &str) -> Result<(), LaunchError> {
    #[cfg(windows)]
    {
        // `ShellExecuteW` instead of `Command`: `CreateProcessW` ignores
        // `wShowWindow` without `STARTF_USESHOWWINDOW`, and
        // `CommandExt::show_window` is unstable. It resolves bare names
        // against `PATH` and App Paths as `Command` does.
        //
        // Quoting is safe because a Windows file name cannot contain `"`.
        let quoted = format!("\"{path}\"");
        shell_execute(program, Some(&quoted)).map_err(|e| match e {
            // Here "file not found" refers to the program, not the document.
            LaunchError::Io(_) if !Path::new(program).is_file() => {
                LaunchError::ViewerNotFound(program.to_string())
            }
            other => other,
        })
    }

    #[cfg(not(windows))]
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

/// Allows the next launched program to take the foreground.
///
/// Windows only lets the foreground process grant this, so it must run on the
/// panel's thread before the panel is dismissed, not on the open worker.
/// `ASFW_ANY` because the target process id is unknown (or, for the shell
/// route, belongs to an already-running process).
#[cfg(windows)]
pub fn allow_foreground_handover() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    // SAFETY: no pointers, no handles. Returns zero harmlessly when this
    // process is not in the foreground.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

/// No-op off Windows.
#[cfg(not(windows))]
pub fn allow_foreground_handover() {}

/// Opens a file with the program registered for its type. Also used by the
/// settings window to open the configuration file.
#[cfg(windows)]
pub fn shell_open(path: &str) -> Result<(), LaunchError> {
    shell_execute(path, None)
}

/// Opens Explorer with `path` selected, via `explorer.exe /select,"<path>"`.
///
/// Uses a raw argument so only the path is quoted; quoting the whole
/// `/select,` switch makes Explorer open Documents with nothing selected.
/// Explorer's exit code is meaningless, so only a failure to start is reported.
#[cfg(windows)]
pub fn reveal(path: &str) -> Result<(), LaunchError> {
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LaunchError::ViewerNotFound("explorer.exe".into()),
            _ => LaunchError::Io(e.to_string()),
        })
}

/// Always fails off Windows.
#[cfg(not(windows))]
pub fn reveal(path: &str) -> Result<(), LaunchError> {
    let _ = path;
    Err(LaunchError::ViewerNotFound("explorer.exe".into()))
}

/// Calls `ShellExecuteW` with the "open" verb, maximized.
///
/// `params` is `None` to open a document with its association, or `Some` to
/// run `path` as a program with those arguments.
#[cfg(windows)]
fn shell_execute(path: &str, params: Option<&str>) -> Result<(), LaunchError> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWMAXIMIZED;

    /// `ShellExecuteW` returns a pseudo-`HINSTANCE`: values above this mean
    /// success, others are error codes.
    const SUCCESS_THRESHOLD: isize = 32;
    /// No application is associated with this file type.
    const SE_ERR_NOASSOC: isize = 31;
    const SE_ERR_FNF: isize = 2;

    let file = crate::index::win_util::wide_path(Path::new(path), false);
    let verb: [u16; 5] = [b'o' as u16, b'p' as u16, b'e' as u16, b'n' as u16, 0];
    let args: Option<Vec<u16>> = params.map(|p| p.encode_utf16().chain(Some(0)).collect());
    let args_ptr = args.as_ref().map_or(std::ptr::null(), |a| a.as_ptr());

    // The shell requires COM on the calling thread. This runs on the dedicated
    // open worker; repeat initialisation returns S_FALSE, which is fine.
    //
    // SAFETY: both arguments are the documented "no aggregate, default
    // options" values, and this thread makes no other COM calls.
    unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
    }

    // SAFETY: `file`, `verb` and `args` are NUL-terminated UTF-16 buffers that
    // outlive the call; the remaining pointers are null, which the API
    // documents as "no working directory" and "no owner window".
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            args_ptr,
            std::ptr::null(),
            // Maximized rather than `SW_SHOWNORMAL`, which restores the last
            // size. Only a hint: an already-running viewer receives the file
            // via DDE or COM and keeps its current window state.
            SW_SHOWMAXIMIZED,
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

/// Development fallback off Windows.
#[cfg(not(windows))]
fn shell_open(path: &str) -> Result<(), LaunchError> {
    spawn("xdg-open", path)
}

/// Returns whether a program resolves, for the startup warning and `--doctor`.
///
/// Resolves the name in-process (current directory, then `PATH`, each with
/// `PATHEXT` extensions) as `CreateProcess` does. Spawning `where` from this
/// GUI-subsystem binary would flash a console window at every startup, since
/// this is called from `app::new`. A name containing a separator is checked as
/// a path.
pub fn program_on_path(program: &str) -> bool {
    resolve_on_path(program, &path_dirs(), &path_exts()).is_some()
}

/// Returns the directories searched for a bare name, in order: the current
/// directory, then `PATH`, matching `CreateProcess`.
fn path_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::current_dir().into_iter().collect();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs
}

/// Returns the extensions tried against a name, from `PATHEXT`.
///
/// Falls back to the Windows default when `PATHEXT` is unset, as in some
/// service environments.
fn path_exts() -> Vec<String> {
    if cfg!(not(windows)) {
        return vec![String::new()];
    }
    let raw = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.VBS;.JS;.WSF;.MSC".to_owned());
    // Empty first, so `avwin.exe` is found as written before `avwin.exe.exe`.
    std::iter::once(String::new())
        .chain(
            raw.split(';')
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_ascii_lowercase),
        )
        .collect()
}

/// Returns the first file `program` resolves to.
///
/// Takes its inputs as parameters so tests need not modify the process
/// environment, which is unsafe to do in parallel.
fn resolve_on_path(
    program: &str,
    dirs: &[std::path::PathBuf],
    exts: &[String],
) -> Option<std::path::PathBuf> {
    let named = std::path::Path::new(program);
    // A name with a separator is a path, resolved against the current
    // directory as `CreateProcess` does.
    if named.components().count() > 1 {
        return named.is_file().then(|| named.to_path_buf());
    }

    for dir in dirs {
        for ext in exts {
            let mut name = program.to_owned();
            name.push_str(ext);
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_found_in_the_first_place_it_appears() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("thing.exe"), b"").unwrap();

        let exts = vec![String::new(), ".exe".to_owned()];
        let found = resolve_on_path("thing", &[first.clone(), second.clone()], &exts);
        assert_eq!(found, Some(second.join("thing.exe")));

        std::fs::write(first.join("thing.exe"), b"").unwrap();
        let found = resolve_on_path("thing", &[first.clone(), second], &exts);
        assert_eq!(found, Some(first.join("thing.exe")), "order was not kept");
    }

    #[test]
    fn a_name_that_already_has_its_extension_is_not_given_a_second() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        std::fs::write(dir.path().join("avwin.exe"), b"").unwrap();
        let exts = vec![String::new(), ".exe".to_owned()];
        assert_eq!(
            resolve_on_path("avwin.exe", &[dir.path().to_path_buf()], &exts),
            Some(dir.path().join("avwin.exe"))
        );
    }

    #[test]
    fn a_directory_is_not_a_program() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        std::fs::create_dir_all(dir.path().join("thing")).unwrap();
        let exts = vec![String::new()];
        assert_eq!(
            resolve_on_path("thing", &[dir.path().to_path_buf()], &exts),
            None
        );
    }

    #[test]
    fn the_extension_list_tries_the_bare_name_first() {
        let exts = path_exts();
        assert_eq!(exts.first().map(String::as_str), Some(""));
        if cfg!(windows) {
            assert!(exts.iter().any(|e| e == ".exe"), "{exts:?} has no .exe");
        }
    }

    /// Relies on `cmd.exe` being on `PATH`.
    #[test]
    #[cfg(windows)]
    fn a_program_that_is_really_there_is_found() {
        assert!(program_on_path("cmd"));
        assert!(program_on_path("cmd.exe"));
        assert!(!program_on_path("files-no-such-program-anywhere"));
    }

    #[test]
    fn every_error_carries_a_usable_detail() {
        assert!(
            LaunchError::ViewerNotFound("avwin.exe".into())
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

    /// The result depends on the machine; only the absence of a panic is
    /// checked.
    #[test]
    fn probing_for_a_viewer_does_not_panic() {
        let _ = program_on_path("avwin.exe");
    }
}
