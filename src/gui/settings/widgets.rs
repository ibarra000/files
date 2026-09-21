//! The pieces a settings page is built from.
//!
//! Each one takes a `Ui` and a [`Theme`] and returns a `Response`, so they
//! compose the way egui's own widgets do. Some are egui widgets wearing the
//! style [`crate::gui::theme::apply_style`] set; the rest are painted here,
//! and each says which it is and why.
//!
//! # A switch, not a checkbox
//!
//! The one place this file spends real effort. egui has no switch, and a
//! checkbox is the wrong control for a setting that takes effect the moment
//! it moves: a tick box reads as a choice that will be applied later, which
//! is exactly the promise this window does not make. So the switch is
//! painted.
//!
//! What is *not* re-implemented is what it tells a screen reader. It reports
//! itself as a checkbox through [`egui::WidgetInfo::selected`], because that
//! is the role Narrator and the accessibility tree already understand, and
//! because `tests/panel.rs` reads that tree rather than the pixels.

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Rect, Response, Sense, Stroke, StrokeKind,
    Ui, UiBuilder, WidgetInfo, WidgetText, WidgetType, pos2, vec2,
};

use super::{measure, report};
use crate::gui::theme::{self, Icon, Theme, Weight};
use crate::view::status::Tone;

/// How tall a switch is, and how wide. Two-to-one, which is the proportion
/// every platform's switch has and the one that reads as a switch.
const SWITCH_H: f32 = 20.0;
const SWITCH_W: f32 = 40.0;

/// How tall one entry in the list of pages is.
const NAV_H: f32 = 32.0;

/// A small button with no frame until you touch it.
const ICON_BTN: f32 = 24.0;

/// The tile one setting is drawn on.
///
/// A stock `Frame`, flat: a fill one step off the page, a four-point corner,
/// no border and no shadow. See the module note in [`super`] for why this is
/// not `theme::raise`.
pub fn card<R>(ui: &mut Ui, theme: &Theme, body: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme.card)
        .corner_radius(theme::radius(theme::RADIUS_MEDIUM))
        .inner_margin(measure::CARD_PAD)
        .show(ui, body)
        .inner
}

/// The heading over a run of settings.
///
/// Smaller than the labels it heads, which looks wrong written down and is
/// right on screen: it is a signpost rather than a title, and the labels
/// under it are what somebody is actually reading for. Ueli's is 12 pt
/// semibold over 14 pt regular, and this is the same relationship in this
/// program's sizes.
pub fn group_heading(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::font(theme::SIZE_CAPTION, Weight::Semibold))
            .color(theme.dim),
    );
    ui.add_space(measure::LABEL_GAP);
}

/// What a setting row needs to know about itself.
///
/// A struct rather than six arguments, because four of them are strings and
/// a call site with four bare strings in it is a call site nobody can read.
pub struct Row<'a> {
    pub label: &'a str,
    /// One sentence on what the setting does.
    pub help: &'a str,
    /// Why it will not save, or when it will apply. Drawn under the help in
    /// the warning tone, because both of those are disappointments.
    pub caveat: Option<&'a str>,
    /// False when something outranks the file and the value cannot be
    /// written. The control is drawn disabled rather than hidden: hiding it
    /// would answer "why can I not change this?" with silence.
    pub enabled: bool,
    /// How much room the control would *like*. Declared rather than
    /// measured, because the prose is laid out against what is left and
    /// something has to be decided first. See [`super::measure`].
    ///
    /// A want and not a width: in a narrow window
    /// [`super::measure::control_width`] takes some of it back. What the
    /// control is actually given is the rectangle it is built into, and a
    /// control that ignores that rectangle - which is every egui widget with
    /// a `desired_width`, and `ComboBox` in particular - will draw straight
    /// through the prose beside it.
    pub control_w: f32,
}

/// One setting: a label, a sentence, and a control to the right of both.
///
/// Measured and then painted rather than laid out with a container, and
/// [`super::measure`]'s module note is the argument for that. The short
/// version: the control is centred against the whole of the prose, and no
/// egui container can express it.
pub fn setting_row<R>(
    ui: &mut Ui,
    theme: &Theme,
    row: Row<'_>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let avail = ui.available_width();
    // The control first, because it is the one that gives way. See
    // `measure::control_width` - this pair of lines is the overlap fix.
    let control_w = measure::control_width(avail, row.control_w);
    let prose_w = measure::prose_width(avail, control_w);
    let painter = ui.painter().clone();

    // One line, ellipsised. A label that wraps turns every tile into a
    // different height for no gain: the sentence under it is where the
    // detail is meant to go.
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
    // The help's own height, taken before the galley is handed over, and
    // *not* the caveat's. Advancing by the height of the line about to be
    // drawn rather than the one just drawn painted the caveat on top of the
    // second line of the help - which is legible in a picture and invisible
    // in a one-line description, so the snapshot is what caught it.
    let help_h = help.rect.height();
    painter.galley(at, help, theme.dim);
    if let Some(caveat) = caveat {
        at.y += help_h + measure::LABEL_GAP;
        painter.galley(at, caveat, theme.tone(Tone::Warn));
    }

    // The whole row reads as one thing to a screen reader, because that is
    // what it is: a label that says nothing without the sentence under it.
    // The same argument `gui::row` makes for a result.
    // The caveat is part of the name, not decoration. It is the line that
    // says the setting will not save or will not apply yet, and somebody
    // reading this window aloud needs it more than somebody looking at it,
    // not less.
    // Joined with a space rather than a stop: the help is a whole sentence
    // and already ends in one, so a second gives a screen reader "either
    // way.. Applies when files next starts".
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
    // The second half of the overlap fix, and the one that bites at *every*
    // window width rather than only at narrow ones.
    //
    // `ComboBox::width` is a minimum and not a width - egui says so in a
    // comment beside it - so a drop-down holding a long label grows past
    // whatever slot it was measured against and runs under the sentence to
    // its left. Truncating here sets the policy on the child `Ui` that every
    // control in this window is built into, so it holds for the drop-downs,
    // the buttons and anything added later, rather than needing a
    // `.truncate()` remembered at each call site.
    child.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    if !row.enabled {
        child.disable();
    }
    control(&mut child)
}

/// One entry in a list: a drive, or an alias.
///
/// The sibling of [`Row`], for the two blocks in this window that are not
/// settings. A setting has one value and one control; an entry has three or
/// four facts about a thing, side by side, and a button that takes it away.
pub struct Entry<'a> {
    /// A drive letter or an alias name. Short, Semibold, and the column the
    /// eye scans down.
    pub name: &'a str,
    /// What it stands for: a path, or a job code. Elided from the *left*,
    /// because the tail of a path is what tells two of them apart.
    pub detail: &'a str,
    /// A kind, or somebody's note about why the alias exists.
    pub note: Option<&'a str>,
    /// Why this entry will not do what it says. Drawn under the row in the
    /// warning tone - the same place and the same colour a setting puts its
    /// caveat, because it is the same kind of disappointment.
    pub caveat: Option<&'a str>,
    /// Room for a control before the name: the checkbox that turns a drive
    /// off. Zero where there is none.
    pub lead_w: f32,
    /// And room for the ones after the note.
    pub control_w: f32,
}

/// The name column, which does not grow.
///
/// A drive letter is two characters and the longest alias anybody has is
/// about eight, so a name column that took a share of a widening window
/// would be a column of air with a word at the front of it.
const NAME_W: f32 = 84.0;

/// The narrowest the two flexible columns may be squeezed to.
///
/// A path at 120 still shows a folder and a half after the ellipsis, which
/// is what distinguishes two rows; a note at 60 shows a word.
const MIN_DETAIL_W: f32 = 120.0;
const MIN_NOTE_W: f32 = 60.0;

/// Between one column of a list row and the next.
const CELL_GAP: f32 = 8.0;

/// One row of a list: the entry, a control before it, and controls after.
///
/// Measured with [`measure::row_cells`] rather than with a run of hard pixel
/// widths, which is what the two lists used to do - twelve of them between
/// them, none of which ever read `available_width`, which is why the window
/// was resizable and resizing it changed nothing. Two of the five columns
/// are flexible and the rest are what they are.
pub fn list_row<R>(
    ui: &mut Ui,
    theme: &Theme,
    entry: Entry<'_>,
    lead: impl FnOnce(&mut Ui),
    trailing: impl FnOnce(&mut Ui) -> R,
) -> R {
    let avail = ui.available_width();
    let inner = (avail - measure::CARD_PAD * 2.0).max(0.0);

    // The note is the column that goes first. A drive whose kind is not
    // shown is a drive missing a fact; a drive whose *path* is not shown is
    // a row about nothing.
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
            // Narrower than the minimums. Everything gets its least and the
            // row is as wide as it is - which at this point is a window
            // below `MIN_SIZE`, i.e. a window Windows will not make.
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

    // Left to right along the top line, whatever the tile ended up being
    // tall enough for.
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
    // Each galley centred on the line rather than hung from its top. The
    // note is set two points smaller than the name beside it, and three
    // galleys sharing one `y` puts the small one visibly high.
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

    // One thing to a screen reader, like a setting row: a drive letter on its
    // own says nothing and a path on its own says nothing about which drive.
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

/// A label laid out to one line, with an ellipsis where it ran out.
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

/// A boolean, as a switch.
///
/// Painted. See the module note for why it is not `ui.checkbox`, and for why
/// it nevertheless reports itself as one.
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

    // The panel has no motion in it, and `gui::anim` records why: the panel
    // *is* the window, so every tween frame was a `SetWindowPos` and a
    // swapchain reconfigure. None of that is true here - this is a
    // fixed-size ordinary window and a moving knob costs one repaint of a
    // static surface. A switch that teleports is the one control that reads
    // worse without it.
    let t = ui.ctx().animate_bool_responsive(response.id, value);

    let visuals = ui.style().interact(&response);
    let radius = CornerRadius::same((SHT / 2.0) as u8);
    let track = blend(theme.control, theme.accent_fill, t);
    let knob = blend(theme.dim, theme.accent_fg, t);

    let painter = ui.painter();
    painter.rect_filled(rect, radius, gray(ui, track));
    // Only while off. A filled track needs no outline, and fading the
    // outline out with the fill is what stops the two reading as separate
    // events.
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

/// `SWITCH_H` as an expression the corner radius can be built from without
/// tripping the const-evaluation rules. Named rather than inlined so the two
/// radii below cannot drift apart.
const SHT: f32 = SWITCH_H;

/// Mixes two colours. `t` of zero is the first, one is the second.
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

/// Greys a colour out where the control is disabled.
///
/// A pinned setting is drawn rather than hidden, so its value has to stay
/// readable while it is refused. egui already knows how far to fade.
fn gray(ui: &Ui, colour: Color32) -> Color32 {
    if ui.is_enabled() {
        colour
    } else {
        ui.visuals().gray_out(colour)
    }
}

/// One entry in the list of pages.
///
/// Painted, for the indicator bar. The bar is the same three points
/// `theme::MARKER_W` gives the selected result row, and that is not a
/// coincidence worth losing: the panel and this window should say "this one"
/// the same way.
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
        // `accent`, not `accent_fill`. A bar is read against the ground it
        // sits on rather than against text drawn on top of it, and in the
        // dark theme `CompoundBrandBackground` is 2.5:1 on a tile - under the
        // 3:1 WCAG asks of a boundary. `BrandForeground1` clears it in both.
        // The same value the panel's selected-row bar uses, which is the
        // point: they are the same mark.
        painter.rect_filled(bar, theme::radius(theme::RADIUS_LARGE), theme.accent);
    }

    // The gutter closes up entirely where there is no icon font, rather than
    // standing empty. See `theme::icons`.
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

    // Weight as well as colour. The palette is greys, and `theme::weight`
    // records that brightness alone carries about three steps before two of
    // them stop being two.
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

/// A button with no frame until it is touched, holding one mark.
///
/// The tooltip is also the accessible name, because a button with no text has
/// nothing else to offer a screen reader - so it is never optional.
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

/// A button that is the obvious thing to press.
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

/// And one that is not.
pub fn button(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(text).min_size(vec2(0.0, theme::CONTROL_H)))
}

/// A line that says something went wrong, or is about to.
///
/// A tinted band is what Fluent would draw here and it is refused on the
/// palette's own rule: colour is never the only carrier. So this is an
/// ordinary tile with the tone's glyph in front of the words, which is the
/// same pair the status line has used since the terminal build.
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

/// The `doctor` report: read-only, monospaced and selectable.
///
/// A `TextEdit` rather than a `Label` because the point of this box is to end
/// up in an email, and a label cannot be selected with the mouse. Not
/// interactive, so it takes no focus and answers no keystroke.
///
/// Takes a [`report::View`] rather than a string, because there are three
/// things to draw and only one of them is a box. A reading that has not
/// arrived is a spinner and no box at all - an empty box is a report that
/// found nothing, which is the opposite of what is true.
pub fn report_box(ui: &mut Ui, theme: &Theme, view: report::View<'_>) {
    let report = match view {
        report::View::Waiting => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Taking a reading\u{2026}")
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
                    egui::RichText::new("Taking a fresh reading\u{2026}")
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

/// A label that is read and not changed.
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

    // Painted text is not in the accessibility tree by itself - only a
    // widget is - so without this a version number, a path and a status
    // line are on screen and unreadable to anything that is not a pair of
    // eyes. The whole pair reads as one thing, the way a setting row does.
    let spoken = format!("{label_text}. {value_text}");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &spoken));

    painter.rect_filled(tile, theme::radius(theme::RADIUS_MEDIUM), theme.card);
    let mut at = measure::prose_origin(tile);
    painter.galley(at, label.clone(), theme.text);
    at.y += label.rect.height() + measure::LABEL_GAP;
    painter.galley(at, value, colour);
}

/// Room between one group of settings and the next.
pub fn group_gap(ui: &mut Ui) {
    ui.add_space(theme::GROUP_GAP);
}

/// A drop-down, sized to its widest option so the column does not step in and
/// out as the value changes.
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

/// A single-line box, with an optional small button inside its right edge.
///
/// Committed when the box gives up the keyboard, which is both Enter and
/// clicking away - and *not* on Escape, which reverts. That distinction is
/// the point: Escape is how somebody abandons a half-typed path, and a box
/// that saved it anyway would be a box nobody could back out of.
pub struct Typed {
    pub response: Response,
    /// Whether the value should be written now.
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

    // egui drops widget focus on an un-consumed Escape, so a box that lost
    // focus on the same frame Escape was pressed was abandoned rather than
    // finished. Asked here, where the box is, because by the time the window
    // reads the key the distinction is gone.
    let abandoned = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));

    Typed {
        commit: response.lost_focus() && !abandoned,
        trailing: pressed,
        response,
    }
}

/// How wide a control of each kind is, so [`Row::control_w`] and the widget
/// that fills it cannot disagree.
pub mod width {
    pub const SWITCH: f32 = super::SWITCH_W;
    pub const DROPDOWN: f32 = 150.0;
    pub const TEXT: f32 = 240.0;
    /// A text box with a small button inside its right edge.
    pub const TEXT_WITH_BUTTON: f32 = TEXT + super::ICON_BTN + 4.0;
}

/// Asks before doing something that cannot be undone.
///
/// The only one in this window, and deliberately: a form that confirms
/// everything trains people to dismiss the question without reading it.
/// Removing a drive earns it because the path is the part nobody remembers,
/// and because the configuration file loses its comments around the drive
/// list when it is rewritten.
///
/// Returns `Some(true)` when the user went ahead, `Some(false)` when they
/// backed out, and `None` while the question is still up. Escape and a click
/// on the backdrop both count as backing out - `egui::Modal` consumes the key
/// itself, which is also what stops it reaching the window behind and
/// closing that instead.
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
            // The two buttons on the right, which is where a dialog puts
            // them and the one place `Sides` is exactly the right tool: both
            // halves are one line tall, so the objection in
            // `super::measure` does not apply.
            egui::Sides::new()
                .show(
                    ui,
                    |_| {},
                    |ui| {
                        // Right to left, so the primary ends up rightmost.
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

    /// A control's declared width and the widget that fills it have to agree,
    /// or the prose column is measured against one number and the control
    /// drawn at another - which overflows the tile by the difference.
    #[test]
    fn a_switch_is_as_wide_as_a_row_is_told_it_is() {
        assert_eq!(width::SWITCH, SWITCH_W);
    }

    /// The trailing button has to be paid for out of the declared width
    /// rather than added to it.
    #[test]
    fn a_box_with_a_button_in_it_asks_for_room_for_the_button() {
        assert_eq!(width::TEXT_WITH_BUTTON, width::TEXT + ICON_BTN + 4.0);
    }

    /// A switch is a switch and not a square.
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
