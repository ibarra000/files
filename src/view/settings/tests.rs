//! What the form promises, asserted.

use super::*;
use crate::app::state::AppState;
use crate::config::ThemeChoice;
use crate::view::style;
use std::time::Instant;

fn state() -> AppState {
    AppState::new(Settings::default(), Instant::now())
}

fn form(settings: &Settings) -> Vec<Page> {
    pages(&state(), settings, None)
}

fn rows(settings: &Settings) -> Vec<Row> {
    form(settings)
        .into_iter()
        .flat_map(|p| p.groups)
        .flat_map(|g| g.blocks)
        .filter_map(|b| match b {
            Block::Rows(rows) => Some(rows),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Every word on every page, in one check.
///
/// The version before this hand-listed eighteen constants, which checked the
/// eighteen it named. [`Page::prose`] is exhaustive over [`Block`], so this
/// one checks whatever is there.
#[test]
fn every_word_on_every_page_keeps_the_house_style() {
    check_prose(&form(&Settings::default()));
}

/// And with a configuration file, which changes what the caveats say.
#[test]
fn the_words_keep_the_house_style_with_a_file_to_save_to() {
    let settings = Settings {
        have_file: true,
        ..Settings::default()
    };
    check_prose(&form(&settings));
}

/// A page title and a group heading start the way a line should. Not the
/// help, which may legitimately begin with a program name or a switch.
#[test]
fn every_title_and_heading_starts_the_way_a_line_should() {
    for page in form(&Settings::default()) {
        assert!(
            style::starts_capitalised(page.id.title()),
            "the {} page title does not",
            page.id.slug()
        );
        for heading in page.groups.iter().filter_map(|g| g.heading) {
            assert!(style::starts_capitalised(heading), "{heading:?} does not");
        }
    }
}

/// Everything the writer can write should be reachable, or the file is still
/// the only way to change it - which is the thing this window exists to stop
/// being true.
#[test]
fn every_writable_setting_appears_on_the_pages_exactly_once() {
    let rows = rows(&Settings::default());
    for key in SettingKey::ALL {
        let found = rows.iter().filter(|r| r.key == key).count();
        assert_eq!(found, 1, "{} appears {found} times", key.name());
    }
    assert_eq!(rows.len(), SettingKey::ALL.len());
}

/// A page with nothing on it is a nav entry that wastes a click, and a group
/// with nothing in it is a heading over nothing.
#[test]
fn every_page_has_a_group_and_every_group_has_a_block() {
    for page in form(&Settings::default()) {
        assert!(
            !page.groups.is_empty(),
            "the {} page is empty",
            page.id.slug()
        );
        for group in &page.groups {
            assert!(
                !group.blocks.is_empty(),
                "a group on the {} page holds nothing",
                page.id.slug()
            );
            for block in &group.blocks {
                if let Block::Rows(rows) = block {
                    assert!(
                        !rows.is_empty(),
                        "an empty run of rows on the {} page",
                        page.id.slug()
                    );
                }
            }
        }
    }
}

/// The registry and the nav have to name the same pages in the same order,
/// or a page exists that nothing can reach.
#[test]
fn the_registry_and_the_list_of_pages_agree() {
    let ids: Vec<PageId> = form(&Settings::default()).iter().map(|p| p.id).collect();
    assert_eq!(ids, PageId::ALL.to_vec());
}

/// Two pages that share a title or a slug are one page and a wasted entry.
#[test]
fn every_page_is_distinguishable_from_the_others() {
    for field in [PageId::title, PageId::slug] {
        let mut seen: Vec<_> = PageId::ALL.iter().map(|p| field(*p)).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), PageId::ALL.len(), "two pages share a name");
    }
}

/// An exhaustive `match` that returns nothing for a variant is a corner of
/// the form the house-style check silently skips.
#[test]
fn every_kind_of_block_contributes_something_to_the_corpus() {
    let blocks = [
        Block::Rows(rows(&Settings::default())),
        Block::Facts(vec![Fact::new("Label", "value")]),
        Block::Actions(vec![Action::new(ActionId::CopyReport, "Copy")]),
        Block::Aliases,
        Block::Drives,
        Block::Report { intro: "Words." },
    ];
    for block in blocks {
        let page = Page {
            id: PageId::General,
            groups: vec![Group {
                heading: None,
                blocks: vec![block.clone()],
            }],
        };
        // One for the page title, and at least one more for the block.
        assert!(
            page.prose().len() > 1,
            "{block:?} contributes nothing to the check"
        );
    }
}

/// A value read out of the configuration keeps its own spelling, so it is
/// not held to the house style. A line this program wrote is.
#[test]
fn a_value_read_out_of_the_configuration_is_not_checked_as_prose() {
    let page = Page {
        id: PageId::General,
        groups: vec![Group {
            heading: None,
            blocks: vec![Block::Facts(vec![
                // Owned: a path, which breaks two rules and must not be
                // checked.
                Fact::new("Path", String::from(r"C:\a  b\c...d")),
                // Borrowed: a line we wrote.
                Fact::new("Stored in", "Nowhere"),
            ])],
        }],
    };
    let prose: Vec<String> = page.prose().iter().map(|c| c.to_string()).collect();
    assert!(prose.contains(&"Nowhere".to_string()));
    assert!(
        !prose.iter().any(|p| p.contains("c...d")),
        "a path was held to the house style: {prose:?}"
    );
    // And the whole thing passes, which is the point.
    check_prose(&[page]);
}

/// The form describes what is in force, so every control has to start on the
/// value that is actually set.
#[test]
fn a_choice_starts_on_the_setting_that_is_in_force() {
    for theme in [ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::System] {
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
        help: "\u{2026}",
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

/// Where a setting lives is a decision about where somebody will look for
/// it, so moving one should be a visible change to a table rather than a
/// silent change to a function.
#[test]
fn every_setting_is_on_the_page_it_is_meant_to_be_on() {
    const HOME: [(SettingKey, PageId); 17] = [
        (SettingKey::Hotkey, PageId::General),
        (SettingKey::History, PageId::General),
        (SettingKey::HideOnBlur, PageId::General),
        (SettingKey::HideAfterOpening, PageId::General),
        (SettingKey::HideOnEscape, PageId::General),
        (SettingKey::Theme, PageId::Appearance),
        (SettingKey::ResultLayout, PageId::Appearance),
        (SettingKey::LiveUpdates, PageId::Drives),
        (SettingKey::StaleNotices, PageId::Drives),
        (SettingKey::HideExtensions, PageId::Searching),
        (SettingKey::HideSystemFiles, PageId::Searching),
        (SettingKey::Viewer, PageId::Opening),
        (SettingKey::PdfViewer, PageId::Opening),
        (SettingKey::PdfReadOnly, PageId::Opening),
        (SettingKey::UpdateFrom, PageId::About),
        (SettingKey::DevMode, PageId::Diagnostics),
        (SettingKey::IndexLog, PageId::Diagnostics),
    ];

    let pages = form(&Settings::default());
    for (key, want) in HOME {
        let found = pages
            .iter()
            .find(|page| {
                page.groups.iter().flat_map(|g| &g.blocks).any(|b| match b {
                    Block::Rows(rows) => rows.iter().any(|r| r.key == key),
                    _ => false,
                })
            })
            .map(|p| p.id);
        assert_eq!(
            found,
            Some(want),
            "{} is not where it should be",
            key.name()
        );
    }
}

/// The two lists and the report each have exactly one home, so nothing is
/// drawn twice and nothing is unreachable.
#[test]
fn the_lists_and_the_report_appear_exactly_once_each() {
    let pages = form(&Settings::default());
    let blocks: Vec<&Block> = pages
        .iter()
        .flat_map(|p| &p.groups)
        .flat_map(|g| &g.blocks)
        .collect();

    for (what, count) in [
        (
            "aliases",
            blocks
                .iter()
                .filter(|b| matches!(b, Block::Aliases))
                .count(),
        ),
        (
            "drives",
            blocks.iter().filter(|b| matches!(b, Block::Drives)).count(),
        ),
        (
            "the report",
            blocks
                .iter()
                .filter(|b| matches!(b, Block::Report { .. }))
                .count(),
        ),
    ] {
        assert_eq!(count, 1, "{what} appears {count} times");
    }
}

/// A button that reaches outside the window is one the shell has to handle,
/// so every one the form can produce must be a variant the shell knows.
#[test]
fn every_action_on_the_form_is_one_the_shell_can_act_on() {
    let settings = Settings {
        have_file: true,
        update_from: Some(std::path::PathBuf::from(r"\\server\share")),
        ..Settings::default()
    };
    let pages = form(&settings);
    let actions: Vec<ActionId> = pages
        .iter()
        .flat_map(|p| &p.groups)
        .flat_map(|g| &g.blocks)
        .filter_map(|b| match b {
            Block::Actions(list) => Some(list),
            _ => None,
        })
        .flatten()
        .map(|a| a.id)
        .collect();

    assert!(actions.contains(&ActionId::CheckForUpdates));
    assert!(actions.contains(&ActionId::CopyReport));
    // Not `ForgetPlacement`: there is no remembered position in this
    // fixture, and a button to forget one would have nothing to forget.
    assert!(!actions.contains(&ActionId::ForgetPlacement));
}

/// And it appears as soon as there is something to forget.
#[test]
fn a_remembered_position_brings_a_way_to_forget_it() {
    let pages = pages(&state(), &Settings::default(), Some((100, 200)));
    let has = pages
        .iter()
        .flat_map(|p| &p.groups)
        .flat_map(|g| &g.blocks)
        .filter_map(|b| match b {
            Block::Actions(list) => Some(list),
            _ => None,
        })
        .flatten()
        .any(|a| a.id == ActionId::ForgetPlacement);
    assert!(has, "there was a position and no way to forget it");
}
