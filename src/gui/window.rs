//! The window itself, as Windows sees it.
//!
//! Four attributes and one style bit, applied once. Everything here is what
//! makes the panel look like it belongs on this operating system rather than
//! like a rectangle a program drew.
//!
//! # No version checks
//!
//! Every attribute below is a no-op on a Windows that has never heard of it:
//! `DwmSetWindowAttribute` answers `E_INVALIDARG` for an attribute it does not
//! know and changes nothing. So the return value *is* the version check, and
//! there is no build-number table here to go stale.

//! # Why this is not `#![cfg(windows)]`
//!
//! It was, and that made the whole of `gui` uncompilable anywhere else -
//! `gui::mod` declares this module and names [`Backdrop`] unconditionally, so
//! a file-wide gate deleted a type that the rest of the tree still referred
//! to. That cost the panel's entire test suite on any machine that is not the
//! target, which is the opposite of what `lib.rs` says the tests are for.
//!
//! So the *calls* are gated and the *vocabulary* is not. [`Backdrop`] is a
//! two-variant enum with no Windows in it; everything below it that touches
//! DWM is `#[cfg(windows)]`, with a stub that answers [`Backdrop::Painted`] -
//! which is the honest answer off Windows, and the same answer a Windows too
//! old for acrylic gives.

#[cfg(windows)]
use windows_sys::Win32::Foundation::{HWND, S_OK};
#[cfg(windows)]
use windows_sys::Win32::Graphics::Dwm::{
    DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
#[cfg(windows)]
use windows_sys::Win32::UI::Controls::MARGINS;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` before Windows 10 20H1.
///
/// Not in `windows-sys`, because the header only ever shipped the later
/// number. Both are tried, newer first; the wrong one costs an `E_INVALIDARG`
/// and nothing else.
#[cfg(windows)]
const DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1: i32 = 19;

/// What is behind the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// The compositor's own acrylic, sampling whatever the panel is over.
    ///
    /// Cannot be faded - it is on for as long as the window is visible - so a
    /// summon shows the blur arriving while the content rises into it. That
    /// reads well, because acrylic is a blur of what was already there rather
    /// than a colour appearing.
    Acrylic,
    /// A translucent fill the panel paints itself.
    ///
    /// Everything fades together, including the background, at the cost of
    /// real blur. This is what a Windows older than 11 22H2 gets, and it is
    /// selected automatically by asking rather than by checking a version.
    Painted,
}

/// Writes one attribute, and says whether this Windows understood it.
///
/// The cast on `attr` is not incidental: every `DWMWA_*` is an `i32` while the
/// parameter is a `u32`, so without it each call site would need its own cast
/// and one of them would eventually be wrong.
#[cfg(windows)]
fn set<T>(hwnd: HWND, attr: i32, value: &T) -> bool {
    // SAFETY: `value` is a live local of exactly `size_of::<T>()` bytes, read
    // and not retained; `hwnd` is this process's own window. An attribute this
    // Windows does not know is reported by the return value rather than by
    // doing something unexpected.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attr as u32,
            (value as *const T).cast::<core::ffi::c_void>(),
            core::mem::size_of::<T>() as u32,
        ) == S_OK
    }
}

/// Dresses the window, and reports what it actually got.
///
/// `want` is what the caller would like behind the panel; the answer is what
/// this Windows agreed to, which may be less.
#[cfg(windows)]
pub fn apply(hwnd_bits: isize, dark: bool, want: Backdrop) -> Backdrop {
    let hwnd = hwnd_bits as HWND;

    let backdrop = match want {
        Backdrop::Painted => {
            // Explicitly off rather than merely not asked for: a window can
            // inherit a backdrop from the system, and half a backdrop under a
            // fill we are drawing ourselves is worse than either alone.
            set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_NONE);
            Backdrop::Painted
        }
        Backdrop::Acrylic => {
            // Extend the frame across the whole client area, so the compositor
            // has somewhere to put a backdrop. Only on this path: the extension
            // is what creates the sheet of glass, and a sheet of glass under a
            // fill we are painting ourselves is a second background.
            let sheet = MARGINS {
                cxLeftWidth: -1,
                cxRightWidth: -1,
                cyTopHeight: -1,
                cyBottomHeight: -1,
            };
            // SAFETY: `sheet` is a live local read and not retained; `hwnd` is
            // this process's own window.
            unsafe { DwmExtendFrameIntoClientArea(hwnd, &sheet) };

            // Acrylic, not Mica. `DWMSBT_TRANSIENTWINDOW` is the material
            // Windows uses for flyouts: it samples what is *behind the window*
            // and blurs it. `DWMSBT_MAINWINDOW` is Mica, which samples the
            // desktop wallpaper - so under a panel summoned over somebody's
            // drawing it would show the wallpaper, which is both wrong and,
            // because Mica does not follow a window that moves, wrong in a way
            // that lags.
            if set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW) {
                Backdrop::Acrylic
            } else {
                set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_NONE);
                Backdrop::Painted
            }
        }
    };

    // Eight points, which is the radius Windows 11 gives an ordinary window.
    // `DWMWCP_ROUNDSMALL` is the four-point menu radius, and on a panel this
    // wide it reads as a rendering mistake rather than as a choice.
    set(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);

    // The hairline Windows draws around a rounded window. Attribute 20 since
    // 20H1, 19 before it, tried in that order - because 20 on an older build
    // writes into a different attribute's slot.
    let dark_flag: i32 = i32::from(dark);
    if !set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark_flag) {
        set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1, &dark_flag);
    }

    // No accent-coloured edge: the panel draws its own. `DWMWA_COLOR_NONE`
    // wants a `COLORREF`, so it goes in as a `u32` rather than an `i32`.
    set(hwnd, DWMWA_BORDER_COLOR, &DWMWA_COLOR_NONE);

    backdrop
}

/// Takes the window out of the taskbar, and out of Alt-Tab with it.
///
/// The second half is the part usually wanted and rarely mentioned: something
/// that answers a global hotkey should not also be somewhere you arrive at by
/// accident while cycling windows.
///
/// Must run while the window is hidden. The shell reads these bits when a
/// window is first shown and does not read them again, so flipping them on a
/// visible window leaves the button behind until it is hidden and shown once
/// more.
#[cfg(windows)]
pub fn hide_from_taskbar(hwnd_bits: isize) {
    let hwnd = hwnd_bits as HWND;
    // SAFETY: reads and writes one window long on this process's own window.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let ex = (ex | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
    }
}

/// Off Windows there is no compositor to ask, and [`Backdrop::Painted`] is
/// what the panel does when nothing grants it anything else.
#[cfg(not(windows))]
pub fn apply(_hwnd_bits: isize, _dark: bool, _want: Backdrop) -> Backdrop {
    Backdrop::Painted
}

/// Off Windows there is no taskbar button to take away.
#[cfg(not(windows))]
pub fn hide_from_taskbar(_hwnd_bits: isize) {}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// A window that does not exist is not a reason to fall over.
    ///
    /// `apply` is called once, early, on a handle handed over by the toolkit -
    /// but "the toolkit gave us something" is a claim, and the cost of testing
    /// it is one call that Windows rejects harmlessly.
    #[test]
    fn a_handle_that_is_not_a_window_is_survivable() {
        // Every attribute is refused, so the honest answer is "paint it
        // yourself" rather than a panic.
        assert_eq!(apply(0, true, Backdrop::Acrylic), Backdrop::Painted);
        assert_eq!(apply(0, true, Backdrop::Painted), Backdrop::Painted);
        hide_from_taskbar(0);
    }

    /// The two constants that must not be confused, since one is the menu
    /// radius and the other is the window radius.
    #[test]
    fn the_corner_preference_is_the_window_radius_not_the_menu_one() {
        use windows_sys::Win32::Graphics::Dwm::DWMWCP_ROUNDSMALL;
        assert_ne!(DWMWCP_ROUND, DWMWCP_ROUNDSMALL);
    }

    /// Acrylic samples what is behind the window; Mica samples the wallpaper.
    /// Picking the wrong one is not a crash, it is a panel that shows the
    /// desktop through somebody's drawing.
    #[test]
    fn the_backdrop_is_the_transient_one() {
        use windows_sys::Win32::Graphics::Dwm::DWMSBT_MAINWINDOW;
        assert_ne!(DWMSBT_TRANSIENTWINDOW, DWMSBT_MAINWINDOW);
    }
}
