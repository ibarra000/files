//! Toolkit-independent key events consumed by the state machine.
//!
//! The set is exactly what `app::state::keys` matches on, so the binding
//! table in `tests/bindings.rs` can be exhaustive over it. Keeping the type
//! independent of any UI library lets the state machine be tested without one.
//!
//! # Characters are not keys
//!
//! [`Key::Char`] carries a character the layout has already composed, not a
//! physical key. Shift, AltGr and dead keys are already applied, so modifiers
//! on a `Char` describe a combination, never how the letter was typed.
//! Treating them otherwise breaks `@` on a German layout.

/// A key, named by meaning rather than by scancode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character the layout has already composed. See the module note.
    Char(char),
    Backspace,
    Delete,
    Enter,
    Esc,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// The modifiers held during a keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };
    pub const CTRL: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
    };
    pub const ALT: Self = Self {
        ctrl: false,
        alt: true,
        shift: false,
    };
    pub const SHIFT: Self = Self {
        ctrl: false,
        alt: false,
        shift: true,
    };
    /// AltGr, which Windows reports as Ctrl+Alt.
    ///
    /// Layouts such as German, Polish and French use it to type `@`, `{`, `[`
    /// and accented letters; `keys.rs` lets characters with it through.
    pub const ALTGR: Self = Self {
        ctrl: true,
        alt: true,
        shift: false,
    };

    pub const fn is_none(self) -> bool {
        !self.ctrl && !self.alt && !self.shift
    }
}

/// Press or release.
///
/// Auto-repeat is reported as a press, so held Backspace keeps deleting. The
/// state machine acts on presses only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeyPhase {
    #[default]
    Press,
    Release,
}

/// One keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
    pub phase: KeyPhase,
}

impl KeyEvent {
    /// Returns a press of `key` with `mods`.
    pub const fn new(key: Key, mods: Mods) -> Self {
        Self {
            key,
            mods,
            phase: KeyPhase::Press,
        }
    }

    pub const fn released(key: Key, mods: Mods) -> Self {
        Self {
            key,
            mods,
            phase: KeyPhase::Release,
        }
    }

    pub const fn is_press(self) -> bool {
        matches!(self.phase, KeyPhase::Press)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_key_carries_no_modifiers() {
        assert!(Mods::NONE.is_none());
        assert!(!Mods::CTRL.is_none());
        assert!(!Mods::SHIFT.is_none());
    }

    #[test]
    fn a_release_is_not_a_press() {
        assert!(KeyEvent::new(Key::Enter, Mods::NONE).is_press());
        assert!(!KeyEvent::released(Key::Enter, Mods::NONE).is_press());
    }
}
