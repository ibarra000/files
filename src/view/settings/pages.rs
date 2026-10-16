//! Builds every page of the settings form from the live state, each frame.

use super::shape::{Action, ActionId, Block, Fact, Group, Page, PageId, Switch};
use std::borrow::Cow;

use super::{
    BACKDROPS, COLUMNS, DOCKS, Field, LAYOUTS, THEMES, index_of, row, viewer_choices, viewer_index,
};
use crate::config::write::SettingKey;
use crate::config::{Dock, Settings};
use crate::view::status::Tone;

/// Returns the whole form.
///
/// Takes the update result rather than `AppState`, so the settings window can
/// run in a process without one. `autostart` is `None` when the registry
/// could not be read.
pub fn pages(
    update: Option<&crate::update::Found>,
    settings: &Settings,
    placement: Option<(i32, i32)>,
    panel: bool,
    autostart: Option<bool>,
) -> Vec<Page> {
    vec![
        general(settings, autostart),
        appearance(settings, placement),
        drives(settings),
        aliases(),
        searching(settings),
        opening(settings),
        about(update, settings, panel),
        diagnostics(settings),
    ]
}

fn general(settings: &Settings, autostart: Option<bool>) -> Page {
    let mut remembering = vec![Block::Rows(vec![row(
        settings,
        SettingKey::History,
        "Recent searches",
        "Remember searches so the up arrow can recall them later.",
        Field::Toggle {
            on: settings.history,
        },
    )])];
    // Shown even when there is nowhere to keep them, so the page keeps its
    // shape.
    remembering.push(Block::Facts(vec![match &settings.history_path {
        Some(path) => Fact::new("Stored in", path.display().to_string()),
        None => Fact::new("Stored in", "Not saved \u{b7} cleared when files closes"),
    }]));

    Page {
        id: PageId::General,
        groups: vec![
            Group {
                heading: Some("Shortcut"),
                blocks: vec![Block::Rows(vec![row(
                    settings,
                    SettingKey::Hotkey,
                    "Hotkey",
                    "Shows the panel from any application. Enter off to disable it.",
                    Field::Text {
                        value: hotkey_of(settings),
                        placeholder: "ctrl+shift+space",
                    },
                )])],
            },
            Group {
                heading: Some("Starting"),
                blocks: vec![starting(autostart)],
            },
            Group {
                heading: Some("History"),
                blocks: remembering,
            },
            Group {
                heading: Some("Auto-hide"),
                blocks: vec![Block::Rows(vec![
                    row(
                        settings,
                        SettingKey::HideOnBlur,
                        "When it loses focus",
                        "Hide the panel when another window is clicked.",
                        Field::Toggle {
                            on: settings.hide_on_blur,
                        },
                    ),
                    row(
                        settings,
                        SettingKey::HideAfterOpening,
                        "After opening a file",
                        "Hide the panel once a file opens. Errors appear in a message \
                         box. Copying always keeps the panel open.",
                        Field::Toggle {
                            on: settings.hide_after_opening,
                        },
                    ),
                    row(
                        settings,
                        SettingKey::HideOnEscape,
                        "When Escape is pressed",
                        "Escape hides the panel instead of clearing the search box. \
                         When off, only the hotkey hides it.",
                        Field::Toggle {
                            on: settings.hide_on_escape,
                        },
                    ),
                ])],
            },
            Group {
                heading: Some("Configuration file"),
                blocks: config_file(),
            },
        ],
    }
}

/// Returns the start-at-sign-in switch, or why it is unavailable.
///
/// A sign-in launch always starts hidden in the tray.
fn starting(autostart: Option<bool>) -> Block {
    match autostart {
        Some(on) => Block::Switches(vec![Switch::new(
            "Start with Windows",
            "Start files in the tray at sign-in, so the hotkey works immediately.",
            on,
            ActionId::StartWithWindows,
            ActionId::StopStartingWithWindows,
        )]),
        None => Block::Facts(vec![Fact::toned(
            "Start with Windows",
            "Unknown \u{b7} the registry could not be read",
            Tone::Warn,
        )]),
    }
}

/// Returns the configuration path and a button to open it in the `.toml`
/// handler.
fn config_file() -> Vec<Block> {
    match crate::config::file::default_config_path() {
        Some(path) => vec![
            Block::Facts(vec![Fact::new("Path", path.display().to_string())]),
            Block::Actions(vec![Action::new(
                ActionId::OpenConfigFile,
                "Open configuration file",
            )]),
        ],
        None => vec![Block::Facts(vec![Fact::new(
            "Path",
            "None \u{b7} using built-in defaults",
        )])],
    }
}

fn appearance(settings: &Settings, placement: Option<(i32, i32)>) -> Page {
    let mut where_it_appears = vec![Block::Rows(vec![row(
        settings,
        SettingKey::Dock,
        "Docked",
        "Floating can be dragged anywhere. Top or bottom pins it to that edge as a \
         full-width bar.",
        Field::Choice {
            options: Cow::Borrowed(DOCKS),
            current: index_of(DOCKS, settings.dock.name()),
        },
    )])];
    // A docked panel cannot be dragged, so the line says where it is. The
    // remembered position is kept for when it floats again.
    where_it_appears.push(Block::Facts(vec![match (settings.dock, placement) {
        (Dock::Top, _) => Fact::new("Position", "Top edge of the screen"),
        (Dock::Bottom, _) => Fact::new("Position", "Bottom edge of the screen"),
        (Dock::Free, Some((left, top))) => {
            Fact::new("Position", format!("Last dragged to {left},{top}"))
        }
        (Dock::Free, None) => Fact::new("Position", "Automatic \u{b7} drag the panel to move it"),
    }]));
    // Only when there is a remembered position to forget.
    if placement.is_some() {
        where_it_appears.push(Block::Actions(vec![Action::new(
            ActionId::ForgetPlacement,
            "Reset position",
        )]));
    }

    Page {
        id: PageId::Appearance,
        groups: vec![
            Group {
                heading: Some("Theme"),
                blocks: vec![Block::Rows(vec![row(
                    settings,
                    SettingKey::Theme,
                    "Colours",
                    "Follow Windows, or always use light or dark.",
                    Field::Choice {
                        options: Cow::Borrowed(THEMES),
                        current: index_of(THEMES, settings.theme.name()),
                    },
                )])],
            },
            Group {
                heading: Some("Background"),
                blocks: vec![Block::Rows(vec![row(
                    settings,
                    SettingKey::Backdrop,
                    "Backdrop",
                    "Acrylic blurs the windows behind the panel. Mica tints it with the \
                     desktop wallpaper. None uses a solid fill, as do Windows versions \
                     without these effects.",
                    Field::Choice {
                        options: Cow::Borrowed(BACKDROPS),
                        current: index_of(BACKDROPS, settings.backdrop.name()),
                    },
                )])],
            },
            Group {
                heading: Some("Results"),
                blocks: vec![Block::Rows(vec![
                    row(
                        settings,
                        SettingKey::ResultLayout,
                        "Rows",
                        "Compact shows six results with name and drive. Detailed shows \
                         four and adds the folder, to tell apart files with the same name.",
                        Field::Choice {
                            options: Cow::Borrowed(LAYOUTS),
                            current: index_of(LAYOUTS, settings.result_layout.name()),
                        },
                    ),
                    row(
                        settings,
                        SettingKey::Columns,
                        "Columns",
                        "More columns show more results with less room for each name. \
                         Up and Down move within a column; Tab and Shift+Tab move between \
                         columns.",
                        Field::Choice {
                            options: Cow::Borrowed(COLUMNS),
                            current: index_of(COLUMNS, &settings.columns.to_string()),
                        },
                    ),
                ])],
            },
            Group {
                heading: Some("Position"),
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
                heading: Some("Drives"),
                blocks: vec![Block::Drives],
            },
            Group {
                heading: Some("Change tracking"),
                blocks: vec![Block::Rows(vec![
                    row(
                        settings,
                        SettingKey::LiveUpdates,
                        "Live updates",
                        "Let the file server report changed folders, so new files appear \
                         within seconds instead of at the next full scan.",
                        Field::Toggle {
                            on: settings.live_updates,
                        },
                    ),
                    row(
                        settings,
                        SettingKey::StaleNotices,
                        "Stale drive warnings",
                        "Show a status line notice when a drive stops tracking changes.",
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
            // No heading; it would repeat the page title.
            heading: None,
            blocks: vec![Block::Aliases],
        }],
    }
}

fn searching(settings: &Settings) -> Page {
    Page {
        id: PageId::Searching,
        groups: vec![Group {
            heading: Some("Excluded from results"),
            blocks: vec![Block::Rows(vec![
                row(
                    settings,
                    SettingKey::HideExtensions,
                    "Hidden file types",
                    "File extensions never shown in results, separated by commas. Leave \
                     empty to show all types.",
                    Field::Text {
                        // Undotted, as in the file; `suffixes` adds the dots
                        // for matching.
                        value: settings
                            .hidden
                            .suffixes()
                            .map(|s| s.trim_start_matches('.'))
                            .collect::<Vec<_>>()
                            .join(", "),
                        placeholder: "None",
                    },
                ),
                row(
                    settings,
                    SettingKey::HideSystemFiles,
                    "Hidden and system files",
                    "Also exclude files Windows marks hidden or system. Takes effect \
                     after F5 rescans the drive.",
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
                    "Automatic uses each file's default app. PDF combines a base name's pages \
                     into one document. Viewers defined as [[viewer]] in the configuration \
                     file are also listed. F2 switches between them.",
                    Field::Choice {
                        options: viewer_choices(settings),
                        current: viewer_index(settings),
                    },
                ),
                row(
                    settings,
                    SettingKey::PdfViewer,
                    "PDF viewer",
                    "Program for combined PDFs. Leave empty to use the default PDF app.",
                    Field::Text {
                        value: path_of(settings.pdf_viewer.as_deref()),
                        placeholder: "Default app",
                    },
                ),
                row(
                    settings,
                    SettingKey::PdfReadOnly,
                    "Open combined PDFs read-only",
                    "Combined PDFs are named by a hash of their contents, so saving into \
                     one leaves a misnamed file. Turn off to annotate and keep the result. \
                     Files opened from the drive are unaffected.",
                    Field::Toggle {
                        on: settings.pdf_read_only,
                    },
                ),
            ])],
        }],
    }
}

fn about(update: Option<&crate::update::Found>, settings: &Settings, panel: bool) -> Page {
    let source = crate::update::Source::of(settings);
    let mut updates = vec![Block::Rows(vec![
        row(
            settings,
            SettingKey::CheckForUpdates,
            "Check for updates",
            "Check GitHub for a new release every four hours. Nothing installs until \
             you choose to. A folder set below replaces GitHub.",
            Field::Toggle {
                on: settings.check_for_updates,
            },
        ),
        row(
            settings,
            SettingKey::UpdateFrom,
            "Update source",
            "A shared folder containing latest.toml and its installer, for offices that \
             publish their own builds. Leave empty to use GitHub.",
            Field::Text {
                value: path_of(settings.update_from.as_deref()),
                placeholder: "GitHub",
            },
        ),
    ])];

    // Everything below the rows depends on there being somewhere to look.
    if let Some(source) = &source {
        let mut facts = vec![Fact::new("Source", source.describe())];
        match update {
            // "Looking" (no answer yet) is distinct from "up to date".
            None => facts.push(Fact::new("Status", "Checking\u{2026}")),
            Some(crate::update::Found::UpToDate) => {
                facts.push(Fact::new("Status", "Up to date"));
            }
            Some(crate::update::Found::Unavailable { detail }) => {
                facts.push(Fact::new("Status", detail.clone()));
            }
            Some(crate::update::Found::Available {
                manifest,
                msi_present,
                ..
            }) => {
                facts.push(Fact::new("Available", manifest.version.to_string()));
                if let Some(notes) = &manifest.notes {
                    facts.push(Fact::new("Release notes", notes.clone()));
                }
                if !*msi_present {
                    facts.push(Fact::toned(
                        "Installer",
                        if source.is_remote() {
                            "Not downloaded \u{b7} retried at the next check"
                        } else {
                            "Missing from the update folder \u{b7} contact the publisher"
                        },
                        Tone::Warn,
                    ));
                }
            }
        }
        updates.push(Block::Facts(facts));

        let ready = matches!(
            update,
            Some(crate::update::Found::Available { msi_present, .. }) if *msi_present
        );
        let mut buttons = vec![Action::new(ActionId::CheckForUpdates, "Check now")];
        // Only with a running panel: `update::apply::hand_over` uses the
        // caller's pid and executable path, so a standalone settings window
        // would wait on and relaunch itself.
        if ready && panel {
            buttons.push(
                Action::new(ActionId::InstallUpdate, "Install and restart")
                    .saying("Closes files, asks Windows for permission, then restarts it."),
            );
        } else if ready {
            updates.push(Block::Facts(vec![Fact::new(
                "Installing",
                "Start files first \u{b7} the update replaces the running program",
            )]));
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
                heading: Some("Error details"),
                blocks: vec![Block::Rows(vec![
                    row(
                        settings,
                        SettingKey::DevMode,
                        "Show technical detail",
                        "Include error codes and folder paths in messages. The report \
                         below always includes them.",
                        Field::Toggle {
                            on: settings.dev_mode,
                        },
                    ),
                    row(
                        settings,
                        SettingKey::IndexLog,
                        "Index log",
                        "Log each decision to re-read a drive to this file: the trigger, \
                         the folder stamp, and the outcome. Leave empty unless \
                         troubleshooting.",
                        Field::Text {
                            value: path_of(settings.index_log.as_deref()),
                            placeholder: "Off",
                        },
                    ),
                ])],
            },
            Group {
                heading: Some("Report"),
                // The button goes above the long report so it needs no
                // scrolling.
                blocks: vec![
                    Block::Actions(vec![
                        Action::new(ActionId::CopyReport, "Copy to clipboard")
                            .saying("Paste into an email when asking for help."),
                        Action::new(ActionId::RefreshReport, "Refresh").saying(
                            "The report is cached for one minute. Refresh after changing a setting.",
                        ),
                    ]),
                    Block::Report {
                        intro: "files-cli --doctor prints the same report.",
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
