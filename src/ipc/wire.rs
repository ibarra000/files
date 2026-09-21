//! What the panel and the settings window say to each other, as text.
//!
//! One message per line, each a verb and then its fields, separated by
//! spaces. `show diagnostics`. `live 100,200 hotkey=taken:the%20chord%20is%20
//! held%20by%20another%20program`. `closing`.
//!
//! # Why text, and why by hand
//!
//! There is a serialisation crate for this and it is not used, for the same
//! reason `Cargo.toml` gives for keeping `syn` out of the build: this is a
//! dozen messages with at most three fields each, and a derive macro would
//! be a proc-macro dependency and a compile-time cost to save sixty lines.
//!
//! Text rather than a packed struct because of what goes wrong. A protocol
//! between two processes is debugged by looking at it - in a debugger, in a
//! log, in a hex dump of a pipe - and `show diagnostics` is legible in all
//! three where `0x03 0x07` is not. It is also what makes the round-trip test
//! below worth anything: a format whose failure mode is "the bytes meant
//! something else" is one where a test that encodes and decodes proves only
//! that the two halves agree with each other.
//!
//! # The parser may not panic
//!
//! Anything can connect to a named pipe. The settings window is not a
//! trusted peer merely because this program wrote it - a crash here takes
//! the panel down with somebody's search open, which is a worse hole than
//! the one the pipe was opened for. So every field is parsed with `Option`
//! and a malformed line is `None` rather than an index out of range, and
//! there is a test that throws every byte string at it.

use crate::view::settings::PageId;

/// What the panel tells the settings window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToSettings {
    /// Come to the front, on this page.
    Show(PageId),
    /// Go away. What the tray menu's toggle does when the window is up.
    Close,
    /// The facts only the running panel knows.
    ///
    /// Not the whole of `AppState`, and this is the part that made the split
    /// affordable: the settings window reads exactly two things it cannot
    /// work out for itself. Everything else on the form comes from the
    /// configuration file, which both processes can read.
    Live(Live),
    /// The configuration file has changed underneath you. Read it again.
    Reload,
    /// The panel is shutting down. Anything that needs it is about to stop
    /// working; say so on the form rather than failing when pressed.
    Exiting,
}

/// And what the settings window tells the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToPanel {
    /// The configuration file has been written. Read it again.
    Changed,
    /// Install the update the About page found.
    ///
    /// A message rather than something the settings process does itself.
    /// `update::apply::hand_over` bakes in `std::process::id()` and
    /// `current_exe()`, so a settings process running it would wait on its
    /// own pid and relaunch itself instead of the panel.
    InstallUpdate,
    /// Forget the remembered panel position.
    ForgetPlacement,
    /// The window is closing.
    Closing,
}

/// The facts only the running panel knows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Live {
    /// Where the panel has been dragged to, if anywhere.
    pub placement: Option<(i32, i32)>,
    /// Whether the panel holds the global chord.
    pub hotkey: Held,
}

/// Whether the global chord is claimed, as the panel's listener reported it.
///
/// Carried rather than probed, because `RegisterHotKey` is per-thread: a
/// settings process asking Windows whether the chord is free would be told
/// it is taken, by the panel, which is the answer that matters least. See
/// [`crate::hotkey::Probe`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Held {
    /// The listener has not said, which for a hotkey that is switched off is
    /// for ever.
    #[default]
    Unknown,
    /// This program holds it.
    Yes,
    /// It could not be claimed, and why.
    No(String),
}

// -- encoding ---------------------------------------------------------------

/// Percent-encodes the characters that would break a line into fields.
///
/// Space, percent itself, and anything below it. Deliberately not a general
/// URL encoder: the only strings that go on the wire are error messages this
/// program produced, so what is needed is a rule that round-trips, not one
/// that satisfies a specification.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            c if (c as u32) < 0x20 => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// And the other way. `None` on a malformed escape rather than a guess.
fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut bytes = text.chars();
    while let Some(ch) = bytes.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let hi = bytes.next()?;
        let lo = bytes.next()?;
        let code = u32::from_str_radix(&format!("{hi}{lo}"), 16).ok()?;
        out.push(char::from_u32(code)?);
    }
    Some(out)
}

fn page_slug(page: PageId) -> &'static str {
    page.slug()
}

fn page_of(slug: &str) -> Option<PageId> {
    PageId::ALL.into_iter().find(|p| p.slug() == slug)
}

impl Live {
    fn encode(&self) -> String {
        let at = match self.placement {
            Some((left, top)) => format!("{left},{top}"),
            None => "-".to_owned(),
        };
        let hotkey = match &self.hotkey {
            Held::Unknown => "unknown".to_owned(),
            Held::Yes => "yes".to_owned(),
            Held::No(why) => format!("no:{}", escape(why)),
        };
        format!("{at} {hotkey}")
    }

    fn decode(at: &str, hotkey: &str) -> Option<Self> {
        let placement = if at == "-" {
            None
        } else {
            let (left, top) = at.split_once(',')?;
            Some((left.parse().ok()?, top.parse().ok()?))
        };
        let hotkey = match hotkey {
            "unknown" => Held::Unknown,
            "yes" => Held::Yes,
            other => Held::No(unescape(other.strip_prefix("no:")?)?),
        };
        Some(Self { placement, hotkey })
    }
}

impl ToSettings {
    /// One line, with no terminator. The transport adds that.
    pub fn encode(&self) -> String {
        match self {
            Self::Show(page) => format!("show {}", page_slug(*page)),
            Self::Close => "close".to_owned(),
            Self::Live(live) => format!("live {}", live.encode()),
            Self::Reload => "reload".to_owned(),
            Self::Exiting => "exiting".to_owned(),
        }
    }

    /// `None` on anything that is not one of these, which includes anything
    /// that is not this program.
    pub fn decode(line: &str) -> Option<Self> {
        let mut parts = line.split(' ');
        match parts.next()? {
            "show" => {
                let page = page_of(parts.next()?)?;
                parts.next().is_none().then_some(Self::Show(page))
            }
            "close" => parts.next().is_none().then_some(Self::Close),
            "live" => {
                let live = Live::decode(parts.next()?, parts.next()?)?;
                parts.next().is_none().then_some(Self::Live(live))
            }
            "reload" => parts.next().is_none().then_some(Self::Reload),
            "exiting" => parts.next().is_none().then_some(Self::Exiting),
            _ => None,
        }
    }

    /// Every variant, for the round-trip test and for anybody adding one.
    #[cfg(test)]
    pub fn all() -> Vec<Self> {
        let mut out = vec![Self::Close, Self::Reload, Self::Exiting];
        out.extend(PageId::ALL.into_iter().map(Self::Show));
        for placement in [None, Some((0, 0)), Some((-1920, -32)), Some((100, 200))] {
            for hotkey in [
                Held::Unknown,
                Held::Yes,
                Held::No("the chord is held by another program".into()),
                Held::No("100% taken \u{2013} by something".into()),
            ] {
                out.push(Self::Live(Live { placement, hotkey }));
            }
        }
        out
    }
}

impl ToPanel {
    pub fn encode(&self) -> String {
        match self {
            Self::Changed => "changed",
            Self::InstallUpdate => "install-update",
            Self::ForgetPlacement => "forget-placement",
            Self::Closing => "closing",
        }
        .to_owned()
    }

    pub fn decode(line: &str) -> Option<Self> {
        match line {
            "changed" => Some(Self::Changed),
            "install-update" => Some(Self::InstallUpdate),
            "forget-placement" => Some(Self::ForgetPlacement),
            "closing" => Some(Self::Closing),
            _ => None,
        }
    }

    /// Every variant, for the round-trip test.
    #[cfg(test)]
    pub const ALL: [Self; 4] = [
        Self::Changed,
        Self::InstallUpdate,
        Self::ForgetPlacement,
        Self::Closing,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every message this program can send comes back as itself.
    ///
    /// Over a `const`-ish list rather than a handful of examples, so a
    /// variant added without an encoding is a failing test rather than a
    /// message that silently decodes as `None` on the other side.
    #[test]
    fn every_message_survives_the_round_trip() {
        for msg in ToSettings::all() {
            let line = msg.encode();
            assert_eq!(
                ToSettings::decode(&line),
                Some(msg.clone()),
                "{msg:?} encoded as {line:?}"
            );
        }
        for msg in ToPanel::ALL {
            let line = msg.encode();
            assert_eq!(ToPanel::decode(&line), Some(msg.clone()), "{line:?}");
        }
    }

    /// A line is a line: nothing encodes to something with a newline in it,
    /// or one message would arrive as two.
    #[test]
    fn nothing_encodes_to_more_than_one_line() {
        for line in ToSettings::all().iter().map(ToSettings::encode) {
            assert!(!line.contains('\n'), "{line:?}");
            assert!(!line.contains('\r'), "{line:?}");
            assert!(!line.is_empty());
        }
    }

    /// A space inside an error message would be read as a field separator,
    /// so it is escaped - and the escape survives.
    #[test]
    fn a_message_with_spaces_in_it_stays_one_field() {
        let msg = ToSettings::Live(Live {
            placement: Some((10, 20)),
            hotkey: Held::No("Ctrl+Shift+Space is taken".into()),
        });
        let line = msg.encode();
        assert_eq!(line.split(' ').count(), 3, "{line:?}");
        assert_eq!(ToSettings::decode(&line), Some(msg));
    }

    /// The escape covers itself, which is the one case a percent encoder
    /// gets wrong by forgetting.
    #[test]
    fn a_percent_sign_survives() {
        for text in ["100%", "%20", "%", "%%", "a % b", "\u{1}\u{7f}"] {
            let escaped = escape(text);
            assert!(!escaped.contains(' '), "{escaped:?}");
            assert_eq!(unescape(&escaped).as_deref(), Some(text));
        }
    }

    /// Nothing that is not one of these decodes as one of these.
    #[test]
    fn a_line_this_program_did_not_write_is_refused() {
        for line in [
            "",
            " ",
            "show",
            "show nosuchpage",
            "show general extra",
            "close extra",
            "live",
            "live 1,2",
            "live x,y yes",
            "live 1,2 maybe",
            "live 1,2 no",
            "reload now",
            "CHANGED",
            "\0",
            "changed\n",
        ] {
            assert_eq!(ToSettings::decode(line), None, "{line:?} was accepted");
        }
        for line in ["", "change", "closing!", "install update"] {
            assert_eq!(ToPanel::decode(line), None, "{line:?} was accepted");
        }
    }

    /// The property that matters most, because anything at all can connect
    /// to a named pipe: no byte string takes the parser down.
    ///
    /// Exhaustive over short inputs rather than random, which for a parser
    /// this small is both cheaper and stronger - every one- and two-token
    /// line built from the alphabet the format uses, plus a sweep of raw
    /// bytes.
    #[test]
    fn no_input_panics_the_parser() {
        let pieces = [
            "",
            " ",
            "%",
            "%2",
            "%zz",
            "-",
            ",",
            "0",
            "-1",
            "show",
            "live",
            "close",
            "reload",
            "exiting",
            "yes",
            "no:",
            "no:%",
            "unknown",
            "\u{7f}",
            "\u{1f600}",
            "1,2",
        ];
        for a in pieces {
            let _ = ToSettings::decode(a);
            let _ = ToPanel::decode(a);
            for b in pieces {
                let line = format!("{a} {b}");
                let _ = ToSettings::decode(&line);
                let _ = ToPanel::decode(&line);
                for c in pieces {
                    let line = format!("{a} {b} {c}");
                    let _ = ToSettings::decode(&line);
                    let _ = ToPanel::decode(&line);
                }
            }
        }
        // And a sweep of every single byte, because a `char` boundary is the
        // other way a parser like this falls over.
        for byte in 0u8..=255 {
            let s = String::from_utf8_lossy(&[byte]).into_owned();
            let _ = ToSettings::decode(&s);
            let _ = ToSettings::decode(&format!("live {s} {s}"));
            let _ = ToSettings::decode(&format!("show {s}"));
            let _ = ToPanel::decode(&s);
        }
    }

    /// A negative position is a real one: a second monitor to the left of
    /// the first has negative coordinates, and a parser that took an
    /// unsigned integer here would put the window back on the wrong screen.
    #[test]
    fn a_window_on_a_monitor_to_the_left_round_trips() {
        let msg = ToSettings::Live(Live {
            placement: Some((-1920, -540)),
            hotkey: Held::Yes,
        });
        assert_eq!(ToSettings::decode(&msg.encode()), Some(msg));
    }
}
