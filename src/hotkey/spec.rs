//! `"ctrl+shift+space"` - the pair `RegisterHotKey` wants.
//!
//! Pure, and deliberately holding its own copy of the Win32 modifier bits and
//! virtual-key codes so it compiles and is tested on machines that have no
//! Win32 at all - which is most of the machines this crate's tests run on. The
//! copies are proved equal to the real ones at compile time under
//! `cfg(windows)`, so a table that drifts from the platform is a build failure
//! rather than a hotkey that quietly fires on the wrong key.

/// `HOT_KEY_MODIFIERS` bits. Fixed ABI; proved against the real ones below.
pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
/// Without this, holding the chord down repeats it at the keyboard's autorepeat
/// rate and the window is summoned and dismissed thirty times a second. The
/// parser adds it unconditionally rather than letting configuration decide,
/// because there is no reading of "hold the key down" that anyone wants.
pub const MOD_NOREPEAT: u32 = 0x4000;

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_INSERT: u16 = 0x2D;
const VK_DELETE: u16 = 0x2E;
const VK_F1: u16 = 0x70;
const VK_OEM_3: u16 = 0xC0;

/// The shipped default. Unclaimed by Windows itself, and reachable one-handed.
pub const DEFAULT: &str = "ctrl+shift+space";

/// The largest function key `RegisterHotKey` knows about.
const MAX_FKEY: u8 = 24;

/// A registered chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    /// Always includes [`MOD_NOREPEAT`].
    pub mods: u32,
    pub vk: u16,
}

/// What the configuration asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeySpec {
    /// The literal `"off"`. No thread is started and no key is claimed.
    Off,
    Bound(Hotkey),
}

impl HotkeySpec {
    pub fn bound(self) -> Option<Hotkey> {
        match self {
            Self::Off => None,
            Self::Bound(h) => Some(h),
        }
    }
}

impl Default for HotkeySpec {
    /// The default as a literal rather than a parse.
    ///
    /// Startup must not depend on a fallible call whose only failure mode is a
    /// typo in this file. `the_default_literal_agrees_with_the_default_text`
    /// is what keeps the two in step.
    fn default() -> Self {
        Self::Bound(Hotkey {
            mods: MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
            vk: VK_SPACE,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyParseError {
    Empty,
    /// A token that is neither a modifier nor a key name.
    UnknownToken(String),
    /// Modifiers but no key, as in `"ctrl+shift"`.
    NoKey,
    /// Two keys, as in `"ctrl+a+b"`.
    TwoKeys {
        first: String,
        second: String,
    },
    /// A key with no modifier at all.
    ///
    /// Refused rather than accepted: `RegisterHotKey` claims a bare key
    /// system-wide, so `hotkey = "f9"` would make F9 dead in every other
    /// program on the desktop for as long as this one is running.
    NoModifier(String),
}

impl HotkeyParseError {
    pub fn detail(&self) -> String {
        match self {
            Self::Empty => "a hotkey cannot be empty (use \"off\" to claim no key)".into(),
            Self::UnknownToken(t) => format!("unknown key or modifier {t:?}"),
            Self::NoKey => "a hotkey needs a key, not only modifiers".into(),
            Self::TwoKeys { first, second } => {
                format!("a hotkey takes one key, but names both {first:?} and {second:?}")
            }
            Self::NoModifier(k) => format!(
                "{k:?} has no modifier, and a hotkey without one would take that key away \
                 from every other program (try \"ctrl+{k}\", or \"off\")"
            ),
        }
    }
}

impl std::fmt::Display for HotkeyParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail())
    }
}

impl std::error::Error for HotkeyParseError {}

const fn vk_of_fkey(n: u8) -> u16 {
    VK_F1 + (n as u16) - 1
}

/// Parses a chord, or `"off"`.
pub fn parse(text: &str) -> Result<HotkeySpec, HotkeyParseError> {
    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return Err(HotkeyParseError::Empty);
    }
    if lowered == "off" || lowered == "none" {
        return Ok(HotkeySpec::Off);
    }

    let mut mods = 0u32;
    let mut key: Option<(String, u16)> = None;

    for token in lowered.split('+') {
        let token = token.trim();
        if token.is_empty() {
            return Err(HotkeyParseError::Empty);
        }
        if let Some(bit) = modifier_of(token) {
            mods |= bit;
            continue;
        }
        let vk = key_of(token).ok_or_else(|| HotkeyParseError::UnknownToken(token.to_string()))?;
        if let Some((first, _)) = key {
            return Err(HotkeyParseError::TwoKeys {
                first,
                second: token.to_string(),
            });
        }
        key = Some((token.to_string(), vk));
    }

    let (name, vk) = key.ok_or(HotkeyParseError::NoKey)?;
    if mods == 0 {
        return Err(HotkeyParseError::NoModifier(name));
    }
    Ok(HotkeySpec::Bound(Hotkey {
        mods: mods | MOD_NOREPEAT,
        vk,
    }))
}

fn modifier_of(token: &str) -> Option<u32> {
    Some(match token {
        "ctrl" | "control" => MOD_CONTROL,
        "alt" => MOD_ALT,
        "shift" => MOD_SHIFT,
        "win" | "super" | "meta" | "cmd" => MOD_WIN,
        _ => return None,
    })
}

fn key_of(token: &str) -> Option<u16> {
    // f1 - f24 before the single-character cases: "f" alone is the letter, and
    // testing length first would leave "f1" matching nothing at all.
    if let Some(digits) = token.strip_prefix('f')
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && let Ok(n) = digits.parse::<u8>()
        && (1..=MAX_FKEY).contains(&n)
    {
        return Some(vk_of_fkey(n));
    }

    if token.len() == 1 {
        let b = token.as_bytes()[0];
        // Virtual-key codes for letters are the ASCII uppercase values, and
        // for digits the ASCII values themselves.
        if b.is_ascii_lowercase() {
            return Some(b.to_ascii_uppercase() as u16);
        }
        if b.is_ascii_digit() {
            return Some(b as u16);
        }
    }

    Some(match token {
        "space" => VK_SPACE,
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "esc" | "escape" => VK_ESCAPE,
        "backspace" => VK_BACK,
        "insert" | "ins" => VK_INSERT,
        "delete" | "del" => VK_DELETE,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" | "pgup" => VK_PRIOR,
        "pagedown" | "pgdn" => VK_NEXT,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "`" | "backtick" | "grave" => VK_OEM_3,
        _ => return None,
    })
}

/// The canonical spelling, for `--doctor` and for the toast that names a clash.
pub fn describe(hk: Hotkey) -> String {
    let mut out = String::new();
    // Fixed order, so two spellings of the same chord read identically.
    for (bit, name) in [
        (MOD_CONTROL, "Ctrl"),
        (MOD_ALT, "Alt"),
        (MOD_SHIFT, "Shift"),
        (MOD_WIN, "Win"),
    ] {
        if hk.mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&name_of_vk(hk.vk));
    out
}

fn name_of_vk(vk: u16) -> String {
    if (VK_F1..=vk_of_fkey(MAX_FKEY)).contains(&vk) {
        return format!("F{}", vk - VK_F1 + 1);
    }
    if (b'A' as u16..=b'Z' as u16).contains(&vk) || (b'0' as u16..=b'9' as u16).contains(&vk) {
        return ((vk as u8) as char).to_string();
    }
    match vk {
        VK_SPACE => "Space".into(),
        VK_RETURN => "Enter".into(),
        VK_TAB => "Tab".into(),
        VK_ESCAPE => "Esc".into(),
        VK_BACK => "Backspace".into(),
        VK_INSERT => "Insert".into(),
        VK_DELETE => "Delete".into(),
        VK_HOME => "Home".into(),
        VK_END => "End".into(),
        VK_PRIOR => "PageUp".into(),
        VK_NEXT => "PageDown".into(),
        VK_LEFT => "Left".into(),
        VK_RIGHT => "Right".into(),
        VK_UP => "Up".into(),
        VK_DOWN => "Down".into(),
        VK_OEM_3 => "`".into(),
        other => format!("0x{other:02X}"),
    }
}

/// Proof that the copies above are the platform's own values.
///
/// A constant that drifted would register a chord nobody asked for, and the
/// only symptom would be a key that does nothing.
#[cfg(windows)]
mod abi {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse as km;

    const _: () = assert!(super::MOD_ALT == km::MOD_ALT);
    const _: () = assert!(super::MOD_CONTROL == km::MOD_CONTROL);
    const _: () = assert!(super::MOD_SHIFT == km::MOD_SHIFT);
    const _: () = assert!(super::MOD_WIN == km::MOD_WIN);
    const _: () = assert!(super::MOD_NOREPEAT == km::MOD_NOREPEAT);
    const _: () = assert!(super::VK_SPACE == km::VK_SPACE);
    const _: () = assert!(super::VK_RETURN == km::VK_RETURN);
    const _: () = assert!(super::VK_TAB == km::VK_TAB);
    const _: () = assert!(super::VK_ESCAPE == km::VK_ESCAPE);
    const _: () = assert!(super::VK_BACK == km::VK_BACK);
    const _: () = assert!(super::VK_INSERT == km::VK_INSERT);
    const _: () = assert!(super::VK_DELETE == km::VK_DELETE);
    const _: () = assert!(super::VK_HOME == km::VK_HOME);
    const _: () = assert!(super::VK_END == km::VK_END);
    const _: () = assert!(super::VK_PRIOR == km::VK_PRIOR);
    const _: () = assert!(super::VK_NEXT == km::VK_NEXT);
    const _: () = assert!(super::VK_LEFT == km::VK_LEFT);
    const _: () = assert!(super::VK_RIGHT == km::VK_RIGHT);
    const _: () = assert!(super::VK_UP == km::VK_UP);
    const _: () = assert!(super::VK_DOWN == km::VK_DOWN);
    const _: () = assert!(super::VK_OEM_3 == km::VK_OEM_3);
    const _: () = assert!(super::vk_of_fkey(1) == km::VK_F1);
    const _: () = assert!(super::vk_of_fkey(24) == km::VK_F24);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(text: &str) -> Hotkey {
        match parse(text) {
            Ok(HotkeySpec::Bound(h)) => h,
            other => panic!("expected a chord from {text:?}, got {other:?}"),
        }
    }

    /// The shipped default has to parse, or the program starts with no hotkey
    /// and nothing says why.
    #[test]
    fn the_default_literal_agrees_with_the_default_text() {
        assert_eq!(parse(DEFAULT).unwrap(), HotkeySpec::default());
    }

    #[test]
    fn the_default_is_control_shift_space() {
        let hk = bound(DEFAULT);
        assert_eq!(hk.mods & MOD_CONTROL, MOD_CONTROL);
        assert_eq!(hk.mods & MOD_SHIFT, MOD_SHIFT);
        assert_eq!(hk.mods & MOD_ALT, 0);
        assert_eq!(hk.mods & MOD_WIN, 0);
        assert_eq!(hk.vk, VK_SPACE);
    }

    /// Holding the chord must not summon and dismiss the window at the
    /// keyboard's autorepeat rate, so this bit is not the caller's to forget.
    #[test]
    fn every_chord_refuses_to_autorepeat() {
        for text in ["ctrl+space", "alt+f4", "win+shift+f23", "ctrl+alt+delete"] {
            assert_eq!(bound(text).mods & MOD_NOREPEAT, MOD_NOREPEAT, "{text}");
        }
    }

    #[test]
    fn modifiers_may_be_written_in_any_order_or_case() {
        assert_eq!(bound("ctrl+shift+space"), bound("Shift+CTRL+Space"));
        assert_eq!(bound("ctrl+shift+space"), bound("  shift + ctrl + space "));
    }

    #[test]
    fn control_and_ctrl_and_the_window_key_aliases_all_parse() {
        assert_eq!(bound("control+a"), bound("ctrl+a"));
        assert_eq!(bound("win+a"), bound("super+a"));
        assert_eq!(bound("win+a"), bound("meta+a"));
        assert_eq!(bound("win+a"), bound("cmd+a"));
    }

    /// The physical Copilot key on current keyboards sends this chord, so the
    /// grammar has to cover it for the feature to live up to its name.
    #[test]
    fn the_copilot_key_itself_is_expressible() {
        let hk = bound("shift+win+f23");
        assert_eq!(hk.mods & MOD_SHIFT, MOD_SHIFT);
        assert_eq!(hk.mods & MOD_WIN, MOD_WIN);
        assert_eq!(hk.vk, vk_of_fkey(23));
    }

    #[test]
    fn function_keys_run_from_f1_to_f24_and_no_further() {
        assert_eq!(bound("ctrl+f1").vk, VK_F1);
        assert_eq!(bound("ctrl+f24").vk, VK_F1 + 23);
        assert!(matches!(
            parse("ctrl+f0"),
            Err(HotkeyParseError::UnknownToken(_))
        ));
        assert!(matches!(
            parse("ctrl+f25"),
            Err(HotkeyParseError::UnknownToken(_))
        ));
    }

    /// "f" is the letter and "f5" is the function key; reading the length
    /// first would make one of the two unreachable.
    #[test]
    fn a_lone_f_is_the_letter_not_a_function_key() {
        assert_eq!(bound("ctrl+f").vk, b'F' as u16);
        assert_ne!(bound("ctrl+f").vk, bound("ctrl+f1").vk);
    }

    #[test]
    fn letters_and_digits_use_their_ascii_codes() {
        assert_eq!(bound("ctrl+a").vk, b'A' as u16);
        assert_eq!(bound("ctrl+z").vk, b'Z' as u16);
        assert_eq!(bound("ctrl+0").vk, b'0' as u16);
        assert_eq!(bound("ctrl+9").vk, b'9' as u16);
    }

    #[test]
    fn the_literal_off_claims_no_key_at_all() {
        assert_eq!(parse("off").unwrap(), HotkeySpec::Off);
        assert_eq!(parse("OFF").unwrap(), HotkeySpec::Off);
        assert_eq!(parse(" none ").unwrap(), HotkeySpec::Off);
        assert!(HotkeySpec::Off.bound().is_none());
    }

    /// A bare key would be claimed system-wide, making it dead in every other
    /// program on the desktop. Refused loudly rather than honoured.
    #[test]
    fn a_key_with_no_modifier_is_refused_with_a_reason() {
        let err = parse("f9").unwrap_err();
        assert!(matches!(err, HotkeyParseError::NoModifier(_)));
        assert!(err.detail().contains("every other program"));
        assert!(matches!(parse("a"), Err(HotkeyParseError::NoModifier(_))));
    }

    #[test]
    fn modifiers_with_no_key_are_refused() {
        assert_eq!(parse("ctrl+shift"), Err(HotkeyParseError::NoKey));
        assert_eq!(parse("win"), Err(HotkeyParseError::NoKey));
    }

    #[test]
    fn two_keys_are_refused_and_both_are_named() {
        match parse("ctrl+a+b") {
            Err(HotkeyParseError::TwoKeys { first, second }) => {
                assert_eq!(first, "a");
                assert_eq!(second, "b");
            }
            other => panic!("expected two keys, got {other:?}"),
        }
    }

    #[test]
    fn empty_and_dangling_separators_are_refused() {
        assert_eq!(parse(""), Err(HotkeyParseError::Empty));
        assert_eq!(parse("   "), Err(HotkeyParseError::Empty));
        assert_eq!(parse("+"), Err(HotkeyParseError::Empty));
        assert_eq!(parse("ctrl+"), Err(HotkeyParseError::Empty));
    }

    /// An unknown name is reported rather than dropped: a typo that silently
    /// fell back to the default would look exactly like a working hotkey right
    /// up until the user pressed the combination they chose.
    #[test]
    fn an_unknown_key_name_is_reported_rather_than_ignored() {
        match parse("ctrl+shift+spcae") {
            Err(HotkeyParseError::UnknownToken(t)) => assert_eq!(t, "spcae"),
            other => panic!("expected the typo to be named, got {other:?}"),
        }
    }

    #[test]
    fn describe_round_trips_through_parse() {
        for text in [
            "ctrl+shift+space",
            "alt+f4",
            "shift+win+f23",
            "ctrl+alt+delete",
            "ctrl+`",
            "ctrl+7",
        ] {
            let hk = bound(text);
            assert_eq!(bound(&describe(hk)), hk, "{text} -> {}", describe(hk));
        }
    }

    #[test]
    fn describe_spells_the_default_the_way_a_person_would() {
        assert_eq!(describe(bound(DEFAULT)), "Ctrl+Shift+Space");
    }
}
