//! Widgets for settings pages. Some are egui widgets styled by
//! [`crate::gui::theme::apply_style`]; the rest are painted here.
//!
//! The switch is painted because egui has none and a checkbox implies a
//! deferred apply. It still reports itself as a checkbox via
//! [`egui::WidgetInfo::selected`], which screen readers and `tests/panel.rs`
//! rely on.

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Rect, Response, Sense, Stroke, StrokeKind,
    Ui, UiBuilder, WidgetInfo, WidgetText, WidgetType, pos2, vec2,
};

use super::{measure, report};
use crate::gui::theme::{self, Icon, Theme, Weight};
use crate::view::status::Tone;

/// Switch height and width, in the standard 1:2 proportion.
const SWITCH_H: f32 = 20.0;
const SWITCH_W: f32 = 40.0;

/// Height of a nav entry.
const NAV_H: f32 = 32.0;

/// Size of a frameless icon button.
const ICON_BTN: f32 = 24.0;

/// Draws a flat card tile with no border or shadow. See [`super`] for why
/// this does not use `theme::raise`.
pub fn card<R>(ui: &mut Ui, theme: &Theme, body: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme.card)
        .corner_radius(theme::radius(theme::RADIUS_MEDIUM))
        .inner_margin(measure::CARD_PAD)
        .show(ui, body)
        .inner
}

/// Draws a group heading: caption size, semibold, smaller than the labels
/// beneath it.
pub fn group_heading(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::font(theme::SIZE_CAPTION, Weight::Semibold))
            .color(theme.dim),
    );
    ui.add_space(measure::LABEL_GAP);
}

/// Describes one setting row.
pub struct Row<'a> {
    pub label: &'a str,
    /// One sentence on what the setting does.
    pub help: &'a str,
    /// Why it will not save, or when it will apply; drawn in the warning tone.
    pub caveat: Option<&'a str>,
    /// False when the value is pinned and cannot be written; the control is
    /// then drawn disabled.
    pub enabled: bool,
    /// The control's preferred width. [`super::measure::control_width`] may
    /// shrink it in a narrow window, so controls must stay within the rect
    /// they are given.
    pub control_w: f32,
}

/// Draws one setting: label and help on the left, control on the right.
///
/// Measured then painted, so the control can be centred on the whole tile;
/// see [`super::measure`].
pub fn setting_row<R>(
    ui: &mut Ui,
    theme: &Theme,
    row: Row<'_>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let avail = ui.available_width();
    // Control first, since it is the column that shrinks.
    let control_w = measure::control_width(avail, row.control_w);
    let prose_w = measure::prose_width(avail, control_w);
    let painter = ui.painter().clone();

    // Single line with ellipsis, so tile heights stay consistent.
    let label = truncated(
        &painter,
        row.label,
        theme::font(theme::SIZE_BODY, Weight::Regular),
        theme.text,
        prose_w,
    );
    let help = painter.layout(
        row.help.to_owned(),
        theme::font(theme::SIZE_CAPTION, Weight::Regular),
        theme.dim,
        prose_w,
    );
    let caveat = row.caveat.map(|text| {
        painter.layout(
            text.to_owned(),
            theme::font(
                theme::SIZE_CAPTION,
                theme.weight(crate::view::Emphasis::Body),
            ),
            theme.tone(Tone::Warn),
            prose_w,
        )
    });

    let mut prose_h = label.rect.height() + measure::LABEL_GAP + help.rect.height();
    if let Some(caveat) = &caveat {
        prose_h += measure::LABEL_GAP + caveat.rect.height();
    }

    let control_h = SWITCH_H.max(theme::CONTROL_H);
    let height = measure::tile_height(prose_h, control_h);
    let (tile, response) = ui.allocate_exact_size(vec2(avail, height), Sense::hover());

    painter.rect_filled(tile, theme::radius(theme::RADIUS_MEDIUM), theme.card);

    let mut at = measure::prose_origin(tile);
    painter.galley(at, label.clone(), theme.text);
    at.y += label.rect.height() + measure::LABEL_GAP;
    // Advance by the help's height (read before the galley is moved), not
    // the caveat's, or the caveat overlaps multi-line help.
    let help_h = help.rect.height();
    painter.galley(at, help, theme.dim);
    if let Some(caveat) = caveat {
        at.y += help_h + measure::LABEL_GAP;
        painter.galley(at, caveat, theme.tone(Tone::Warn));
    }

    // The row is one accessible node: label, help, and caveat together.
    // The caveat is joined with a space because the help already ends in a
    // full stop.
    let spoken = match row.caveat {
        Some(caveat) => format!("{}. {} {caveat}", row.label, row.help),
        None => format!("{}. {}", row.label, row.help),
    };
    let enabled = row.enabled;
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, enabled, &spoken));

    let slot = measure::control_slot(tile, control_w, control_h);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(slot)
            .layout(Layout::right_to_left(Align::Center)),
    );
    // `ComboBox::width` is only a minimum, so a long option would grow past
    // the slot into the prose. Truncating on the child `Ui` applies to every
    // control built in it.
    child.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    if !row.enabled {
        child.disable();
    }
    control(&mut child)
}

/// Describes one list entry (a drive or an alias).
pub struct Entry<'a> {
    /// A drive name or alias name.
    pub name: &'a str,
    /// A path or job code. Elided from the left, since a path's tail is what
    /// distinguishes it.
    pub detail: &'a str,
    /// A drive kind, or an alias note.
    pub note: Option<&'a str>,
    /// A warning drawn under the row in the warning tone.
    pub caveat: Option<&'a str>,
    /// Width reserved for a leading control (the drive checkbox); zero if
    /// none.
    pub lead_w: f32,
    /// Width reserved for trailing controls.
    pub control_w: f32,
}

/// Fixed width of the name column; names are short.
const NAME_W: f32 = 84.0;

/// Minimum widths of the flexible detail and note columns.
const MIN_DETAIL_W: f32 = 120.0;
const MIN_NOTE_W: f32 = 60.0;

/// Gap between list row columns.
const CELL_GAP: f32 = 8.0;

/// Draws one list row: optional leading control, entry columns, and trailing
/// controls. Columns are sized with [`measure::row_cells`]; detail and note
/// flex, the rest are fixed.
pub fn list_row<R>(
    ui: &mut Ui,
    theme: &Theme,
    entry: Entry<'_>,
    lead: impl FnOnce(&mut Ui),
    trailing: impl FnOnce(&mut Ui) -> R,
) -> R {
    let avail = ui.available_width();
    let inner = (avail - measure::CARD_PAD * 2.0).max(0.0);

    // The note column is dropped first when space runs out.
    let with_note = entry.note.is_some();
    let cells = |note: bool| {
        let mut v = vec![
            measure::Cell::Fixed(entry.lead_w),
            measure::Cell::Fixed(NAME_W),
            measure::Cell::Flex(MIN_DETAIL_W),
        ];
        if note {
            v.push(measure::Cell::Flex(MIN_NOTE_W));
        }
        v.push(measure::Cell::Fixed(entry.control_w));
        v
    };
    let (shown_note, widths) = match measure::row_cells(inner, CELL_GAP, &cells(with_note)) {
        Some(w) if with_note => (true, w),
        Some(w) => (false, w),
        None => match measure::row_cells(inner, CELL_GAP, &cells(false)) {
            Some(w) => (false, w),
            // Below the minimums, which only happens under the window's
            // `MIN_SIZE`; use each column's minimum.
            None => (
                false,
                cells(false).iter().map(|c| c.least()).collect::<Vec<_>>(),
            ),
        },
    };

    let painter = ui.painter().clone();
    let name_font = theme::font(theme::SIZE_BODY, Weight::Semibold);
    let detail_font = theme::font(theme::SIZE_BODY, Weight::Regular);
    let note_font = theme::font(theme::SIZE_CAPTION, Weight::Regular);

    let name = truncated(&painter, entry.name, name_font, theme.text, widths[1]);
    let detail = crate::gui::text::elide_left(entry.detail, widths[2], &|text| {
        painter
            .layout_no_wrap(text.to_owned(), detail_font.clone(), theme.text)
            .rect
            .width()
    });
    let detail = truncated(&painter, &detail, detail_font, theme.text, widths[2]);
    let note = (shown_note && entry.note.is_some()).then(|| {
        truncated(
            &painter,
            entry.note.unwrap_or_default(),
            note_font.clone(),
            theme.dim,
            widths[3],
        )
    });
    let caveat = entry.caveat.map(|text| {
        painter.layout(
            text.to_owned(),
            note_font.clone(),
            theme.tone(Tone::Warn),
            (inner - entry.lead_w - CELL_GAP).max(MIN_DETAIL_W),
        )
    });

    let line_h = detail.rect.height().max(name.rect.height());
    let mut prose_h = line_h;
    if let Some(caveat) = &caveat {
        prose_h += measure::LABEL_GAP + caveat.rect.height();
    }
    let control_h = theme::CONTROL_H;
    let height = measure::tile_height(prose_h, control_h);
    let (tile, response) = ui.allocate_exact_size(vec2(avail, height), Sense::hover());
    painter.rect_filled(tile, theme::radius(theme::RADIUS_MEDIUM), theme.card);

    let top = measure::prose_origin(tile).y;
    let mut x = measure::prose_origin(tile).x;
    let slot = |x: f32, w: f32, h: f32| {
        Rect::from_min_size(pos2(x, tile.center().y - h / 2.0), vec2(w, h))
    };
    let lead_slot = slot(x, widths[0], control_h);
    x += widths[0] + if widths[0] > 0.0 { CELL_GAP } else { 0.0 };

    let text_y = if caveat.is_some() {
        top
    } else {
        tile.center().y - line_h / 2.0
    };
    // Centre each galley on the line, since the note uses a smaller font.
    let centred = |g: &std::sync::Arc<egui::Galley>| text_y + (line_h - g.rect.height()) / 2.0;
    painter.galley(pos2(x, centred(&name)), name, theme.text);
    x += widths[1] + CELL_GAP;
    painter.galley(pos2(x, centred(&detail)), detail, theme.text);
    x += widths[2] + CELL_GAP;
    if let Some(note) = note {
        painter.galley(pos2(x, centred(&note)), note, theme.dim);
        x += widths[3] + CELL_GAP;
    }
    let _ = x;

    if let Some(caveat) = caveat {
        let under = measure::prose_origin(tile).x
            + entry.lead_w
            + if entry.lead_w > 0.0 { CELL_GAP } else { 0.0 };
        painter.galley(
            pos2(under, text_y + line_h + measure::LABEL_GAP),
            caveat,
            theme.tone(Tone::Warn),
        );
    }

    // One accessible node for the whole row, like a setting row.
    let spoken = [
        Some(entry.name),
        Some(entry.detail),
        entry.note,
        entry.caveat,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" \u{b7} ");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, &spoken));

    if entry.lead_w > 0.0 {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(lead_slot)
                .layout(Layout::left_to_right(Align::Center)),
        );
        lead(&mut child);
    }

    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(measure::control_slot(tile, entry.control_w, control_h))
            .layout(Layout::right_to_left(Align::Center)),
    );
    child.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    trailing(&mut child)
}

/// Lays out `text` on one line, ending in an ellipsis if it overflows.
fn truncated(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('\u{2026}');
    painter.layout_job(job)
}

/// Draws a toggle switch. See the module note for its accessibility role.
pub fn switch(ui: &mut Ui, theme: &Theme, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(SWITCH_W, SWITCH_H), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }

    let value = *on;
    let enabled = ui.is_enabled();
    let label = label.to_owned();
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, value, &label));

    if !ui.is_rect_visible(rect) {
        return response;
    }

    // Animated here, unlike the panel (see `gui::anim`): in this ordinary
    // window a tween costs only a repaint.
    let t = ui.ctx().animate_bool_responsive(response.id, value);

    let visuals = ui.style().interact(&response);
    let radius = CornerRadius::same((SHT / 2.0) as u8);
    let track = blend(theme.control, theme.accent_fill, t);
    let knob = blend(theme.dim, theme.accent_fg, t);

    let painter = ui.painter();
    painter.rect_filled(rect, radius, gray(ui, track));
    // The outline fades out as the fill fades in.
    if t < 1.0 {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, fade(gray(ui, theme.stroke), 1.0 - t)),
            StrokeKind::Inside,
        );
    }
    let travel = rect.shrink(SWITCH_H / 2.0);
    let centre = pos2(
        egui::lerp(travel.left()..=travel.right(), t),
        rect.center().y,
    );
    painter.circle_filled(centre, SWITCH_H / 2.0 - 3.0, gray(ui, knob));

    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same((SWITCH_H / 2.0 + 2.0) as u8),
            Stroke::new(2.0, visuals.bg_stroke.color),
            StrokeKind::Outside,
        );
    }

    response
}

/// Alias of `SWITCH_H` used for the track's corner radius.
const SHT: f32 = SWITCH_H;

/// Linearly mixes two colours; `t = 0` gives `from`, `t = 1` gives `to`.
fn blend(from: Color32, to: Color32, t: f32) -> Color32 {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
        mix(from.a(), to.a()),
    )
}

fn fade(colour: Color32, alpha: f32) -> Color32 {
    theme::faded(colour, alpha)
}

/// Greys out `colour` when the `Ui` is disabled, keeping the value readable.
fn gray(ui: &Ui, colour: Color32) -> Color32 {
    if ui.is_enabled() {
        colour
    } else {
        ui.visuals().gray_out(colour)
    }
}

/// Draws one nav entry. The selection bar uses `theme::MARKER_W` to match the
/// panel's selected result row.
pub fn nav_item(
    ui: &mut Ui,
    theme: &Theme,
    icon: Icon,
    label: &str,
    selected: bool,
    icons: bool,
) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, NAV_H), Sense::click());
    let name = label.to_owned();
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, &name));

    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter();
    let radius = theme::radius(theme::RADIUS_MEDIUM);
    if selected {
        painter.rect_filled(rect, radius, theme.selection);
    } else if response.hovered() {
        painter.rect_filled(rect, radius, theme.hover);
    }

    if selected {
        let bar = Rect::from_center_size(
            pos2(rect.left() + 2.0 + theme::MARKER_W / 2.0, rect.center().y),
            vec2(theme::MARKER_W, NAV_H * 0.5),
        );
        // `accent` (`BrandForeground1`) rather than `accent_fill`: in the
        // dark theme `CompoundBrandBackground` is 2.5:1 on a tile, under
        // WCAG's 3:1 for boundaries. Matches the panel's selected-row bar.
        painter.rect_filled(bar, theme::radius(theme::RADIUS_LARGE), theme.accent);
    }

    // Without the icon font the gutter collapses. See `theme::icons`.
    let text_x = if icons {
        let colour = if selected { theme.strong } else { theme.dim };
        painter.text(
            pos2(rect.left() + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            icon.text(),
            theme::icon_font(theme::ICON_NAV),
            colour,
        );
        rect.left() + 42.0
    } else {
        rect.left() + 14.0
    };

    // Selection is marked by weight as well as colour; see `theme::weight`.
    let weight = if selected {
        Weight::Semibold
    } else {
        Weight::Regular
    };
    painter.text(
        pos2(text_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        theme::font(theme::SIZE_BODY, weight),
        if selected { theme.strong } else { theme.text },
    );

    response
}

/// Draws a frameless icon button. The tooltip is required because it is also
/// the accessible name.
pub fn subtle_icon_button(ui: &mut Ui, theme: &Theme, icon: Icon, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ICON_BTN, ICON_BTN), Sense::click());
    let name = tooltip.to_owned();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), &name));

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.hover);
        }
        let colour = if response.hovered() {
            theme.strong
        } else {
            theme.dim
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon.text(),
            theme::icon_font(theme::ICON_INLINE),
            gray(ui, colour),
        );
    }

    response.on_hover_text(tooltip)
}

/// Draws an accent-filled primary button.
pub fn primary_button(ui: &mut Ui, theme: &Theme, text: &str) -> Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .font(theme::font(theme::SIZE_BODY, Weight::Regular))
                .color(theme.accent_fg),
        )
        .fill(theme.accent_fill)
        .corner_radius(theme::radius(theme::RADIUS_MEDIUM))
        .min_size(vec2(0.0, theme::CONTROL_H)),
    )
}

/// Draws a standard button.
pub fn button(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(text).min_size(vec2(0.0, theme::CONTROL_H)))
}

/// Draws a status message on a card, prefixed by the tone's glyph so colour
/// is not the only signal.
pub fn message_bar(ui: &mut Ui, theme: &Theme, tone: Tone, text: &str) {
    card(ui, theme, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(theme.glyph(tone).to_string())
                    .font(theme::font(theme::SIZE_CAPTION, Weight::Semibold))
                    .color(theme.tone(tone)),
            );
            ui.label(
                egui::RichText::new(text)
                    .font(theme::font(
                        theme::SIZE_CAPTION,
                        theme.weight(crate::view::Emphasis::Tone(tone)),
                    ))
                    .color(theme.tone(tone)),
            );
        });
    });
}

/// Draws the `doctor` report as a read-only, monospaced, selectable box.
///
/// Uses a non-interactive `TextEdit` so the text can be mouse-selected. While
/// waiting, shows a spinner instead of an empty box.
pub fn report_box(ui: &mut Ui, theme: &Theme, view: report::View<'_>) {
    let report = match view {
        report::View::Waiting => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Running diagnostics\u{2026}")
                        .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                        .color(theme.dim),
                );
            });
            return;
        }
        report::View::Stale(text) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Refreshing diagnostics\u{2026}")
                        .font(theme::font(theme::SIZE_CAPTION, Weight::Regular))
                        .color(theme.dim),
                );
            });
            ui.add_space(6.0);
            text
        }
        report::View::Ready(text) => text,
    };
    let mut text = report.to_owned();
    ui.add(
        egui::TextEdit::multiline(&mut text)
            .interactive(false)
            .desired_width(f32::INFINITY)
            .desired_rows(16)
            .font(FontId::monospace(theme::SIZE_CAPTION))
            .text_color(theme.text),
    );
}

/// Draws a read-only label and value on a card.
pub fn fact(ui: &mut Ui, theme: &Theme, label: &str, value: &str, colour: Color32) {
    let label_text = label.to_owned();
    let value_text = value.to_owned();
    let avail = ui.available_width();
    let painter = ui.painter().clone();
    let prose_w = measure::prose_width(avail, 0.0);

    let label = truncated(
        &painter,
        label,
        theme::font(theme::SIZE_BODY, Weight::Regular),
        theme.text,
        prose_w,
    );
    let value = painter.layout(
        value.to_owned(),
        theme::font(theme::SIZE_CAPTION, Weight::Regular),
        colour,
        prose_w,
    );

    let prose_h = label.rect.height() + measure::LABEL_GAP + value.rect.height();
    let height = measure::tile_height(prose_h, 0.0);
    let (tile, response) = ui.allocate_exact_size(vec2(avail, height), Sense::hover());

    // Painted text is not in the accessibility tree, so expose the pair as
    // one labelled node.
    let spoken = format!("{label_text}. {value_text}");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &spoken));

    painter.rect_filled(tile, theme::radius(theme::RADIUS_MEDIUM), theme.card);
    let mut at = measure::prose_origin(tile);
    painter.galley(at, label.clone(), theme.text);
    at.y += label.rect.height() + measure::LABEL_GAP;
    painter.galley(at, value, colour);
}

/// Adds the vertical gap between setting groups.
pub fn group_gap(ui: &mut Ui) {
    ui.add_space(theme::GROUP_GAP);
}

/// Draws a drop-down of fixed `width`; returns the newly picked index, if any.
pub fn dropdown(
    ui: &mut Ui,
    salt: &str,
    current: usize,
    options: &[&str],
    width: f32,
) -> Option<usize> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(WidgetText::from(
            options.get(current).copied().unwrap_or(""),
        ))
        .width(width)
        .show_ui(ui, |ui| {
            for (i, option) in options.iter().enumerate() {
                if ui.selectable_label(i == current, *option).clicked() && i != current {
                    picked = Some(i);
                }
            }
        });
    picked
}

/// The result of [`text_field`].
///
/// A value commits when the box loses focus via Enter or click-away, but not
/// via Escape, which reverts.
pub struct Typed {
    pub response: Response,
    /// Whether the value should be saved now.
    pub commit: bool,
    /// Whether the trailing button was pressed.
    pub trailing: bool,
}

pub fn text_field(
    ui: &mut Ui,
    theme: &Theme,
    buffer: &mut String,
    hint: &str,
    width: f32,
    trailing: Option<(Icon, &str)>,
) -> Typed {
    let mut pressed = false;
    let field_w = match trailing {
        Some(_) => width - ICON_BTN - 4.0,
        None => width,
    };

    let response = ui.add(
        egui::TextEdit::singleline(buffer)
            .desired_width(field_w)
            .font(theme::font(theme::SIZE_BODY, Weight::Regular))
            .text_color(theme.input)
            .hint_text(
                egui::RichText::new(hint)
                    .font(theme::font(theme::SIZE_BODY, Weight::Regular))
                    .color(theme.caption),
            ),
    );

    theme::focus_rule(ui.painter(), theme, response.rect, response.has_focus());

    if let Some((icon, tooltip)) = trailing {
        pressed = subtle_icon_button(ui, theme, icon, tooltip).clicked();
    }

    // egui drops focus on an unconsumed Escape, so losing focus on the same
    // frame as Escape means abandoned. Checked here, before the key is gone.
    let abandoned = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));

    Typed {
        commit: response.lost_focus() && !abandoned,
        trailing: pressed,
        response,
    }
}

/// Control widths, shared by [`Row::control_w`] and the widgets themselves.
pub mod width {
    pub const SWITCH: f32 = super::SWITCH_W;
    pub const DROPDOWN: f32 = 150.0;
    pub const TEXT: f32 = 240.0;
    /// A text box with a small button inside its right edge.
    pub const TEXT_WITH_BUTTON: f32 = TEXT + super::ICON_BTN + 4.0;
}

/// Shows a modal confirmation for an irreversible action (used only for
/// removing a drive).
///
/// Returns `Some(true)` to proceed, `Some(false)` to cancel, and `None` while
/// open. Escape and a backdrop click cancel; `egui::Modal` consumes Escape so
/// it does not reach the window behind.
pub fn confirm(
    ctx: &egui::Context,
    theme: &Theme,
    id: &str,
    question: &str,
    go: &str,
    cancel: &str,
) -> Option<bool> {
    let response = egui::Modal::new(egui::Id::new(id))
        .frame(
            egui::Frame::new()
                .fill(theme.card)
                .corner_radius(theme::radius(theme::RADIUS_MEDIUM))
                .inner_margin(20.0),
        )
        .backdrop_color(theme::tint(0x000000, 0x80))
        .show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.label(
                egui::RichText::new(question)
                    .font(theme::font(theme::SIZE_BODY, Weight::Regular))
                    .color(theme.text),
            );
            ui.add_space(16.0);
            // Buttons right-aligned. `Sides` works here because both halves
            // are one line tall (see `super::measure`).
            egui::Sides::new()
                .show(
                    ui,
                    |_| {},
                    |ui| {
                        // Laid out right to left, so the primary is rightmost.
                        let went = primary_button(ui, theme, go).clicked();
                        let backed = button(ui, cancel).clicked();
                        (went, backed)
                    },
                )
                .1
        });

    let (went, backed) = response.inner;
    if went {
        Some(true)
    } else if backed || response.should_close() {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_is_as_wide_as_a_row_is_told_it_is() {
        assert_eq!(width::SWITCH, SWITCH_W);
    }

    #[test]
    fn a_box_with_a_button_in_it_asks_for_room_for_the_button() {
        assert_eq!(width::TEXT_WITH_BUTTON, width::TEXT + ICON_BTN + 4.0);
    }

    #[test]
    fn a_switch_is_twice_as_wide_as_it_is_tall() {
        assert_eq!(SWITCH_W, SWITCH_H * 2.0);
    }

    #[test]
    fn a_blend_at_either_end_is_one_of_the_two_colours() {
        let a = Color32::from_rgb(0x10, 0x20, 0x30);
        let b = Color32::from_rgb(0xF0, 0xE0, 0xD0);
        assert_eq!(blend(a, b, 0.0), a);
        assert_eq!(blend(a, b, 1.0), b);
    }

    #[test]
    fn a_blend_in_the_middle_is_between_them() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(100, 100, 100);
        assert_eq!(blend(a, b, 0.5), Color32::from_rgb(50, 50, 50));
    }
}
