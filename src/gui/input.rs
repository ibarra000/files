//! Translates toolkit input events into crate events.
//!
//! # One source for characters, one for bindings
//!
//! Typed characters come only from `Event::Text`, and bindings only from
//! `Event::Key`. `egui-winit` sends an `Event::Key` for every press and
//! withholds `Event::Text` only while a modifier is held, so forwarding a
//! letter from both doubles it. [`binding`] therefore holds only non-printable
//! keys, and the Ctrl letter bindings live in [`chord`], consulted only while
//! Ctrl is held.
//!
//! # Clipboard chords
//!
//! `egui-winit` converts Ctrl+C, Ctrl+X and Ctrl+V (and Ctrl+Insert,
//! Shift+Delete) into `Event::Copy`, `Event::Cut` and `Event::Paste` instead of
//! key events. [`translate`] converts them back into the chords the state
//! machine binds, and `C`, `V` and `X` are absent from [`chord`] so only one
//! route can fire.
//!
//! # AltGr
//!
//! On Windows AltGr is reported as Ctrl+Alt, and `egui-winit` drops
//! `Event::Text` whenever Ctrl is set, so characters such as `@ { [ ]` on
//! German, Polish or French layouts never reach egui. `egui::Key` cannot
//! represent them, so `altgr` asks the OS what the active layout would produce,
//! only on the Ctrl+Alt path. If an `Event::Text` is present in the same frame,
//! [`translate`] skips the reconstruction so the two never both fire.

use eframe::egui;

use crate::app::event::AppEvent;
use crate::app::key::{Key, KeyEvent, Mods};

/// Converts one frame of toolkit input into crate events.
pub fn translate(input: &egui::InputState) -> Vec<AppEvent> {
    // Toolkit text takes precedence over AltGr reconstruction, so a future
    // `egui-winit` that stops withholding text does not double characters.
    let toolkit_typed = input
        .events
        .iter()
        .any(|event| matches!(event, egui::Event::Text(_)));

    let mut out = Vec::new();
    for event in &input.events {
        match event {
            egui::Event::Text(text) => {
                for c in text.chars() {
                    out.push(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)));
                }
            }
            egui::Event::Paste(text) => out.push(AppEvent::Paste(text.clone())),
            // Forwarded as-is; whether focus loss hides the panel is a setting
            // handled by the state layer.
            egui::Event::WindowFocused(has_focus) => {
                out.push(AppEvent::WindowFocus(*has_focus));
            }
            // The toolkit consumed these keystrokes; restore the chords. See the
            // module docs.
            egui::Event::Copy => {
                out.push(AppEvent::Key(KeyEvent::new(Key::Char('c'), Mods::CTRL)));
            }
            egui::Event::Cut => {
                out.push(AppEvent::Key(KeyEvent::new(Key::Char('x'), Mods::CTRL)));
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let mods = Mods {
                    ctrl: modifiers.ctrl,
                    alt: modifiers.alt,
                    shift: modifiers.shift,
                };
                // AltGr is checked first, since on affected layouts it is the
                // character. Evaluated lazily to avoid a Windows call on every
                // keystroke.
                let reconstructed = (!toolkit_typed && mods.ctrl && mods.alt)
                    .then(|| altgr::character(*key))
                    .flatten();
                if let Some(c) = reconstructed {
                    out.push(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::ALTGR)));
                    continue;
                }
                // Letters are bindings only while Ctrl is held, which is when
                // the toolkit withholds their text.
                let mapped = binding(*key).or_else(|| mods.ctrl.then(|| chord(*key)).flatten());
                if let Some(mapped) = mapped {
                    out.push(AppEvent::Key(KeyEvent::new(mapped, mods)));
                }
            }
            // The mouse wheel is intentionally ignored; it must not move the
            // selection or open recall via the Up arrow.
            _ => {}
        }
    }
    out
}

/// Maps non-printable keys to bindings.
///
/// Printable keys must not appear here: they arrive as `Event::Text`, and
/// listing them would double them. Ctrl letter bindings are in [`chord`].
fn binding(key: egui::Key) -> Option<Key> {
    use egui::Key as E;
    Some(match key {
        E::ArrowLeft => Key::Left,
        E::ArrowRight => Key::Right,
        E::ArrowUp => Key::Up,
        E::ArrowDown => Key::Down,
        E::Home => Key::Home,
        E::End => Key::End,
        E::PageUp => Key::PageUp,
        E::PageDown => Key::PageDown,
        E::Backspace => Key::Backspace,
        E::Delete => Key::Delete,
        E::Enter => Key::Enter,
        E::Escape => Key::Esc,
        E::Tab => Key::Tab,
        E::F1 => Key::F(1),
        E::F2 => Key::F(2),
        E::F3 => Key::F(3),
        E::F4 => Key::F(4),
        E::F5 => Key::F(5),
        E::F6 => Key::F(6),
        E::F7 => Key::F(7),
        E::F8 => Key::F(8),
        E::F9 => Key::F(9),
        E::F10 => Key::F(10),
        E::F11 => Key::F(11),
        E::F12 => Key::F(12),
        _ => return None,
    })
}

/// Maps the keys bound with Ctrl. Called only while Ctrl is held.
///
/// `C`, `V` and `X` are absent because the toolkit converts them to clipboard
/// events (see [`translate`]). Every entry here must have a matching arm in
/// `src/app/state/keys.rs` and vice versa; `tests/bindings.rs` drives this
/// function to check that.
fn chord(key: egui::Key) -> Option<Key> {
    use egui::Key as E;
    Some(match key {
        E::A => Key::Char('a'),
        E::Q => Key::Char('q'),
        E::U => Key::Char('u'),
        E::W => Key::Char('w'),
        // Actions: open as document, open with first viewer, show in Explorer,
        // and the actions menu.
        E::D => Key::Char('d'),
        E::E => Key::Char('e'),
        E::O => Key::Char('o'),
        E::K => Key::Char('k'),
        // Home-row alternatives for the arrows and the search box.
        E::P => Key::Char('p'),
        E::N => Key::Char('n'),
        E::L => Key::Char('l'),
        // Ctrl+, opens settings. Listed here rather than in `binding` because a
        // bare comma is text.
        E::Comma => Key::Char(','),
        _ => return None,
    })
}

/// Queries the OS for the character the active layout would produce.
#[cfg(windows)]
mod altgr {
    use eframe::egui;

    /// Returns the virtual-key code for a printable `egui::Key`.
    ///
    /// Uses the US mapping because virtual keys are assigned by position under
    /// US names; `ToUnicodeEx` then returns the active layout's character.
    fn virtual_key(key: egui::Key) -> Option<u16> {
        use egui::Key as E;
        Some(match key {
            E::Num0 => 0x30,
            E::Num1 => 0x31,
            E::Num2 => 0x32,
            E::Num3 => 0x33,
            E::Num4 => 0x34,
            E::Num5 => 0x35,
            E::Num6 => 0x36,
            E::Num7 => 0x37,
            E::Num8 => 0x38,
            E::Num9 => 0x39,
            E::Minus => 0xBD,
            E::Equals | E::Plus => 0xBB,
            E::OpenBracket | E::OpenCurlyBracket => 0xDB,
            E::CloseBracket | E::CloseCurlyBracket => 0xDD,
            E::Backslash | E::Pipe => 0xDC,
            E::Semicolon | E::Colon => 0xBA,
            E::Quote => 0xDE,
            E::Backtick => 0xC0,
            E::Comma => 0xBC,
            E::Period => 0xBE,
            E::Slash | E::Questionmark => 0xBF,
            // A..Z share their ASCII codes as virtual keys.
            _ => {
                let name = key.name();
                let byte = name.as_bytes();
                if byte.len() == 1 && byte[0].is_ascii_uppercase() {
                    byte[0] as u16
                } else {
                    return None;
                }
            }
        })
    }

    /// Returns the character the active layout produces for `key` with the
    /// currently held modifiers.
    ///
    /// `None` for non-printable results, so genuine Ctrl+Alt bindings are not
    /// intercepted.
    pub fn character(key: egui::Key) -> Option<char> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            GetKeyboardLayout, GetKeyboardState, MapVirtualKeyExW, ToUnicodeEx,
        };

        let vk = virtual_key(key)?;
        let mut state = [0u8; 256];
        // SAFETY: writes 256 bytes into a 256-byte local. Fails harmlessly on a
        // thread with no message queue, which is reported by the return value.
        if unsafe { GetKeyboardState(state.as_mut_ptr()) } == 0 {
            return None;
        }

        // SAFETY: no pointers; returns the calling thread's layout handle,
        // which is what `ToUnicodeEx` below must be asked about.
        let layout = unsafe { GetKeyboardLayout(0) };
        // 0 is MAPVK_VK_TO_VSC.
        // SAFETY: `vk` is a virtual-key code; the call reads no memory.
        let scan = unsafe { MapVirtualKeyExW(vk as u32, 0, layout) };

        let mut buffer = [0u16; 8];
        // Flag bit 2 leaves the keyboard state untouched, so a pending dead key
        // is not consumed. Supported since Windows 10 1607, ignored before.
        //
        // SAFETY: `state` is 256 bytes as documented, and the buffer length is
        // passed as its true length. A negative return means a dead key, which
        // is handled below rather than indexed into.
        let written = unsafe {
            ToUnicodeEx(
                vk as u32,
                scan,
                state.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as i32,
                1 << 2,
                layout,
            )
        };

        // Accept exactly one printable character; reject dead keys (-1) and
        // control characters.
        if written != 1 {
            return None;
        }
        char::from_u32(buffer[0] as u32).filter(|c| !c.is_control())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_virtual_keys_are_the_ones_windows_uses() {
            assert_eq!(virtual_key(egui::Key::A), Some(0x41));
            assert_eq!(virtual_key(egui::Key::Z), Some(0x5A));
            assert_eq!(virtual_key(egui::Key::Num0), Some(0x30));
            assert_eq!(virtual_key(egui::Key::Num9), Some(0x39));
            assert_eq!(virtual_key(egui::Key::Minus), Some(0xBD));
            assert_eq!(virtual_key(egui::Key::OpenBracket), Some(0xDB));
        }

        #[test]
        fn keys_that_are_not_characters_have_no_virtual_key_here() {
            for key in [
                egui::Key::Enter,
                egui::Key::Escape,
                egui::Key::F5,
                egui::Key::ArrowUp,
                egui::Key::Backspace,
            ] {
                assert_eq!(virtual_key(key), None, "{key:?}");
            }
        }

        /// Querying any layout must not panic or return a control character.
        #[test]
        fn asking_the_layout_is_always_survivable() {
            for key in [
                egui::Key::A,
                egui::Key::Num2,
                egui::Key::Minus,
                egui::Key::Enter,
                egui::Key::F1,
            ] {
                if let Some(c) = character(key) {
                    assert!(!c.is_control(), "{key:?} produced {c:?}");
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod altgr {
    use eframe::egui;

    /// Always `None`: AltGr is reported as Ctrl+Alt only on Windows.
    pub fn character(_key: egui::Key) -> Option<char> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_with(events: Vec<egui::Event>) -> egui::InputState {
        let mut state = egui::InputState::default();
        state.events = events;
        state
    }

    fn keys(events: Vec<egui::Event>) -> Vec<KeyEvent> {
        translate(&input_with(events))
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::Key(key) => Some(key),
                _ => None,
            })
            .collect()
    }

    fn press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn a_typed_character_is_reported_exactly_once() {
        let typed = keys(vec![
            egui::Event::Text("1".into()),
            press(egui::Key::Num1, egui::Modifiers::NONE),
        ]);
        assert_eq!(typed, vec![KeyEvent::new(Key::Char('1'), Mods::NONE)]);
    }

    /// Letters that are also Ctrl chords are typed once when pressed plainly.
    #[test]
    fn a_letter_that_is_also_a_chord_is_still_typed_once() {
        for (key, c) in [
            (egui::Key::A, 'a'),
            (egui::Key::C, 'c'),
            (egui::Key::L, 'l'),
            (egui::Key::Q, 'q'),
            (egui::Key::U, 'u'),
            (egui::Key::V, 'v'),
            (egui::Key::W, 'w'),
        ] {
            let typed = keys(vec![
                egui::Event::Text(c.to_string()),
                press(key, egui::Modifiers::NONE),
            ]);
            assert_eq!(typed, vec![KeyEvent::new(Key::Char(c), Mods::NONE)], "{c}");
        }
    }

    /// Shift does not turn a letter into a binding.
    #[test]
    fn a_shifted_letter_types_the_capital_and_nothing_else() {
        let typed = keys(vec![
            egui::Event::Text("A".into()),
            press(egui::Key::A, egui::Modifiers::SHIFT),
        ]);
        assert_eq!(typed, vec![KeyEvent::new(Key::Char('A'), Mods::NONE)]);
    }

    #[test]
    fn the_toolkits_copy_event_becomes_the_chord_the_program_binds() {
        assert_eq!(
            keys(vec![egui::Event::Copy]),
            vec![KeyEvent::new(Key::Char('c'), Mods::CTRL)]
        );
    }

    #[test]
    fn the_toolkits_cut_event_becomes_the_chord_the_program_binds() {
        assert_eq!(
            keys(vec![egui::Event::Cut]),
            vec![KeyEvent::new(Key::Char('x'), Mods::CTRL)]
        );
    }

    /// A Ctrl+C key event alongside `Event::Copy` must not copy twice.
    #[test]
    fn a_clipboard_chord_is_produced_from_one_place_only() {
        let both = keys(vec![
            egui::Event::Copy,
            press(egui::Key::C, egui::Modifiers::CTRL),
        ]);
        assert_eq!(both, vec![KeyEvent::new(Key::Char('c'), Mods::CTRL)]);
    }

    #[test]
    fn a_release_types_nothing() {
        let released = keys(vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(released.is_empty(), "{released:?}");
    }

    #[test]
    fn the_keys_that_move_the_selection_arrive_as_bindings() {
        assert_eq!(
            keys(vec![press(egui::Key::ArrowDown, egui::Modifiers::NONE)]),
            vec![KeyEvent::new(Key::Down, Mods::NONE)]
        );
        assert_eq!(
            keys(vec![press(egui::Key::F1, egui::Modifiers::NONE)]),
            vec![KeyEvent::new(Key::F(1), Mods::NONE)]
        );
    }

    #[test]
    fn a_control_chord_keeps_its_modifier() {
        assert_eq!(
            keys(vec![press(egui::Key::W, egui::Modifiers::CTRL)]),
            vec![KeyEvent::new(Key::Char('w'), Mods::CTRL)]
        );
    }

    /// Held Backspace keeps deleting, so a repeat counts as a press.
    #[test]
    fn an_auto_repeat_is_another_press() {
        let repeated = keys(vec![egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(repeated, vec![KeyEvent::new(Key::Backspace, Mods::NONE)]);
    }

    fn wheel(dy: f32) -> Vec<KeyEvent> {
        keys(vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        }])
    }

    #[test]
    fn the_wheel_does_nothing_at_all() {
        for dy in [-40.0, -1.0, -0.05, 0.0, 0.2, 1.0, 40.0, f32::NAN] {
            assert!(wheel(dy).is_empty(), "a wheel event of {dy} did something");
        }
    }

    /// When toolkit text accompanies a Ctrl+Alt press, the text wins and the
    /// AltGr reconstruction is skipped.
    #[test]
    fn text_alongside_a_ctrl_alt_press_is_not_reconstructed_as_well() {
        let both = keys(vec![
            egui::Event::Text("@".into()),
            press(egui::Key::Q, egui::Modifiers::CTRL | egui::Modifiers::ALT),
        ]);
        let typed: Vec<_> = both
            .iter()
            .filter(|k| matches!(k.key, Key::Char(_)) && k.mods.is_none())
            .collect();
        assert_eq!(typed.len(), 1, "{both:?}");
    }

    #[test]
    fn a_paste_arrives_whole_rather_than_as_characters() {
        let events = translate(&input_with(vec![egui::Event::Paste("11-D-0704".into())]));
        assert!(
            matches!(events.as_slice(), [AppEvent::Paste(text)] if text == "11-D-0704"),
            "{events:?}"
        );
    }
}
