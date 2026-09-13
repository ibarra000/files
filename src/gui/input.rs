//! Toolkit events in, crate events out.
//!
//! # One source for characters, one source for bindings
//!
//! `Event::Text` is the only thing that produces a typed character, and
//! `Event::Key` is the only thing that produces a binding. Forwarding a bare
//! letter from both is how a search box doubles every character - the same
//! failure the terminal build's press/release guard exists for, arriving from
//! the other direction.
//!
//! # AltGr, for the second time
//!
//! The terminal build had a bug where a German, Polish or French layout lost
//! `@ { [ ]`, because on Windows **AltGr is reported as Ctrl+Alt** and the
//! binding table swallowed it. It was fixed there, and it comes back here one
//! layer down: `egui-winit` drops `Event::Text` whenever `modifiers.ctrl` is
//! set, to avoid emitting a character for Ctrl+C and friends. AltGr sets ctrl,
//! so the character never reaches egui at all.
//!
//! There is no way to recover it from an egui event - `egui::Key` has no
//! variant for `@` - so [`altgr`] asks the operating system directly what the
//! current layout would have produced, and only ever on the Ctrl+Alt path. If a
//! future `egui-winit` stops dropping the text, [`translate`] sees the
//! `Event::Text` and skips the reconstruction, so the two can never both fire.

use eframe::egui;

use crate::app::event::AppEvent;
use crate::app::key::{Key, KeyEvent, Mods};

/// Turns one frame of toolkit input into crate events.
pub fn translate(input: &egui::InputState) -> Vec<AppEvent> {
    // If the toolkit gave us any typed text this frame, it is the authority and
    // the reconstruction below stays out of it. Without this the two would
    // double every character the day `egui-winit` changes its mind.
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
                // AltGr first: on the layouts where it matters, this *is* the
                // character, and treating it as a binding is the bug.
                // Lazily: `altgr::character` asks Windows what the layout
                // would type, and doing that on every keystroke is a syscall
                // per character to answer a question only Ctrl+Alt raises.
                let reconstructed = (!toolkit_typed && mods.ctrl && mods.alt)
                    .then(|| altgr::character(*key))
                    .flatten();
                if let Some(c) = reconstructed {
                    out.push(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::ALTGR)));
                    continue;
                }
                if let Some(mapped) = binding(*key) {
                    out.push(AppEvent::Key(KeyEvent::new(mapped, mods)));
                }
            }
            _ => {}
        }
    }
    out
}

/// The keys that mean something regardless of what they would type.
///
/// Printable keys are deliberately absent: they arrive as `Event::Text`, and
/// listing them here as well is the doubling described above. The exception is
/// the small set the state machine binds with Ctrl - those arrive as
/// `Event::Key` only, because the toolkit suppresses their text.
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
        // The Ctrl chords the program binds. A letter only reaches this arm
        // when the toolkit withheld its text, which it does exactly when a
        // modifier made it a command rather than a character.
        E::A => Key::Char('a'),
        E::C => Key::Char('c'),
        E::L => Key::Char('l'),
        E::Q => Key::Char('q'),
        E::U => Key::Char('u'),
        E::V => Key::Char('v'),
        E::W => Key::Char('w'),
        _ => return None,
    })
}

/// Asking the operating system what the layout would have typed.
#[cfg(windows)]
mod altgr {
    use eframe::egui;

    /// The virtual-key code a printable `egui::Key` sits on.
    ///
    /// A US-layout mapping, which is the right one: `ToUnicodeEx` wants the
    /// *virtual key*, and Windows assigns those by position using US names. The
    /// character that comes back is the active layout's, which is the whole
    /// point.
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

    /// What this keyboard layout produces for `key` with the modifiers that are
    /// held right now.
    ///
    /// `None` for anything that is not a printable character, which includes
    /// every real Ctrl chord - so a genuine Ctrl+Alt binding is not stolen by
    /// this path.
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
        // Bit 2 asks Windows not to disturb the keyboard state, so a dead key
        // waiting for its next character is not consumed by our asking. Present
        // since Windows 10 1607 and ignored before it.
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

        // Exactly one character, and a printable one. A dead key (-1) or a
        // control character is not something to insert into a job code.
        if written != 1 {
            return None;
        }
        char::from_u32(buffer[0] as u32).filter(|c| !c.is_control())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The letters share their ASCII codes, and the punctuation does not.
        /// Getting one of these wrong means AltGr types the wrong character,
        /// which is worse than typing none.
        #[test]
        fn the_virtual_keys_are_the_ones_windows_uses() {
            assert_eq!(virtual_key(egui::Key::A), Some(0x41));
            assert_eq!(virtual_key(egui::Key::Z), Some(0x5A));
            assert_eq!(virtual_key(egui::Key::Num0), Some(0x30));
            assert_eq!(virtual_key(egui::Key::Num9), Some(0x39));
            assert_eq!(virtual_key(egui::Key::Minus), Some(0xBD));
            assert_eq!(virtual_key(egui::Key::OpenBracket), Some(0xDB));
        }

        /// Keys that type nothing must not be handed to `ToUnicodeEx` as if
        /// they did.
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

        /// Whatever the layout, asking must not panic and must not invent a
        /// control character.
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

    /// Nothing to reconstruct: AltGr is only reported as Ctrl+Alt on Windows.
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

    /// A character comes from the text event and from nowhere else. Both would
    /// mean a search box that types everything twice.
    #[test]
    fn a_typed_character_is_reported_exactly_once() {
        let typed = keys(vec![
            egui::Event::Text("1".into()),
            press(egui::Key::Num1, egui::Modifiers::NONE),
        ]);
        assert_eq!(typed, vec![KeyEvent::new(Key::Char('1'), Mods::NONE)]);
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

    /// The toolkit withholds the text for a Ctrl chord, so the letter has to
    /// come through the binding arm - with its modifiers intact, or `Ctrl+W`
    /// becomes a `w`.
    #[test]
    fn a_control_chord_keeps_its_modifier() {
        assert_eq!(
            keys(vec![press(egui::Key::W, egui::Modifiers::CTRL)]),
            vec![KeyEvent::new(Key::Char('w'), Mods::CTRL)]
        );
    }

    /// Held Backspace must keep deleting, so a repeat is a press.
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

    /// The guard that keeps the AltGr reconstruction and the toolkit from both
    /// firing. If a future `egui-winit` stops withholding the text, the text
    /// wins and nothing is typed twice.
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
