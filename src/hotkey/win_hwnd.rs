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

/// The usable area of the monitor nearest a point, in device pixels.
///
/// The companion to [`work_area`], and it exists because the two questions have
/// different answers exactly when it matters. A remembered position is restored
/// on the first summon after a cold start, at which moment the window is still
/// wherever the toolkit created it - almost always the primary monitor. Asking
/// `MonitorFromWindow` there would clamp a position saved on the second screen
/// into the first one, and the panel would come back on the wrong display with
/// nothing to say it had moved.
///
/// `MONITOR_DEFAULTTONEAREST` is what makes an unplugged monitor survivable: a
/// point that is now on no display at all resolves to the closest one that
/// exists, and [`super::geometry::place_at`] pulls the panel inside it.
pub fn work_area_at(at: (i32, i32)) -> Option<super::geometry::RectPx> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };

    // SAFETY: `POINT` and `MONITORINFO` are live locals, the latter zeroed with
    // `cbSize` set as documented. `MONITOR_DEFAULTTONEAREST` guarantees the
    // monitor handle is never null, whatever coordinate is passed in.
    unsafe {
        let point = POINT { x: at.0, y: at.1 };
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
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

    /// A coordinate on no display at all is the unplugged-monitor case, and it
    /// has to name a monitor rather than fail - otherwise a position saved
    /// against a screen somebody took home would lose the panel entirely.
    #[test]
    fn a_point_on_no_monitor_still_names_the_nearest_one() {
        for at in [(0, 0), (-30_000, -30_000), (i32::MAX, i32::MIN)] {
            if let Some(work) = work_area_at(at) {
                assert!(work.width() > 0, "{at:?} gave {work:?}");
                assert!(work.height() > 0, "{at:?} gave {work:?}");
            }
        }
    }
}
