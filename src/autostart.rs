//! Start-at-sign-in, stored as the `files` value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! The installer's "Start with Windows" feature writes the same value (see
//! `wix/files.wxs`, `RunAtLogin`), so keep the names in sync. The registry is
//! the only source of truth; nothing is stored in `config.toml`, because Task
//! Manager's Startup tab and the installer both edit this value directly.

use std::io;
use std::path::Path;

/// The Run value name. Must match the installer's.
const VALUE: &str = "files";

/// Returns whether the Run value exists.
///
/// Does not check which executable it names, so a development build reports
/// an installed copy's entry as on.
pub fn is_on() -> io::Result<bool> {
    imp::exists(VALUE)
}

/// Registers the current executable to start at sign-in.
pub fn enable() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    imp::write(VALUE, &command_line(&exe))
}

/// Removes the Run value. Succeeds if it is already absent.
pub fn disable() -> io::Result<()> {
    imp::delete(VALUE)
}

/// Returns the quoted command line for the Run value.
///
/// Quoted because an unquoted path containing spaces is ambiguous to Windows.
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
        // SAFETY: both strings are NUL-terminated and outlive the call. Null
        // output pointers query only existence and size.
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
        // closed below on every path that opened it. Create-or-open handles a
        // profile where the `Run` key is missing.
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

    // Each test uses its own value name so it never touches the real entry
    // and parallel tests do not interfere.

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
