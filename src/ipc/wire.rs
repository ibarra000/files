//! Text codec for messages between the panel and the settings window.
//!
//! One message per line: a verb, then space-separated fields. Fields are
//! percent-escaped so they contain no spaces or control characters.
//!
//! ```text
//! show diagnostics
//! live 100,200 no:Ctrl+Shift+Space%20is%20taken
//! live - yes
//! closing
//! ```
//!
//! Hand-written to avoid a serialisation dependency (see `Cargo.toml`), and
//! text so the protocol is readable in logs and debuggers.
//!
//! Any process can connect to the pipe, so decoding must never panic: every
//! field is parsed through `Option`, and a malformed line decodes to `None`.

use crate::view::settings::PageId;

/// Messages from the panel to the settings window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToSettings {
    /// Bring the window to the front on this page.
    Show(PageId),
    /// Close the window (tray menu toggle).
    Close,
    /// Runtime state only the panel knows; everything else comes from the
    /// configuration file.
    Live(Live),
    /// The configuration file changed; reload it.
    Reload,
    /// The panel is shutting down; disable actions that need it.
    Exiting,
}

/// Messages from the settings window to the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToPanel {
    /// The configuration file was written; reload it.
    Changed,
    /// Install the update found by the About page.
    ///
    /// Must run in the panel: `update::apply::hand_over` uses the current
    /// process id and executable, so the settings process would relaunch
    /// itself.
    InstallUpdate,
    /// Forget the remembered panel position.
    ForgetPlacement,
    /// The window is closing.
    Closing,
}

/// Runtime state only the running panel knows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Live {
    /// The remembered panel position, if any.
    pub placement: Option<(i32, i32)>,
    /// Whether the panel holds the global chord.
    pub hotkey: Held,
}

/// Whether the panel's hotkey listener holds the global chord.
///
/// Sent by the panel because a probe from the settings process would find the
/// chord taken by the panel itself. See [`crate::hotkey::Probe`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Held {
    /// Not reported yet; permanent when the hotkey is off.
    #[default]
    Unknown,
    /// The panel holds it.
    Yes,
    /// Registration failed, with the reason.
    No(String),
}

// -- encoding ---------------------------------------------------------------

/// Percent-encodes `%`, space and control characters below U+0020.
///
/// Not a general URL encoder; it only needs to round-trip.
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

/// Reverses [`escape`]; returns `None` on a malformed escape.
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
    /// Encodes as one line without a terminator; the transport adds it.
    pub fn encode(&self) -> String {
        match self {
            Self::Show(page) => format!("show {}", page_slug(*page)),
            Self::Close => "close".to_owned(),
            Self::Live(live) => format!("live {}", live.encode()),
            Self::Reload => "reload".to_owned(),
            Self::Exiting => "exiting".to_owned(),
        }
    }

    /// Decodes one line; returns `None` for anything malformed or unknown.
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

    /// Representative values of every variant, for the round-trip test.
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

    #[test]
    fn nothing_encodes_to_more_than_one_line() {
        for line in ToSettings::all().iter().map(ToSettings::encode) {
            assert!(!line.contains('\n'), "{line:?}");
            assert!(!line.contains('\r'), "{line:?}");
            assert!(!line.is_empty());
        }
    }

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

    #[test]
    fn a_percent_sign_survives() {
        for text in ["100%", "%20", "%", "%%", "a % b", "\u{1}\u{7f}"] {
            let escaped = escape(text);
            assert!(!escaped.contains(' '), "{escaped:?}");
            assert_eq!(unescape(&escaped).as_deref(), Some(text));
        }
    }

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

    /// Exhaustive over lines of up to three tokens from the format's alphabet,
    /// plus every single byte.
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
        // Every single byte, to exercise `char` boundary handling.
        for byte in 0u8..=255 {
            let s = String::from_utf8_lossy(&[byte]).into_owned();
            let _ = ToSettings::decode(&s);
            let _ = ToSettings::decode(&format!("live {s} {s}"));
            let _ = ToSettings::decode(&format!("show {s}"));
            let _ = ToPanel::decode(&s);
        }
    }

    #[test]
    fn a_window_on_a_monitor_to_the_left_round_trips() {
        let msg = ToSettings::Live(Live {
            placement: Some((-1920, -540)),
            hotkey: Held::Yes,
        });
        assert_eq!(ToSettings::decode(&msg.encode()), Some(msg));
    }
}
