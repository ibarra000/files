//! The form as it stands right now: every page, group, row and word.
//!
//! A pure function of the live state, called every frame, for the reason the
//! module note above gives. Nothing here is a buffer and nothing here is
//! remembered.

use super::shape::{Action, ActionId, Block, Fact, Group, Page, PageId};
use super::{BACKDROPS, Field, LAYOUTS, THEMES, VIEWERS, index_of, row};
use crate::app::state::AppState;
use crate::config::Settings;
use crate::config::write::SettingKey;
use crate::view::status::Tone;

/// The whole form.
///
/// Three arguments rather than one because three things are being described:
/// the settings, the runtime state the About page reads, and the remembered
/// window position, which is neither. The same shape `view::status::render`
/// and `view::hints::Hints::of` already have.
pub fn pages(state: &AppState, settings: &Settings, placement: Option<(i32, i32)>) -> Vec<Page> {
    vec![
        general(settings),
        appearance(settings, placement),
        drives(settings),
        aliases(),
        searching(settings),
        opening(settings),
        about(state, settings),
        diagnostics(settings),
    ]
}

fn general(settings: &Settings) -> Page {
    let mut remembering = vec![Block::Rows(vec![row(
        settings,
        SettingKey::History,
        "Recent codes",
        "Keep the codes you search for, so the up arrow brings them back tomorrow.",
        Field::Toggle {
            on: settings.history,
        },
    )])];
    // Said whether or not there is anywhere to keep them. A block that
    // vanishes is a page that changes shape, and a page that changes shape
    // is harder to learn than one that says "nowhere".
    remembering.push(Block::Facts(vec![match &settings.history_path {
        Some(path) => Fact::new("Stored in", path.display().to_string()),
        None => Fact::new(
            "Stored in",
            "Nowhere \u{b7} recall lasts until files closes",
        ),
    }]));

    Page {
        id: PageId::General,
        groups: vec![
            Group {
                heading: Some("Shortcut"),
                blocks: vec![Block::Rows(vec![row(
                    settings,
                    SettingKey::Hotkey,
                    "Summon with",
                    "The key that brings the panel up from wherever you are working. Use \
                     off to claim no key at all.",
                    Field::Text {
                        value: hotkey_of(settings),
                        placeholder: "ctrl+shift+space",
                    },
                )])],
            },
            Group {
                heading: Some("Remembering"),
                blocks: remembering,
            },
            Group {
                heading: Some("When the panel puts itself away"),
                blocks: vec![Block::Rows(vec![row(
                    settings,
                    SettingKey::AutoHide,
                    "Close the panel when something opens",
                    "Leave this off and the panel stays up, so whatever the open has to \
                     say is somewhere you can still read it and a second code is a \
                     keystroke rather than the shortcut again. Escape and the shortcut \
                     close the panel either way.",
                    Field::Toggle {
                        on: settings.auto_hide,
                    },
                )])],
            },
            Group {
                heading: Some("Configuration file"),
                blocks: config_file(),
            },
        ],
    }
}

/// The path, and a button to open it in whatever handles a `.toml`.
///
/// On General rather than with the diagnostics, which is where a repair tool
/// would go: this file is what every page in this window writes to, not
/// something you reach for when something is broken.
fn config_file() -> Vec<Block> {
    match crate::config::file::default_config_path() {
        Some(path) => vec![
            Block::Facts(vec![Fact::new("Path", path.display().to_string())]),
            Block::Actions(vec![Action::new(
                ActionId::OpenConfigFile,
                "Open the configuration file",
            )]),
        ],
        None => vec![Block::Facts(vec![Fact::new(
            "Path",
            "None \u{b7} running on the built-in defaults",
        )])],
    }
}

fn appearance(settings: &Settings, placement: Option<(i32, i32)>) -> Page {
    let mut where_it_appears = vec![Block::Facts(vec![match placement {
        Some((left, top)) => Fact::new("Position", format!("Where you left it, {left},{top}")),
        None => Fact::new(
            "Position",
            "Chosen by the program \u{b7} drag the panel to move it",
        ),
    }])];
    // Only when there is something to forget. A button that does nothing is
    // a button that should not have been there.
    if placement.is_some() {
        where_it_appears.push(Block::Actions(vec![Action::new(
            ActionId::ForgetPlacement,
            "Forget the remembered position",
        )]));
    }

    Page {
        id: PageId::Appearance,
        groups: vec![
            Group {
                heading: Some("Colours"),
                blocks: vec![Block::Rows(vec![row(
                    settings,
                    SettingKey::Theme,
                    "Colours",
                    "Follow Windows to change with the system, or pick one and keep it.",
                    Field::Choice {
                        options: THEMES,
                        current: index_of(THEMES, settings.theme.name()),
                    },
                )])],
            },
            Group {
                heading: Some("What shows through"),
                blocks: vec![Block::Rows(vec![row(
                    settings,
                    SettingKey::Backdrop,
                    "Behind the panel",
                    "Acrylic blurs the window underneath, which is what the panel is \
                     usually over. Mica samples the desktop wallpaper instead, so it \
                     shows the wallpaper wherever the panel happens to be. None lets \
                     the panel paint its own, which is what an older Windows gives \
                     you whatever you pick here.",
                    Field::Choice {
                        options: BACKDROPS,
                        current: index_of(BACKDROPS, settings.backdrop.name()),
                    },
                )])],
            },
            Group {
                heading: Some("How a result is listed"),
                blocks: vec![Block::Rows(vec![row(
                    settings,
                    SettingKey::ResultLayout,
                    "Rows",
                    "Compact fits six results on screen and shows the name and the \
                     drive. Detailed fits four and puts the folder under each name, \
                     which is how you tell two drawings with the same name apart.",
                    Field::Choice {
                        options: LAYOUTS,
                        current: index_of(LAYOUTS, settings.result_layout.name()),
                    },
                )])],
            },
            Group {
                heading: Some("Where the panel appears"),
                blocks: where_it_appears,
            },
        ],
    }
}

fn drives(settings: &Settings) -> Page {
    Page {
        id: PageId::Drives,
        groups: vec![
            Group {
                heading: Some("Where to search"),
                blocks: vec![Block::Drives],
            },
            Group {
                heading: Some("Keeping drives up to date"),
                blocks: vec![Block::Rows(vec![
                    row(
                        settings,
                        SettingKey::LiveUpdates,
                        "Follow changes as they happen",
                        "Let the file server say which folders have changed, so a new job \
                         appears within seconds instead of at the next full read.",
                        Field::Toggle {
                            on: settings.live_updates,
                        },
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
                    ),
                ])],
            },
        ],
    }
}

fn aliases() -> Page {
    Page {
        id: PageId::Aliases,
        groups: vec![Group {
            // No heading. The page is called Aliases and the group would be
            // called Aliases, which is the repetition `Group::heading` is
            // optional to avoid.
            heading: None,
            blocks: vec![Block::Aliases],
        }],
    }
}

fn searching(settings: &Settings) -> Page {
    Page {
        id: PageId::Searching,
        groups: vec![Group {
            heading: Some("What is left out of results"),
            blocks: vec![Block::Rows(vec![
                row(
                    settings,
                    SettingKey::HideExtensions,
                    "Hidden file types",
                    "Extensions never shown, however well they match. Separate them with \
                     commas, or empty the box to hide nothing.",
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
                ),
            ])],
        }],
    }
}

fn opening(settings: &Settings) -> Page {
    Page {
        id: PageId::Opening,
        groups: vec![Group {
            heading: None,
            blocks: vec![Block::Rows(vec![
                row(
                    settings,
                    SettingKey::Viewer,
                    "Enter opens",
                    "Automatic assembles a document from its pages and hands anything else \
                     to avwin. F2 switches this while you work.",
                    Field::Choice {
                        options: VIEWERS,
                        current: index_of(VIEWERS, settings.viewer.name()),
                    },
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
                ),
                row(
                    settings,
                    SettingKey::PdfReadOnly,
                    "Assembled documents open read-only",
                    "An assembled document is named after a hash of its own contents, so a \
                     viewer that saves into it leaves a file whose name no longer describes \
                     it. Turn this off to annotate one and keep the result. A file opened \
                     straight off the drive is untouched either way.",
                    Field::Toggle {
                        on: settings.pdf_read_only,
                    },
                ),
            ])],
        }],
    }
}

fn about(state: &AppState, settings: &Settings) -> Page {
    let mut updates = vec![Block::Rows(vec![row(
        settings,
        SettingKey::UpdateFrom,
        "Look for new versions in",
        "A folder holding latest.toml and the installer beside it. Empty it and no \
         looking happens at all.",
        Field::Text {
            value: path_of(settings.update_from.as_deref()),
            placeholder: "Nowhere, so nothing is checked",
        },
    )])];

    // Everything below the row depends on there being a folder at startup,
    // because that is when the checker thread was started or not started.
    // See `app::state::settings::apply_live`.
    if settings.update_from.is_some() {
        let mut facts = Vec::new();
        match &state.update {
            // Three states, and the third is not the second. "Looking" means
            // the checker has not answered, which is a different thing from
            // having looked and found nothing - claiming to be up to date
            // before knowing would be the one lie this page could tell.
            None => facts.push(Fact::new("Status", "Looking\u{2026}")),
            Some(crate::update::Found::UpToDate) => {
                facts.push(Fact::new("Status", "This is the newest version"));
            }
            Some(crate::update::Found::Unavailable { detail }) => {
                facts.push(Fact::new("Status", detail.clone()));
            }
            Some(crate::update::Found::Available { manifest, msi }) => {
                facts.push(Fact::new("Available", manifest.version.to_string()));
                if let Some(notes) = &manifest.notes {
                    facts.push(Fact::new("What changed", notes.clone()));
                }
                if !msi.is_file() {
                    facts.push(Fact::toned(
                        "Installer",
                        "Not where the manifest says it is \u{b7} ask whoever published it",
                        Tone::Warn,
                    ));
                }
            }
        }
        updates.push(Block::Facts(facts));

        let ready = matches!(
            &state.update,
            Some(crate::update::Found::Available { msi, .. }) if msi.is_file()
        );
        let mut buttons = vec![Action::new(ActionId::CheckForUpdates, "Check now")];
        if ready {
            buttons.push(
                Action::new(ActionId::InstallUpdate, "Install and restart").saying(
                    "Installing closes files, asks Windows for permission, and opens it \
                     again.",
                ),
            );
        }
        updates.push(Block::Actions(buttons));
    }

    Page {
        id: PageId::About,
        groups: vec![
            Group {
                heading: Some("Version"),
                blocks: vec![Block::Facts(vec![Fact::new(
                    "This version",
                    crate::update::Version::current().to_string(),
                )])],
            },
            Group {
                heading: Some("Updates"),
                blocks: updates,
            },
        ],
    }
}

fn diagnostics(settings: &Settings) -> Page {
    Page {
        id: PageId::Diagnostics,
        groups: vec![
            Group {
                heading: Some("Detail"),
                blocks: vec![Block::Rows(vec![
                    row(
                        settings,
                        SettingKey::DevMode,
                        "Show technical detail",
                        "Add the error codes and folder paths behind a message. Useful when \
                         somebody is helping you, and noise the rest of the time. The \
                         report below carries them either way.",
                        Field::Toggle {
                            on: settings.dev_mode,
                        },
                    ),
                    row(
                        settings,
                        SettingKey::IndexLog,
                        "Record why the drives are read again",
                        "Append a line to a file each time the index decides whether to \
                         read a drive again: what woke it, what the folder stamp said, and \
                         what it did. Leave this empty unless somebody has asked you to \
                         turn it on.",
                        Field::Text {
                            value: path_of(settings.index_log.as_deref()),
                            placeholder: "Nothing is recorded",
                        },
                    ),
                ])],
            },
            Group {
                heading: Some("Report"),
                // The button first, then the report. It used to be the
                // other way round, which put the one thing anybody comes to
                // this page to do underneath three hundred points of
                // monospaced text they had to scroll past to reach it.
                blocks: vec![
                    Block::Actions(vec![
                        Action::new(ActionId::CopyReport, "Copy to clipboard")
                            .saying("Paste this into an email if you are asking for help."),
                    ]),
                    Block::Report {
                        intro: "Everything below is also what files --doctor prints.",
                    },
                ],
            },
        ],
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
