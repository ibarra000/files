//! Native message boxes for messages that have no window to appear in.
//!
//! Used when the program cannot start, and when an open finishes after the
//! panel has hidden (skipped pages, partial opens, failures). Results are
//! reported on another thread, so without this they would go unseen. Success
//! is never reported this way.

// Win32 FFI: MessageBoxW.
#![allow(unsafe_code)]

/// Shows `message` in a topmost, foreground message box.
///
/// Blocks the calling thread until dismissed, so the box is not hidden behind
/// the window that was just opened.
pub fn tell(title: &str, message: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
        };

        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let text = wide(message);
        let caption = wide(title);
        // SAFETY: two live NUL-terminated wide strings and a null owner
        // window, the documented way to show an unparented message box.
        // `MB_TOPMOST | MB_SETFOREGROUND` because a just-launched viewer is
        // taking the foreground at the same time.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                MB_OK | MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND,
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!("{title}: {message}");
}
