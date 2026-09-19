//! What the settings window offers, and what it says about it.
//!
//! The shape of the form and every word on it, as plain data. No colour, no
//! rectangle and no widget - for the reason the rest of this module gives, and
//! for one more that is particular to this screen: a form is mostly prose, and
//! prose is the thing the house style is checked on.
//!
//! # The window does not hold a second copy of the truth
//!
//! That was the stated reason the settings window was read-only, and it is a
//! good one: a window with its own idea of the settings is a window that comes
//! to disagree with the file. Nothing here is a buffer. [`sections`] is a pure
//! function of the live [`Settings`], called every frame, so what is on screen is
//! what is in force. A change is written through [`crate::config::write`] and
//! read back, or it is refused with a reason - there is no third state in
//! which the window believes something the file does not.
//!
//! # Saying what will not stick, and what will not stick yet
//!
//! Two different disappointments, and they are kept apart.
//!
//! [`Row::pin`] means the value cannot be *saved*: the environment or a flag
//! outranks the file, so writing it would report a success and change nothing
//! at the next start. That is [`crate::config::Pin`], and it names what is
//! holding the setting so it can be found and undone.
//!
//! [`Row::needs_restart`] means the value saves perfectly well but nothing
//! reads it again until the program starts. `Settings` is cloned into the
//! backend, every index actor and both workers, so a field one of them
//! captured cannot be changed underneath it. Four settings are read where they
//! are used rather than captured, and those apply immediately; the rest say so
//! rather than appearing to work.

use crate::config::write::SettingKey;
use crate::config::{Pin, Settings};

/// One option in a [`Field::Choice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// What is written to the file.
    pub value: &'static str,
    /// What the window shows.
    pub label: &'static str,
}

/// What kind of control a setting needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    /// One of a fixed set, where the set is short enough to show at once.
    Choice {
        options: &'static [Choice],
        /// Index into `options`. Always valid: it is derived from the live
        /// setting, which is already one of them.
        current: usize,
    },
    Toggle {
        on: bool,
    },
    /// Free text, because the value is a path, a chord or a list and no fixed
    /// set could hold it.
    Text {
        value: String,
        /// Shown when the value is empty, and it says what *not* setting this
        /// does rather than repeating the label.
        placeholder: &'static str,
    },
}

/// One setting, with everything the window has to say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: SettingKey,
    pub label: &'static str,
    /// One sentence on what it does. Whole sentences: this is body prose.
    pub help: &'static str,
    pub field: Field,
    /// Why this cannot be written back, if it cannot.
    pub pin: Option<Pin>,
    /// Whether a change waits for the next start.
    pub needs_restart: bool,
}

impl Row {
    /// What to say under the control, or nothing when there is nothing to say.
    ///
    /// A pin outranks a restart notice, and deliberately: a setting that will
    /// not be saved at all is not also worth telling somebody it would have
    /// applied at the next start.
    pub fn caveat(&self) -> Option<String> {
        match (&self.pin, self.needs_restart) {
            (Some(pin), _) => Some(pin.detail()),
            (None, true) => Some("Applies when files next starts".into()),
            (None, false) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: &'static str,
    pub rows: Vec<Row>,
}

const THEMES: &[Choice] = &[
    Choice {
        value: "light",
        label: "Light",
    },
    Choice {
        value: "dark",
        label: "Dark",
    },
    Choice {
        value: "system",
        label: "Follow Windows",
    },
];

const VIEWERS: &[Choice] = &[
    Choice {
        value: "auto",
        label: "Automatic",
    },
    Choice {
        value: "pdf",
        label: "PDF",
    },
    Choice {
        value: "avwin",
        label: "avwin",
    },
];

/// Where `options` holds `value`, or zero.
///
/// Zero rather than a panic: the value came out of a `Settings` whose every
/// variant is in the list above, so the fallback is unreachable - and a
/// settings window that refuses to open is a poor way to report that somebody
/// added a fourth theme and forgot this file.
fn index_of(options: &[Choice], value: &str) -> usize {
    options.iter().position(|o| o.value == value).unwrap_or(0)
}

/// The whole form, as it stands right now.
pub fn sections(settings: &Settings) -> Vec<Section> {
    vec![
        Section {
            heading: "Appearance",
            rows: vec![row(
                settings,
                SettingKey::Theme,
                "Colours",
                "Follow Windows to change with the system, or pick one and keep it.",
                Field::Choice {
                    options: THEMES,
                    current: index_of(THEMES, settings.theme.name()),
                },
                Applies::Now,
            )],
        },
        Section {
            heading: "Opening",
            rows: vec![
                row(
                    settings,
                    SettingKey::Viewer,
                    "Enter opens",
                    "Automatic assembles a document from its pages and hands anything \
                     else to avwin. F2 switches this while you work.",
                    Field::Choice {
                        options: VIEWERS,
                        current: index_of(VIEWERS, settings.viewer.name()),
                    },
                    Applies::Now,
                ),
                row(
                    settings,
                    SettingKey::PdfViewer,
                    "PDF viewer",
                    "A program to open assembled documents with, instead of whatever \
                     Windows has registered for PDFs.",
                    Field::Text {
                        value: path_of(settings.pdf_viewer.as_deref()),
                        placeholder: "Whatever Windows uses",
                    },
                    Applies::AtNextStart,
                ),
            ],
        },
        Section {
            heading: "Shortcut",
            rows: vec![row(
                settings,
                SettingKey::Hotkey,
                "Summon with",
                "The key that brings this window up from wherever you are working. \
                 Use off to claim no key at all.",
                Field::Text {
                    value: hotkey_of(settings),
                    placeholder: "ctrl+shift+space",
                },
                Applies::AtNextStart,
            )],
        },
        Section {
            heading: "Remembering",
            rows: vec![row(
                settings,
                SettingKey::History,
                "Recent codes",
                "Keep the codes you search for, so the up arrow brings them back \
                 tomorrow.",
                Field::Toggle {
                    on: settings.history,
                },
                Applies::Now,
            )],
        },
        Section {
            heading: "Keeping drives up to date",
            rows: vec![
                row(
                    settings,
                    SettingKey::LiveUpdates,
                    "Follow changes as they happen",
                    "Let the file server say which folders have changed, so a new job \
                     appears within seconds instead of at the next full read.",
                    Field::Toggle {
                        on: settings.live_updates,
                    },
                    Applies::AtNextStart,
                ),
                row(
                    settings,
                    SettingKey::StaleNotices,
                    "Warn when a drive is out of date",
                    "Say so on the status line when a drive has stopped tracking what \
                     is on it.",
                    Field::Toggle {
                        on: settings.stale_notices,
                    },
                    Applies::Now,
                ),
            ],
        },
        Section {
            heading: "What is left out of results",
            rows: vec![
                row(
                    settings,
                    SettingKey::HideExtensions,
                    "Hidden file types",
                    "Extensions never shown, however well they match. Separate them \
                     with commas, or empty the box to hide nothing.",
                    Field::Text {
                        // Undotted, which is how the file is written and how
                        // the help above asks for them. `suffixes` dots them
                        // because that is what the matching wants.
                        value: settings
                            .hidden
                            .suffixes()
                            .map(|s| s.trim_start_matches('.'))
                            .collect::<Vec<_>>()
                            .join(", "),
                        placeholder: "Nothing is hidden",
                    },
                    Applies::AtNextStart,
                ),
                row(
                    settings,
                    SettingKey::HideSystemFiles,
                    "Hidden and system files",
                    "Also leave out whatever Windows marks hidden or system. This one \
                     needs the drive read again, which F5 does.",
                    Field::Toggle {
                        on: settings.hidden.hides_system(),
                    },
                    Applies::AtNextStart,
                ),
            ],
        },
    ]
}

/// Whether a change to a setting is read again without a restart.
///
/// Named rather than a bare bool at eight call sites, because `true` in that
/// position reads as "yes, restart" about as easily as "yes, applies now".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applies {
    /// Read where it is used, so a change is visible at once.
    Now,
    /// Captured by a worker at startup, so a change waits for the next one.
    AtNextStart,
}

fn row(
    settings: &Settings,
    key: SettingKey,
    label: &'static str,
    help: &'static str,
    field: Field,
    applies: Applies,
) -> Row {
    Row {
        key,
        label,
        help,
        field,
        pin: settings.pin(key),
        needs_restart: applies == Applies::AtNextStart,
    }
}

fn path_of(path: Option<&std::path::Path>) -> String {
    path.map(|p| p.display().to_string()).unwrap_or_default()
}

fn hotkey_of(settings: &Settings) -> String {
    match settings.hotkey.bound() {
        Some(hk) => crate::hotkey::spec::describe(hk),
        None => "off".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::style::{self, Slot};

    fn rows(settings: &Settings) -> Vec<Row> {
        sections(settings).into_iter().flatten_rows()
    }

    trait FlattenRows {
        fn flatten_rows(self) -> Vec<Row>;
    }

    impl<I: Iterator<Item = Section>> FlattenRows for I {
        fn flatten_rows(self) -> Vec<Row> {
            self.flat_map(|s| s.rows).collect()
        }
    }

    #[test]
    fn every_word_on_the_form_keeps_the_house_style() {
        let settings = Settings::default();
        let sections = sections(&settings);

        let mut lines: Vec<String> = Vec::new();
        for section in &sections {
            lines.push(section.heading.to_string());
            for row in &section.rows {
                lines.push(row.label.to_string());
                lines.push(row.help.to_string());
                if let Some(caveat) = row.caveat() {
                    lines.push(caveat);
                }
                if let Field::Choice { options, .. } = &row.field {
                    lines.extend(options.iter().map(|o| o.label.to_string()));
                }
                if let Field::Text { placeholder, .. } = &row.field {
                    lines.push(placeholder.to_string());
                }
            }
        }
        style::check_all(
            "the settings form",
            lines.iter().map(String::as_str),
            Slot::Body,
        );
    }

    /// The form describes what is in force, so every control has to start on
    /// the value that is actually set.
    #[test]
    fn a_choice_starts_on_the_setting_that_is_in_force() {
        for theme in [
            crate::config::ThemeChoice::Light,
            crate::config::ThemeChoice::Dark,
            crate::config::ThemeChoice::System,
        ] {
            let settings = Settings {
                theme,
                ..Settings::default()
            };
            let row = rows(&settings)
                .into_iter()
                .find(|r| r.key == SettingKey::Theme)
                .expect("the theme is on the form");
            match row.field {
                Field::Choice { options, current } => {
                    assert_eq!(options[current].value, theme.name())
                }
                other => panic!("the theme should be a choice, not {other:?}"),
            }
        }
    }

    #[test]
    fn a_toggle_starts_on_the_setting_that_is_in_force() {
        for on in [true, false] {
            let settings = Settings {
                history: on,
                ..Settings::default()
            };
            let row = rows(&settings)
                .into_iter()
                .find(|r| r.key == SettingKey::History)
                .expect("recent codes are on the form");
            assert_eq!(row.field, Field::Toggle { on });
        }
    }

    /// Everything the writer can write should be reachable, or the file is
    /// still the only way to change it - which is the thing this screen exists
    /// to stop being true.
    #[test]
    fn every_writable_setting_appears_on_the_form_exactly_once() {
        let rows = rows(&Settings::default());
        for key in SettingKey::ALL {
            let found = rows.iter().filter(|r| r.key == key).count();
            assert_eq!(found, 1, "{} appears {found} times", key.name());
        }
        assert_eq!(rows.len(), SettingKey::ALL.len());
    }

    /// With no file there is nowhere to save anything, and every row says so.
    #[test]
    fn a_session_without_a_configuration_file_says_so_on_every_row() {
        for row in rows(&Settings::default()) {
            assert_eq!(row.pin, Some(Pin::NoFile), "{}", row.label);
            assert_eq!(row.caveat().as_deref(), Some(Pin::NoFile.detail().as_str()));
        }
    }

    /// A setting that cannot be saved at all is not also told it would have
    /// applied at the next start.
    #[test]
    fn a_pin_outranks_a_restart_notice() {
        let row = Row {
            key: SettingKey::PdfViewer,
            label: "PDF viewer",
            help: "…",
            field: Field::Toggle { on: true },
            pin: Some(Pin::CommandLine),
            needs_restart: true,
        };
        assert_eq!(row.caveat(), Some(Pin::CommandLine.detail()));
    }

    #[test]
    fn a_setting_that_applies_at_once_says_nothing_at_all() {
        let settings = Settings {
            have_file: true,
            ..Settings::default()
        };
        let row = rows(&settings)
            .into_iter()
            .find(|r| r.key == SettingKey::Theme)
            .unwrap();
        assert!(!row.needs_restart);
        assert_eq!(row.caveat(), None);
    }
}
