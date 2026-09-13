//! The overlay: a sheet of glass over the desktop that catches the mouse only
//! while a modifier is held.
//!
//! Three of the four files here have no Windows in them and are tested
//! anywhere: [`hit`] is the selection model, [`select`] is the interaction, and
//! [`view`] is the one place screen pixels become egui points. [`win`] is the
//! part that talks to Windows, and it is three style bits and one query - the
//! same split [`crate::hotkey::geometry`] makes against [`crate::hotkey::win`].

pub mod hit;
pub mod select;
pub mod view;
pub mod window;

#[cfg(windows)]
pub mod win;

#[cfg(windows)]
pub use win as platform;

/// Everything the platform cannot do.
///
/// An overlay without click-through would catch every click on the desktop, so
/// off Windows the honest answer is a window that behaves like an ordinary one
/// and a virtual screen of a plausible size. `lens` is a Windows feature; this
/// exists so that the pure modules above compile and are tested on the machines
/// the rest of the crate is developed on.
#[cfg(not(windows))]
pub mod platform {
    use crate::lens::px::{Point, Rect, Screen};

    pub fn dress(_hwnd: isize) {}
    pub fn set_catches_mouse(_hwnd: isize, _catching: bool) {}
    pub fn catches_mouse(_hwnd: isize) -> bool {
        true
    }
    pub fn ex_style(_hwnd: isize) -> u32 {
        0
    }
    pub fn is_dressed(_hwnd: isize) -> bool {
        true
    }
    pub fn virtual_screen() -> Rect<Screen> {
        Rect::at(Point::new(0, 0), 1920, 1080)
    }
}
