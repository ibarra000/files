//! Whether files starts when somebody signs in.
//!
//! The answer is one value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, named `files` and
//! holding the quoted path of the executable - the same value the installer's
//! "Start with Windows" feature writes (see `wix/files.wxs`, `RunAtLogin`).
//! Sharing the name is the point: the switch in the settings window and the
//! box in the installer are two ways of setting one thing, so neither can
//! disagree with the other about whether it is on.
//!
//! # Why the registry is the truth and `config.toml` is not
//!
//! A `start_with_windows = true` in the configuration would be a second copy
//! of a fact Windows already keeps, and the two would drift the first time
//! somebody unticked the installer box, or turned the entry off in Task
//! Manager's Startup tab - which edits this value and knows nothing about our
//! file. So nothing is stored here. The switch reads the value, and flipping
//! it writes or deletes the value.
//!
//! Starting hidden needs nothing from this module: the panel is built
//! invisible and stays that way until it is summoned (`gui::mod`), so a
//! sign-in launch is a tray icon and nothing else.

use std::io;
use std::path::Path;

/// The value's name, and the installer's.
const VALUE: &str = "files";

/// Whether files is set to start at sign-in.
///
/// Whatever the value says rather than whether it names *this* executable. A
/// development build asking about an installed copy's entry should see the
/// switch on, because signing in will start something called files.
pub fn is_on() -> io::Result<bool> {
    imp::exists(VALUE)
}

/// Starts files at sign-in, from wherever this executable is.
pub fn enable() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    imp::write(VALUE, &command_line(&exe))
}

/// Stops files starting at sign-in. Already off is not an error.
pub fn disable() -> io::Result<()> {
    imp::delete(VALUE)
}

/// The command the shell runs at sign-in.
///
/// Quoted, because `C:\Program Files\files\files.exe` has a space in it and an
/// unquoted Run value with a space is one Windows guesses at.
fn command_line(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::ptr;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW,
        RegSetValueExW,
    };

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn check(status: WIN32_ERROR) -> io::Result<()> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    pub fn exists(name: &str) -> io::Result<bool> {
        let (key, name) = (wide(RUN), wide(name));
        // SAFETY: both strings are NUL-terminated and outlive the call. A null
        // data pointer asks only whether the value is there and how big it
        // is, which is all this wants.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        match status {
            ERROR_SUCCESS => Ok(true),
            ERROR_FILE_NOT_FOUND => Ok(false),
            other => check(other).map(|()| false),
        }
    }

    pub fn write(name: &str, data: &str) -> io::Result<()> {
        let (path, name) = (wide(RUN), wide(name));
        let data = wide(data);
        let mut key: HKEY = ptr::null_mut();
        // SAFETY: `path` is NUL-terminated; `key` is written on success and
        // closed below on every path that opened it. `Run` exists on every
        // Windows, but create-or-open costs nothing over open and survives a
        // profile that has had it pruned.
        check(unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                ptr::null(),
                &mut key,
                ptr::null_mut(),
            )
        })?;
        // SAFETY: `key` is open; `data` is a NUL-terminated UTF-16 buffer and
        // the length passed is its size in bytes, terminator included, which
        // is what REG_SZ requires.
        let status = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * size_of::<u16>()) as u32,
            )
        };
        // SAFETY: opened above and not used again.
        unsafe { RegCloseKey(key) };
        check(status)
    }

    pub fn delete(name: &str) -> io::Result<()> {
        let (key, name) = (wide(RUN), wide(name));
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        match status {
            ERROR_FILE_NOT_FOUND => Ok(()),
            other => check(other),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "only Windows starts programs at sign-in",
        )
    }

    pub fn exists(_: &str) -> io::Result<bool> {
        Err(unsupported())
    }

    pub fn write(_: &str, _: &str) -> io::Result<()> {
        Err(unsupported())
    }

    pub fn delete(_: &str) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    // Names nothing else uses, so a test run never touches the real entry -
    // on a developer's machine that entry is the installed copy's. One per
    // test, because the tests run in parallel and share the key.

    #[test]
    fn a_value_that_is_written_is_there_and_one_that_is_deleted_is_not() {
        const TEST_VALUE: &str = "files-autostart-test-round-trip";
        imp::delete(TEST_VALUE).unwrap();
        assert!(!imp::exists(TEST_VALUE).unwrap());

        imp::write(
            TEST_VALUE,
            &command_line(Path::new(r"C:\Program Files\files\files.exe")),
        )
        .unwrap();
        assert!(imp::exists(TEST_VALUE).unwrap());

        imp::delete(TEST_VALUE).unwrap();
        assert!(!imp::exists(TEST_VALUE).unwrap());
    }

    #[test]
    fn switching_off_what_is_already_off_is_not_an_error() {
        const TEST_VALUE: &str = "files-autostart-test-twice";
        imp::delete(TEST_VALUE).unwrap();
        imp::delete(TEST_VALUE).unwrap();
    }

    #[test]
    fn the_command_is_quoted_because_program_files_has_a_space_in_it() {
        assert_eq!(
            command_line(Path::new(r"C:\Program Files\files\files.exe")),
            r#""C:\Program Files\files\files.exe""#
        );
    }
}
