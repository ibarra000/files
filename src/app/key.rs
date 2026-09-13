//! Keys, as this program understands them.
//!
//! Deliberately smaller than any toolkit's key enum. What is here is exactly
//! what [`super::state::keys`] matches on, so the binding table in
//! `tests/bindings.rs` can be exhaustive over it and the catch-all arm has
//! almost nothing left to catch.
//!
//! It exists because the state machine should not know which toolkit is
//! feeding it. That was true in principle while the only source was a
//! terminal, and it stops being a principle the moment there are two: a type
//! owned by whichever library happens to be drawing is a type the state
//! machine cannot be tested against without that library.
//!
//! # Characters are not keys
//!
//! [`Key::Char`] carries a character the layout has already composed, not a
//! physical key. Shift, AltGr and any dead key are spent by the time one of
//! these exists, which is why the modifiers on a `Char` mean "a combination"
//! and never "how this letter was typed". Getting that backwards is how `@` on
//! a German keyboard stops working.

/// A key, named by what it means rather than by a scancode.
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

/// Which modifiers were held.
///
/// Three bools rather than bitflags: every read in `keys.rs` is a single
/// field, the type stays `const`-constructible with no dependency, and a
/// struct literal in a test says what it means without a bitwise or.
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
    /// Named, because it is not a combination anybody chose: it is how a
    /// German, Polish or French layout types `@`, `{`, `[` and every accented
    /// letter, and the arm in `keys.rs` that lets those through exists for
    /// exactly this value.
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
/// Two variants, not three. Windows reports both halves of every keystroke,
/// and without a guard each one is handled twice - but an auto-repeat is a
/// *press*, and a third `Repeat` variant is an invitation to drop it. Held
/// Backspace has to keep deleting, so that decision is made once, at the edge,
/// and the state machine never sees the question.
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
    /// A press.
    ///
    /// Named and shaped like the constructor it replaces, so the tests that
    /// build thousands of these change by a rename rather than a rewrite.
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

    /// The guard the whole press/release story rests on.
    #[test]
    fn a_release_is_not_a_press() {
        assert!(KeyEvent::new(Key::Enter, Mods::NONE).is_press());
        assert!(!KeyEvent::released(Key::Enter, Mods::NONE).is_press());
    }
}
