//! Whether the Windows taskbar is dark, and a watch for when that changes.
//!
//! The taskbar has its own theme (`SystemUsesLightTheme`), separate from the
//! app theme egui follows (`AppsUseLightTheme`). The Windows 11 default is
//! light apps on a dark taskbar, so the tray icon must read this one.

#![cfg(windows)]

use std::io;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET, RRF_RT_REG_DWORD, RegCloseKey,
    RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
};

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const VALUE: &str = "SystemUsesLightTheme";

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

/// Whether the taskbar is dark.
///
/// Dark when the value is missing: Windows versions without it (before
/// Windows 10 1903) only had a dark taskbar.
pub fn is_dark() -> bool {
    let (key, value) = (wide(PERSONALIZE), wide(VALUE));
    let mut light: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: both strings are NUL-terminated and outlive the call; `light`
    // is a writable DWORD and `size` holds its size in bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            (&raw mut light).cast(),
            &mut size,
        )
    };
    status != ERROR_SUCCESS || light == 0
}

/// Calls `on_change` from a background thread whenever the theme key changes.
///
/// The thread blocks in the registry and lives for the rest of the process.
/// Fails if the thread cannot be started; if the key cannot be opened, the
/// thread exits and the tray keeps its starting tint.
pub fn watch(on_change: impl Fn() + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new()
        .name("taskbar-theme".into())
        .spawn(move || {
            if let Err(err) = wait_for_changes(on_change) {
                log::warn!("taskbar theme watch stopped: {err}");
            }
        })
        .map(drop)
}

fn wait_for_changes(on_change: impl Fn()) -> io::Result<()> {
    let path = wide(PERSONALIZE);
    let mut key: HKEY = ptr::null_mut();
    // SAFETY: `path` is NUL-terminated; `key` is written on success and
    // closed below.
    check(unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_NOTIFY, &mut key) })?;
    // Catches a change made between the tray reading the theme and this
    // watch starting; the notification only reports later changes.
    on_change();
    let result = loop {
        // SAFETY: `key` is open with KEY_NOTIFY. Synchronous: no event handle,
        // and the call returns once a value under the key is written.
        let status = unsafe {
            RegNotifyChangeKeyValue(key, 0, REG_NOTIFY_CHANGE_LAST_SET, ptr::null_mut(), 0)
        };
        if let Err(err) = check(status) {
            break Err(err);
        }
        on_change();
    };
    // SAFETY: `key` was opened above and is not used after this.
    unsafe { RegCloseKey(key) };
    result
}
