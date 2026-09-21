//! Handing a finished file to a program.
//!
//! Three routes, and the choice between them is not arbitrary:
//!
//! * **`avwin.exe`** by name on `PATH`, as this program has always done.
//! * **A configured `pdf_viewer`**, spawned directly. Only ever given a
//!   `.pdf`, because the file the user picked is not always one - see
//!   [`open_associated`].
//! * **The file's association**, via `ShellExecuteW`. The default, and under
//!   `Auto` the answer for every kind of file: which program opens a `.pdf` or
//!   a `.xlsx` is the user's decision and Windows already records it.
//!
//! # Why `ShellExecuteW` and not `cmd /C start`
//!
//! `start` reads a first quoted argument as a *window title*, so
//! `start "C:\path with spaces\doc.pdf"` opens an empty console window named
//! after the document. Working around that means an empty `""` title argument,
//! and even then `&` in a path still breaks the command line. `ShellExecuteW`
//! takes the path as a parameter rather than as text to be re-parsed, so there
//! is nothing to quote and nothing to escape.

#[cfg(windows)]
use std::os::windows::process::CommandExt;
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
    ///
    /// Ordinary rather than rare since `Auto` started handing every file to
    /// the shell: a file type nobody on this machine has a program for used to
    /// go to avwin and open. So the message says how to get that back.
    NoHandler(String),
    Io(String),
}

impl LaunchError {
    pub fn detail(&self) -> String {
        match self {
            Self::ViewerNotFound(name) => format!("{name} not found on PATH"),
            Self::NoHandler(kind) if kind.is_empty() => {
                "nothing is registered to open this kind of file \u{b7} F2 for avwin".into()
            }
            Self::NoHandler(kind) => {
                format!("nothing is registered to open {kind} files \u{b7} F2 for avwin")
            }
            Self::Io(e) => e.clone(),
        }
    }
}

/// Opens `path` with `avwin.exe`.
pub fn open_avwin(path: &str) -> Result<(), LaunchError> {
    spawn(AVWIN, path)
}

/// Opens `path` the way the desktop would.
///
/// `configured` is used only when `path` really is a PDF. Under the rule that
/// a selected row outside the code's page group opens on its own, the target
/// can be a `.doc` or a `.dwg`, and handing that to a program chosen for its
/// PDF rendering would be worse than useless. Those go to the shell, which
/// knows what to do with them.
///
/// Named for what it does rather than for the one caller it used to have: the
/// `Auto` mode hands every kind of file through here, and a `pdf_viewer`
/// somebody named by hand is still their choice rather than this program
/// picking one for them.
pub fn open_associated(path: &str, configured: Option<&Path>) -> Result<(), LaunchError> {
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

/// Runs a program with one argument, detached and maximized.
///
/// Stdio is nulled so a chatty viewer cannot write over the terminal this
/// program is drawing in, and the child handle is dropped rather than waited
/// on - the viewer outliving the search is the point.
///
/// # Why the show state is set here and cannot be set later
///
/// A viewer came up *minimized* if that is how it was last closed. Nothing
/// here had any say in it: `Command` sets `STARTF_USESTDHANDLES` and nothing
/// else, so `wShowWindow` is ignored by `CreateProcessW` and the child's first
/// `ShowWindow(SW_SHOWDEFAULT)` falls back to whatever placement it restored
/// for itself.
///
/// `show_window` is what sets `STARTF_USESHOWWINDOW`, and it is the only hook
/// that reaches that *first* call. Afterwards there is nothing to do it to: the
/// window belongs to another process, finding its handle is a race against its
/// own startup, and `ShowWindow` from outside would fight whatever the viewer
/// was in the middle of doing.
fn spawn(program: &str, path: &str) -> Result<(), LaunchError> {
    #[cfg(windows)]
    {
        // Through the shell rather than `Command`, for one reason:
        // `CreateProcessW` ignores `wShowWindow` unless `STARTF_USESHOWWINDOW`
        // is set, and `std`'s only hook for that - `CommandExt::show_window` -
        // is still unstable. `ShellExecuteW` takes the show state as an
        // argument, resolves a bare program name against `PATH` and the App
        // Paths registry exactly as `Command` does, and is already the way
        // every other launch here goes out.
        //
        // The argument is quoted, which is safe rather than hopeful: this is
        // the one place a path becomes text to be re-parsed, and a Windows
        // file name cannot contain a double quote, so wrapping it in quotes
        // cannot be defeated by any name the share can hold.
        let quoted = format!("\"{path}\"");
        shell_execute(program, Some(&quoted)).map_err(|e| match e {
            // The shell says "file not found" about the *program* here.
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

/// Lets the program we are about to start take the foreground from us.
///
/// Windows refuses a foreground change from a process that does not already
/// own the foreground, which is why a viewer launched from here came up
/// *behind* everything with its taskbar button flashing. This is the documented
/// way to hand that right over, and it only works while the caller still has
/// it - so it has to happen on the thread that owns the panel, before the panel
/// is dismissed, rather than on the worker that eventually does the launching.
///
/// `ASFW_ANY` rather than a process id: the id is not known until the child
/// exists, and for the shell path there is no child of ours at all - the
/// association is opened by a process that may already be running.
#[cfg(windows)]
pub fn allow_foreground_handover() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    // SAFETY: no pointers, no handles. Fails harmlessly - returning zero - when
    // this process is not the foreground one, which is exactly the case where
    // there is nothing to hand over.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

/// Off Windows there is no foreground lock to negotiate with.
#[cfg(not(windows))]
pub fn allow_foreground_handover() {}

/// Opens a file with whatever is registered for its type.
///
/// Public because the settings window opens the configuration file with it,
/// which is the same act for the same reason: whatever the user has chosen for
/// that kind of file, rather than an editor this program picked for them.
#[cfg(windows)]
pub fn shell_open(path: &str) -> Result<(), LaunchError> {
    shell_execute(path, None)
}

/// Opens Explorer with `path` already picked out.
///
/// `explorer.exe /select,"<path>"`, which is what "Show in folder" does
/// everywhere else on this machine and is the documented way to ask for it.
///
/// Not through [`shell_execute`], because that quotes the whole argument
/// string and the argument here is *not* one path - it is a switch and a path
/// stuck together. `explorer.exe` parses `/select,` itself, and a version of
/// this that quoted the lot opened the user's Documents folder with nothing
/// selected, silently, which is the one wrong answer that looks like it
/// worked.
///
/// The quotes go round the path and nowhere else. A Windows file name cannot
/// contain a double quote, so that is safe rather than hopeful.
///
/// Explorer's exit code is famously not a report of anything, so the only
/// failure this can honestly detect is not being able to start it at all.
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

/// Off Windows there is no Explorer to ask.
#[cfg(not(windows))]
pub fn reveal(path: &str) -> Result<(), LaunchError> {
    let _ = path;
    Err(LaunchError::ViewerNotFound("explorer.exe".into()))
}

/// `ShellExecuteW`, with an optional command line for the thing being run.
///
/// `params` is `None` when `file` is a document to be opened with whatever is
/// registered for it, and `Some` when `file` is a program and `params` is what
/// to hand it.
#[cfg(windows)]
fn shell_execute(path: &str, params: Option<&str>) -> Result<(), LaunchError> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWMAXIMIZED;

    /// `ShellExecuteW` returns a fake `HINSTANCE`. Anything above this is
    /// success; at or below it the value is an error code. A genuinely
    /// terrible interface, and the threshold is the documented contract.
    const SUCCESS_THRESHOLD: isize = 32;
    /// No application is associated with this file type.
    const SE_ERR_NOASSOC: isize = 31;
    const SE_ERR_FNF: isize = 2;

    let file = crate::index::win_util::wide_path(Path::new(path), false);
    let verb: [u16; 5] = [b'o' as u16, b'p' as u16, b'e' as u16, b'n' as u16, 0];
    let args: Option<Vec<u16>> = params.map(|p| p.encode_utf16().chain(Some(0)).collect());
    let args_ptr = args.as_ref().map_or(std::ptr::null(), |a| a.as_ptr());

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
            // Maximized, not `SW_SHOWNORMAL`. That one means "restore it to
            // the size and position it had", which is a positive instruction
            // *not* to fill the screen - and a drawing is the one thing
            // somebody opens precisely in order to look at closely.
            //
            // A hint only: a viewer that is already running takes the file
            // through DDE or COM and uses whatever window state it already
            // has. There is nothing on this side that can change that.
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

/// Off Windows there is no association to consult, so this exists to keep the
/// module compiling and testable on a development machine.
#[cfg(not(windows))]
fn shell_open(path: &str) -> Result<(), LaunchError> {
    spawn("xdg-open", path)
}

/// Whether `avwin.exe` can be found, for the startup warning.
///
/// Looked up rather than run: the point is to warn someone before they need
/// it, not to open a window they did not ask for.
pub fn avwin_available() -> bool {
    program_on_path(AVWIN)
}

/// Whether a bare program name resolves, for the startup warning and
/// `--doctor`.
///
/// # Why this is a directory walk and not `where`
///
/// It used to spawn `where` and read its exit status, which is a correct
/// answer arrived at in the worst possible way.
///
/// This is called from `app::new`, on the frame thread, at every startup.
/// `files.exe` is a GUI-subsystem binary, so it has no console; spawning a
/// console program from one makes Windows allocate a console for the child,
/// and a console window appears on the desktop, flashes, and closes. Every
/// launch. That is the flicker somebody has been watching for as long as
/// this function has existed, and no amount of redirecting the child's
/// handles suppresses it - the window is allocated before the program runs.
/// `CREATE_NO_WINDOW` would have fixed the flash and left the process
/// spawn, which is thirty milliseconds of startup to answer a question that
/// is two environment variables and a handful of `exists` calls.
///
/// So it resolves the name itself, the way `CreateProcess` would: the
/// current directory first, then each entry of `PATH` in order, and against
/// each of those every extension in `PATHEXT` for a name that has none.
/// `where` searches in the same order for the same reason, and unlike
/// `where` this can be tested.
pub fn program_on_path(program: &str) -> bool {
    resolve_on_path(program, &path_dirs(), &path_exts()).is_some()
}

/// Where a bare name is looked for, in order.
///
/// The current directory first, which is what `CreateProcess` does and what
/// `where` does. It is a legitimate place for `avwin.exe` to be - somebody
/// running this from the folder the viewer lives in - and leaving it out
/// would make this answer "no" where the launch would succeed.
fn path_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::current_dir().into_iter().collect();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs
}

/// And which extensions are tried against a name that has none.
///
/// From `PATHEXT`, which is where the answer lives and which a machine can
/// legitimately have customised. The fallback is Windows' own default, for
/// the case where it is unset - which happens inside some service
/// environments and would otherwise make every lookup fail.
fn path_exts() -> Vec<String> {
    if cfg!(not(windows)) {
        return vec![String::new()];
    }
    let raw = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.VBS;.JS;.WSF;.MSC".to_owned());
    // The empty one first: a name written with its extension is found as
    // written rather than as `avwin.exe.exe`.
    std::iter::once(String::new())
        .chain(
            raw.split(';')
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_ascii_lowercase),
        )
        .collect()
}

/// The first file that `program` resolves to, if any.
///
/// Split out from [`program_on_path`] with its inputs passed in, because the
/// rule - which directories, in what order, with which extensions appended -
/// is the whole of the behaviour and the environment is the one thing a test
/// cannot set safely in parallel with other tests.
fn resolve_on_path(
    program: &str,
    dirs: &[std::path::PathBuf],
    exts: &[String],
) -> Option<std::path::PathBuf> {
    let named = std::path::Path::new(program);
    // A name with a separator in it is a path and not a lookup. `where`
    // refuses these outright; resolving it against the current directory is
    // friendlier and is what `CreateProcess` does.
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

    /// A name is found in the first directory that has it, and the order is
    /// the order it was given in.
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

    /// A name written with its extension is found as written, rather than as
    /// itself with another extension stuck on the end.
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

    /// A name that is nowhere is nowhere, rather than a directory that
    /// happens to share its name.
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

    /// The empty extension comes first, and every real one is present.
    #[test]
    fn the_extension_list_tries_the_bare_name_first() {
        let exts = path_exts();
        assert_eq!(exts.first().map(String::as_str), Some(""));
        if cfg!(windows) {
            assert!(exts.iter().any(|e| e == ".exe"), "{exts:?} has no .exe");
        }
    }

    /// And the real thing answers without a console appearing, which is the
    /// whole reason this is a walk. `cmd.exe` is on the path of every
    /// Windows this program runs on.
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
