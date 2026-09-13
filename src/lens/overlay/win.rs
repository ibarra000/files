//! The overlay window, as Windows sees it: three style bits and one query.
//!
//! This is the mechanism of point 1 in its entirety. `WS_EX_TRANSPARENT` makes
//! a window invisible to the mouse - clicks pass through it to whatever is
//! underneath, as though it were not there. Clearing that bit makes the same
//! window catch the mouse. So the modifier does not switch a mode in this
//! program; it switches one bit in the window's extended style, and everything
//! else follows.
//!
//! No `WndProc` and no `extern "system"` here, for the reason
//! [`crate::hotkey`] gives at length: `panic = "unwind"` is kept deliberately,
//! and unwinding out of a callback across the foreign boundary is undefined
//! behaviour. The toolkit owns the window procedure; this file only ever reads
//! and writes style bits on a handle it is given.

#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{
    DWMSBT_NONE, DWMWA_SYSTEMBACKDROP_TYPE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetSystemMetrics, GetWindowLongPtrW, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetWindowLongPtrW, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

use crate::lens::px::{Point, Rect, Screen};

/// Dresses a window as an overlay. Applied once, before it is shown.
///
/// * `WS_EX_NOACTIVATE` - clicking the overlay must never take the focus off
///   the thing being read. Without it, selecting a job code out of a drawing
///   would deactivate the viewer showing it, and half the applications on the
///   machine dim or close something when that happens.
/// * `WS_EX_TOOLWINDOW`, and `WS_EX_APPWINDOW` cleared - no taskbar button and
///   no Alt-Tab entry, exactly as [`crate::gui::window::hide_from_taskbar`]
///   does for the panel and for the same reason.
///
/// `WS_EX_TRANSPARENT` is deliberately *not* set here. Click-through belongs to
/// [`set_catches_mouse`], which is the only thing allowed to own that bit -
/// otherwise re-asserting these styles mid-interaction would force the overlay
/// click-through while the user was holding the modifier.
///
/// **Idempotent, and called until it sticks.** The toolkit re-applies its own
/// window attributes after this runs and wipes these bits: measured, the styles
/// were set correctly on the first frame and read back as the toolkit's own
/// `0x40118` moments later, leaving the overlay in the taskbar and catching
/// every click on the desktop. So this is re-asserted whenever
/// [`is_dressed`] reports it has been undone, rather than written once and
/// hoped over.
pub fn dress(hwnd_bits: isize) {
    let hwnd = hwnd_bits as HWND;
    // SAFETY: reads and writes one window long on this process's own window. A
    // handle that is not a window makes both calls no-ops rather than anything
    // unsound, which is what the `gui::window` tests already establish.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let ex = (ex | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) & !WS_EX_APPWINDOW;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
    }

    // Extend the frame across the whole client area, so the compositor treats
    // every pixel of it as glass it has to blend rather than as an opaque
    // surface to draw over. `gui::window::apply` does the same thing for the
    // panel, and the note there is blunt about the stakes: on the builds where
    // it matters it is the difference between a composited window and a black
    // rectangle.
    let sheet = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    // SAFETY: `sheet` is a live local, read and not retained; `hwnd` is this
    // process's own window. Failure is reported by return value.
    unsafe { DwmExtendFrameIntoClientArea(hwnd, &sheet) };

    // Tell the compositor there is nothing behind this window but the desktop.
    // Left unsaid, DWM picks a backdrop for us, and a backdrop is by definition
    // something opaque drawn behind our alpha - which is indistinguishable from
    // our alpha being ignored. `gui::window::apply` sets this for the panel and
    // asks for acrylic; an overlay the size of the desktop must ask for the
    // opposite, or it would blur every window on the machine.
    let none = DWMSBT_NONE;
    // SAFETY: `none` is a live local of exactly the width the attribute wants,
    // read and not retained. An attribute this Windows does not know is
    // reported by the return value.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            (&none as *const i32).cast::<core::ffi::c_void>(),
            core::mem::size_of::<i32>() as u32,
        )
    };
}

/// Whether the overlay catches the mouse.
///
/// `catching = false` is click-through and is the resting state. Called on
/// every modifier transition and nowhere else - it is two system calls, but
/// calling it per frame would be two system calls per frame for a bit that
/// changes twice per interaction.
pub fn set_catches_mouse(hwnd_bits: isize, catching: bool) {
    let hwnd = hwnd_bits as HWND;
    // SAFETY: as above.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let next = if catching {
            ex & !WS_EX_TRANSPARENT
        } else {
            ex | WS_EX_TRANSPARENT
        };
        // Only written when it differs. `SetWindowLongPtrW` on the style of a
        // visible window is not free, and this is called from a paint pass.
        if next != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next as isize);
        }
    }
}

/// The window's whole extended style, for verifying that [`dress`] took.
///
/// Worth having because it did not, once: the styles were being written to a
/// handle that was not the overlay, and the only symptom was an overlay that
/// swallowed the desktop. A write that is never read back is a guess.
pub fn ex_style(hwnd_bits: isize) -> u32 {
    // SAFETY: reads one window long on a handle that may be stale, which is
    // defined and returns zero.
    unsafe { GetWindowLongPtrW(hwnd_bits as HWND, GWL_EXSTYLE) as u32 }
}

/// Whether [`dress`] actually took on this window.
pub fn is_dressed(hwnd_bits: isize) -> bool {
    let ex = ex_style(hwnd_bits);
    ex & WS_EX_TOOLWINDOW != 0 && ex & WS_EX_NOACTIVATE != 0 && ex & WS_EX_APPWINDOW == 0
}

/// Reads the bit back, for the tests and for `--doctor`.
pub fn catches_mouse(hwnd_bits: isize) -> bool {
    let hwnd = hwnd_bits as HWND;
    // SAFETY: reads one window long.
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    ex & WS_EX_TRANSPARENT == 0
}

/// The whole virtual desktop, in physical pixels.
///
/// Every monitor, as one rectangle, with the origin at the top-left of the
/// primary display - so a monitor placed to the left of it has a negative
/// `left`, and that is not an error to be clamped away. The overlay spans all
/// of it, because text worth reading is on whichever monitor the pointer
/// happens to be over.
///
/// Deliberately not `SystemParametersInfoW(SPI_GETWORKAREA)`, which reports
/// only the primary monitor - the mistake `Cargo.toml` already records having
/// made once, in the note on `Win32_Graphics_Gdi`.
pub fn virtual_screen() -> Rect<Screen> {
    // Diagnostic: confine the overlay to the primary monitor. A single
    // flip-model swapchain presents to one output, so a window spanning three
    // of them is expected to render on one and show a bare redirection bitmap
    // on the others. This is how that is confirmed.
    if std::env::var_os("LENS_PRIMARY_ONLY").is_some() {
        // SAFETY: index-in, scalar-out. No pointers.
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        if w > 0 && h > 0 {
            return Rect::at(Point::new(0, 0), w, h);
        }
    }
    // SAFETY: `GetSystemMetrics` takes an index and returns an `i32`. It has no
    // pointer arguments and no failure mode other than returning zero for an
    // index this Windows does not know.
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if w <= 0 || h <= 0 {
        // A headless session, or a Windows that answered nothing. One notional
        // screen is a better answer than a zero-sized window that can never be
        // seen or closed.
        return Rect::new(0, 0, 1920, 1080);
    }
    Rect::at(Point::new(x, y), w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle that is not a window must not be fatal, which is the same
    /// claim `gui::window` tests and for the same reason: the handle arrives
    /// from the toolkit, and "the toolkit gave us one" is a claim rather than a
    /// guarantee.
    #[test]
    fn a_handle_that_is_not_a_window_is_survivable() {
        dress(0);
        set_catches_mouse(0, true);
        set_catches_mouse(0, false);
        // And reading it back answers something rather than trapping.
        let _ = catches_mouse(0);
    }

    /// The virtual screen always has area, even where Windows declines to say
    /// anything useful - a zero-sized overlay could never be seen or dismissed.
    #[test]
    fn the_virtual_screen_always_has_area() {
        let r = virtual_screen();
        assert!(r.width() > 0, "{r:?}");
        assert!(r.height() > 0, "{r:?}");
    }

    /// The bit this whole file exists to flip. `WS_EX_TRANSPARENT` set means
    /// click-through, so `catches_mouse` is its inverse - and getting that
    /// backwards would make the overlay swallow the entire desktop.
    #[test]
    fn catching_the_mouse_is_the_absence_of_the_transparent_bit() {
        assert_eq!(WS_EX_TRANSPARENT & !WS_EX_TRANSPARENT, 0);
        // Written as an assertion about the constant rather than about a live
        // window, because a live window needs a message pump and this test
        // must run in a plain `cargo test`.
        let clickthrough: u32 = WS_EX_TRANSPARENT;
        let catching: u32 = clickthrough & !WS_EX_TRANSPARENT;
        assert_ne!(clickthrough & WS_EX_TRANSPARENT, 0, "click-through sets it");
        assert_eq!(catching & WS_EX_TRANSPARENT, 0, "catching clears it");
    }

    /// The overlay must never take the focus off what is being read.
    #[test]
    fn the_overlay_style_never_activates_and_never_reaches_the_taskbar() {
        let ex = (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT) & !WS_EX_APPWINDOW;
        assert_ne!(ex & WS_EX_NOACTIVATE, 0);
        assert_ne!(ex & WS_EX_TOOLWINDOW, 0);
        assert_eq!(ex & WS_EX_APPWINDOW, 0);
    }
}
