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
        ui.horizontal(|ui| {
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
            ui.allocate_ui_with_layout(
                egui::vec2(84.0, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new(mapping.name.as_ref())
                            .font(theme::font(theme::SIZE_CAPTION, Weight::Semibold))
                            .color(theme.accent),
                    );
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(220.0, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new(mapping.path.display().to_string())
                            .font(theme::font(theme::SIZE_BODY, Weight::Regular))
                            .color(theme.text),
                    );
                },
            );
            ui.label(
                egui::RichText::new(mapping.kind.label())
                    .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                    .color(theme.dim),
            );
            // Asked rather than done. This is the one control in the
            // window that cannot be undone by pressing it again: the path
            // is the part nobody remembers, and the rewrite costs the
            // comments around the drive list in the configuration file.
            if ui.button(DRIVES.remove).clicked() {
                form.drive.confirming = Some(mapping.id);
            }
        });

        // The one error with no symptom, said out loud. A warning rather than
        // a refusal: this file roams, and a laptop at home has none of them.
        if mapping.enabled && !mapping.path.as_os_str().is_empty() && !mapping.path.is_dir() {
            ui.horizontal(|ui| {
                ui.add_space(28.0);
                ui.label(
                    egui::RichText::new(DRIVES.not_there)
                        .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                        .color(theme.tone(view::status::Tone::Warn)),
                );
            });
        }
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.drive.name)
                .hint_text(DRIVES.name_hint)
                .desired_width(84.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut form.drive.path)
                .hint_text(DRIVES.path_hint)
                .desired_width(214.0),
        );
        egui::ComboBox::from_id_salt("files-drive-kind")
            .selected_text(form.drive.kind.label())
            .width(76.0)
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
        ui.label(
            egui::RichText::new(crate::view::sentence(problem))
                .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                .color(theme.tone(view::status::Tone::Warn)),
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
                .color(theme.faint),
        );
    }

    for alias in current {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(90.0, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new(alias.name.as_ref())
                            .font(theme::font(theme::SIZE_CAPTION, Weight::Semibold))
                            .color(theme.accent),
                    );
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(200.0, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new(alias.code.as_ref())
                            .font(theme::font(theme::SIZE_BODY, Weight::Regular))
                            .color(theme.text),
                    );
                },
            );
            if ui.button(ALIASES.remove).clicked() {
                // The whole list, minus this one. Rewriting the array wholesale
                // is what keeps "what the window shows" and "what the file
                // says" the same object rather than two that have to be kept
                // in step.
                form.aliases = Some(
                    current
                        .iter()
                        .filter(|a| a.name != alias.name)
                        .cloned()
                        .collect(),
                );
            }
            if let Some(note) = &alias.note {
                ui.label(
                    egui::RichText::new(note.as_ref())
                        .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                        .color(theme.dim),
                );
            }
        });
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.draft.name)
                .hint_text(ALIASES.name_hint)
                .desired_width(84.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut form.draft.code)
                .hint_text(ALIASES.code_hint)
                .desired_width(194.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut form.draft.note)
                .hint_text(ALIASES.note_hint)
                .desired_width(150.0),
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
        ui.label(
            egui::RichText::new(crate::view::sentence(problem))
                .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                .color(theme.tone(view::status::Tone::Warn)),
        );
    }
}

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
