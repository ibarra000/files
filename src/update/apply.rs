//! Installing what the share offered.
//!
//! # Why this program cannot install its own update
//!
//! Windows Installer replaces the files a package owns, and two of those are
//! `files.exe` and `files-cli.exe`. A running executable cannot be replaced,
//! so an installer started by the program it is upgrading finds its own caller
//! in the way: at best the Restart Manager asks the user to close an
//! application they did not knowingly open, at worst the upgrade completes
//! only after a reboot nobody wanted.
//!
//! So the work is handed to a copy of `files-cli.exe` placed outside the
//! installation - see [`stage`] - which waits for this process to exit before
//! it starts `msiexec`. With nothing of ours running there is no file in use,
//! no Restart Manager prompt and no reboot.
//!
//! # Why elevation is not asked for here
//!
//! The package is `perMachine` and installs into Program Files, so applying it
//! needs administrator rights. `msiexec` obtains them itself when it needs
//! them, which is one standard consent dialog naming Windows Installer rather
//! than a second one naming us.
//!
//! # What is not attempted
//!
//! No rollback. `MajorUpgrade` in `wix/files.wxs` already removes the previous
//! version and restores it if the new one fails to install, and Windows
//! Installer is far better at that than anything written here would be. If it
//! fails, what is on the machine is what was on it before.

use std::path::{Path, PathBuf};

use crate::update::Manifest;

/// Where a staged installer and its helper are put.
///
/// Under `%LOCALAPPDATA%`, not `%APPDATA%`: this is a scratch copy of bytes
/// that already exist on a share, so it should not roam between machines.
const STAGE_DIR: &str = "update";

/// Why an update could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    NoCacheDir,
    /// The installer named by the manifest is not beside it.
    MissingInstaller(PathBuf),
    /// What was copied is not what the manifest described.
    WrongDigest,
    Io(String),
    /// The helper could not be found to copy.
    NoHelper,
    CouldNotStart(String),
}

impl Problem {
    pub fn detail(&self) -> String {
        match self {
            Self::NoCacheDir => "there is nowhere to put the installer".into(),
            Self::MissingInstaller(p) => format!("{} is not there", p.display()),
            Self::WrongDigest => {
                "the installer did not copy correctly \u{b7} try again in a moment".into()
            }
            Self::Io(e) => e.clone(),
            Self::NoHelper => "files-cli.exe could not be found beside this program".into(),
            Self::CouldNotStart(e) => format!("the installer would not start: {e}"),
        }
    }
}

/// Copies the installer and the helper somewhere the upgrade will not replace.
///
/// The installer is copied off the share rather than run from it, so that a
/// laptop leaving the building mid-install does not take the file with it -
/// and so that the digest is checked against the bytes that will actually be
/// installed rather than against a second read of a file that may have moved.
pub fn stage(manifest: &Manifest, msi: &Path, cache_dir: Option<&Path>) -> Result<Staged, Problem> {
    if !msi.is_file() {
        return Err(Problem::MissingInstaller(msi.to_path_buf()));
    }
    let dir = cache_dir.ok_or(Problem::NoCacheDir)?.join(STAGE_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| Problem::Io(e.to_string()))?;

    let staged_msi = dir.join(&manifest.msi);
    std::fs::copy(msi, &staged_msi).map_err(|e| Problem::Io(e.to_string()))?;

    if let Some(want) = &manifest.sha256 {
        let got = sha256_of(&staged_msi).map_err(|e| Problem::Io(e.to_string()))?;
        if &got != want {
            // Removed rather than left to be found later and wondered about.
            let _ = std::fs::remove_file(&staged_msi);
            return Err(Problem::WrongDigest);
        }
    }

    // The helper must not live inside the installation, or the upgrade would
    // be replacing the executable driving it - which is the whole problem this
    // module exists to avoid, reintroduced one directory along.
    let helper = helper_source().ok_or(Problem::NoHelper)?;
    let staged_helper = dir.join("files-update.exe");
    std::fs::copy(&helper, &staged_helper).map_err(|e| Problem::Io(e.to_string()))?;

    Ok(Staged {
        msi: staged_msi,
        helper: staged_helper,
    })
}

/// An installer and a helper, both outside the installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub msi: PathBuf,
    pub helper: PathBuf,
}

/// `files-cli.exe`, beside whatever is running.
///
/// The console half of this program is the helper, rather than a third
/// executable: it already ships, it is already signed by whatever signs the
/// rest, and it already has a command-line parser to hang a flag on.
fn helper_source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let beside = exe.with_file_name("files-cli.exe");
    beside.is_file().then_some(beside)
}

/// Starts the helper and returns, leaving it to wait for this process to end.
///
/// Detached deliberately. The helper outlives its parent by design - that is
/// the entire point of it - so nothing here waits on it or holds a handle to
/// it.
#[cfg(windows)]
pub fn hand_over(staged: &Staged) -> Result<(), Problem> {
    use std::os::windows::process::CommandExt;

    // No window, and not tied to this process's console. `files-cli` is a
    // console program, and without this the upgrade would flash a black
    // rectangle on the way past.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    let mut command = std::process::Command::new(&staged.helper);
    command
        .arg("--apply-update")
        .arg("--msi")
        .arg(&staged.msi)
        .arg("--wait-pid")
        .arg(std::process::id().to_string());

    // Where to start again afterwards: this executable, at the path it is at
    // now. `MajorUpgrade` installs over the same folder, so the new version
    // lands exactly here - and asking the running program where it lives beats
    // any guess at where the installer will put things.
    if let Ok(exe) = std::env::current_exe() {
        command.arg("--relaunch").arg(exe);
    }

    command
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map_err(|e| Problem::CouldNotStart(e.to_string()))?;
    Ok(())
}

#[cfg(not(windows))]
pub fn hand_over(_staged: &Staged) -> Result<(), Problem> {
    Err(Problem::CouldNotStart(
        "updates are only installed on Windows".into(),
    ))
}

/// How long the helper waits for the program that started it to exit.
///
/// Generous, because the alternative to waiting is starting an installer
/// while the files it replaces are still open. Bounded, because a program
/// wedged on a network call must not leave a helper resident forever.
const EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The helper: waits for the caller to exit, installs, and starts it again.
///
/// Runs in `files-cli.exe`, from a copy outside the installation. Everything
/// here happens after the program that asked for it has gone, so there is
/// nothing to report to and nowhere to report it - the outcome is the state
/// of the machine afterwards, and `msiexec` speaks for itself while it works.
pub fn run_helper(msi: &Path, wait_pid: u32, relaunch: Option<&Path>) -> Result<(), String> {
    wait_for_exit(wait_pid, EXIT_TIMEOUT);

    // `/qb` rather than `/quiet`: a progress bar is what tells somebody the
    // thing they asked for is happening, and this is the window in which the
    // panel has vanished and not yet come back.
    let status = std::process::Command::new("msiexec")
        .arg("/i")
        .arg(msi)
        .arg("/qb")
        .status()
        .map_err(|e| format!("msiexec would not start: {e}"))?;

    if !status.success() {
        // Deliberately not restarted. Windows Installer has already rolled
        // back to what was there before, and starting the old version now
        // would make a failed upgrade look like a successful one.
        return Err(format!("the installer stopped: {status}"));
    }

    // The staged copy is tens of megabytes that will never be wanted again.
    let _ = std::fs::remove_file(msi);

    if let Some(exe) = relaunch {
        std::process::Command::new(exe)
            .spawn()
            .map_err(|e| format!("the new version would not start: {e}"))?;
    }
    Ok(())
}

/// Blocks until the process exits, or until the timeout.
///
/// By handle rather than by polling for the PID: a PID is reused, and a helper
/// that waited on a recycled one would either return at once or wait on a
/// stranger.
#[cfg(windows)]
fn wait_for_exit(pid: u32, timeout: std::time::Duration) {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: the handle is checked for null before it is waited on and is
    // closed on every path out. A pid that has already exited opens as null,
    // which is the success case and returns immediately.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return;
        }
        let _ = WaitForSingleObject(handle, timeout.as_millis() as u32) == WAIT_OBJECT_0;
        CloseHandle(handle);
    }
}

#[cfg(not(windows))]
fn wait_for_exit(_pid: u32, _timeout: std::time::Duration) {}

/// SHA-256 of a file, through the operating system's own implementation.
///
/// BCrypt rather than a crate. The digest is wanted for one file, once, at the
/// moment somebody clicks a button - so what a hand-written or imported
/// implementation would buy is nothing measurable, against a dependency in a
/// manifest that argues for each one it has.
#[cfg(windows)]
fn sha256_of(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    use windows_sys::Win32::Security::Cryptography::*;

    let mut file = std::fs::File::open(path)?;
    let mut hash = [0u8; 32];

    // SAFETY: every call below is checked, every handle is closed on both the
    // success and the failure path, and the two buffers handed to BCrypt are
    // sized by BCrypt itself in the calls immediately above their use.
    unsafe {
        let mut alg = std::ptr::null_mut();
        if BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_SHA256_ALGORITHM, std::ptr::null(), 0) != 0
        {
            return Err(std::io::Error::other("SHA-256 is unavailable"));
        }

        let mut object_len = 0u32;
        let mut written = 0u32;
        if BCryptGetProperty(
            alg,
            BCRYPT_OBJECT_LENGTH,
            (&mut object_len as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut written,
            0,
        ) != 0
        {
            BCryptCloseAlgorithmProvider(alg, 0);
            return Err(std::io::Error::other("SHA-256 is unavailable"));
        }

        let mut object = vec![0u8; object_len as usize];
        let mut hasher = std::ptr::null_mut();
        if BCryptCreateHash(
            alg,
            &mut hasher,
            object.as_mut_ptr(),
            object_len,
            std::ptr::null(),
            0,
            0,
        ) != 0
        {
            BCryptCloseAlgorithmProvider(alg, 0);
            return Err(std::io::Error::other("SHA-256 is unavailable"));
        }

        // A fixed buffer rather than the whole file: an installer is tens of
        // megabytes, and there is no reason for all of it to be resident at
        // once to be summed.
        let mut buf = vec![0u8; 64 << 10];
        let result = loop {
            match file.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    if BCryptHashData(hasher, buf.as_ptr(), n as u32, 0) != 0 {
                        break Err(std::io::Error::other("the digest could not be computed"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };

        if result.is_ok() && BCryptFinishHash(hasher, hash.as_mut_ptr(), hash.len() as u32, 0) != 0
        {
            BCryptDestroyHash(hasher);
            BCryptCloseAlgorithmProvider(alg, 0);
            return Err(std::io::Error::other("the digest could not be computed"));
        }
        BCryptDestroyHash(hasher);
        BCryptCloseAlgorithmProvider(alg, 0);
        result?;
    }

    Ok(hash.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(not(windows))]
fn sha256_of(_path: &Path) -> std::io::Result<String> {
    Err(std::io::Error::other("no digest implementation"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Version;

    fn manifest(sha256: Option<&str>) -> Manifest {
        Manifest {
            version: Version::new(0, 3, 0),
            msi: "files-0.3.0-x64.msi".into(),
            sha256: sha256.map(str::to_string),
            notes: None,
        }
    }

    /// The empty input, which is the one SHA-256 value everybody can check
    /// against a reference without trusting this code.
    #[cfg(windows)]
    #[test]
    fn the_digest_agrees_with_the_published_value_for_no_bytes_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, b"").unwrap();

        assert_eq!(
            sha256_of(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_digest_agrees_with_the_published_value_for_abc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc");
        std::fs::write(&path, b"abc").unwrap();

        assert_eq!(
            sha256_of(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Longer than one read of the buffer, so the incremental path is what is
    /// being measured rather than a single call.
    #[cfg(windows)]
    #[test]
    fn a_file_larger_than_the_read_buffer_is_summed_the_same_way() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big");
        std::fs::write(&path, vec![b'a'; (64 << 10) * 3 + 17]).unwrap();

        let digest = sha256_of(&path).unwrap();
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn an_installer_that_is_not_there_is_refused_before_anything_is_copied() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.msi");
        assert_eq!(
            stage(&manifest(None), &missing, Some(dir.path())),
            Err(Problem::MissingInstaller(missing))
        );
    }

    #[test]
    fn with_nowhere_to_stage_it_the_attempt_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let msi = dir.path().join("files-0.3.0-x64.msi");
        std::fs::write(&msi, b"not really an installer").unwrap();

        assert_eq!(stage(&manifest(None), &msi, None), Err(Problem::NoCacheDir));
    }

    /// A truncated copy is the hazard this guards, and it must not be left
    /// behind to be found and wondered about later.
    #[cfg(windows)]
    #[test]
    fn a_copy_that_does_not_match_the_manifest_is_refused_and_removed() {
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let msi = source.path().join("files-0.3.0-x64.msi");
        std::fs::write(&msi, b"abc").unwrap();

        let wrong = manifest(Some(&"0".repeat(64)));
        assert_eq!(
            stage(&wrong, &msi, Some(cache.path())),
            Err(Problem::WrongDigest)
        );
        assert!(
            !cache.path().join(STAGE_DIR).join(&wrong.msi).exists(),
            "a rejected installer was left behind"
        );
    }

    #[test]
    fn every_problem_explains_itself() {
        for problem in [
            Problem::NoCacheDir,
            Problem::MissingInstaller("x".into()),
            Problem::WrongDigest,
            Problem::Io("x".into()),
            Problem::NoHelper,
            Problem::CouldNotStart("x".into()),
        ] {
            assert!(!problem.detail().is_empty());
        }
    }
}
