//! The content band: a caption, a list, and the only scrollbar on the panel.
//!
//! Ueli's content area is one element with `padding: 10`, `overflow-y: auto`
//! and a `gap` of ten between groups, where a group is a caption over a list.
//! This is that, with four kinds of list rather than two - the results, the
//! codes used before, the drive picker, and a reason there are none.
//!
//! # Why the wheel works now
//!
//! It did not before, and the note in `gui::input` said why: the panel drew a
//! twelve-row window over `hits` that the *state machine* owned, so the only
//! thing a wheel event could have moved was the selection - and a hand resting
//! on a mouse walked somebody's cursor through their own search results. So it
//! was dropped.
//!
//! A [`ScrollArea`] moves the viewport instead and touches nothing else, which
//! is what a wheel is for. The selection is a separate thing that the arrows
//! move, and it is brought back into view only when it has left it.
//!
//! # `block: "nearest"`
//!
//! Ueli scrolls the selected row into view with `scrollIntoView({ block:
//! "nearest" })`, which moves by the least it can and does nothing at all when
//! the row is already on screen. egui spells the same policy `align: None`.
//!
//! It is asked for only on the frames the selection actually *changed*, which
//! is the part that is easy to get wrong: requested every frame, the view
//! would snap back to the selection the instant somebody wheeled away from it,
//! and the scrollbar would be decoration.
//!
//! And it lands in one frame, because the scroller is built with
//! `animated(false)`. egui's default is to ease a scroll target over a couple
//! of hundred milliseconds, which for a held Down key means the view chasing a
//! cursor it never catches; `scrollIntoView` with no `behavior` does not do
//! that either, and neither does anything else on this panel. See
//! [`crate::gui::anim`].

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align, Align2, Id, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder, pos2, vec2};
use std::time::SystemTime;

use super::row;
use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::anim::Content;
use crate::gui::theme::{self, Icon, Theme, Weight};
use crate::view::{self, Emphasis, Run};

/// How tall one line of the empty state is.
const LINE_H: f32 = 24.0;

/// A caption's left inset, which is five points less than a row's so that the
/// heading reads as a label *over* the list rather than as an item in it.
const HEADING_PAD_X: f32 = 5.0;

/// Draws whichever body is on screen, and reports what the pointer did to it.
pub fn show(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    band: Rect,
    content: Content,
    wall: SystemTime,
) -> Vec<Intent> {
    // The empty state is a message rather than a list: it is at most five
    // lines, it can never overflow the band, and Ueli centres its equivalent
    // on both axes. So it is drawn straight onto the band with no scroller to
    // put a bar down the side of something that does not scroll.
    if content == Content::Empty {
        let blocks = view::empty::view(&super::empty_reason(state), state.input.text());
        centred_blocks(ui, theme, band, &blocks);
        return Vec::new();
    }

    let mut intents = Vec::new();
    // Named, and this is load-bearing rather than tidiness. A `Ui` built
    // without an `id_salt` takes an automatic one derived from how many ids
    // its parent has handed out so far - and the header and the footer hand
    // out a different number every frame, because the hint chips that fit
    // change with the phase. So the scroller underneath it was a *different*
    // scroller each frame, its remembered offset was thrown away every time,
    // and a list that had been scrolled snapped back before anybody saw it.
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt("files-list")
            .max_rect(band)
            .layout(Layout::top_down(Align::Min)),
    );

    ScrollArea::vertical()
        // One offset per body, so stepping into the drive picker from halfway
        // down three hundred results does not open the picker scrolled past
        // its last drive.
        .id_salt(("files-list", discriminant(content)))
        // Both axes: the band is a fixed height whether or not there is
        // anything in it, and a scroller that shrank to its content would
        // leave the rows floating in the middle of it.
        .auto_shrink([false, false])
        // Instant, like everything else on this panel. egui's default is to
        // ease a `scroll_to` target over a couple of hundred milliseconds,
        // which for a held Down key is a view chasing a cursor it never
        // catches; `scrollIntoView` with no `behavior` does not do that
        // either. See [`crate::gui::anim`] for why nothing here moves.
        .animated(false)
        .show(&mut child, |ui| {
            // The band's padding goes on the inside, so the scrollbar rides
            // the panel's edge rather than sitting ten points in from it -
            // which is where `overflow-y: auto` puts it, padding and all.
            let inner = ui.max_rect().shrink2(vec2(theme::BAND_PAD, 0.0));
            let mut body = ui.new_child(
                UiBuilder::new()
                    .id_salt("files-list-body")
                    .max_rect(inner)
                    .layout(Layout::top_down(Align::Min)),
            );
            // The air between rows is allocated between them rather than
            // left to the toolkit, because `gap` means *between*: egui's
            // `item_spacing` would also put five points under the caption,
            // which already carries its own, and five under the last row on
            // top of the band's padding.
            body.spacing_mut().item_spacing.y = 0.0;

            // Allocated rather than `add_space`d, both ends: a space only
            // moves the cursor, and what the scroller measures is the
            // rectangle everything was allocated in. The bottom pad would
            // have been the difference between a last row clear of the rule
            // and one touching it.
            space(&mut body, theme::BAND_PAD);
            if let Some(text) = heading_of(content) {
                heading(&mut body, theme, text);
            }
            intents = match content {
                Content::Results => results(&mut body, state, theme),
                Content::Recent => recent(&mut body, state, theme),
                Content::Aliases => aliases(&mut body, state, theme),
                Content::Shares => {
                    shares(&mut body, state, theme, wall);
                    Vec::new()
                }
                // Drawn above, without a scroller.
                Content::Empty => Vec::new(),
            };
            space(&mut body, theme::BAND_PAD);

            ui.advance_cursor_after_rect(body.min_rect());
        });

    intents
}

/// Air in the list, allocated rather than skipped over.
///
/// `Ui::add_space` only moves the cursor, and what the scroller measures is
/// the rectangle everything was *allocated* in - so a space at the foot of
/// the list would be the difference between a last row clear of the rule and
/// one touching it.
fn space(ui: &mut Ui, amount: f32) {
    ui.allocate_exact_size(vec2(ui.available_width(), amount), Sense::hover());
}

/// The caption over a group.
///
/// Ueli's is a twelve-point caption in the dimmest foreground the theme has,
/// five points in from the left and five points clear of the list under it.
fn heading(ui: &mut Ui, theme: &Theme, text: &str) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), theme::HEADING_H), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    ui.painter().text(
        pos2(rect.left() + HEADING_PAD_X, rect.top()),
        Align2::LEFT_TOP,
        text,
        theme::font(theme::SIZE_CAPTION, Weight::Semibold),
        theme.dim,
    );
    super::announce(ui, rect, ("heading", text), text);
}

/// What the group above each list is called.
///
/// Two of Ueli's groups, renamed for what this program has: its `favorites`
/// and `searchResults` are an installed application and a fuzzy match over
/// one, and ours are a code somebody typed and the drawings it found.
///
/// There is no heading over the empty state. A caption saying "Results" over
/// a sentence explaining that there are none would be the panel arguing with
/// itself.
const fn heading_of(content: Content) -> Option<&'static str> {
    match content {
        Content::Results => Some("Results"),
        Content::Recent => Some("Recent codes"),
        Content::Aliases => Some("Shortcuts"),
        Content::Shares => Some("Drives"),
        Content::Empty => None,
    }
}

/// A stable number per body, so each keeps its own scroll offset.
///
/// Without it every body shares one, and stepping into the drive picker from
/// halfway down three hundred results would open the picker scrolled past its
/// last drive.
const fn discriminant(content: Content) -> u8 {
    match content {
        Content::Results => 0,
        Content::Recent => 1,
        Content::Shares => 2,
        Content::Empty => 3,
        Content::Aliases => 4,
    }
}

// -- the results ------------------------------------------------------------

fn results(ui: &mut Ui, state: &AppState, theme: &Theme) -> Vec<Intent> {
    let mut intents = Vec::new();

    let style = row::Style {
        theme,
        layout: state.settings.result_layout,
        routes: &state.settings.routes,
        // Once, rather than once per row: the answer goes through the font
        // atlas and is the same for all three hundred of them.
        icons: theme::has_icon(ui.ctx(), Icon::File),
        // The term, not the line: `report ext:pdf` is fourteen bytes and the
        // match is six. `view::row::highlight` answers an out-of-range length
        // by returning a plain name, so the symptom would be an underline
        // quietly never appearing rather than anything louder.
        query_len: state.query().term().len(),
    };
    let selected = state.selected_row();
    let follow = selection_changed(ui, "results", selected);
    let mut hovered = None;

    for (rank, hit) in state.hits.iter().enumerate() {
        if rank > 0 {
            space(ui, theme::ROW_GAP);
        }
        let response = row::show(ui, &style, hit, selected == Some(rank));
        if selected == Some(rank) && follow {
            bring_into_view(ui, response.rect);
        }
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

/// The codes used before, which is what the Up arrow shows.
///
/// Drawn deliberately unlike a result - no folder column, no extension, and
/// the accent colour a code is typed in - so that "Enter takes the row you are
/// on" needs no explanation of which kind of row this is.
fn recent(ui: &mut Ui, state: &AppState, theme: &Theme) -> Vec<Intent> {
    let mut intents = Vec::new();
    let cursor = state.history.cursor();
    let follow = selection_changed(ui, "recent", cursor);

    for (rank, entry) in state.history.entries().iter().enumerate() {
        if rank > 0 {
            space(ui, theme::ROW_GAP);
        }
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), theme::ROW_COMPACT_H),
            Sense::click(),
        );
        if !ui.is_rect_visible(rect) && cursor != Some(rank) {
            continue;
        }
        let entry = entry.clone();
        response.widget_info(|| {
            eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Button, true, &entry)
        });

        let painter = ui.painter();
        // The band, drawn before the text so the text sits on it. The same
        // treatment a result gets, because it is the same thing: the row the
        // field came from.
        if cursor == Some(rank) {
            theme::raise(painter, theme, rect, theme::RADIUS_MEDIUM);
            painter.rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.selection);
            row::marker(ui, theme, rect);
            if follow {
                bring_into_view(ui, rect);
            }
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.hover);
        }
        ui.painter().text(
            pos2(rect.left() + theme::ROW_PAD_X, rect.center().y),
            Align2::LEFT_CENTER,
            &entry,
            theme::font(theme::SIZE_BODY, theme.weight(Emphasis::Accent)),
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

/// The shortcuts somebody configured, which is what an empty box shows.
///
/// Drawn like the recent codes and not like a result: the accent colour a
/// code is typed in, because that is what taking one of these puts in the
/// box. What is on the right is the code it stands for, dimmed - so the list
/// reads as `pw \u{2192} 11-D-0704` and is its own documentation.
fn aliases(ui: &mut Ui, state: &AppState, theme: &Theme) -> Vec<Intent> {
    let mut intents = Vec::new();
    let cursor = state.alias_cursor();
    let follow = selection_changed(ui, "aliases", cursor);
    let small = theme::font(theme::SIZE_CAPTION, Weight::Regular);

    for (rank, alias) in state.settings.aliases.all().iter().enumerate() {
        if rank > 0 {
            space(ui, theme::ROW_GAP);
        }
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), theme::ROW_COMPACT_H),
            Sense::click(),
        );
        if !ui.is_rect_visible(rect) && cursor != Some(rank) {
            continue;
        }
        // The note as well, where there is one: it is what the person who
        // wrote the list put there to remind themselves a year later, and a
        // reader has no column width to be elided to.
        let spoken = match &alias.note {
            Some(note) => format!("{} \u{b7} {note}", alias.describe()),
            None => alias.describe(),
        };
        response.widget_info(|| {
            eframe::egui::WidgetInfo::selected(
                eframe::egui::WidgetType::Button,
                true,
                cursor == Some(rank),
                &spoken,
            )
        });

        if cursor == Some(rank) {
            theme::raise(ui.painter(), theme, rect, theme::RADIUS_MEDIUM);
            ui.painter()
                .rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.selection);
            row::marker(ui, theme, rect);
            if follow {
                bring_into_view(ui, rect);
            }
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.hover);
        }

        let painter = ui.painter().clone();
        painter.text(
            pos2(rect.left() + theme::ROW_PAD_X, rect.center().y),
            Align2::LEFT_CENTER,
            alias.name.as_ref(),
            theme::font(theme::SIZE_BODY, theme.weight(Emphasis::Accent)),
            theme.accent,
        );
        painter.text(
            pos2(rect.right() - theme::ROW_PAD_X, rect.center().y),
            Align2::RIGHT_CENTER,
            alias.code.as_ref(),
            small.clone(),
            theme.dim,
        );

        if response.clicked() {
            intents.push(Intent::UseAlias(rank));
        }
    }
    intents
}

/// Which drive to re-read.
///
/// `F5` used to re-read every share at once. Almost every press wanted one of
/// them, and across a few hundred people the difference is between a handful
/// of passes over a share of three hundred thousand folders and several
/// hundred simultaneous ones. So the key asks - and asking is a list, which is
/// the one reason anything still borrows the body.
fn shares(ui: &mut Ui, state: &AppState, theme: &Theme, wall: SystemTime) {
    let font = theme::font(theme::SIZE_BODY, Weight::Regular);
    let small = theme::font(theme::SIZE_CAPTION, Weight::Regular);
    let chosen = state.shares_cursor();
    let follow = selection_changed(ui, "shares", Some(chosen));

    for (rank, id) in state.share_ids().iter().enumerate() {
        if rank > 0 {
            space(ui, theme::ROW_GAP);
        }
        let (rect, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), theme::ROW_COMPACT_H),
            Sense::hover(),
        );
        if rank == chosen {
            ui.painter()
                .rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.selection);
            row::marker(ui, theme, rect);
            if follow {
                bring_into_view(ui, rect);
            }
        }
        if !ui.is_rect_visible(rect) {
            continue;
        }

        let Some(share) = state.share_row(*id) else {
            continue;
        };
        let block = view::shares::row(&share, wall);
        super::announce(ui, rect, ("share", rank), &view::plain(&block));

        // The name on the left and everything else on the right. Three runs
        // come back - the name, the age, and only sometimes a reason - so the
        // tail is laid out from the right edge backwards rather than at fixed
        // positions, and a row with no reason simply has a shorter tail.
        let painter = ui.painter().clone();
        let mut runs = block.iter();
        if let Some(name) = runs.next() {
            painter.text(
                pos2(rect.left() + theme::ROW_PAD_X, rect.center().y),
                Align2::LEFT_CENTER,
                name.text.as_ref(),
                font.clone(),
                theme.emphasis(name.emphasis),
            );
        }
        let tail: Vec<_> = runs.collect();
        let mut x = rect.right() - theme::ROW_PAD_X;
        for run in tail.into_iter().rev() {
            let galley = painter.layout_no_wrap(
                run.text.to_string(),
                small.clone(),
                theme.emphasis(run.emphasis),
            );
            x -= galley.rect.width();
            painter.galley(
                pos2(x, rect.center().y - galley.rect.height() / 2.0),
                galley,
                theme.dim,
            );
            x -= 12.0;
        }
    }
}

// -- the empty state --------------------------------------------------------

/// The reason there is nothing, in the middle of the band.
///
/// Centred on both axes, which is Ueli's shape for the same screen and is the
/// one arrangement that does not read as a list that failed to load. It used
/// to be left-aligned at the top of a twelve-row box, which is a grey sentence
/// in the corner of three hundred points of nothing.
///
/// `view::empty` is deliberately silent about this: where a block sits is
/// layout, and the module note there says so.
fn centred_blocks(ui: &Ui, theme: &Theme, band: Rect, blocks: &[view::Block]) {
    let painter = ui.painter().clone();
    let top = band.center().y - blocks.len() as f32 * LINE_H / 2.0;

    for (line, block) in blocks.iter().enumerate() {
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
            band.center().x - galley.rect.width() / 2.0,
            top + line as f32 * LINE_H,
        );
        super::announce(
            ui,
            Rect::from_min_size(at, vec2(galley.rect.width().max(1.0), LINE_H)),
            ("block", line),
            &view::plain(block),
        );
        painter.galley(at, galley, theme.text);
    }
}

/// The weight the theme asks for a run, at the one size a block is set in.
///
/// There used to be a second size here: `Emphasis::Strong` was drawn at a
/// seventeen-point headline. Fluent carries strength with weight and not with
/// scale - `body1` against `body1Strong` is 400 against 600 at the same 14 -
/// and Ueli's own empty state is one body-sized line. So the size is no
/// longer a property of the run, and emphasis is the theme's business alone.
fn weight_of(theme: &Theme, run: &Run) -> eframe::egui::FontId {
    theme::font(theme::SIZE_BODY, theme.weight(run.emphasis))
}

// -- following the selection ------------------------------------------------

/// Moves the view by the least it takes to put `rect` on screen.
///
/// `align: None` is `block: "nearest"` exactly: scroll down if the row is
/// below the band, up if it is above, and not at all if it is already on it.
/// Any other alignment would yank the selected row to the middle of the list
/// on every arrow press.
///
/// It lands in one frame because the scroller was built with `animated(false)`
/// - see [`show`].
fn bring_into_view(ui: &Ui, rect: Rect) {
    ui.scroll_to_rect(rect, None);
}

/// Whether the selection moved since the last frame this body was drawn.
///
/// The whole of `block: "nearest"` is in *when* it is asked for. egui's
/// `scroll_to_rect` is a request the scroller honours on the frame it is made,
/// and a request made on every frame is a view pinned to the selection: the
/// wheel would move the list and the next frame would put it straight back.
///
/// So the last rank is kept in egui's own per-frame store, keyed by the body
/// it belongs to, and the request is made only when the two differ. The first
/// frame after a summon has nothing stored and therefore follows, which is
/// right - a panel that opens with the fourth row selected should open showing
/// it.
fn selection_changed(ui: &Ui, what: &'static str, selected: Option<usize>) -> bool {
    let id = Id::new(("files-follow", what));
    ui.data_mut(|data| {
        let last: Option<Option<usize>> = data.get_temp(id);
        data.insert_temp(id, selected);
        last != Some(selected)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every body keeps its own scroll offset, which needs every body to have
    /// its own number.
    #[test]
    fn every_body_scrolls_separately() {
        let mut seen: Vec<u8> = [
            Content::Results,
            Content::Recent,
            Content::Shares,
            Content::Empty,
            Content::Aliases,
        ]
        .into_iter()
        .map(discriminant)
        .collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 5, "two bodies share a scroll offset");
    }

    /// The two bodies that are lists of somebody's own things get a caption;
    /// the one that is a sentence does not.
    #[test]
    fn only_a_list_gets_a_caption() {
        assert!(heading_of(Content::Results).is_some());
        assert!(heading_of(Content::Recent).is_some());
        assert!(heading_of(Content::Shares).is_some());
        assert!(heading_of(Content::Aliases).is_some());
        assert!(heading_of(Content::Empty).is_none());
    }
}
