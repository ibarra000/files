//! Native window attributes: backdrop material, corner rounding, dark-mode
//! border, and taskbar visibility.
//!
//! # No version checks
//!
//! `DwmSetWindowAttribute` returns `E_INVALIDARG` for an unknown attribute and
//! changes nothing, so the return value serves as the version check.
//!
//! # Not `#![cfg(windows)]`
//!
//! `gui::mod` names [`Backdrop`] unconditionally, so a file-wide gate would
//! break compilation, and the tests, on other platforms. Only the DWM calls are
//! `#[cfg(windows)]`; the non-Windows stubs return [`Backdrop::Painted`].
//!
//! # Requested versus granted
//!
//! [`Material`] is what the configuration requests. [`Backdrop`] is what the
//! compositor granted, reduced to the one question the renderer needs: whether
//! to paint its own fill.

// Win32 FFI: DWM backdrop, corners and frame.
#![allow(unsafe_code)]

#[cfg(windows)]
use windows_sys::Win32::Foundation::{HWND, S_OK};
#[cfg(windows)]
use windows_sys::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TABBEDWINDOW, DWMSBT_TRANSIENTWINDOW,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
#[cfg(windows)]
use windows_sys::Win32::UI::Controls::MARGINS;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` before Windows 10 20H1.
///
/// Not in `windows-sys`, which ships only the later value. Both are tried,
/// newer first.
#[cfg(windows)]
const DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1: i32 = 19;

/// The backdrop material requested from the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Material {
    /// The flyout material: blurs what is behind the window.
    ///
    /// The default because Mica samples the wallpaper and does not track a
    /// moving window, which looks wrong on a panel that is summoned over other
    /// windows and can be dragged.
    #[default]
    Acrylic,
    /// Samples the desktop wallpaper rather than the window behind.
    Mica,
    /// Mica's variant for tabbed windows.
    Tabbed,
    /// No system backdrop; the panel paints its own translucent fill.
    None,
}

impl Material {
    pub const ALL: [Self; 4] = [Self::Acrylic, Self::Mica, Self::Tabbed, Self::None];

    /// The configuration-file spelling.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Acrylic => "acrylic",
            Self::Mica => "mica",
            Self::Tabbed => "tabbed",
            Self::None => "none",
        }
    }

    /// The label shown in the settings window.
    pub const fn display(self) -> &'static str {
        match self {
            Self::Acrylic => "Acrylic",
            Self::Mica => "Mica",
            Self::Tabbed => "Tabbed",
            Self::None => "None",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|m| m.name().eq_ignore_ascii_case(text.trim()))
    }
}

/// The backdrop actually in effect after the compositor responds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// Drawn by the compositor, and visible whenever the window is.
    Compositor,
    /// A translucent fill painted by the panel.
    ///
    /// Used for [`Material::None`] and on Windows versions older than 11 22H2,
    /// which reject the backdrop attribute.
    Painted,
}

/// Sets one DWM attribute and returns whether Windows accepted it.
///
/// `DWMWA_*` constants are `i32` while the parameter is `u32`; the cast lives
/// here so call sites need none.
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

/// Rounds or squares the window's corners.
///
/// Square while docked, so a panel flush against the work-area edge shows no
/// gaps at its corners.
#[cfg(windows)]
pub fn set_rounded(hwnd_bits: isize, rounded: bool) {
    let preference = if rounded {
        DWMWCP_ROUND
    } else {
        DWMWCP_DONOTROUND
    };
    set(
        hwnd_bits as HWND,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &preference,
    );
}

/// Applies the window attributes and returns the backdrop actually granted.
///
/// `rounded` is false while the panel is docked; see [`set_rounded`].
#[cfg(windows)]
pub fn apply(hwnd_bits: isize, dark: bool, want: Material, rounded: bool) -> Backdrop {
    let hwnd = hwnd_bits as HWND;

    let backdrop = match want {
        Material::None => {
            // Explicitly off, since a window can inherit a system backdrop that
            // would show under the painted fill.
            set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_NONE);
            Backdrop::Painted
        }
        Material::Acrylic | Material::Mica | Material::Tabbed => {
            // Extend the frame over the whole client area so the compositor
            // can draw the backdrop there. Only on this path, since the
            // extended frame would otherwise show under the painted fill.
            let sheet = MARGINS {
                cxLeftWidth: -1,
                cxRightWidth: -1,
                cyTopHeight: -1,
                cyBottomHeight: -1,
            };
            // SAFETY: `sheet` is a live local read and not retained; `hwnd` is
            // this process's own window.
            unsafe { DwmExtendFrameIntoClientArea(hwnd, &sheet) };

            // `DWMSBT_TRANSIENTWINDOW` is acrylic; `DWMSBT_MAINWINDOW` is
            // Mica. See `Material::Acrylic`.
            let kind = match want {
                Material::Mica => DWMSBT_MAINWINDOW,
                Material::Tabbed => DWMSBT_TABBEDWINDOW,
                _ => DWMSBT_TRANSIENTWINDOW,
            };
            if set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind) {
                Backdrop::Compositor
            } else {
                // Unsupported on this Windows: clear it and paint a fill.
                set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_NONE);
                Backdrop::Painted
            }
        }
    };

    // `DWMWCP_ROUND` is the 8 pt Windows 11 window radius; `DWMWCP_ROUNDSMALL`
    // is the 4 pt menu radius. DWM provides the panel's only rounding and
    // outline, so the painted surface (`paint_surface` in
    // `src/gui/panel/mod.rs`) need not match a second curve. The hotkey thread
    // re-applies this on each summon, when the dock setting takes effect.
    set_rounded(hwnd_bits, rounded);

    // Sets the border colour for the theme. Attribute 20 since 20H1, 19
    // before; 20 is tried first because on older builds it is a different
    // attribute.
    let dark_flag: i32 = i32::from(dark);
    if !set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark_flag) {
        set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1, &dark_flag);
    }

    backdrop
}

/// Removes the window from the taskbar and from Alt-Tab.
///
/// Must run while the window is hidden: the shell reads these style bits only
/// when a window is first shown.
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

/// Non-Windows stub; always [`Backdrop::Painted`].
#[cfg(not(windows))]
pub fn apply(_hwnd_bits: isize, _dark: bool, _want: Material) -> Backdrop {
    Backdrop::Painted
}

/// Non-Windows stub; does nothing.
#[cfg(not(windows))]
pub fn hide_from_taskbar(_hwnd_bits: isize) {}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// An invalid handle yields [`Backdrop::Painted`] rather than a panic.
    #[test]
    fn a_handle_that_is_not_a_window_is_survivable() {
        for material in Material::ALL {
            assert_eq!(
                apply(0, true, material, true),
                Backdrop::Painted,
                "{material:?}"
            );
        }
        hide_from_taskbar(0);
    }

    #[test]
    fn the_corner_preference_is_the_window_radius_not_the_menu_one() {
        use windows_sys::Win32::Graphics::Dwm::DWMWCP_ROUNDSMALL;
        assert_ne!(DWMWCP_ROUND, DWMWCP_ROUNDSMALL);
    }

    #[test]
    fn every_material_asks_for_a_different_thing() {
        let mut seen = vec![
            DWMSBT_TRANSIENTWINDOW,
            DWMSBT_MAINWINDOW,
            DWMSBT_TABBEDWINDOW,
            DWMSBT_NONE,
        ];
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 4, "two materials ask for the same backdrop");
    }

    /// Names round-trip case-insensitively, since they are hand-edited.
    #[test]
    fn every_material_parses_back_from_what_it_writes() {
        for material in Material::ALL {
            assert_eq!(Material::parse(material.name()), Some(material));
            assert_eq!(
                Material::parse(&material.name().to_uppercase()),
                Some(material)
            );
        }
        assert_eq!(Material::parse("glass"), None);
    }
}
