//! The alias and drive list editors.
//!
//! Neither keeps a draft copy of its list: each add or remove writes the
//! whole array immediately, so the window always matches the config file.
//! Only the "add" row's input boxes hold unsaved state.

use eframe::egui;

use crate::config::Settings;
use crate::gui::settings::measure;
use crate::gui::settings::page::Form;
use crate::gui::settings::widgets;
use crate::gui::theme::{self, Theme, Weight};
use crate::view;
use crate::view::settings::{ALIASES, DRIVES};

pub struct DriveDraft {
    name: String,
    path: String,
    kind: crate::paths::MappingKind,
    problem: Option<String>,
    /// The drive whose removal is awaiting confirmation.
    confirming: Option<crate::paths::MappingId>,
}

impl Default for DriveDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            // The common case; indexed by a background walk rather than a
            // round trip per search.
            kind: crate::paths::MappingKind::Tree,
            problem: None,
            confirming: None,
        }
    }
}

/// The input boxes on the row that adds an alias.
#[derive(Default)]
pub struct AliasDraft {
    name: String,
    code: String,
    note: String,
    /// Validation error, set only after an add attempt.
    problem: Option<String>,
}

/// Draws the drive list with add and remove controls.
///
/// A missing path is flagged as a warning rather than rejected, because the
/// config roams to machines where a drive may legitimately be unmapped.
/// Changes take effect at the next start, since each drive's index actor is
/// started once.
pub fn drives(ui: &mut egui::Ui, theme: &Theme, settings: &Settings, form: &mut Form<'_>) {
    intro(ui, theme, DRIVES.help);

    let current = settings.routes.all();
    for mapping in current {
        let missing = mapping.enabled
            && !mapping.path.as_os_str().is_empty()
            && !form.exists.is_dir(&mapping.path, form.now, |p| p.is_dir());
        let path = mapping.path.display().to_string();
        let entry = widgets::Entry {
            name: mapping.name.as_ref(),
            detail: &path,
            note: Some(mapping.kind.label()),
            caveat: missing.then_some(DRIVES.not_there),
            lead_w: CHECKBOX_W,
            control_w: BUTTON_W,
        };
        widgets::list_row(
            ui,
            theme,
            entry,
            |ui| {
                let mut enabled = mapping.enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    form.mappings = Some(
                        current
                            .iter()
                            .map(|m| {
                                let mut m = m.clone();
                                if m.id == mapping.id {
                                    m.enabled = enabled;
                                }
                                m
                            })
                            .collect(),
                    );
                }
            },
            |ui| {
                // Confirmed first: removal is not reversible from the UI, and
                // rewriting the list drops its comments in the file.
                if ui.button(DRIVES.remove).clicked() {
                    form.drive.confirming = Some(mapping.id);
                }
            },
        );
    }

    ui.add_space(6.0);
    add_row(ui, |ui, widths| {
        ui.add(
            egui::TextEdit::singleline(&mut form.drive.name)
                .hint_text(DRIVES.name_hint)
                .desired_width(widths[0]),
        );
        ui.add(
            egui::TextEdit::singleline(&mut form.drive.path)
                .hint_text(DRIVES.path_hint)
                .desired_width(widths[1]),
        );
        egui::ComboBox::from_id_salt("files-drive-kind")
            .selected_text(form.drive.kind.label())
            .width(widths[2])
            .show_ui(ui, |ui| {
                for kind in [
                    crate::paths::MappingKind::Flat,
                    crate::paths::MappingKind::Tree,
                    crate::paths::MappingKind::Live,
                ] {
                    ui.selectable_value(&mut form.drive.kind, kind, kind.label());
                }
            });
        if ui.button(DRIVES.add).clicked() {
            match new_drive(current, form.drive) {
                Ok(next) => {
                    form.mappings = Some(next);
                    *form.drive = DriveDraft::default();
                }
                Err(problem) => form.drive.problem = Some(problem),
            }
        }
    });

    if let Some(id) = form.drive.confirming {
        match widgets::confirm(
            ui.ctx(),
            theme,
            "files-remove-drive",
            DRIVES.confirm,
            DRIVES.confirm_go,
            DRIVES.confirm_keep,
        ) {
            Some(true) => {
                form.mappings = Some(current.iter().filter(|m| m.id != id).cloned().collect());
                form.drive.confirming = None;
            }
            Some(false) => form.drive.confirming = None,
            None => {}
        }
    }

    if let Some(problem) = &form.drive.problem {
        widgets::message_bar(
            ui,
            theme,
            view::status::Tone::Warn,
            &crate::view::sentence(problem),
        );
    }
}

/// Returns the list with the drafted drive appended, or why it is invalid.
///
/// Set-level rules come from `paths::conflicts`, as in the loader; the
/// per-entry checks mirror the parser's.
fn new_drive(
    current: &[crate::paths::Mapping],
    draft: &DriveDraft,
) -> Result<Vec<crate::paths::Mapping>, String> {
    let name = draft.name.trim();
    let path = draft.path.trim();
    if name.is_empty() {
        return Err(DRIVES.needs_name.into());
    }
    if path.is_empty() {
        return Err(DRIVES.needs_path.into());
    }
    if current.iter().any(|m| m.name.eq_ignore_ascii_case(name)) {
        return Err(DRIVES.name_taken.into());
    }

    let mut next = current.to_vec();
    next.push(crate::paths::Mapping {
        // Ids are list positions, so the appended entry takes the next index.
        id: crate::paths::MappingId(next.len() as u16),
        name: name.into(),
        path: crate::util::winpath::normalise_root(std::path::Path::new(path)),
        kind: draft.kind,
        enabled: true,
        refresh: crate::paths::RefreshPolicy::default_for(draft.kind),
        depth: crate::config::DEFAULT_LIVE_DEPTH,
    });

    match crate::paths::conflicts(&next).first() {
        Some(conflict) => Err(conflict.detail()),
        None => Ok(next),
    }
}

/// Draws the alias list with add and remove controls.
pub fn aliases(ui: &mut egui::Ui, theme: &Theme, settings: &Settings, form: &mut Form<'_>) {
    intro(ui, theme, ALIASES.help);

    let current = settings.aliases.all();
    if current.is_empty() {
        ui.label(
            egui::RichText::new(ALIASES.empty)
                .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                .color(theme.caption),
        );
    }

    for alias in current {
        let entry = widgets::Entry {
            name: alias.name.as_ref(),
            detail: alias.code.as_ref(),
            note: alias.note.as_deref(),
            caveat: None,
            lead_w: 0.0,
            control_w: BUTTON_W,
        };
        widgets::list_row(
            ui,
            theme,
            entry,
            |_| {},
            |ui| {
                if ui.button(ALIASES.remove).clicked() {
                    form.aliases = Some(
                        current
                            .iter()
                            .filter(|a| a.name != alias.name)
                            .cloned()
                            .collect(),
                    );
                }
            },
        );
    }

    ui.add_space(6.0);
    add_row(ui, |ui, widths| {
        ui.add(
            egui::TextEdit::singleline(&mut form.draft.name)
                .hint_text(ALIASES.name_hint)
                .desired_width(widths[0]),
        );
        ui.add(
            egui::TextEdit::singleline(&mut form.draft.code)
                .hint_text(ALIASES.code_hint)
                .desired_width(widths[1]),
        );
        ui.add(
            egui::TextEdit::singleline(&mut form.draft.note)
                .hint_text(ALIASES.note_hint)
                .desired_width(widths[2]),
        );
        if ui.button(ALIASES.add).clicked() {
            // Same check the loader applies, so anything accepted here loads.
            match crate::alias::check(&form.draft.name, &form.draft.code, current) {
                Ok(()) => {
                    let mut next = current.to_vec();
                    next.push(crate::alias::Alias {
                        name: form.draft.name.trim().into(),
                        code: form.draft.code.trim().into(),
                        note: Some(form.draft.note.trim())
                            .filter(|n| !n.is_empty())
                            .map(Into::into),
                    });
                    form.aliases = Some(next);
                    *form.draft = AliasDraft::default();
                }
                Err(problem) => form.draft.problem = Some(problem.detail()),
            }
        }
    });

    if let Some(problem) = &form.draft.problem {
        widgets::message_bar(
            ui,
            theme,
            view::status::Tone::Warn,
            &crate::view::sentence(problem),
        );
    }
}

/// Fixed widths of a drive row's checkbox and a list row's button, in points.
const CHECKBOX_W: f32 = 24.0;
const BUTTON_W: f32 = 84.0;

/// Draws the add row at the foot of a list: three boxes and a button.
///
/// Unlike other rows, this wraps rather than elides when narrow, because a
/// truncated text box cannot be typed into. It is the only caller that acts
/// on `row_cells` returning `None`.
fn add_row(ui: &mut egui::Ui, build: impl FnOnce(&mut egui::Ui, [f32; 3])) {
    /// Preferred and minimum width of each box.
    const WANT: [f32; 3] = [84.0, 214.0, 150.0];
    const LEAST: [f32; 3] = [56.0, 120.0, 72.0];

    let avail = ui.available_width();
    let cells: Vec<_> = LEAST.iter().map(|w| measure::Cell::Flex(*w)).collect();
    let room = avail - BUTTON_W - CELL_GAP * 4.0;
    match measure::row_cells(room, CELL_GAP, &cells) {
        Some(widths) => {
            let capped = [
                widths[0].min(WANT[0]),
                widths[1].min(WANT[1]),
                widths[2].min(WANT[2]),
            ];
            ui.horizontal(|ui| build(ui, capped));
        }
        None => {
            // Stack the boxes, each at full width.
            let full = (avail - CELL_GAP).max(LEAST[0]);
            ui.vertical(|ui| build(ui, [full, full, full]));
        }
    }
}

/// Gap between boxes on the add row; matches the list row gap.
const CELL_GAP: f32 = 8.0;

/// Draws the explanatory line above a list, styled like a row's help text.
fn intro(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
            .color(theme.dim),
    );
    ui.add_space(6.0);
}
