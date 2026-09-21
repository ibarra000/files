//! The panel.
//!
//! Five bands in Ueli's arrangement - a header, a hairline, the content, a
//! hairline, a footer - inside a window that is six hundred by four hundred
//! and never moves. Everything the panel *says* comes from [`crate::view`];
//! what is left here is arrangement.
//!
//! # The content band is the only thing that scrolls
//!
//! This is the change the whole rewrite turns on. The panel used to be as
//! tall as its result count, with the state machine holding a twelve-row
//! window over `hits` and the renderer drawing `hits[window]` - so the wheel
//! could not be used for scrolling (it walked the selection instead, which is
//! why it was unbound), the footer had to say "1-12 of 300" because the other
//! 288 were unreachable, and every change to the row count was a
//! `SetWindowPos`.
//!
//! The bands are constants now and the content between them is an
//! [`egui::ScrollArea`] holding the whole list. The wheel scrolls the view,
//! the selection is brought into view only when it is off it and only by as
//! much as it takes, and the window does not move at all.
//!
//! # One list, one field, one way in
//!
//! The terminal build had a five-way focus, because a terminal has one pane
//! and everything had to take turns in it. A window does not, so:
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

pub mod actions;
pub mod chip;
pub mod field;
pub mod footer;
pub mod icons;
pub mod list;
pub mod row;

use eframe::egui::{CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::time::{Instant, SystemTime};

use crate::app::state::pointer::Intent;
use crate::app::state::{AppState, EmptyReason};
use crate::gui::anim::Content;
use crate::gui::theme::{self, Theme};
use crate::gui::window::Backdrop;

/// Which of the four bodies the state calls for.
///
/// The one thing the view still measures. There used to be a `Measured`
/// alongside it carrying a height, a row count and the y of the selected row;
/// the window is a fixed size, the scroller holds however many rows there are,
/// and a row paints its own highlight - so what is left is which body.
pub fn body_of(state: &AppState) -> Content {
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
    // What an empty box shows, when there is anything to show on it. Ueli's
    // favourites; see `AppState::showing_aliases` for why the recent codes
    // are deliberately not here and this is.
    if state.showing_aliases() {
        return Content::Aliases;
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
pub(crate) fn empty_reason(state: &AppState) -> EmptyReason {
    state.empty_reason.clone().unwrap_or(EmptyReason::NoQuery)
}

/// The two bands the window may be dragged by.
///
/// Ueli's policy, said out loud: its header and footer carry
/// `-webkit-app-region: drag` and its content does not. The panel used to
/// take `ui.max_rect()` and rely on every widget registered afterwards to
/// claim the pixels it wanted, which made the handle "whatever nothing else
/// asked for" and cost nothing to maintain.
///
/// That stops working the moment the content band is a scroller. The gaps
/// between rows belong to the [`egui::ScrollArea`], and a press in one of
/// them has to scroll the list rather than carry the window off across the
/// desktop. So the handle is named rather than left over.
///
/// The header is still *layered* rather than cut out: the search box is
/// registered on top of it and takes its own presses, which is what leaves
/// the ten points of air around the box draggable without anything having to
/// compute where that air is.
pub fn handles(rect: Rect) -> (Rect, Rect) {
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), theme::HEADER_H));
    let footer = Rect::from_min_size(
        pos2(rect.left(), rect.bottom() - theme::FOOTER_H),
        vec2(rect.width(), theme::FOOTER_H),
    );
    (header, footer)
}

/// Draws the whole panel and reports everything the pointer did to it.
///
/// A `Vec` rather than an `Option`: one frame can carry both a hover and a
/// click, and dropping either would mean a row that lights up only after it
/// has been clicked.
pub fn show(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    content: Content,
    backdrop: Option<Backdrop>,
    now: Instant,
    wall: SystemTime,
) -> Vec<Intent> {
    let rect = ui.max_rect();
    paint_surface(ui, theme, rect, backdrop);

    let mut cursor = rect;
    let header = take(&mut cursor, theme::HEADER_H);
    let mut intents = field::show(ui, state, theme, header);

    let upper = take(&mut cursor, theme::DIVIDER);
    let band = take_bottom(&mut cursor, theme::FOOTER_H);
    let lower = take_bottom(&mut cursor, theme::DIVIDER);

    rule(ui, theme, upper);
    rule(ui, theme, lower);

    intents.extend(footer::show(ui, state, theme, band, now, wall));
    intents.extend(list::show(ui, state, theme, cursor, content, wall));
    // Last, and over everything: the menu floats above the footer that opens
    // it and the list it is about.
    intents.extend(actions::show(ui, state, theme, rect));
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
pub(crate) fn announce(
    ui: &Ui,
    rect: Rect,
    what: impl std::hash::Hash + std::fmt::Debug,
    text: &str,
) {
    if text.trim().is_empty() {
        return;
    }
    let response = ui.interact(rect, Id::new(("files-a11y", what)), Sense::hover());
    let text = text.to_owned();
    response.widget_info(|| {
        eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Label, true, &text)
    });
}

/// The panel's own background.
///
/// Skipped entirely when the compositor granted a backdrop: painting a fill
/// behind acrylic is painting over it. On the fallback path this fill *is* the
/// depth, so it is drawn.
///
/// # Square, and with no outline
///
/// It used to be rounded and stroked, and both were the same mistake. Three
/// things were drawing the panel's edge and they did not agree about where it
/// was: DWM clips the window at `DWMWCP_ROUND` in *device pixels*, and this
/// filled and stroked at eight *egui points*. Points are device pixels only
/// while `pixels_per_point` is exactly the monitor's scale - not under a zoom
/// factor, not in the frames either side of a `WM_DPICHANGED`, not while the
/// window straddles two monitors scaled differently. Wherever the two curves
/// parted, a sliver of the fill sat outside the clip or a sliver of ground
/// sat inside it, which is the misaligned edge that got reported.
///
/// A square fill under a rounded clip is correct at every scale, because the
/// clip is the corner and nothing else has an opinion about it. The outline
/// is DWM's again too, and getting it back was a deletion in
/// [`crate::gui::window`] rather than anything added here. This is also
/// exactly what Ueli does: `roundedCorners: true` and no CSS radius on the
/// root element.
fn paint_surface(ui: &Ui, theme: &Theme, rect: Rect, backdrop: Option<Backdrop>) {
    if backdrop != Some(Backdrop::Compositor) {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme.surface);
    }
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

/// The hairline between two bands.
///
/// Edge to edge, which is what a band separator is. It used to be inset by the
/// panel's horizontal padding, so it read as a rule drawn *on* one surface
/// rather than as the join between two.
fn rule(ui: &Ui, theme: &Theme, band: Rect) {
    ui.painter().hline(
        band.x_range(),
        band.center().y,
        Stroke::new(theme::DIVIDER, theme.edge),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::AppEvent;
    use crate::app::key::{Key, KeyEvent, Mods};
    use crate::app::state::pointer::Intent;
    use crate::config::Settings;
    use crate::search::matcher::Hit;
    use std::sync::Arc;

    /// Puts the cursor on `row` the way a click does.
    ///
    /// Through the real intent rather than by assigning `selected_path`, so
    /// the state follows the cursor exactly as it does in the program.
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
        assert_eq!(body_of(&state()), Content::Empty);
    }

    /// The recent codes are behind the Up arrow, and only behind it.
    #[test]
    fn the_recent_codes_appear_only_after_the_up_arrow() {
        let mut state = state();
        state.history.record("11-D-0704");

        // Unasked for. These used to be what an empty field showed, so every
        // summon of an empty panel put the job codes this person had looked up
        // in front of whoever was standing behind them.
        assert_eq!(body_of(&state), Content::Empty);

        state.update(
            AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)),
            std::time::Instant::now(),
        );
        assert_eq!(body_of(&state), Content::Recent);
    }

    #[test]
    fn typing_something_that_matches_shows_the_results() {
        assert_eq!(body_of(&with_hits(3)), Content::Results);
    }

    #[test]
    fn typing_something_that_matches_nothing_shows_why() {
        let mut state = state();
        state.input.set_text("zzzz");
        assert_eq!(body_of(&state), Content::Empty);
    }

    /// The drive picker replaces the body rather than floating over it, which
    /// is what makes it the one thing left that borrows it.
    #[test]
    fn the_drive_picker_replaces_whatever_was_there() {
        let mut state = with_hits(3);
        assert_eq!(body_of(&state), Content::Results);

        state.picking_share = true;
        assert_eq!(body_of(&state), Content::Shares);
    }

    /// A search that matched four hundred files is four hundred rows now.
    ///
    /// This used to assert the opposite - that the panel never asked for more
    /// rows than it had room for - because the renderer drew a window over
    /// the list and anything past it was unreachable. The content band
    /// scrolls, so the whole list is on it and the only cap left is
    /// `MAX_RESULTS`.
    #[test]
    fn a_long_result_list_is_all_of_it() {
        for n in [0, 1, 7, 8, 9, 400] {
            let state = with_hits(n);
            assert_eq!(state.hits.len(), n);
            assert_eq!(
                body_of(&state),
                if n == 0 {
                    Content::Empty
                } else {
                    Content::Results
                },
                "{n} hits"
            );
        }
    }

    /// An untouched panel has nothing in its body.
    ///
    /// There used to be a `Content::Quiet` for this, which dropped the
    /// footer as well. What makes the first screen empty now is one step
    /// further out: `view::empty` answers `NoQuery` with no blocks at all,
    /// so the body is `Empty` and there is nothing in it.
    #[test]
    fn an_untouched_panel_has_nothing_in_its_body() {
        let mut state = state();
        settle_index(&mut state);
        assert_eq!(body_of(&state), Content::Empty);
        assert!(
            crate::view::empty::view(&empty_reason(&state), state.input.text()).is_empty(),
            "the first screen has something on it"
        );
    }

    /// An empty box shows the shortcuts somebody configured, and nothing at
    /// all when there are none.
    ///
    /// Ueli's favourites. The second half is the important one: `b719a20`
    /// stripped the first screen to a search box and nothing else, and what
    /// it removed was five lines of *instructions*. A list somebody wrote
    /// themselves is not instructions, and a machine with no aliases still
    /// gets exactly what that commit left behind.
    #[test]
    fn an_empty_box_shows_the_shortcuts_and_only_if_there_are_any() {
        let mut state = state();
        settle_index(&mut state);
        assert_eq!(body_of(&state), Content::Empty, "nothing is configured");

        state.settings.aliases =
            std::sync::Arc::new(crate::alias::Aliases::new(vec![crate::alias::Alias {
                name: "pw".into(),
                code: "11-D-0704".into(),
                note: None,
            }]));
        assert_eq!(body_of(&state), Content::Aliases);
    }

    /// And the codes used before are still behind the Up arrow, whatever
    /// else is on the first screen.
    ///
    /// The line this feature is drawn against: an alias is a record of
    /// nothing and a search history is a record of everything this person
    /// has looked for, and a panel summoned over somebody's shoulder must
    /// not put the second on screen unasked.
    #[test]
    fn the_shortcuts_do_not_bring_the_recent_codes_with_them() {
        let mut state = state();
        settle_index(&mut state);
        state.settings.aliases =
            std::sync::Arc::new(crate::alias::Aliases::new(vec![crate::alias::Alias {
                name: "pw".into(),
                code: "11-D-0704".into(),
                note: None,
            }]));
        state.history.record("22-A-1234");

        assert_eq!(body_of(&state), Content::Aliases);
        state.update(
            AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)),
            Instant::now(),
        );
        assert_eq!(body_of(&state), Content::Recent, "Up is still the way in");
    }

    /// And something worth saying still reaches the footer, which is what
    /// the quiet panel used to be able to swallow.
    #[test]
    fn something_to_say_is_not_lost() {
        let mut state = state();
        settle_index(&mut state);

        // A real one, raised the way the program raises it.
        state.update(
            crate::app::event::AppEvent::Clipboard(crate::app::event::ClipboardMsg::Copied {
                chars: 9,
            }),
            Instant::now(),
        );
        assert!(state.toast.is_some(), "the fixture raised no toast");
        assert_eq!(body_of(&state), Content::Empty);
    }

    /// Selecting a row does not change which body is on screen, which is the
    /// one thing `body_of` could get wrong now that it is the whole of the
    /// measurement.
    #[test]
    fn selecting_a_row_leaves_the_body_alone() {
        let mut state = with_hits(300);
        assert_eq!(body_of(&state), Content::Results);
        select(&mut state, 0);
        assert_eq!(body_of(&state), Content::Results);
        select(&mut state, 299);
        assert_eq!(body_of(&state), Content::Results);
    }

    /// The bands come out of the panel in the order they are drawn in, and
    /// the content is what is left between them.
    #[test]
    fn the_bands_partition_the_panel() {
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_W, theme::PANEL_H));
        let mut cursor = panel;
        let header = take(&mut cursor, theme::HEADER_H);
        let upper = take(&mut cursor, theme::DIVIDER);
        let footer = take_bottom(&mut cursor, theme::FOOTER_H);
        let lower = take_bottom(&mut cursor, theme::DIVIDER);

        assert_eq!(header.top(), panel.top());
        assert_eq!(upper.top(), header.bottom());
        assert_eq!(cursor.top(), upper.bottom());
        assert_eq!(cursor.bottom(), lower.top());
        assert_eq!(lower.bottom(), footer.top());
        assert_eq!(footer.bottom(), panel.bottom());
        assert_eq!(
            cursor.height(),
            theme::CONTENT_H,
            "the content band is not what the constant says it is"
        );
    }

    /// The header and the footer are handles; the list between them is not.
    #[test]
    fn the_drag_handles_are_the_header_and_the_footer() {
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_W, theme::PANEL_H));
        let (header, footer) = handles(panel);

        assert!(header.contains(pos2(300.0, 5.0)), "the header is not one");
        assert!(
            footer.contains(pos2(300.0, theme::PANEL_H - 5.0)),
            "the footer is not one"
        );

        // The middle of the list, which belongs to the scroller.
        let middle = pos2(300.0, theme::PANEL_H / 2.0);
        assert!(
            !header.contains(middle) && !footer.contains(middle),
            "a press in the list would carry the window off"
        );
    }
}
