//! Saying something to somebody who has no window to be told in.
//!
//! A native message box, which is the bluntest thing this program does and is
//! used for exactly two kinds of moment: the program cannot start at all, and
//! the panel has gone away with something still to say.
//!
//! # Why the second kind exists
//!
//! The panel puts itself away once a file is on its way, which is what a
//! launcher does and what Ueli does. The objection to that - written into
//! `assets/default_config.toml` when the setting shipped switched *off* - was
//! specific and correct: an open is answered on another thread, so "Opened 11
//! of 13 pages", the list of pages skipped and "Could not open …" all arrived
//! at a window that had already gone. Nobody ever read one.
//!
//! A document quietly missing page seven is the worst outcome this program
//! can produce, because nothing on screen would ever reveal it. So the panel
//! is allowed to leave, and the messages that matter follow the user instead
//! of waiting on a surface nobody is looking at. Ueli does the same thing
//! with `dialog.showErrorBox`.
//!
//! Only the ones that matter. An open that went perfectly says nothing, and a
//! setting that saved says nothing - a modal for either would be a program
//! that interrupts to report success.

/// Puts `message` in front of the user, with no window of ours to put it on.
///
/// Blocking, and deliberately: it is called from the thread that dispatches
/// commands, and the alternative - a box that appears behind whatever just
/// opened - is a box nobody sees, which is the failure this exists to fix.
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
        // window, which is the documented way to show a message box with no
        // parent. `MB_TOPMOST | MB_SETFOREGROUND` because the viewer this is
        // about has just been launched and is taking the foreground itself.
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
