//! Draws one settings page from the blocks [`crate::view::settings`]
//! describes. Dispatch is an exhaustive `match` on [`Block`], so a new block
//! kind without a renderer fails to compile.

use eframe::egui;

use super::{lists, widgets};
use crate::app::state::SettingChange;
use crate::config::Settings;
use crate::config::write::Typed;
use crate::gui::theme::{self, Theme, Weight};
use crate::view::settings::{ActionId, Block, Field, Page, Row as FormRow};

/// Per-frame state a page reads and the edits it produces.
pub struct Form<'a> {
    pub editing: &'a mut Option<(crate::config::write::SettingKey, String)>,
    pub draft: &'a mut lists::AliasDraft,
    pub drive: &'a mut lists::DriveDraft,
    pub changed: &'a mut Vec<SettingChange>,
    /// Buttons pressed this frame, in the order they were pressed.
    pub actions: &'a mut Vec<ActionId>,
    pub aliases: Option<Vec<crate::alias::Alias>>,
    pub mappings: Option<Vec<crate::paths::Mapping>>,
    /// Cached drive existence checks; see [`super::exists`].
    pub exists: &'a mut super::exists::DirCache,
    /// The clock, read once per frame.
    pub now: std::time::Instant,
}

/// Draws one page.
pub fn show(
    ui: &mut egui::Ui,
    theme: &Theme,
    page: &Page,
    settings: &Settings,
    report: super::report::View<'_>,
    form: &mut Form<'_>,
) {
    for (i, group) in page.groups.iter().enumerate() {
        // Gap between groups only, not above the first.
        if i > 0 {
            widgets::group_gap(ui);
        }
        if let Some(heading) = group.heading {
            widgets::group_heading(ui, theme, heading);
        }
        for block in &group.blocks {
            match block {
                Block::Rows(rows) => {
                    for row in rows {
                        control(ui, theme, row, form);
                    }
                }
                Block::Facts(facts) => {
                    for fact in facts {
                        widgets::fact(
                            ui,
                            theme,
                            fact.label,
                            &fact.value,
                            theme.emphasis(fact.emphasis),
                        );
                    }
                }
                Block::Actions(actions) => {
                    ui.horizontal(|ui| {
                        for action in actions {
                            let pressed = ui
                                .add_enabled_ui(action.enabled, |ui| {
                                    widgets::button(ui, action.label).clicked()
                                })
                                .inner;
                            if pressed {
                                // Copy is handled here because it needs only
                                // the report text already in hand.
                                if action.id == ActionId::CopyReport {
                                    // Copies nothing while the report is pending.
                                    if let super::report::View::Ready(text)
                                    | super::report::View::Stale(text) = report
                                    {
                                        ui.ctx().copy_text(text.to_owned());
                                    }
                                }
                                form.actions.push(action.id);
                            }
                        }
                    });
                    for action in actions.iter().filter_map(|a| a.help) {
                        ui.label(
                            egui::RichText::new(action)
                                .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                                .color(theme.dim),
                        );
                    }
                }
                Block::Switches(switches) => {
                    for switch in switches {
                        let spec = widgets::Row {
                            label: switch.label,
                            help: switch.help,
                            caveat: None,
                            enabled: true,
                            control_w: widgets::width::SWITCH,
                        };
                        let actions = &mut *form.actions;
                        widgets::setting_row(ui, theme, spec, |ui| {
                            let mut value = switch.on;
                            if widgets::switch(ui, theme, &mut value, switch.label).changed() {
                                actions.push(switch.flipped());
                            }
                        });
                    }
                }
                Block::Aliases => lists::aliases(ui, theme, settings, form),
                Block::Drives => lists::drives(ui, theme, settings, form),
                Block::Report { intro } => {
                    ui.label(
                        egui::RichText::new(*intro)
                            .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                            .color(theme.dim),
                    );
                    ui.add_space(6.0);
                    widgets::report_box(ui, theme, report);
                }
            }
        }
    }
}

/// Draws one setting with the control its field type needs.
fn control(ui: &mut egui::Ui, theme: &Theme, row: &FormRow, form: &mut Form<'_>) {
    use widgets::width;

    let changed = &mut *form.changed;
    let editing = &mut *form.editing;
    let mut push = |typed| {
        changed.push(SettingChange {
            key: row.key,
            typed,
            label: row.label,
        })
    };

    let caveat = row.caveat();
    let control_w = match &row.field {
        Field::Choice { .. } => width::DROPDOWN,
        Field::Toggle { .. } => width::SWITCH,
        Field::Text { .. } => width::TEXT,
    };
    let spec = widgets::Row {
        label: row.label,
        help: row.help,
        caveat: caveat.as_deref(),
        // A setting pinned by the environment or a flag is shown disabled,
        // with a caveat naming what pins it.
        enabled: row.pin.is_none(),
        control_w,
    };

    widgets::setting_row(ui, theme, spec, |ui| match &row.field {
        // A drop-down fits the fixed-width control column; inline options of
        // unequal length would not align.
        Field::Choice { options, current } => {
            let labels: Vec<&str> = options.iter().map(|o| o.label.as_ref()).collect();
            let salt = format!("files-setting-{}", row.key.name());
            if let Some(i) = widgets::dropdown(ui, &salt, *current, &labels, control_w) {
                push(Typed::Text(options[i].value.to_string()));
            }
        }
        Field::Toggle { on } => {
            let mut value = *on;
            if widgets::switch(ui, theme, &mut value, row.label).changed() {
                push(Typed::Flag(value));
            }
        }
        // Committed on Enter or click-away, not per keystroke, so partial
        // paths are never written to the config file. Escape discards.
        Field::Text { value, placeholder } => {
            let mut text = match &editing {
                Some((key, buffer)) if *key == row.key => buffer.clone(),
                _ => value.clone(),
            };
            let typed = widgets::text_field(ui, theme, &mut text, placeholder, control_w, None);
            if typed.commit {
                if text.trim() != value.trim() {
                    push(Typed::Text(text));
                }
                *editing = None;
            } else if typed.response.has_focus() {
                *editing = Some((row.key, text));
            } else if typed.response.lost_focus() {
                // Escape: drop the draft and revert to the saved value.
                *editing = None;
            }
        }
    });
}

/// Returns whether `page` contains a `doctor` report block.
pub fn wants_report(page: &Page) -> bool {
    page.groups
        .iter()
        .flat_map(|g| &g.blocks)
        .any(|b| matches!(b, Block::Report { .. }))
}

/// Returns the page with this id, if any.
pub fn find(pages: &[Page], id: crate::view::settings::PageId) -> Option<&Page> {
    pages.iter().find(|p| p.id == id)
}
