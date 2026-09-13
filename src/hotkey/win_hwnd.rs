//! Which monitor the panel is on.
//!
//! # What used to be here
//!
//! Three hundred and fifty lines of finding somebody else's window.
//! `GetConsoleWindow` lies under Windows Terminal - it returns the
//! pseudo-console's hidden window, and `SetWindowPos` on that succeeds, moves
//! nothing, and reports no error - so this module walked the process table with
//! ToolHelp to find the terminal's own process, then walked the Z-order with
//! `GetTopWindow`/`GetWindow` looking for a top-level window belonging to it
//! that was plausibly the one on screen.
//!
//! It worked, and every line of it was an approximation of one thing: a window
//! of our own. The window is ours now, so there is nothing to find, nothing to
//! confirm, and no way for the answer to be wrong. What is left is the one
//! question that is still worth asking - *which screen is it on* - and that one
//! has an exact answer.

#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;

/// The usable area of the monitor `hwnd` is on, in device pixels.
///
/// The monitor the window is actually on, not the primary one. `SPI_GETWORKAREA`
/// would have given the primary monitor's, which is how an overlay ends up
/// off-screen for anyone with two displays - and two displays is the ordinary
/// case in the office this runs in.
///
/// "Work area" rather than "monitor": it excludes the taskbar, so a panel
/// placed in it cannot come up underneath one.
pub fn work_area(hwnd: HWND) -> Option<super::geometry::RectPx> {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };

    // SAFETY: `MONITORINFO` is zeroed with `cbSize` set, which is the
    // documented requirement; the monitor handle comes from the call above and
    // `MONITOR_DEFAULTTONEAREST` guarantees it is never null. A window handle
    // that is not a window still resolves to the nearest monitor rather than
    // failing, which is the behaviour wanted for a panel that has not been
    // created yet.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return None;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let w = info.rcWork;
        Some(super::geometry::RectPx::new(
            w.left, w.top, w.right, w.bottom,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle that is not a window must still answer, because the panel asks
    /// before it has one on the first summon after a cold start.
    #[test]
    fn a_handle_that_is_not_a_window_still_names_a_monitor() {
        let work = work_area(std::ptr::null_mut());
        // Headless build agents have no monitor at all, so `None` is a legal
        // answer; what is not legal is a panic or a nonsense rectangle.
        if let Some(work) = work {
            assert!(work.width() > 0, "{work:?}");
            assert!(work.height() > 0, "{work:?}");
        }
    }
}
