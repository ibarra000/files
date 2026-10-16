//! Installing a downloaded or shared update.
//!
//! A running executable cannot be replaced, so the install is handed to a copy
//! of `files-cli.exe` staged outside the installation (see [`stage`]). It waits
//! for this process to exit before starting `msiexec`, avoiding Restart Manager
//! prompts and reboots.
//!
//! Elevation is left to `msiexec`, which prompts once for the per-machine
//! install. Rollback is left to `MajorUpgrade` in `wix/files.wxs`.

// Win32 FFI: BCrypt SHA-256 and OpenProcess, to wait for the panel to exit.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use crate::update::Manifest;

/// Subdirectory of the (non-roaming, `%LOCALAPPDATA%`) cache that holds the
/// staged installer and helper.
pub(crate) const STAGE_DIR: &str = "update";

/// Why an update could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    NoCacheDir,
    /// The installer named by the manifest is missing.
    MissingInstaller(PathBuf),
    /// The copied installer does not match the manifest's digest.
    WrongDigest,
    Io(String),
    /// `files-cli.exe` was not found to copy.
    NoHelper,
    CouldNotStart(String),
}

impl Problem {
    pub fn detail(&self) -> String {
        match self {
            Self::NoCacheDir => "no cache folder is available for the installer".into(),
            Self::MissingInstaller(p) => format!("{} not found", p.display()),
            Self::WrongDigest => {
                "the installer did not copy correctly \u{b7} try again in a moment".into()
            }
            Self::Io(e) => e.clone(),
            Self::NoHelper => "files-cli.exe was not found next to files.exe".into(),
            Self::CouldNotStart(e) => format!("the installer would not start: {e}"),
        }
    }
}

/// Copies the installer and the helper outside the installation.
///
/// The installer is copied locally so the install survives losing the share,
/// and the digest is checked against the exact bytes that will be installed.
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
            // Do not leave a bad copy behind.
            let _ = std::fs::remove_file(&staged_msi);
            return Err(Problem::WrongDigest);
        }
    }

    // The helper must run from outside the installation it replaces.
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

/// Returns `files-cli.exe` beside the running executable, which serves as the
/// update helper.
fn helper_source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let beside = exe.with_file_name("files-cli.exe");
    beside.is_file().then_some(beside)
}

/// Starts the helper and returns, leaving it to wait for this process to end.
///
/// The helper is detached because it must outlive this process.
#[cfg(windows)]
pub fn hand_over(staged: &Staged) -> Result<(), Problem> {
    use std::os::windows::process::CommandExt;

    // `files-cli` is a console program; these flags prevent a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    let mut command = std::process::Command::new(&staged.helper);
    command
        .arg("--apply-update")
        .arg("--msi")
        .arg(&staged.msi)
        .arg("--wait-pid")
        .arg(std::process::id().to_string());

    // Relaunch from the current path; `MajorUpgrade` installs into the same
    // folder.
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

/// How long the helper waits for its parent to exit. Bounded so a wedged
/// parent cannot leave the helper resident forever.
const EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Helper entry point: waits for the parent to exit, installs, and relaunches.
///
/// Runs in a staged copy of `files-cli.exe` after the parent has exited, so
/// there is no UI to report to beyond `msiexec`'s own.
pub fn run_helper(msi: &Path, wait_pid: u32, relaunch: Option<&Path>) -> Result<(), String> {
    wait_for_exit(wait_pid, EXIT_TIMEOUT);

    // `/qb` shows a basic progress bar while the app is closed.
    let status = std::process::Command::new("msiexec")
        .arg("/i")
        .arg(msi)
        .arg("/qb")
        .status()
        .map_err(|e| format!("msiexec would not start: {e}"))?;

    if !status.success() {
        // Not relaunched: the installer rolled back, and relaunching would
        // make the failed upgrade look successful.
        return Err(format!("the installer stopped: {status}"));
    }

    // The staged installer is no longer needed.
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
/// Waits on a process handle rather than polling the PID, which can be reused.
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

/// Returns the lowercase hex SHA-256 of a file, computed with Windows BCrypt to
/// avoid a hashing dependency.
#[cfg(windows)]
pub(crate) fn sha256_of(path: &Path) -> std::io::Result<String> {
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

        // Hash in fixed-size chunks rather than loading the whole installer.
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
pub(crate) fn sha256_of(_path: &Path) -> std::io::Result<String> {
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

    /// Checks against the published SHA-256 of empty input.
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

    /// Spans several reads, exercising the incremental path.
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
