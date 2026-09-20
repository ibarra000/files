//! The footer: what the program knows, and which keys do what.
//!
//! Ported across rather than rebuilt. Ueli's footer is a gear on the left and
//! a pair of buttons on the right - the default action with an `↵` chip, and
//! a `⋮` that opens everything else - and that is where this is going, but
//! the actions it would name do not exist yet. Until they do this is the line
//! it replaces: a tone glyph, a count, whatever the program has to say, and
//! the key hints laid out from the right.
//!
//! One thing did change, and it had to. The count used to read "1-12 of 300",
//! because the list was a twelve-row window over three hundred results and
//! the other 288 were unreachable - so where in them you were was a fact
//! somebody needed. The content band scrolls now and all three hundred are
//! reachable, which makes the range a description of the scrollbar. What is
//! left is the number: `300`.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align2, Color32, Id, Rect, Sense, Ui, pos2, vec2};
use std::time::{Instant, SystemTime};

use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::theme::{self, Theme, Weight};
use crate::view::{self, Emphasis};

/// The gap between two hint chips.
const CHIP_GAP: f32 = 8.0;

/// The gap between the result count and the status prose beside it.
const COUNT_GAP: f32 = 12.0;

/// Room kept for the status line, whether or not it has anything to say.
///
/// Wide enough for the longest transient - `Checking the drive…` - with a
/// warning longer than that left to truncate, which is what truncation is
/// for.
const STATUS_RESERVE: f32 = 120.0;

pub fn show(
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
        pos2(rect.left() + theme::FOOTER_PAD, rect.center().y),
        Align2::LEFT_CENTER,
        theme.glyph(status.tone),
        theme::font(theme::SIZE_SMALL, theme.weight(Emphasis::Tone(status.tone))),
        theme.tone(status.tone),
    );
    let mut status_left = rect.left() + theme::FOOTER_PAD + 16.0;

    // The reserved slot: drawn before the prose and outside its wrap width, so
    // it is the one part of this line that truncation cannot reach.
    //
    // Strong rather than dim. How much a code found is something somebody is
    // looking for, not a note about the state of the program.
    let found = view::status::found(state);
    if !found.is_empty() {
        let width = painter
            .layout_no_wrap(found.clone(), font.clone(), Color32::WHITE)
            .rect
            .width();
        painter.text(
            pos2(status_left, rect.center().y),
            Align2::LEFT_CENTER,
            &found,
            font.clone(),
            theme.text,
        );
        super::announce(
            ui,
            Rect::from_min_size(pos2(status_left, rect.top()), vec2(width, rect.height())),
            "found",
            &found,
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
    super::announce(
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

/// How much of the footer the key hints may have.
///
/// Everything the count did not take, less a *fixed* reserve for the status
/// line - not the width that line actually needs. A budget that tracked the
/// text would add and remove chips every time the phase changed, which is a
/// footer reflowing under somebody reading it. Fixed, it never moves.
fn chip_budget(rect: Rect, status_left: f32) -> f32 {
    (rect.right() - theme::FOOTER_PAD - status_left - CHIP_GAP * 2.0 - STATUS_RESERVE).max(0.0)
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
    super::announce(
        ui,
        Rect::from_min_max(
            pos2(rect.right() - theme::FOOTER_PAD - 1.0, rect.top()),
            pos2(rect.right() - theme::FOOTER_PAD, rect.bottom()),
        ),
        "chips",
        &view::hints::plain(&kept, labelled),
    );

    let mut intents = Vec::new();
    let mut x = rect.right() - theme::FOOTER_PAD;
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
