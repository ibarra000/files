//! The alias list and the drive list.
//!
//! Moved out of `gui::windows` when the form became pages: these two are the
//! only blocks with logic of their own, they carry the most careful comments
//! in the window, and both were well past the size a module is held to here.
//!
//! Neither holds a draft of the list it edits. Adding and removing each write
//! the whole array immediately, so the window never has a version of the
//! aliases or the drives the file does not - which is the promise the rest of
//! the form makes, kept the same way. The boxes on the "add" row are the
//! exception that proves it: they are what somebody is typing, not what is
//! configured.

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
    /// The drive whose removal is waiting on an answer.
    ///
    /// Same category as the boxes above: the state a control has while it
    /// is being used, and not a copy of the drive list.
    confirming: Option<crate::paths::MappingId>,
}

impl Default for DriveDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            // What most drives are, and the one whose cost is a background
            // walk rather than a round trip per search.
            kind: crate::paths::MappingKind::Tree,
            problem: None,
            confirming: None,
        }
    }
}

/// The three boxes on the row that adds an alias.
#[derive(Default)]
pub struct AliasDraft {
    name: String,
    code: String,
    note: String,
    /// Shown under the row, and only after an attempt: complaining that a name
    /// is empty before anybody has typed one is nagging.
    problem: Option<String>,
}

/// The drives, with a row to add one and a button to take one away.
///
/// The most dangerous control in this window, and the one written most
/// carefully. A mistyped drive path is the single configuration error with no
/// symptom: the search finds nothing, and a code with no files looks exactly
/// like a job with no files. So a path that is not there is called out on the
/// row rather than accepted silently - as a warning and not a refusal,
/// because this configuration follows people onto laptops where `R:\`
/// legitimately is not mapped.
///
/// Every change is written whole and takes effect at the next start. Nothing
/// is applied live: an index actor per drive is started once, and telling one
/// to become a different drive is a much larger thing than editing a list.
pub fn drives(ui: &mut egui::Ui, theme: &Theme, settings: &Settings, form: &mut Form<'_>) {
    intro(ui, theme, DRIVES.help);

    let current = settings.routes.all();
    for mapping in current {
        // The one error with no symptom, said out loud on the row it is
        // about. A warning rather than a refusal: this file roams, and a
        // laptop at home has none of these drives mapped.
        let missing =
            mapping.enabled && !mapping.path.as_os_str().is_empty() && !mapping.path.is_dir();
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
                // Asked rather than done. This is the one control in the
                // window that cannot be undone by pressing it again: the
                // path is the part nobody remembers, and the rewrite costs
                // the comments around the drive list in the file.
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

/// The list as it would be with this drive added, or why it cannot be.
///
/// The set rules come from `paths::conflicts`, which is what the loader uses,
/// so a drive this accepts is one the next start accepts. The rest - a name,
/// a path, a name that is already taken - are the per-entry rules the parser
/// applies against a line, restated here against a box.
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
        // The position it is about to occupy. Ids are positions in this list,
        // so the one being appended takes the index at the end of it.
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

/// The alias list, with a row to add one and a button to take one away.
///
/// There is no draft list. Adding and removing each write the whole array
/// immediately, so the window never holds a version of the aliases the file
/// does not - which is the same promise the rest of this form makes, kept the
/// same way. The three boxes on the "add" row are the exception that proves
/// it: they are what somebody is typing, not what is configured.
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
                    // The whole list, minus this one. Rewriting the array
                    // wholesale is what keeps "what the window shows" and
                    // "what the file says" the same object rather than two
                    // that have to be kept in step.
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
            // The very same check the loader applies, from the same function.
            // Anything this accepts is something the next start will accept,
            // which is the entire point of it living in `crate::alias`.
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

    // Only after an attempt. Complaining that a name is empty before anybody
    // has typed one is nagging at somebody who has done nothing wrong.
    if let Some(problem) = &form.draft.problem {
        widgets::message_bar(
            ui,
            theme,
            view::status::Tone::Warn,
            &crate::view::sentence(problem),
        );
    }
}

/// Room for the checkbox that turns a drive off, and for a button after it.
///
/// Two numbers where there used to be twelve. Both are what a control of
/// that kind actually occupies rather than a column somebody sized by eye,
/// and neither varies with the window - which is the point of declaring
/// them as `Cell::Fixed`.
const CHECKBOX_W: f32 = 24.0;
const BUTTON_W: f32 = 84.0;

/// The row at the foot of a list, with three boxes and a button.
///
/// The one place in this window where a narrow window wraps rather than
/// elides. Everywhere else an over-long value is cut with an ellipsis and
/// the eye fills it in; a text box treated that way is a box somebody cannot
/// type a path into, so when the three will not fit on one line they go onto
/// two. That is what `row_cells` returning `None` means, and this is the
/// only caller that acts on it.
fn add_row(ui: &mut egui::Ui, build: impl FnOnce(&mut egui::Ui, [f32; 3])) {
    /// What each of the three boxes would like, and the least each can be.
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
            // Two lines. Each box gets the whole width, which is the one
            // arrangement that is never cramped.
            let full = (avail - CELL_GAP).max(LEAST[0]);
            ui.vertical(|ui| build(ui, [full, full, full]));
        }
    }
}

/// Between the boxes on the add row, and the same gap a list row uses.
const CELL_GAP: f32 = 8.0;

/// The sentence over a list, saying what the list is for.
///
/// Not a `setting_row`: a list has no single value and so no control to put
/// beside one, and wrapping it in a tile would make an empty tile above the
/// entries. Plain prose, at the same size and colour a row uses for its
/// help, so the two read as the same kind of line.
fn intro(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
            .color(theme.dim),
    );
    ui.add_space(6.0);
}
