//! The content band: a caption over one of the results, recent searches, aliases
//! or drive picker, in the panel's only [`ScrollArea`]; or a centred empty-state
//! message.
//!
//! The mouse wheel scrolls the viewport only; the selection moves with the
//! arrow keys. The selected row is scrolled into view by the minimum amount
//! (`align: None`), and only on frames where the selection changed, otherwise
//! the view would snap back after every wheel scroll. Scrolling is not
//! animated (see [`crate::gui::anim`]).

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align, Align2, Id, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder, pos2, vec2};
use std::time::SystemTime;

use super::row;
use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::anim::Content;
use crate::gui::theme::{self, Icon, Theme, Weight};
use crate::view::{self, Emphasis, Run};

/// A caption's left inset, smaller than a row's so it reads as a heading.
const HEADING_PAD_X: f32 = 5.0;

/// Draws the current body and returns the pointer intents it produced.
pub fn show(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    band: Rect,
    content: Content,
    wall: SystemTime,
) -> Vec<Intent> {
    // The empty state is a short message that cannot overflow, so it is drawn
    // directly on the band without a scroller.
    if content == Content::Empty {
        if let Some(block) = view::empty::view(&super::empty_reason(state), state.input.text()) {
            centred_block(ui, theme, band, &block);
        }
        return Vec::new();
    }

    let mut intents = Vec::new();
    // The explicit `id_salt` is required. An automatic id depends on how many
    // ids the parent has issued, which can vary between frames; the scroller
    // would then lose its offset.
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt("files-list")
            .max_rect(band)
            .layout(Layout::top_down(Align::Min)),
    );

    // Scrollbar style for this scroller only: flush to the edge with no
    // margins, unlike the settings window's style from `theme::style`. Set on
    // the child so it does not leak into the context.
    {
        let scroll = &mut child.spacing_mut().scroll;
        scroll.bar_width = theme::SCROLL_W;
        scroll.bar_inner_margin = 0.0;
        scroll.bar_outer_margin = 0.0;
        // Always visible and never overlaid on content.
        scroll.floating = false;
    }

    ScrollArea::vertical()
        // One scroll offset per body.
        .id_salt(("files-list", discriminant(content)))
        // The band has a fixed size regardless of content.
        .auto_shrink([false, false])
        // Instant scrolling, so a held arrow key does not outrun an eased
        // scroll. See [`crate::gui::anim`].
        .animated(false)
        .show(&mut child, |ui| {
            // Padding goes inside the scroller so the scrollbar sits on the
            // panel edge.
            let inner = ui.max_rect().shrink2(vec2(theme::BAND_PAD, 0.0));
            let mut body = ui.new_child(
                UiBuilder::new()
                    .id_salt("files-list-body")
                    .max_rect(inner)
                    .layout(Layout::top_down(Align::Min)),
            );
            // Row gaps are allocated explicitly; `item_spacing` would also add
            // space after the caption and the last row.
            body.spacing_mut().item_spacing.y = 0.0;

            // Allocated padding at both ends; see `space`.
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

/// Allocates vertical space in the list.
///
/// `Ui::add_space` only moves the cursor, but the scroller measures allocated
/// rects, so trailing padding must be allocated to count.
fn space(ui: &mut Ui, amount: f32) {
    ui.allocate_exact_size(vec2(ui.available_width(), amount), Sense::hover());
}

/// Draws the caption over a group.
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
        // `NeutralForeground4` at weight 500, which resolves to Semibold
        // because Segoe UI has no Medium.
        theme.caption,
    );
    super::announce(ui, rect, ("heading", text), text);
}

/// Returns the caption for a body; the empty state has none.
const fn heading_of(content: Content) -> Option<&'static str> {
    match content {
        Content::Results => Some("Results"),
        Content::Recent => Some("Recent searches"),
        Content::Aliases => Some("Shortcuts"),
        Content::Shares => Some("Drives"),
        Content::Empty => None,
    }
}

/// Returns a stable number per body, so each keeps its own scroll offset.
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
        // Checked once per frame, not per row.
        icons: theme::has_icon(ui.ctx(), Icon::File),
        // The search term's length, not the whole input's (`report ext:pdf`
        // matches on six bytes). `view::row::highlight` silently skips an
        // out-of-range length.
        query_len: state.query().term().len(),
    };
    let selected = state.selected_row();
    let follow = selection_changed(ui, "results", selected);
    let mut hovered = None;

    // Handles pointer interaction for one result, in either layout.
    let mut note = |ui: &Ui, rank: usize, response: eframe::egui::Response| {
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
    };

    let columns = state.columns();
    if columns == 1 {
        for (rank, hit) in state.hits.iter().enumerate() {
            if rank > 0 {
                space(ui, theme::ROW_GAP);
            }
            let response = row::show(ui, &style, hit, selected == Some(rank));
            note(ui, rank, response);
        }
    } else {
        // Row-major, matching `app::state::grid` navigation. Each line is cut
        // into cells, each drawn by `row::show` in its own child `Ui`, so rows
        // hit-test themselves. Column and line gaps are equal.
        let row_h = theme::row_h(style.layout);
        let gap = theme::ROW_GAP;
        for (line, chunk) in state.hits.chunks(columns).enumerate() {
            if line > 0 {
                space(ui, gap);
            }
            let width = ui.available_width();
            let cell_w = ((width - gap * (columns - 1) as f32) / columns as f32).max(0.0);
            let (_, band) = ui.allocate_space(vec2(width, row_h));
            for (i, hit) in chunk.iter().enumerate() {
                let rank = line * columns + i;
                let left = band.left() + i as f32 * (cell_w + gap);
                let cell = Rect::from_min_size(pos2(left, band.top()), vec2(cell_w, row_h));
                let response = ui
                    .scope_builder(UiBuilder::new().max_rect(cell), |ui| {
                        row::show(ui, &style, hit, selected == Some(rank))
                    })
                    .inner;
                note(ui, rank, response);
            }
        }
    }

    if hovered != state.hovered() {
        intents.push(Intent::Hover(hovered));
    }
    intents
}

/// Draws the recent-searches list shown by the Up arrow.
///
/// Styled unlike a result (accent weight, no folder or extension) so the two
/// kinds of row are distinguishable.
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
        // Selection band, drawn before the text, as for a result.
        if cursor == Some(rank) {
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
            theme.text,
        );
        // Recall by rank, so a click picks the clicked entry.
        if response.clicked() {
            intents.push(Intent::Recall(rank));
        }
    }
    intents
}

/// Draws the configured aliases, shown when the box is empty.
///
/// Styled like the recent searches, with the alias name on the left and the
/// search it stands for dimmed on the right.
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
        // The accessible name includes the alias's note, if any.
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
            theme.text,
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

/// Draws the drive picker that `F5` opens to choose which share to re-read.
///
/// Re-reading one share instead of all limits simultaneous passes over large
/// shares across many users.
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

        // Name on the left. The remaining runs (age, optional reason) are laid
        // out from the right edge backwards.
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

/// Draws the empty-state message centred on both axes of the band.
///
/// Placement lives here; `view::empty` supplies only the text.
fn centred_block(ui: &Ui, theme: &Theme, band: Rect, block: &view::Block) {
    let painter = ui.painter().clone();
    let mut layout = LayoutJob::default();
    for run in block {
        layout.append(
            &run.text,
            0.0,
            TextFormat {
                font_id: weight_of(theme, run),
                color: theme.emphasis(run.emphasis),
                ..Default::default()
            },
        );
    }
    let galley = painter.layout_job(layout);
    let at = pos2(
        band.center().x - galley.rect.width() / 2.0,
        band.center().y - galley.rect.height() / 2.0,
    );
    super::announce(
        ui,
        Rect::from_min_size(at, galley.rect.size()),
        ("block", 0usize),
        &view::plain(block),
    );
    painter.galley(at, galley, theme.text);
}

/// Returns the body-size font with the theme's weight for a run's emphasis.
///
/// Emphasis is carried by weight only, as in Fluent (`body1` 400 vs
/// `body1Strong` 600, both at 14).
fn weight_of(theme: &Theme, run: &Run) -> eframe::egui::FontId {
    theme::font(theme::SIZE_BODY, theme.weight(run.emphasis))
}

// -- following the selection ------------------------------------------------

/// Scrolls the minimum distance needed to put `rect` on screen.
///
/// `align: None` does nothing if `rect` is already visible. The scroll is
/// instant because the scroller uses `animated(false)`; see [`show`].
fn bring_into_view(ui: &Ui, rect: Rect) {
    ui.scroll_to_rect(rect, None);
}

/// Returns whether the selection changed since this body was last drawn.
///
/// Scroll requests are made only on change; a request every frame would pin
/// the view to the selection and defeat the mouse wheel. The last value is
/// kept in egui temp data per body; the first frame has none and so follows.
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

    #[test]
    fn only_a_list_gets_a_caption() {
        assert!(heading_of(Content::Results).is_some());
        assert!(heading_of(Content::Recent).is_some());
        assert!(heading_of(Content::Shares).is_some());
        assert!(heading_of(Content::Aliases).is_some());
        assert!(heading_of(Content::Empty).is_none());
    }
}
