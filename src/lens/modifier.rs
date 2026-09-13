//! Whether the modifier is held, right now.
//!
//! Point 1: a modifier keydown makes the overlay start catching the mouse, and
//! keyup makes it stop. The overlay window carries `WS_EX_NOACTIVATE` and never
//! takes the focus, so egui never sees a keystroke - the answer has to come
//! from outside the toolkit entirely.
//!
//! # Why this polls, for now
//!
//! `GetAsyncKeyState` on a short timer, on one thread, reporting only
//! transitions. Not the low-level keyboard hook that would be the zero-idle-cost
//! answer, and deliberately not yet: a `WH_KEYBOARD_LL` hook needs an
//! `extern "system"` callback, and [`crate::hotkey`] explains at length why this
//! crate has gone to some trouble to define none - `panic = "unwind"` is kept on
//! purpose and unwinding across the foreign boundary is undefined behaviour.
//! Polling needs no callback at all, so the whole overlay can be built and
//! finished against it before that hazard is taken on.
//!
//! The cost is real but small: one syscall every [`POLL`], on a thread that is
//! asleep the rest of the time. It is *not* an egui repaint - the toolkit is
//! woken only when the answer changes, which is twice per interaction rather
//! than a hundred times a second. The hook replaces this later and the seam is
//! [`Modifier`], which does not say how it knows.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::hotkey::spec::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

/// How often the keyboard is asked.
///
/// Eight milliseconds is half a frame at 120 Hz. Point 3 puts the I-beam inside
/// the user's reaction time, which is some two hundred milliseconds, so this is
/// twenty-five times finer than it needs to be and still costs one syscall.
pub const POLL: Duration = Duration::from_millis(8);

/// The shipped default.
///
/// Two modifiers rather than one, which is a departure from the specification's
/// "a modifier" and worth saying why: a single Ctrl would arm the overlay during
/// every Ctrl+C, Ctrl+S and Ctrl+Tab anybody types, so the I-beam would flicker
/// over text all day and the overlay would spend its life catching the mouse.
/// Two is still one gesture and is claimed by nothing.
pub const DEFAULT: &str = "ctrl+shift";

/// The modifiers that must all be held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord(u32);

impl Chord {
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The literal `"off"`: no chord, and the overlay never arms.
    pub const fn is_off(self) -> bool {
        self.0 == 0
    }
}

impl Default for Chord {
    fn default() -> Self {
        Self(MOD_CONTROL | MOD_SHIFT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    /// A token that is not a modifier. Ordinary keys are refused rather than
    /// accepted, because this is held down rather than pressed, and holding a
    /// letter down types it into whatever is underneath.
    NotAModifier(String),
}

impl ParseError {
    pub fn detail(&self) -> String {
        match self {
            Self::Empty => "a modifier cannot be empty (use \"off\" to disable the overlay)".into(),
            Self::NotAModifier(t) => format!(
                "{t:?} is not a modifier - the overlay arms while a key is *held*, so it has to \
                 be one of ctrl, alt, shift or win"
            ),
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail())
    }
}

impl std::error::Error for ParseError {}

/// Parses `"ctrl+shift"`, or `"off"`.
///
/// Deliberately the same grammar as [`crate::hotkey::spec::parse`], minus the
/// key: somebody who has configured one of these should not have to learn a
/// second spelling for the other.
pub fn parse(text: &str) -> Result<Chord, ParseError> {
    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return Err(ParseError::Empty);
    }
    if lowered == "off" || lowered == "none" {
        return Ok(Chord(0));
    }
    let mut bits = 0u32;
    for token in lowered.split('+') {
        let token = token.trim();
        if token.is_empty() {
            return Err(ParseError::Empty);
        }
        bits |= match token {
            "ctrl" | "control" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" | "super" | "meta" | "cmd" => MOD_WIN,
            other => return Err(ParseError::NotAModifier(other.to_string())),
        };
    }
    Ok(Chord(bits))
}

/// The canonical spelling, for `--doctor` and for the hint line.
pub fn describe(chord: Chord) -> String {
    if chord.is_off() {
        return "off".into();
    }
    let mut out = String::new();
    for (bit, name) in [
        (MOD_CONTROL, "Ctrl"),
        (MOD_ALT, "Alt"),
        (MOD_SHIFT, "Shift"),
        (MOD_WIN, "Win"),
    ] {
        if chord.bits() & bit != 0 {
            if !out.is_empty() {
                out.push('+');
            }
            out.push_str(name);
        }
    }
    out
}

/// A live answer to "is the modifier held".
///
/// The seam. Nothing reading this knows whether it is polled or hooked, which
/// is what lets the hook replace the poll without the overlay noticing.
#[derive(Clone)]
pub struct Modifier {
    held: Arc<AtomicBool>,
    chord: Chord,
}

impl Modifier {
    /// An answer that never changes. For tests, and for `--fixture` on a
    /// machine with no keyboard to ask.
    pub fn fixed(held: bool) -> Self {
        Self {
            held: Arc::new(AtomicBool::new(held)),
            chord: Chord::default(),
        }
    }

    pub fn is_held(&self) -> bool {
        self.held.load(Ordering::Relaxed)
    }

    pub fn chord(&self) -> Chord {
        self.chord
    }

    /// Sets the answer by hand. Only for tests and for `--fixture`.
    pub fn set(&self, held: bool) {
        self.held.store(held, Ordering::Relaxed);
    }
}

/// Starts watching the keyboard.
///
/// `wake` is called on a transition and never otherwise - it is
/// `Context::request_repaint`, and calling it on every poll would turn an
/// eight-millisecond syscall into an eight-millisecond redraw of the whole
/// desktop.
///
/// The thread is detached: it holds nothing that needs releasing, and it parks
/// in a sleep that a join could not shorten. `Ok(None)` off Windows, where
/// there is no way to ask.
pub fn watch(chord: Chord, wake: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Option<Modifier>> {
    if chord.is_off() {
        return Ok(None);
    }
    imp::watch(chord, wake)
}

#[cfg(windows)]
mod imp {
    use super::{Chord, Modifier, POLL};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };

    use crate::hotkey::spec::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

    /// The high bit of `GetAsyncKeyState` is "down now". The low bit is "has
    /// been pressed since this was last asked", which is emphatically not what
    /// is wanted here - it is true once for a key that was tapped and released
    /// before the poll, which would arm the overlay for a keystroke the user
    /// had already finished.
    const DOWN: i16 = -0x8000; // i.e. bit 15

    fn is_down(vk: u16) -> bool {
        // SAFETY: takes a virtual-key code and returns a scalar. No pointers,
        // and an unknown code returns zero.
        let state = unsafe { GetAsyncKeyState(vk as i32) };
        state & DOWN != 0
    }

    pub fn held(chord: Chord) -> bool {
        let bits = chord.bits();
        let want = |bit: u32, vk: u16| bits & bit == 0 || is_down(vk);
        want(MOD_CONTROL, VK_CONTROL)
            && want(MOD_SHIFT, VK_SHIFT)
            && want(MOD_ALT, VK_MENU)
            // Windows has no combined "either Win key" code, unlike the other
            // three, so both are asked.
            && (bits & MOD_WIN == 0 || is_down(VK_LWIN) || is_down(VK_RWIN))
    }

    pub fn watch(
        chord: Chord,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Option<Modifier>> {
        let flag = Arc::new(AtomicBool::new(false));
        let shared = Arc::clone(&flag);
        std::thread::Builder::new()
            .name("lens-modifier".into())
            .spawn(move || {
                let mut last = false;
                loop {
                    let now = held(chord);
                    if now != last {
                        last = now;
                        shared.store(now, Ordering::Relaxed);
                        // Only on a transition. See the note on `watch`.
                        wake();
                    }
                    std::thread::sleep(POLL);
                }
            })?;
        Ok(Some(Modifier { held: flag, chord }))
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Chord, Modifier};
    use std::sync::Arc;

    pub fn held(_chord: Chord) -> bool {
        false
    }

    pub fn watch(
        _chord: Chord,
        _wake: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Option<Modifier>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_literal_agrees_with_the_default_text() {
        assert_eq!(parse(DEFAULT).unwrap(), Chord::default());
    }

    #[test]
    fn modifiers_may_be_written_in_any_order_or_case() {
        assert_eq!(parse("ctrl+shift").unwrap(), parse("SHIFT + Ctrl").unwrap());
        assert_eq!(parse("control+alt").unwrap(), parse("alt+ctrl").unwrap());
    }

    #[test]
    fn the_window_key_aliases_all_parse() {
        let win = parse("win").unwrap();
        for alias in ["super", "meta", "cmd"] {
            assert_eq!(parse(alias).unwrap(), win);
        }
    }

    /// The overlay arms while a key is *held*, so an ordinary key would be held
    /// down - which types it into whatever is underneath. Refused with a reason
    /// rather than accepted.
    #[test]
    fn an_ordinary_key_is_refused_with_a_reason() {
        let err = parse("ctrl+a").unwrap_err();
        assert!(matches!(err, ParseError::NotAModifier(_)));
        assert!(err.detail().contains("held"));
    }

    #[test]
    fn off_disables_the_overlay_entirely() {
        assert!(parse("off").unwrap().is_off());
        assert!(parse("NONE").unwrap().is_off());
        assert!(!Chord::default().is_off());
    }

    #[test]
    fn empty_and_dangling_separators_are_refused() {
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert_eq!(parse("  "), Err(ParseError::Empty));
        assert_eq!(parse("ctrl+"), Err(ParseError::Empty));
    }

    #[test]
    fn describe_round_trips_through_parse() {
        for text in ["ctrl+shift", "alt", "win", "ctrl+alt+shift+win", "off"] {
            let chord = parse(text).unwrap();
            assert_eq!(parse(&describe(chord)).unwrap(), chord, "{text}");
        }
    }

    #[test]
    fn describe_spells_the_default_the_way_a_person_would() {
        assert_eq!(describe(Chord::default()), "Ctrl+Shift");
    }

    /// A chord of `off` starts no thread at all.
    #[test]
    fn watching_an_off_chord_starts_nothing() {
        let watcher = watch(parse("off").unwrap(), Arc::new(|| {})).unwrap();
        assert!(watcher.is_none());
    }

    #[test]
    fn a_fixed_modifier_answers_what_it_was_told() {
        let m = Modifier::fixed(false);
        assert!(!m.is_held());
        m.set(true);
        assert!(m.is_held());
    }

    /// Asking the real keyboard must not depend on anything being pressed, but
    /// it must not trap either - this runs on a build machine with no keyboard.
    #[test]
    #[cfg(windows)]
    fn asking_the_keyboard_answers_rather_than_trapping() {
        let _ = imp::held(Chord::default());
        let _ = imp::held(parse("win").unwrap());
    }
}
