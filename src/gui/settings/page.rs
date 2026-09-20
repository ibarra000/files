//! One page of the form, drawn from the blocks [`crate::view::settings`]
//! describes.
//!
//! Every branch here is a `match` on a [`Block`], so a block added to the
//! model without a way of drawing it is a compile error rather than a page
//! with a hole in it.

use eframe::egui;

use super::{lists, widgets};
use crate::app::state::SettingChange;
use crate::config::Settings;
use crate::config::write::Typed;
use crate::gui::theme::{self, Theme, Weight};
use crate::view::settings::{ActionId, Block, Field, Page, Row as FormRow};

/// Everything one frame of a page produced or carried over.
pub struct Form<'a> {
    pub editing: &'a mut Option<(crate::config::write::SettingKey, String)>,
    pub draft: &'a mut lists::AliasDraft,
    pub drive: &'a mut lists::DriveDraft,
    pub changed: &'a mut Vec<SettingChange>,
    /// Buttons pressed this frame, in the order they were pressed.
    pub actions: &'a mut Vec<ActionId>,
    pub aliases: Option<Vec<crate::alias::Alias>>,
    pub mappings: Option<Vec<crate::paths::Mapping>>,
}

/// Draws one page.
pub fn show(
    ui: &mut egui::Ui,
    theme: &Theme,
    page: &Page,
    settings: &Settings,
    report: &str,
    form: &mut Form<'_>,
) {
    for (i, group) in page.groups.iter().enumerate() {
        // Between groups and not before the first, so a page does not open
        // with forty points of nothing.
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
                                // Copying is the one action that needs
                                // nothing but the text this function was
                                // handed, so it is done here rather than
                                // sent out and handled a frame later by
                                // something that would have to be given the
                                // report all over again.
                                if action.id == ActionId::CopyReport {
                                    ui.ctx().copy_text(report.to_owned());
                                }
                                form.actions.push(action.id);
                            }
                        }
                    });
                    for action in actions.iter().filter_map(|a| a.help) {
                        ui.label(
                            egui::RichText::new(action)
                                .font(theme::font(theme::SIZE_SMALL, Weight::Regular))
                                .color(theme.dim),
                        );
                    }
                }
                Block::Aliases => lists::aliases(ui, theme, settings, form),
                Block::Drives => lists::drives(ui, theme, settings, form),
                Block::Report { intro } => {
                    ui.label(
                        egui::RichText::new(*intro)
                            .font(theme::font(theme::SIZE_SMALL, Weight::Regular))
                            .color(theme.dim),
                    );
                    ui.add_space(6.0);
                    widgets::report_box(ui, theme, report);
                }
            }
        }
    }
}

/// One setting, drawn as whatever kind of control it needs.
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
        // A setting the environment or a flag is holding is drawn disabled
        // rather than hidden. Hiding it would answer "why can I not change
        // the theme?" with silence; this way the setting is there, its value
        // is there, and the line underneath names what is holding it.
        enabled: row.pin.is_none(),
        control_w,
    };

    widgets::setting_row(ui, theme, spec, |ui| match &row.field {
        // A drop-down, where this used to show every option at once. That
        // was the right answer in a 620-point column with the controls in a
        // 190-point gutter: three words side by side cost nothing and saved
        // a click. It is the wrong answer in a tile whose right-hand column
        // is a fixed width, because three options of unequal length make
        // three rows that line up with nothing.
        Field::Choice { options, current } => {
            let labels: Vec<&str> = options.iter().map(|o| o.label).collect();
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
        // Committed when the box gives up the keyboard, which is Enter and
        // clicking away and is *not* Escape. Not per keystroke: every
        // character of a path would otherwise be a write to a file on a
        // network share, and half of them would name a program that does not
        // exist yet.
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
                // Escape. The draft is dropped and the box goes back to what
                // the file says, which is the whole point of having a way
                // out of a half-typed path.
                *editing = None;
            }
        }
    });
}

/// Whether this page needs the `doctor` report taken.
///
/// Asked of the model rather than hard-coded against a page id, so a report
/// added to a second page does not silently show a stale one.
pub fn wants_report(page: &Page) -> bool {
    page.groups
        .iter()
        .flat_map(|g| &g.blocks)
        .any(|b| matches!(b, Block::Report { .. }))
}

/// The page with this id, if the registry has one.
pub fn find(pages: &[Page], id: crate::view::settings::PageId) -> Option<&Page> {
    pages.iter().find(|p| p.id == id)
}
