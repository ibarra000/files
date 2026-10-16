//! Monitor work-area lookups for placing the panel.

// Win32 FFI: MonitorFromWindow and MonitorFromPoint, to place the panel.
#![allow(unsafe_code)]
#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;

/// Returns the work area (excluding the taskbar) of the monitor `hwnd` is on,
/// in device pixels.
///
/// Uses the window's monitor rather than the primary one that
/// `SPI_GETWORKAREA` reports.
pub fn work_area(hwnd: HWND) -> Option<super::geometry::RectPx> {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };

    // SAFETY: `MONITORINFO` is zeroed with `cbSize` set, which is the
    // documented requirement; the monitor handle comes from the call above and
    // `MONITOR_DEFAULTTONEAREST` guarantees it is never null. An invalid window
    // handle still resolves to the nearest monitor.
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

/// Returns the work area of the monitor nearest a point, in device pixels.
///
/// Used for remembered positions: on the first summon the window may still be
/// on another monitor, so [`work_area`] would give the wrong screen. A point
/// on no display (an unplugged monitor) resolves to the nearest one.
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

    #[test]
    fn a_handle_that_is_not_a_window_still_names_a_monitor() {
        let work = work_area(std::ptr::null_mut());
        // Headless agents have no monitor, so `None` is allowed.
        if let Some(work) = work {
            assert!(work.width() > 0, "{work:?}");
            assert!(work.height() > 0, "{work:?}");
        }
    }

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
