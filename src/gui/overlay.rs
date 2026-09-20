//! The panel.
//!
//! Three bands that never move: the field you type in, the body, and a footer
//! that says what the program knows and which keys do what. Everything the
//! panel *says* comes from [`crate::view`]; everything about how it *moves*
//! comes from [`crate::gui::anim`]. What is left here is arrangement.
//!
//! # One list, one field, one way in
//!
//! The terminal build had a five-way focus, because a terminal has one pane and
//! everything had to take turns in it. A window does not, so:
//!
//! * the text field always has the keyboard, and Left/Right always move the
//!   caret;
//! * Up/Down always move the selection, and Enter always takes the row it is
//!   on.
//!
//! The recent codes are the one thing that is still a mode, deliberately. They
//! used to be what an empty field showed, which meant every summon of an empty
//! panel opened onto a list of somebody's job codes with nobody having asked
//! for it. The Up arrow is the only way in now, Escape is the way out, and
//! [`body_of`] reads `history.is_browsing()` rather than deciding for itself -
//! it and `showing_recent` used to be two answers to that question and they
//! disagreed.
//!
//! The list therefore holds two kinds of row, and Enter means the obvious thing
//! for each: on a remembered code it fills the field, on a file it opens it.
//! That is one rule - *Enter takes the row you are on* - and the two kinds look
//! different enough that nobody has to be told.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align2, Color32, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use std::time::{Instant, SystemTime};

use crate::app::state::pointer::Intent;
use crate::app::state::{AppState, EmptyReason};
use crate::gui::anim::{Content, Visual};
use crate::gui::theme::{self, Theme, Weight};
use crate::gui::{row, window::Backdrop};
use crate::view::{self, Emphasis, Run};

/// How tall one line of the empty state or the help list is.
const LINE_H: f32 = 24.0;

/// The gap between two hint chips.
const CHIP_GAP: f32 = 8.0;

/// How much room the caret keeps between itself and the right-hand edge of the
/// field once the code is long enough to scroll.
const CARET_MARGIN: f32 = 12.0;

/// What the panel will be, before it is drawn.
///
/// Measured separately from drawing because the animator has to be told the
/// target *before* it is asked what this frame looks like.
///
/// It used to carry a height and a width too, which is what the window was
/// resized to. The window is a constant six hundred by four hundred now, so
/// what is left is which body belongs on screen and where the highlight goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    pub content: Content,
    pub rows: usize,
    /// Top of the selected row, in points from the top of the list.
    pub selection_y: Option<f32>,
}

/// Which body belongs on screen.
pub fn measure(state: &AppState) -> Measured {
    let content = body_of(state);
    let rows = match content {
        Content::Shares => state.share_ids().len(),
        Content::Recent => state.recent_rows().len(),
        Content::Results => state.hits.len(),
        Content::Empty => empty_lines(state),
        Content::Quiet => 0,
    };

    // What fits between the field and the footer. `gui::theme` asserts that
    // it does.
    let shown = rows.min(theme::MAX_ROWS);

    // Relative to the window, not to the list. This used to be
    // `rank.min(shown - 1)`, which pinned the highlight to the last visible row
    // for every rank past it - so with three hundred results the arrows walked
    // the whole list while the panel drew the same eight rows and the same
    // frozen band, and Enter opened a file that was not on screen.
    let selection_y = match content {
        Content::Results => state
            .selected_row()
            .map(|rank| rank.saturating_sub(state.scroll_top()) as f32 * theme::ROW_H),
        // The recall cursor is the same kind of thing in a different list. It
        // used to have no band at all, so stepping through codes changed the
        // field with nothing on screen to say which row it had come from - and
        // past the twelfth entry nothing moved at all.
        Content::Recent => {
            let window = state.recent_rows();
            state
                .history
                .cursor()
                .map(|c| c.saturating_sub(window.start) as f32 * theme::ROW_H)
        }
        Content::Empty | Content::Quiet | Content::Shares => None,
    };

    Measured {
        content,
        rows: shown,
        selection_y,
    }
}

/// Which of the four bodies the state calls for.
fn body_of(state: &AppState) -> Content {
    if state.picking_share {
        return Content::Shares;
    }
    // Asked before anything looks at the field, because browsing puts a code
    // *in* the field: a rule that looked at emptiness first would drop the list
    // the moment it was stepped onto.
    //
    // Through `showing_recent` rather than by re-deriving it. This function and
    // that one used to be two independent answers to one question, and they
    // already disagreed - which is how the list came to be drawn from an empty
    // field nobody had pressed Up on.
    if state.showing_recent() {
        return Content::Recent;
    }
    // Nothing typed and nothing to say about it. See `AppState::is_quiet`,
    // which owns the question because the footer's height depends on the same
    // answer its contents do.
    if state.is_quiet() {
        return Content::Quiet;
    }
    if state.input.text().is_empty() {
        return Content::Empty;
    }
    if state.hits.is_empty() {
        Content::Empty
    } else {
        Content::Results
    }
}

/// The reason there is nothing to show, defaulting to the one the first screen
/// after an install has.
fn empty_reason(state: &AppState) -> EmptyReason {
    state.empty_reason.clone().unwrap_or(EmptyReason::NoQuery)
}

fn empty_lines(state: &AppState) -> usize {
    view::empty::view(&empty_reason(state), state.input.text()).len()
}

/// Draws the whole panel and reports everything the pointer did to it.
///
/// A `Vec` rather than an `Option`: one frame can carry both a hover and a
/// click, and dropping either would mean a row that lights up only after it has
/// been clicked.
pub fn show(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    visual: &Visual,
    backdrop: Option<Backdrop>,
    now: Instant,
    wall: SystemTime,
) -> Vec<Intent> {
    let mut intents = Vec::new();
    let rect = panel_rect(ui);

    paint_surface(ui, theme, rect, backdrop);

    let mut cursor = rect.shrink2(vec2(0.0, theme::PAD_Y));
    let field = take(&mut cursor, theme::FIELD_H);
    intents.extend(draw_field(ui, state, theme, field));

    // A quiet panel has no footer at all - not an empty one. `measure` gave it
    // no height, so taking the band anyway would take it out of the body's
    // rectangle and draw the rule across the bottom of the field.
    //
    // Safe to skip only because `AppState::is_quiet` has already established
    // there is nothing to put in it: no toast, and nothing standing about a
    // drive. Drawing a message into a band of no height is the one way this
    // could lose something, and the predicate exists to make that impossible
    // rather than unlikely.
    if visual.content != Content::Quiet {
        let footer = take_bottom(&mut cursor, theme::FOOTER_H);
        rule(ui, theme, footer.top(), rect);
        intents.extend(draw_footer(ui, state, theme, footer, now, wall));
    }

    // One pass. The body used to be drawn twice while one cross-faded into the
    // other - which is why everything below took an alpha, and why the outgoing
    // pass had to be told not to accept clicks.
    intents.extend(draw_body(
        ui,
        state,
        theme,
        cursor,
        visual.content,
        visual,
        wall,
    ));

    intents
}

/// Tells the accessibility tree that `text` is at `rect`.
///
/// egui builds that tree out of *widgets*, and this panel is painted rather
/// than built out of them - every string on it goes onto the screen through
/// `Painter::text` or `Painter::galley`, neither of which the toolkit knows
/// anything about. So the panel had no accessibility tree at all: an empty
/// rectangle to Narrator, to a magnifier, and to anything else that reads a
/// window rather than looks at it.
///
/// That is worth fixing on its own account, and it is also the reason
/// `accesskit` is a feature of `eframe` in `Cargo.toml`, where the note says it
/// "is what makes the panel readable by Narrator and by a magnifier - something
/// a grid of characters never could be". It was paid for and not delivered.
///
/// `Sense::hover()` and a namespaced id, so this never takes a click away from
/// the rows and chips that do want one. A row that is already a widget says so
/// through its own [`egui::Response`] instead of coming here.
fn announce(ui: &Ui, rect: Rect, what: impl std::hash::Hash + std::fmt::Debug, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let response = ui.interact(rect, Id::new(("files-a11y", what)), Sense::hover());
    let text = text.to_owned();
    response.widget_info(|| {
        eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Label, true, &text)
    });
}

/// Where the panel goes inside the window it was given.
///
/// The top of it, `visual.height` tall - not the whole window. The shell sizes
/// the window to where the height is *heading* while the panel is up, so that
/// a list narrowing from eight rows to three is one `SetWindowPos` rather than
/// one per frame; the panel easing down inside it is what makes that look like
/// a transition rather than a jump. See [`crate::gui::frame::Frame::resize`].
///
/// The whole window, always.
///
/// This used to clamp a measured height against the window's, because the
/// two disagreed for one frame whenever the panel resized: a viewport command
/// is a round trip and the frame that asked for a taller window is drawn
/// before it arrives. The window is a constant six hundred by four hundred
/// now, so the panel is simply all of it and there is nothing left to
/// arbitrate.
fn panel_rect(ui: &Ui) -> Rect {
    ui.max_rect()
}

/// The panel's own background.
///
/// Skipped entirely when the compositor granted acrylic: painting a fill behind
/// a backdrop is painting over it. On the fallback path this fill *is* the
/// depth, so it is drawn - and it fades with everything else, which acrylic
/// cannot do.
fn paint_surface(ui: &Ui, theme: &Theme, rect: Rect, backdrop: Option<Backdrop>) {
    let painter = ui.painter();
    let radius = theme::radius(theme::PANEL_RADIUS);
    if backdrop != Some(Backdrop::Compositor) {
        painter.rect_filled(rect, radius, theme.surface);
    }
    // The hairline stays on both paths: Windows draws its own border around a
    // rounded window, and without one of ours the panel's edge is whatever
    // happens to be behind it.
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, theme.edge),
        StrokeKind::Inside,
    );
}

fn take(cursor: &mut Rect, height: f32) -> Rect {
    let taken = Rect::from_min_size(
        cursor.min,
        vec2(cursor.width(), height.min(cursor.height())),
    );
    cursor.min.y = taken.max.y;
    taken
}

fn take_bottom(cursor: &mut Rect, height: f32) -> Rect {
    let height = height.min(cursor.height());
    let taken = Rect::from_min_size(
        pos2(cursor.left(), cursor.bottom() - height),
        vec2(cursor.width(), height),
    );
    cursor.max.y = taken.min.y;
    taken
}

fn rule(ui: &Ui, theme: &Theme, y: f32, rect: Rect) {
    ui.painter().hline(
        (rect.left() + theme::PAD_X)..=(rect.right() - theme::PAD_X),
        y,
        Stroke::new(1.0, theme.edge),
    );
}

// -- the field --------------------------------------------------------------

fn draw_field(ui: &mut Ui, state: &AppState, theme: &Theme, rect: Rect) -> Vec<Intent> {
    let painter = ui.painter().clone();
    let fade = |c: Color32| c;
    let font = theme::font(theme::SIZE_INPUT, Weight::Regular);

    // The field reads as pressed into the panel rather than drawn on it: a
    // trough is what a search box looks like in this idiom, and it is the one
    // element here somebody puts something *into*.
    let well = Rect::from_min_max(
        pos2(rect.left() + theme::PAD_X - 4.0, rect.top() + 4.0),
        pos2(rect.right() - theme::PAD_X + 4.0, rect.bottom() - 4.0),
    );
    theme::press(&painter, theme, well, theme::ROW_RADIUS);

    // A magnifier, which is what every search field on this operating system
    // has, so nobody has to be told what the box is for.
    painter.text(
        pos2(rect.left() + theme::PAD_X + 4.0, rect.center().y),
        Align2::LEFT_CENTER,
        "\u{1F50D}",
        theme::font(theme::SIZE_ROW, Weight::Regular),
        fade(theme.dim),
    );

    let text_left = rect.left() + theme::PAD_X + 34.0;
    let text = state.input.text();

    if text.is_empty() {
        // One word. This used to be the whole instruction - "Type a job code,
        // for example 11-D-0704" - on the argument that a placeholder saying
        // "Search" is decoration beside a magnifier that already says it. The
        // argument was sound and the result was still wrong: a box that
        // explains itself is a box somebody reads, and this one is summoned
        // dozens of times an hour by people who learned what it was for on the
        // first day.
        painter.text(
            pos2(text_left, rect.center().y),
            Align2::LEFT_CENTER,
            "Search",
            theme::font(theme::SIZE_ROW, Weight::Regular),
            fade(theme.dim),
        );
    }

    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), fade(theme.input));

    // What an alias stood for, at the right-hand end of the field.
    //
    // Here rather than on the status line because this is where the eye
    // already is, and because that line is a slot other things legitimately
    // claim - a toast, a drive that cannot be reached - which would leave an
    // alias firing with nothing on screen to say so. An alias that fires
    // silently is the program searching for something nobody typed. See
    // [`crate::alias`].
    let expansion = state
        .expansion()
        .map(|alias| format!("\u{2192} {}", alias.code));
    let expansion_galley = expansion.as_ref().map(|shown| {
        painter.layout_no_wrap(
            shown.clone(),
            theme::font(theme::SIZE_ROW, Weight::Regular),
            fade(theme.dim),
        )
    });
    // The code gives up the room the expansion takes, rather than running
    // under it: two overlapping strings in a search box is worse than a code
    // that scrolls slightly sooner.
    let reserved = expansion_galley
        .as_ref()
        .map(|g| g.rect.width() + theme::PAD_X)
        .unwrap_or(0.0);
    if let Some(shown) = expansion_galley {
        painter.galley(
            pos2(
                rect.right() - theme::PAD_X - shown.rect.width(),
                rect.center().y - shown.rect.height() / 2.0,
            ),
            shown,
            fade(theme.dim),
        );
    }

    // How much of the code there is room for.
    let window = Rect::from_min_max(
        pos2(text_left, rect.top()),
        pos2(rect.right() - theme::PAD_X - reserved, rect.bottom()),
    );

    // How far the code is scrolled under that window.
    //
    // Derived from where the caret is, never stored. A remembered offset is
    // how a text field ends up scrolled somewhere the caret is not - the same
    // rule the terminal build's `input_line` followed, and for the same
    // reason.
    //
    // Without any of this a pasted code simply kept going: `layout_no_wrap`
    // has no width to fit into, so thirty characters ran under the hint chips
    // and off the edge of the panel, taking the caret with them. There was no
    // way to see what you had pasted and no way to get back to it.
    let caret_at = x_of(&galley, state.input.caret());
    let overflow = (galley.rect.width() - window.width()).max(0.0);
    let offset = if overflow <= 0.0 {
        0.0
    } else {
        // Enough of a margin that the caret is never *on* the edge it is
        // keeping itself inside.
        (caret_at - window.width() + CARET_MARGIN).clamp(0.0, overflow)
    };
    let origin = text_left - offset;
    let baseline = pos2(origin, rect.center().y - galley.rect.height() / 2.0);

    // Everything that moves with the text is clipped to the window, so a code
    // too long for the field stops at the field's edge rather than at the
    // panel's.
    let painter = painter.with_clip_rect(window);

    // The run about to be copied, drawn under the text rather than over it.
    if let Some((from, to)) = state.input.selection() {
        let x0 = origin + x_of(&galley, from);
        let x1 = origin + x_of(&galley, to);
        painter.rect_filled(
            Rect::from_min_max(
                pos2(x0, baseline.y),
                pos2(x1, baseline.y + galley.rect.height()),
            ),
            theme::radius(theme::CHIP_RADIUS),
            fade(theme.text_selection),
        );
    }

    let caret_x = origin + caret_at;
    painter.galley(baseline, galley, fade(theme.input));

    // The field is the one thing on the panel somebody is *editing*, so it is
    // announced whether or not there is anything in it - a search box that
    // reads as empty is still a search box.
    //
    // The expansion goes in with it. A screen reader hearing `pw` and being
    // read a list of drawings for a code it never said is the same failure as
    // the silent one, reached by the ear instead of the eye.
    let spoken = match (&expansion, text.is_empty()) {
        (Some(shown), _) => format!("{text} {shown}"),
        (None, true) => "Search".to_string(),
        (None, false) => text.to_string(),
    };
    announce(ui, rect, "field", &spoken);

    // Solid rather than blinking. A caret that blinks is a repaint twice a
    // second for as long as the panel is up, and this one is never the only
    // thing on screen that says where the keyboard is going.
    painter.vline(
        caret_x,
        (rect.center().y - theme::SIZE_INPUT * 0.6)..=(rect.center().y + theme::SIZE_INPUT * 0.6),
        Stroke::new(2.0, fade(theme.caret)),
    );

    click_to_caret(ui, rect, origin, text)
}

/// Where a byte offset falls, in points from the start of the text.
fn x_of(galley: &eframe::egui::Galley, byte: usize) -> f32 {
    let chars = galley.text()[..byte.min(galley.text().len())]
        .chars()
        .count();
    galley
        .pos_from_cursor(eframe::egui::text::CCursor::new(chars))
        .min
        .x
}

/// Clicking in the field puts the caret where the pointer is, which is what
/// every other text box on this machine does.
fn click_to_caret(ui: &mut Ui, rect: Rect, text_left: f32, text: &str) -> Vec<Intent> {
    let response = ui.interact(rect, Id::new("files-field"), Sense::click_and_drag());
    let Some(pos) = response.interact_pointer_pos() else {
        return Vec::new();
    };
    if !response.clicked() && !response.dragged() {
        return Vec::new();
    }

    let font = theme::font(theme::SIZE_INPUT, Weight::Regular);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let cursor = galley.cursor_from_pos(vec2(pos.x - text_left, 0.0));
    // `cursor_from_pos` counts characters; `Input` indexes bytes, and a share
    // name is not always ASCII.
    let byte = text
        .char_indices()
        .nth(cursor.index.into())
        .map_or(text.len(), |(offset, _)| offset);

    vec![Intent::Caret {
        byte,
        // A drag extends what a press started, which is how a run gets
        // selected for copying.
        extend: response.dragged(),
    }]
}

// -- the body ---------------------------------------------------------------

fn draw_body(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    rect: Rect,
    content: Content,
    visual: &Visual,
    wall: SystemTime,
) -> Vec<Intent> {
    match content {
        Content::Results => draw_results(ui, state, theme, rect, visual),
        Content::Recent => draw_recent(ui, state, theme, rect),
        // Nothing, and no rectangle to put it in: `measure` gave this body no
        // height at all.
        Content::Quiet => Vec::new(),
        Content::Empty => {
            draw_blocks(
                ui,
                theme,
                rect,
                &view::empty::view(&empty_reason(state), state.input.text()),
            );
            Vec::new()
        }
        Content::Shares => {
            draw_shares(ui, state, theme, rect, wall);
            Vec::new()
        }
    }
}

fn draw_results(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    rect: Rect,
    visual: &Visual,
) -> Vec<Intent> {
    let mut intents = Vec::new();

    // The highlight, drawn before the rows so they sit on top of it. Taken
    // from the measurement rather than from any row's rectangle, because it is
    // the same number and one place to be wrong is better than two.
    if let Some(y) = visual.selection_y {
        let row_rect = Rect::from_min_size(
            pos2(rect.left() + theme::PAD_X, rect.top() + y),
            vec2(rect.width() - theme::PAD_X * 2.0, theme::ROW_H),
        );
        // Raised, then washed. The shading is what says "this one", and the
        // wash is what says which one - the pair is legible where either alone
        // would not be, which is the point of shading a monochrome panel.
        theme::raise(ui.painter(), theme, row_rect, theme::ROW_RADIUS);
        ui.painter()
            .rect_filled(row_rect, theme::radius(theme::ROW_RADIUS), theme.selection);
        row::marker(ui, theme, row_rect);
    }

    // The term, not the line: `report ext:pdf` is fourteen bytes and the
    // match is six. `view::row::highlight` answers an out-of-range length by
    // returning a plain name, so the symptom would be an underline quietly
    // never appearing rather than anything louder.
    let query_len = state.query().term().len();
    let selected = state.selected_row();
    let mut hovered = None;

    let mut child = ui.new_child(
        eframe::egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(theme::PAD_X, 0.0)))
            .layout(*ui.layout()),
    );
    // A row is exactly `ROW_H` tall and the next one starts where it ends.
    //
    // The toolkit's default is three points of air between allocated items,
    // which is right for a form and wrong for a list: `measure` sizes the
    // panel at `rows * ROW_H`, so three points a row meant a full list of
    // eight stood twenty-four points taller than the band it was given and
    // the last row was drawn through the footer. The other three bodies place
    // their rows by arithmetic rather than by allocation, which is why this
    // only ever went wrong here.
    child.spacing_mut().item_spacing.y = 0.0;
    // Absolute ranks, not positions in the drawn window. Every `Intent` below
    // is an index into `state.hits`, which is what `pointer.rs` expects and
    // what lets `apply_hits` re-validate a hover by path across a result
    // change - so the offset is added here, at the one place the two
    // numberings meet, rather than being subtracted again at the far end.
    let window = state.visible_rows();
    for (rank, hit) in state.hits[window.clone()]
        .iter()
        .enumerate()
        .map(|(i, hit)| (window.start + i, hit))
    {
        let response = row::show(&mut child, theme, hit, query_len, selected == Some(rank));
        if response.hovered() {
            hovered = Some(rank);
        }
        if response.double_clicked() {
            intents.push(Intent::Activate(rank));
        } else if response.clicked() {
            intents.push(Intent::Select(rank));
        }
    }

    if hovered != state.hovered() {
        intents.push(Intent::Hover(hovered));
    }
    intents
}

/// The codes used before, which is what an empty field shows.
///
/// Drawn deliberately unlike a result - no folder column, no extension, and the
/// accent colour a code is typed in - so that "Enter takes the row you are on"
/// needs no explanation of which kind of row this is.
fn draw_recent(ui: &mut Ui, state: &AppState, theme: &Theme, rect: Rect) -> Vec<Intent> {
    let mut intents = Vec::new();
    let painter = ui.painter().clone();

    // Absolute ranks, not positions in the drawn window: the click below names
    // an entry in `history.entries()`, so the offset is added here, at the one
    // place the two numberings meet.
    let window = state.recent_rows();
    let cursor = state.history.cursor();
    for (rank, entry) in state.history.entries()[window.clone()]
        .iter()
        .enumerate()
        .map(|(i, entry)| (window.start + i, entry))
    {
        let row_rect = Rect::from_min_size(
            pos2(
                rect.left() + theme::PAD_X,
                rect.top() + (rank - window.start) as f32 * theme::ROW_H,
            ),
            vec2(rect.width() - theme::PAD_X * 2.0, theme::ROW_H),
        );
        let response = ui.interact(row_rect, Id::new(("files-recent", rank)), Sense::click());
        response.widget_info(|| {
            eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Button, true, entry)
        });
        // The band, drawn before the text so the text sits on it. Same
        // treatment the result list gets, because it is the same thing: the row
        // the field came from.
        if cursor == Some(rank) {
            theme::raise(&painter, theme, row_rect, theme::ROW_RADIUS);
            painter.rect_filled(row_rect, theme::radius(theme::ROW_RADIUS), theme.selection);
            row::marker(ui, theme, row_rect);
        } else if response.hovered() {
            painter.rect_filled(row_rect, theme::radius(theme::ROW_RADIUS), theme.hover);
        }
        painter.text(
            pos2(row_rect.left() + theme::ROW_PAD_X, row_rect.center().y),
            Align2::LEFT_CENTER,
            entry,
            theme::font(theme::SIZE_ROW, theme.weight(Emphasis::Accent)),
            theme.accent,
        );
        // By rank. This used to push `Hint(Action::Recall)`, which is the Up
        // arrow with the rank thrown away - so clicking the fifth code stepped
        // one entry older.
        if response.clicked() {
            intents.push(Intent::Recall(rank));
        }
    }
    intents
}

fn draw_blocks(ui: &Ui, theme: &Theme, rect: Rect, blocks: &[view::Block]) {
    let painter = ui.painter().clone();
    for (line, block) in blocks.iter().take(theme::MAX_ROWS).enumerate() {
        let mut job = LayoutJob::default();
        for run in block {
            job.append(
                &run.text,
                0.0,
                TextFormat {
                    font_id: weight_of(theme, run),
                    color: theme.emphasis(run.emphasis),
                    ..Default::default()
                },
            );
        }
        let galley = painter.layout_job(job);
        let at = pos2(
            rect.left() + theme::PAD_X + theme::ROW_PAD_X,
            rect.top() + theme::PAD_Y + line as f32 * LINE_H,
        );
        announce(
            ui,
            Rect::from_min_max(at, pos2(rect.right() - theme::PAD_X, at.y + LINE_H)),
            ("block", line),
            &view::plain(block),
        );
        painter.galley(at, galley, theme.text);
    }
}

/// Emphasis chooses a weight as well as a colour: `Strong` is the headline of
/// an empty state, and a headline that differs from its body only in brightness
/// is not a headline.
/// The size a run is set at, and the weight the theme asks for it.
///
/// Size is a property of the *block* - a headline is bigger - and weight is a
/// property of the emphasis, which is the theme's to decide. This used to
/// hard-code Bold for `Strong` here, which was the only place in the program
/// that emphasis changed anything but colour.
fn weight_of(theme: &Theme, run: &Run) -> eframe::egui::FontId {
    let size = match run.emphasis {
        Emphasis::Strong => theme::SIZE_HEADLINE,
        _ => theme::SIZE_ROW,
    };
    theme::font(size, theme.weight(run.emphasis))
}

/// Which drive to re-read.
///
/// `F5` used to re-read every share at once. Almost every press wanted one of
/// them, and across a few hundred people the difference is between a handful of
/// passes over a share of three hundred thousand folders and several hundred
/// simultaneous ones. So the key asks - and asking is a list, which is the one
/// reason anything still borrows the body.
fn draw_shares(ui: &Ui, state: &AppState, theme: &Theme, rect: Rect, wall: SystemTime) {
    let painter = ui.painter().clone();
    let font = theme::font(theme::SIZE_ROW, Weight::Regular);
    let small = theme::font(theme::SIZE_SMALL, Weight::Regular);

    for (rank, id) in state.share_ids().iter().take(theme::MAX_ROWS).enumerate() {
        let row_rect = Rect::from_min_size(
            pos2(
                rect.left() + theme::PAD_X,
                rect.top() + rank as f32 * theme::ROW_H,
            ),
            vec2(rect.width() - theme::PAD_X * 2.0, theme::ROW_H),
        );
        if rank == state.shares_cursor() {
            painter.rect_filled(row_rect, theme::radius(theme::ROW_RADIUS), theme.selection);
            row::marker(ui, theme, row_rect);
        }

        let Some(share) = state.share_row(*id) else {
            continue;
        };
        let block = view::shares::row(&share, wall);

        // The name on the left and everything else on the right. Three runs
        // come back - the name, the age, and only sometimes a reason - so the
        // tail is laid out from the right edge backwards rather than at fixed
        // positions, and a row with no reason simply has a shorter tail.
        let mut runs = block.iter();
        if let Some(name) = runs.next() {
            painter.text(
                pos2(row_rect.left() + theme::ROW_PAD_X, row_rect.center().y),
                Align2::LEFT_CENTER,
                name.text.as_ref(),
                font.clone(),
                theme.emphasis(name.emphasis),
            );
        }
        let tail: Vec<_> = runs.collect();
        let mut x = row_rect.right() - theme::ROW_PAD_X;
        for run in tail.into_iter().rev() {
            let galley = painter.layout_no_wrap(
                run.text.to_string(),
                small.clone(),
                theme.emphasis(run.emphasis),
            );
            x -= galley.rect.width();
            painter.galley(
                pos2(x, row_rect.center().y - galley.rect.height() / 2.0),
                galley,
                theme.dim,
            );
            x -= 12.0;
        }
    }
}

// -- the footer -------------------------------------------------------------

/// The gap between the result count and the status prose beside it.
const COUNT_GAP: f32 = 12.0;

fn draw_footer(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    rect: Rect,
    now: Instant,
    wall: SystemTime,
) -> Vec<Intent> {
    let painter = ui.painter().clone();
    let status = view::status::render(state, now, wall);
    let font = theme::font(theme::SIZE_SMALL, Weight::Regular);

    // The glyph, then the words. Colour alone does not carry the difference
    // between "updated" and "unreachable" for about one man in twelve.
    painter.text(
        pos2(rect.left() + theme::PAD_X, rect.center().y),
        Align2::LEFT_CENTER,
        theme.glyph(status.tone),
        theme::font(theme::SIZE_SMALL, theme.weight(Emphasis::Tone(status.tone))),
        theme.tone(status.tone),
    );
    let mut status_left = rect.left() + theme::PAD_X + 16.0;

    // The reserved slot: drawn before the prose and outside its wrap width, so
    // it is the one part of this line that truncation cannot reach.
    //
    // Strong rather than dim. Where you are in three hundred results is
    // something somebody is looking for, not a note about the state of the
    // program.
    let range = view::status::visible_range(state, state.visible_rows());
    if !range.is_empty() {
        let width = painter
            .layout_no_wrap(range.clone(), font.clone(), Color32::WHITE)
            .rect
            .width();
        painter.text(
            pos2(status_left, rect.center().y),
            Align2::LEFT_CENTER,
            &range,
            font.clone(),
            theme.text,
        );
        announce(
            ui,
            Rect::from_min_size(pos2(status_left, rect.top()), vec2(width, rect.height())),
            "range",
            &range,
        );
        status_left += width + COUNT_GAP;
    }

    let hints = view::hints::hints(view::hints::Context::of(state));
    let chips = draw_chips(ui, theme, rect, &hints, chip_budget(rect, status_left));

    // Whatever the chips left. Truncated rather than overlapped: a status line
    // running under `Esc  close` is unreadable, and the keys are the part
    // somebody stuck cannot do without.
    let status_right = chips.0 - CHIP_GAP * 2.0;
    let mut job = LayoutJob::single_section(
        status.text.clone(),
        TextFormat {
            font_id: font,
            color: theme.dim,
            ..Default::default()
        },
    );
    job.wrap.max_width = (status_right - status_left).max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('\u{2026}');
    let galley = painter.layout_job(job);
    painter.galley(
        pos2(status_left, rect.center().y - galley.rect.height() / 2.0),
        galley,
        theme.dim,
    );
    // The untruncated text, deliberately. What is painted may be ellipsised to
    // fit beside the chips; what is *said* has no width to fit into, and a
    // reader that got the same abbreviation as the screen would be worse off
    // than one that got the sentence.
    announce(
        ui,
        Rect::from_min_max(
            pos2(status_left, rect.top()),
            pos2(status_right, rect.bottom()),
        ),
        "status",
        &status.text,
    );

    chips.1
}

/// Room kept for the status line, whether or not it has anything to say.
///
/// Wide enough for the longest transient - `Checking the drive...` - with a
/// warning longer than that left to truncate, which is what truncation is for.
const STATUS_RESERVE: f32 = 120.0;

/// How much of the footer the key hints may have.
///
/// Everything the range readout did not take, less a *fixed* reserve for the
/// status line - not the width that line actually needs. A budget that tracked
/// the text would add and remove chips every time the phase changed, which is a
/// footer reflowing under somebody reading it: the exact class of thing the last
/// round of work went to remove. Fixed, it never moves.
///
/// The reserve is empty space when the line is quiet, which it usually now is.
/// The chips are laid out from the right, so what that costs is a gap in the
/// middle of the footer rather than anything anybody is looking at.
///
/// This replaces a flat `rect.width() * 0.55`, which was 396pt whatever else
/// was on the line - and 396pt is not enough for the F2 chip, which is why the
/// viewer indicator had to live on the left until now.
fn chip_budget(rect: Rect, status_left: f32) -> f32 {
    (rect.right() - theme::PAD_X - status_left - CHIP_GAP * 2.0 - STATUS_RESERVE).max(0.0)
}

/// Lays the key hints out from the right, and reports where they start.
fn draw_chips(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    hints: &[view::hints::Hint],
    budget: f32,
) -> (f32, Vec<Intent>) {
    let painter = ui.painter().clone();
    let key_font = theme::font(theme::SIZE_CHIP, Weight::Bold);
    let label_font = theme::font(theme::SIZE_SMALL, Weight::Regular);

    let measure = |text: &str, font: &eframe::egui::FontId| {
        painter
            .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
            .rect
            .width()
    };

    // The same fitting policy the terminal used - drop the lowest-priority
    // hint furthest right, then the labels, never the essentials - measured in
    // points instead of columns.
    let widths = Points {
        key: &|text: &str| measure(text, &key_font),
        label: &|text: &str| measure(text, &label_font),
    };
    let (kept, labelled) = view::hints::fit(hints, budget, &widths);

    // The keys, as one string. A chip is a key and a label sitting beside each
    // other, and reading them out one glyph at a time would be worse than not
    // reading them at all.
    //
    // The *kept* ones, deliberately - unlike the status line above, which is
    // announced in full because its truncation is a lack of room rather than a
    // decision. A dropped hint is a decision: `fit` chose it as the one worth
    // least here, and announcing it anyway would be telling a reader about a
    // key nobody else can see.
    announce(
        ui,
        Rect::from_min_max(
            pos2(rect.right() - theme::PAD_X - 1.0, rect.top()),
            pos2(rect.right() - theme::PAD_X, rect.bottom()),
        ),
        "chips",
        &view::hints::plain(&kept, labelled),
    );

    let mut intents = Vec::new();
    let mut x = rect.right() - theme::PAD_X;
    for hint in kept.iter().rev() {
        let key_w = measure(hint.key, &key_font) + 12.0;
        let label_w = if labelled {
            measure(hint.label, &label_font) + 6.0
        } else {
            0.0
        };
        x -= label_w;
        if labelled {
            painter.text(
                pos2(x + 3.0, rect.center().y),
                Align2::LEFT_CENTER,
                hint.label,
                label_font.clone(),
                theme.dim,
            );
        }
        x -= key_w;

        let chip = Rect::from_min_size(pos2(x, rect.center().y - 11.0), vec2(key_w - 4.0, 22.0));
        theme::cap(&painter, theme, chip, theme::CHIP_RADIUS);
        painter.text(
            chip.center(),
            Align2::CENTER_CENTER,
            hint.key,
            key_font.clone(),
            theme.chip_fg,
        );

        // Only the hints that name a safe action are clickable. `Ctrl+Q` and
        // `Esc` are advertised and not wired: a control surface where one
        // mis-click ends the session is worse than none.
        if let Some(action) = hint.action {
            let response = ui.interact(chip, Id::new(("files-hint", hint.key)), Sense::click());
            if response.clicked() {
                intents.push(Intent::Hint(action));
            }
        }
        x -= CHIP_GAP;
    }
    (x, intents)
}

/// Measures hint chips in points, for [`view::hints::fit`].
struct Points<'a> {
    key: &'a dyn Fn(&str) -> f32,
    label: &'a dyn Fn(&str) -> f32,
}

impl view::hints::Measure for Points<'_> {
    fn chip(&self, hint: &view::hints::Hint, labelled: bool) -> f32 {
        let key = (self.key)(hint.key) + 12.0;
        if labelled {
            key + (self.label)(hint.label) + 6.0
        } else {
            key
        }
    }

    fn gap(&self) -> f32 {
        CHIP_GAP
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::AppEvent;
    use crate::app::key::{Key, KeyEvent, Mods};
    use crate::app::state::pointer::Intent;
    use crate::config::{Settings, VISIBLE_ROWS};
    use crate::search::matcher::Hit;
    use std::sync::Arc;

    /// Puts the cursor on `row` the way a click does.
    ///
    /// Through the real intent rather than by assigning `selected_path`, so the
    /// window follows the cursor exactly as it does in the program - which is
    /// the thing these tests are about.
    fn select(state: &mut AppState, row: usize) {
        state.update(AppEvent::Intent(Intent::Select(row)), Instant::now());
    }

    fn state() -> AppState {
        AppState::new(Settings::default(), Instant::now())
    }

    /// Reports a healthy index, which is what stops a fresh state having the
    /// one standing notice every fresh state has: "No file list yet".
    fn settle_index(state: &mut AppState) {
        let status = crate::index::store::IndexStatus {
            origin: Some(crate::index::store::Origin::Network),
            entries: 10,
            built_at: Some(SystemTime::UNIX_EPOCH),
            ..Default::default()
        };
        state.update(
            crate::app::event::AppEvent::Index(crate::app::event::IndexMsg::Status {
                id: crate::paths::MappingId(0),
                status: std::sync::Arc::new(status),
            }),
            Instant::now(),
        );
    }

    fn with_hits(n: usize) -> AppState {
        let mut state = state();
        state.input.set_text("11-D");
        state.hits = (0..n)
            .map(|i| Hit {
                path: Arc::from(format!(r"R:\jobs\11-D-070{i}.pdf").as_str()),
                name: Arc::from(format!("11-D-070{i}.pdf").as_str()),
                match_pos: 0,
                index: i as u32,
            })
            .collect();
        state
    }

    /// The first screen after a fresh install has no history to show, so it
    /// gets the onboarding block rather than an empty list.
    #[test]
    fn an_empty_field_with_no_history_shows_the_first_run_block() {
        assert_eq!(measure(&state()).content, Content::Empty);
    }

    /// Recall stops being a mode: with nothing typed, the list *is* your
    /// recent codes.
    #[test]
    fn the_recent_codes_appear_only_after_the_up_arrow() {
        let mut state = state();
        state.history.record("11-D-0704");

        // Unasked for. These used to be what an empty field showed, so every
        // summon of an empty panel put the job codes this person had looked up
        // in front of whoever was standing behind them.
        assert_eq!(measure(&state).content, Content::Empty);

        state.update(
            AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)),
            std::time::Instant::now(),
        );
        assert_eq!(measure(&state).content, Content::Recent);
    }

    #[test]
    fn typing_something_that_matches_shows_the_results() {
        assert_eq!(measure(&with_hits(3)).content, Content::Results);
    }

    #[test]
    fn typing_something_that_matches_nothing_shows_why() {
        let mut state = state();
        state.input.set_text("zzzz");
        assert_eq!(measure(&state).content, Content::Empty);
    }

    /// The drive picker replaces the body rather than floating over it, which
    /// is what makes it the one thing left that borrows it.
    #[test]
    fn the_drive_picker_replaces_whatever_was_there() {
        let mut state = with_hits(3);
        assert_eq!(measure(&state).content, Content::Results);

        state.picking_share = true;
        assert_eq!(measure(&state).content, Content::Shares);
    }

    /// A search that matched four hundred files still asks for a list that
    /// fits. The window cannot grow to meet it any more.
    #[test]
    fn the_panel_never_asks_for_more_rows_than_it_has_room_for() {
        for n in [0, 1, 7, 8, 9, 400] {
            let measured = measure(&with_hits(n));
            assert!(
                measured.rows <= theme::MAX_ROWS,
                "{n} hits asked for {} rows",
                measured.rows
            );
        }
    }

    /// An untouched panel is the field and nothing else.
    ///
    /// Nothing in the body at all, which is what `Content::Quiet` is for.
    ///
    /// This used to assert the window was one band tall, because the panel
    /// was the window and a quiet one dropped the body and the footer
    /// entirely. The window is a fixed four hundred points now, so what is
    /// left of the claim - and it is the part that was ever visible - is that
    /// there is nothing in it. The old first screen was 260 points of
    /// instructions.
    #[test]
    fn an_untouched_panel_has_nothing_in_its_body() {
        let mut state = state();
        settle_index(&mut state);
        assert!(state.is_quiet(), "the fixture is not quiet");

        let m = measure(&state);
        assert_eq!(m.content, Content::Quiet);
        assert_eq!(m.rows, 0);
    }

    /// And anything worth saying takes the panel out of quiet, because a
    /// message nobody can see is a message nobody gets.
    #[test]
    fn something_to_say_is_not_a_quiet_panel() {
        let mut state = state();
        settle_index(&mut state);
        assert_eq!(measure(&state).content, Content::Quiet);

        // A real one, raised the way the program raises it.
        state.update(
            crate::app::event::AppEvent::Clipboard(crate::app::event::ClipboardMsg::Copied {
                chars: 9,
            }),
            Instant::now(),
        );
        assert!(state.toast.is_some(), "the fixture raised no toast");
        assert!(!state.is_quiet(), "a toast left the panel quiet");
        assert_ne!(measure(&state).content, Content::Quiet);
    }

    /// One row per result, up to what the panel has room for.
    #[test]
    fn the_list_shows_one_row_per_result_up_to_what_fits() {
        for n in [0, 1, 2, theme::MAX_ROWS - 1, theme::MAX_ROWS, 300] {
            assert_eq!(
                measure(&with_hits(n)).rows,
                n.min(theme::MAX_ROWS),
                "{n} hits"
            );
        }
    }

    /// The highlight is positioned by the animator, which needs a point, not a
    /// rank - and the point is measured from the top of the *window*, not from
    /// the top of the list.
    ///
    /// This used to clamp the rank to the last visible row, which is why a
    /// selection past the window left the band frozen on the bottom row while
    /// the arrows walked on without it.
    #[test]
    fn the_selection_is_reported_as_a_point_inside_the_window() {
        let mut state = with_hits(VISIBLE_ROWS * 3);
        select(&mut state, 3);
        assert_eq!(measure(&state).selection_y, Some(3.0 * theme::ROW_H));

        // Far down the list: the window has followed, so the point is still
        // inside it - and is not pinned to the bottom row.
        let last = state.hits.len() - 1;
        select(&mut state, last);
        let far = measure(&state).selection_y.expect("still selected");
        assert!(
            (0.0..=(VISIBLE_ROWS - 1) as f32 * theme::ROW_H).contains(&far),
            "the highlight was placed at {far}, outside the window"
        );
        assert_eq!(
            far,
            (last - state.scroll_top()) as f32 * theme::ROW_H,
            "the point must be the row's place in the window"
        );
    }

    /// Nothing selected is not row zero selected.
    #[test]
    fn a_body_with_no_selection_places_no_highlight() {
        assert_eq!(measure(&with_hits(3)).selection_y, None);

        let mut state = state();
        state.history.record("11-D-0704");
        assert_eq!(
            measure(&state).selection_y,
            None,
            "the recent list is not the result list"
        );
    }

    /// Browsing past the last visible entry moves the window with the cursor.
    ///
    /// The recall list used to draw `entries[0..12]` and nothing else, with no
    /// band on any of them - so past the twelfth code the field changed and
    /// nothing on screen moved at all. Only the status line's "20 of 200" said
    /// otherwise.
    #[test]
    fn browsing_past_the_window_carries_it_along() {
        let mut state = state();
        for i in 0..40 {
            state.history.record(&format!("code-{i:02}"));
        }
        let now = std::time::Instant::now();

        for step in 1..=20 {
            state.update(AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)), now);
            let window = state.recent_rows();
            let cursor = state.history.cursor().expect("browsing");
            assert!(
                window.contains(&cursor),
                "step {step}: cursor {cursor} is outside the drawn {window:?}"
            );
            assert_eq!(window.len(), VISIBLE_ROWS, "step {step}: short window");

            // And the band is drawn inside the panel rather than below it.
            let y = measure(&state).selection_y.expect("a highlighted row");
            assert!(
                (0.0..VISIBLE_ROWS as f32 * theme::ROW_H).contains(&y),
                "step {step}: the highlight is at {y}, off the list"
            );
        }
    }
}
