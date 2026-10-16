//! Tests for the settings form.

use super::*;
use crate::config::ThemeChoice;
use crate::view::style;

fn form(settings: &Settings) -> Vec<Page> {
    pages(None, settings, None, true, Some(false))
}

/// Returns every switch on the form.
fn switches(pages: &[Page]) -> Vec<Switch> {
    pages
        .iter()
        .flat_map(|p| &p.groups)
        .flat_map(|g| &g.blocks)
        .filter_map(|b| match b {
            Block::Switches(list) => Some(list.clone()),
            _ => None,
        })
        .flatten()
        .collect()
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

#[test]
fn every_word_on_every_page_keeps_the_house_style() {
    check_prose(&form(&Settings::fixture()));
}

/// With a configuration file, which changes the caveats.
#[test]
fn the_words_keep_the_house_style_with_a_file_to_save_to() {
    let settings = Settings {
        have_file: true,
        ..Settings::fixture()
    };
    check_prose(&form(&settings));
}

/// Help text is excluded because it may begin with a program name or switch.
#[test]
fn every_title_and_heading_starts_the_way_a_line_should() {
    for page in form(&Settings::fixture()) {
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

#[test]
fn every_writable_setting_appears_on_the_pages_exactly_once() {
    let rows = rows(&Settings::fixture());
    for key in SettingKey::ALL {
        let found = rows.iter().filter(|r| r.key == key).count();
        assert_eq!(found, 1, "{} appears {found} times", key.name());
    }
    assert_eq!(rows.len(), SettingKey::ALL.len());
}

#[test]
fn every_page_has_a_group_and_every_group_has_a_block() {
    for page in form(&Settings::fixture()) {
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

#[test]
fn the_registry_and_the_list_of_pages_agree() {
    let ids: Vec<PageId> = form(&Settings::fixture()).iter().map(|p| p.id).collect();
    assert_eq!(ids, PageId::ALL.to_vec());
}

#[test]
fn every_page_is_distinguishable_from_the_others() {
    for field in [PageId::title, PageId::slug] {
        let mut seen: Vec<_> = PageId::ALL.iter().map(|p| field(*p)).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), PageId::ALL.len(), "two pages share a name");
    }
}

#[test]
fn every_kind_of_block_contributes_something_to_the_corpus() {
    let blocks = [
        Block::Rows(rows(&Settings::fixture())),
        Block::Facts(vec![Fact::new("Label", "value")]),
        Block::Actions(vec![Action::new(ActionId::CopyReport, "Copy")]),
        Block::Switches(vec![Switch::new(
            "Label",
            "Words.",
            false,
            ActionId::StartWithWindows,
            ActionId::StopStartingWithWindows,
        )]),
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
        // The page title plus at least one string from the block.
        assert!(
            page.prose().len() > 1,
            "{block:?} contributes nothing to the check"
        );
    }
}

#[test]
fn a_value_read_out_of_the_configuration_is_not_checked_as_prose() {
    let page = Page {
        id: PageId::General,
        groups: vec![Group {
            heading: None,
            blocks: vec![Block::Facts(vec![
                // Owned: a path that breaks two rules and must be skipped.
                Fact::new("Path", String::from(r"C:\a  b\c...d")),
                // Borrowed: program-written, so checked.
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
    check_prose(&[page]);
}

#[test]
fn a_choice_starts_on_the_setting_that_is_in_force() {
    for theme in [ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::System] {
        let settings = Settings {
            theme,
            ..Settings::fixture()
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
            ..Settings::fixture()
        };
        let row = rows(&settings)
            .into_iter()
            .find(|r| r.key == SettingKey::History)
            .expect("recent codes are on the form");
        assert_eq!(row.field, Field::Toggle { on });
    }
}

#[test]
fn a_session_without_a_configuration_file_says_so_on_every_row() {
    for row in rows(&Settings::fixture()) {
        assert_eq!(row.pin, Some(Pin::NoFile), "{}", row.label);
        assert_eq!(row.caveat().as_deref(), Some(Pin::NoFile.detail().as_str()));
    }
}

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
        ..Settings::fixture()
    };
    let row = rows(&settings)
        .into_iter()
        .find(|r| r.key == SettingKey::Theme)
        .unwrap();
    assert!(!row.needs_restart);
    assert_eq!(row.caveat(), None);
}

/// Pins each setting's page, so moving one is an explicit change here.
#[test]
fn every_setting_is_on_the_page_it_is_meant_to_be_on() {
    const HOME: [(SettingKey, PageId); 20] = [
        (SettingKey::Hotkey, PageId::General),
        (SettingKey::History, PageId::General),
        (SettingKey::HideOnBlur, PageId::General),
        (SettingKey::HideAfterOpening, PageId::General),
        (SettingKey::HideOnEscape, PageId::General),
        (SettingKey::Theme, PageId::Appearance),
        (SettingKey::ResultLayout, PageId::Appearance),
        (SettingKey::Dock, PageId::Appearance),
        (SettingKey::Columns, PageId::Appearance),
        (SettingKey::LiveUpdates, PageId::Drives),
        (SettingKey::StaleNotices, PageId::Drives),
        (SettingKey::HideExtensions, PageId::Searching),
        (SettingKey::HideSystemFiles, PageId::Searching),
        (SettingKey::Viewer, PageId::Opening),
        (SettingKey::PdfViewer, PageId::Opening),
        (SettingKey::PdfReadOnly, PageId::Opening),
        (SettingKey::UpdateFrom, PageId::About),
        (SettingKey::CheckForUpdates, PageId::About),
        (SettingKey::DevMode, PageId::Diagnostics),
        (SettingKey::IndexLog, PageId::Diagnostics),
    ];

    let pages = form(&Settings::fixture());
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

#[test]
fn the_lists_and_the_report_appear_exactly_once_each() {
    let pages = form(&Settings::fixture());
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

#[test]
fn every_action_on_the_form_is_one_the_shell_can_act_on() {
    let settings = Settings {
        have_file: true,
        update_from: Some(std::path::PathBuf::from(r"\\server\share")),
        ..Settings::fixture()
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
    // The fixture has no remembered position, so nothing to forget.
    assert!(!actions.contains(&ActionId::ForgetPlacement));
}

#[test]
fn a_remembered_position_brings_a_way_to_forget_it() {
    let pages = pages(
        None,
        &Settings::fixture(),
        Some((100, 200)),
        true,
        Some(false),
    );
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

#[test]
fn starting_with_windows_is_a_switch_that_reads_the_registry() {
    for on in [false, true] {
        let pages = pages(None, &Settings::fixture(), None, true, Some(on));
        let found = switches(&pages);
        assert_eq!(found.len(), 1, "one switch, not {found:?}");
        let switch = &found[0];
        assert_eq!(switch.on, on);
        assert_eq!(switch.turn_on, ActionId::StartWithWindows);
        assert_eq!(switch.turn_off, ActionId::StopStartingWithWindows);
    }
}

#[test]
fn starting_with_windows_is_on_the_general_page() {
    let pages = form(&Settings::fixture());
    let general = pages.iter().find(|p| p.id == PageId::General).unwrap();
    assert_eq!(switches(std::slice::from_ref(general)).len(), 1);
}

/// An unreadable registry is reported in text, not drawn as an "off" switch
/// that could invite overwriting an existing entry.
#[test]
fn an_unreadable_registry_is_reported_rather_than_guessed_at() {
    let pages = pages(None, &Settings::fixture(), None, true, None);
    assert!(switches(&pages).is_empty());
    let said = pages
        .iter()
        .find(|p| p.id == PageId::General)
        .unwrap()
        .prose()
        .join(" ");
    assert!(said.contains("Start with Windows"), "{said}");
}
