//! Colours, type and measurements.
//!
//! The palette is carried over from the terminal build's `ui::theme`, which
//! was tuned against the complaint that actually arrived - "the grey is hard to
//! read" - under the fluorescent light of the office this runs in. The hues are
//! the same; what is new is a light variant, because a window follows the
//! system theme and a terminal did not.
//!
//! # Colour is never the only carrier
//!
//! Roughly one man in twelve cannot tell the green from the red, and the line
//! that says a drive is unreachable is the one line here that absolutely has to
//! land. Every tone therefore has a glyph as well - the same five marks the
//! terminal used - and the selected row has a bar, not merely a tint.
//!
//! # The contrast rules are tests, not intentions
//!
//! Every ratio this file claims is asserted at the bottom of it. A palette
//! whose legibility lives in a comment is a palette that drifts the first time
//! somebody nudges a hue, and this one is read by people of every age for hours
//! a day.

use eframe::egui::epaint::Shadow;
use eframe::egui::{Color32, CornerRadius, FontId, Rect};

use crate::config::ResultLayout;
use crate::view::Emphasis;
use crate::view::status::Tone;

// -- measurements -----------------------------------------------------------

/// The panel, in points, and it never changes.
///
/// Ueli's launcher window, to the point. It used to be 720 wide and as tall
/// as the result count made it, which is what paid for `Frame::resize`, the
/// deadband, the resize budgets in `tests/jitter.rs` and the whole
/// measure-then-retarget-then-ask-the-window-system loop. A window that holds
/// still needs none of that, and the list scrolls inside it instead.
pub const PANEL_W: f32 = 600.0;
pub const PANEL_H: f32 = 400.0;

/// The air inside the header, and around the list.
///
/// Ten points, which is the padding Ueli gives both. Small for a window this
/// size, and deliberately so: the list is what somebody is here for and every
/// point of margin is a point the list has not got.
pub const BAND_PAD: f32 = 10.0;

/// The search box. Fluent's `large` input, which is what Ueli asks for.
pub const INPUT_H: f32 = 40.0;

/// The header: the search box with air above and below it.
pub const HEADER_H: f32 = BAND_PAD * 2.0 + INPUT_H;

/// The footer, which is tighter than the header because its controls are.
pub const FOOTER_H: f32 = 40.0;
/// The air inside the footer.
pub const FOOTER_PAD: f32 = 8.0;

/// The hairline between two bands.
///
/// A constant rather than a bare `1.0` at three call sites, because it is
/// taken out of the content band's height as well as painted, and the two
/// have to be the same number.
pub const DIVIDER: f32 = 1.0;

/// A group's caption, and the air under it.
pub const HEADING_H: f32 = 21.0;

/// One result, on one line: an icon, a name and a badge.
pub const ROW_COMPACT_H: f32 = 36.0;
/// One result, with the folder under the name.
pub const ROW_DETAILED_H: f32 = 52.0;
/// The gap between a row's edge and its text.
pub const ROW_PAD_X: f32 = 12.0;

/// How tall a row is, in the layout that is switched on.
pub const fn row_h(layout: ResultLayout) -> f32 {
    match layout {
        ResultLayout::Compact => ROW_COMPACT_H,
        ResultLayout::Detailed => ROW_DETAILED_H,
    }
}

/// What one row costs the list, including the air under it.
pub const fn row_pitch(layout: ResultLayout) -> f32 {
    row_h(layout) + ROW_GAP
}

/// Everything between the two hairlines: the scroller, and nothing else.
pub const CONTENT_H: f32 = PANEL_H - HEADER_H - FOOTER_H - DIVIDER * 2.0;

/// A full page of rows, under its caption, has to fit the content band.
///
/// The direction of this used to be the other way round: the window was made
/// as tall as the row count asked for, so it could not be wrong. Fixed, the
/// two are independent, and a page size one too large is a `PageDown` that
/// scrolls past rows nobody saw. Checked at compile time so it cannot be.
///
/// The gap is counted between rows and not after the last one, which is what
/// `gap` means.
const fn page_h(layout: ResultLayout, rows: usize) -> f32 {
    BAND_PAD * 2.0 + HEADING_H + row_pitch(layout) * rows as f32 - ROW_GAP
}

const _: () = assert!(page_h(ResultLayout::Compact, crate::config::VISIBLE_ROWS) <= CONTENT_H);
const _: () =
    assert!(page_h(ResultLayout::Detailed, crate::config::VISIBLE_ROWS_DETAILED) <= CONTENT_H);

/// And they have to fit *well*: a page that left a row's worth of nothing
/// under it would mean `PageDown` moved less than a screen.
const _: () =
    assert!(CONTENT_H - page_h(ResultLayout::Compact, crate::config::VISIBLE_ROWS) < ROW_COMPACT_H);
const _: () = assert!(
    CONTENT_H - page_h(ResultLayout::Detailed, crate::config::VISIBLE_ROWS_DETAILED)
        < ROW_DETAILED_H
);

/// The accent bar down the selected row.
pub const MARKER_W: f32 = 3.0;

/// Matches the radius Windows 11 gives an ordinary window.
pub const PANEL_RADIUS: u8 = 8;
pub const ROW_RADIUS: u8 = 4;
pub const CHIP_RADIUS: u8 = 4;

// -- type -------------------------------------------------------------------

/// The system's own typeface, in the two weights this draws with.
///
/// Loaded from disk rather than bundled: shipping Segoe UI would be a licence
/// violation, and a Windows without it is a Windows that cannot boot.
pub const FONT_FILES: [(&str, &str); 2] = [
    ("segoe", r"C:\Windows\Fonts\segoeui.ttf"),
    ("segoe-bold", r"C:\Windows\Fonts\segoeuib.ttf"),
];

/// Preferred over [`FONT_FILES`] where the machine has them.
///
/// Segoe UI Variable is Windows 11's redraw of Segoe, cut specifically to stay
/// crisp across sizes: `Display` for large text, `Text` for small, where the
/// original has one set of outlines doing both jobs. Tried first and skipped in
/// silence on Windows 10, which has neither file.
///
/// Same two names, so nothing downstream knows which of the two files it got.
pub const VARIABLE_FONT_FILES: [(&str, &str); 2] = [
    ("segoe", r"C:\Windows\Fonts\SegUIVar.ttf"),
    ("segoe-bold", r"C:\Windows\Fonts\SegUIVarB.ttf"),
];

/// The tail of every family: the glyphs Segoe UI has not got.
///
/// Read from `C:\Windows\Fonts` for the same reason and by the same rule as
/// [`FONT_FILES`] above - these are fonts the machine already has, and shipping
/// a copy would be both a licence violation and a megabyte.
///
/// That megabyte is what this replaces. `eframe`'s `default_fonts` embedded
/// Hack, NotoEmoji, Ubuntu-Light and emoji-icon-font - 1.41 MB - in every
/// binary in this crate, and the only thing the panel ever used them for was
/// this tail: so that an emoji in a filename came out as a character rather
/// than as a box. The system fonts buy the same thing for nothing.
///
/// Symbol before emoji, deliberately. `seguisym` covers arrows, box-drawing,
/// mathematical operators and the dingbats that turn up in a filename;
/// `seguiemj` covers the pictographs. A character in both should be drawn as a
/// glyph rather than as a picture.
pub const FALLBACK_FILES: [(&str, &str); 2] = [
    ("segoe-symbol", r"C:\Windows\Fonts\seguisym.ttf"),
    ("segoe-emoji", r"C:\Windows\Fonts\seguiemj.ttf"),
];

/// What the user is typing: the biggest thing on screen, because it is the
/// only thing they are doing.
pub const SIZE_INPUT: f32 = 22.0;
/// A filename.
pub const SIZE_ROW: f32 = 15.0;
/// The folder beside it, and the status line.
pub const SIZE_SMALL: f32 = 13.0;
/// A key name in a shortcut chip, a group caption, and a menu row.
pub const SIZE_CHIP: f32 = 12.0;
/// The first-run headline in the empty state.
pub const SIZE_HEADLINE: f32 = 17.0;

// -- colour -----------------------------------------------------------------

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// A colour that lets what is behind it through.
///
/// Takes straight (un-premultiplied) channels, because that is how a colour is
/// picked and written down. `Color32` stores them premultiplied, and writing
/// the premultiplied form by hand is how a surface ends up brighter than its
/// own alpha allows - which is not a colour at all.
pub fn tint(hex: u32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, alpha)
}

/// Every colour the panel is drawn with.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub dark: bool,

    /// What the panel paints when the compositor will not supply a backdrop.
    ///
    /// Translucent on purpose: on the fallback path this is all the depth
    /// there is, and an opaque rectangle over somebody's drawing reads as a
    /// dialog rather than as an overlay.
    pub surface: Color32,
    /// The hairline at the panel's edge, and the rules between its bands.
    pub edge: Color32,

    /// A filename, and body text generally.
    pub text: Color32,
    /// A heading, or the selected row.
    pub strong: Color32,
    /// Labels, folders, the status line.
    pub dim: Color32,
    /// Rules and separators. Never text.
    pub faint: Color32,
    /// The one hue that means "this".
    pub accent: Color32,
    /// The code being typed.
    pub input: Color32,
    /// Where the caret is.
    pub caret: Color32,

    /// The row the arrows are on.
    ///
    /// A *tint*, not a colour. Every background in this group is translucent
    /// and painted over the panel's own surface, which is itself translucent
    /// over the desktop. An opaque fill looks right over a dark window and
    /// disappears entirely over a bright one - the panel's surface lightens
    /// with what is behind it and the fill does not follow, so on the painted
    /// fallback path the highlight silently stops existing. A tint tracks its
    /// ground by construction.
    pub selection: Color32,
    /// The row under the pointer. Strictly weaker than [`Self::selection`]:
    /// Enter opens the selection, and the two must never be confusable.
    pub hover: Color32,
    /// The run the user is about to copy out of the search field.
    pub text_selection: Color32,
    /// The matched substring, so the eye lands on why a row is there.
    pub match_run: Color32,

    /// A surface recessed into the panel: the search field.
    ///
    /// A shade off [`Self::surface`] and no more. The code being typed sits on
    /// it, so it is a text ground and is held to the same ratio as one.
    pub well: Color32,

    /// A setting row in the settings window.
    ///
    /// The one surface here that is not part of the panel, and the reason it
    /// exists: the settings window is a *document*, drawn with widgets rather
    /// than painted, and its layout is carried by a stack of small tiles - one
    /// per setting - rather than by shading. So it needs a ground one step off
    /// the window's, which [`Self::well`] cannot be because a text box sits
    /// recessed *inside* one of these tiles and the two have to differ.
    ///
    /// Opaque, unlike most of this group. These windows are read for minutes
    /// at a time and nothing shows through them.
    ///
    /// Lighter than the window on a dark theme and darker on a light one,
    /// which is the direction a tile lifts in each.
    pub card: Color32,

    /// The fill behind a control: a drop-down, a text box, a button.
    ///
    /// Three of them rather than one, because egui asks for a colour per
    /// interaction state up front rather than tinting one at paint time, and a
    /// hover that is computed by compositing would have to be computed
    /// identically in four places.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,

    /// A surface that means "this one": the nav indicator, a switch that is on,
    /// a primary button.
    ///
    /// Not [`Self::accent`], which is a *text* colour one shade off
    /// [`Self::text`] and would be invisible as a fill. This is the other end
    /// of the range, and [`Self::accent_fg`] is what reads on it.
    pub accent_fill: Color32,
    pub accent_fg: Color32,

    /// A success foreground: a hotkey that parses, a check that passed.
    ///
    /// [`Self::tones`] has Warn and Bad with hues of their own and Good
    /// without one, because on the status line a tick carries it and colour
    /// would be the second signal for something nobody needs told twice. A
    /// form is not the status line: a field that validates as you leave it has
    /// no glyph and no room for a sentence, and green against the amber and
    /// red already here is the whole message.
    pub good: Color32,

    /// The light that a raised surface catches, up and to the left.
    ///
    /// Soft UI models an element as pressed out of the panel rather than drawn
    /// on top of it, so there is no border and no fill difference - the shape
    /// is carried entirely by a pair of shadows, one the colour of the light
    /// and one the colour of its absence. Take either away and the element
    /// stops reading as a thing and becomes a smudge.
    pub lit: Color32,
    /// And the shadow it casts, down and to the right.
    pub shade: Color32,

    /// A key name: ` Enter `.
    pub chip_bg: Color32,
    pub chip_fg: Color32,

    tones: [Color32; 5],
}

/// The small marks that say what a line means without using colour.
pub const TONE_GLYPH: [char; 5] = ['\u{2022}', '\u{2219}', '\u{2713}', '\u{25B2}', '\u{00D7}'];

impl Theme {
    /// The dark palette: the same warm scheme on a low ground.
    ///
    /// Softened from the near-black `0x10141C` and cold blue it carried over
    /// from the terminal build. It shares the light theme's hue family so the
    /// two read as one program rather than two, and it is lifted well off
    /// black so the surface has somewhere to put a highlight.
    pub fn dark() -> Self {
        let text = rgb(0xDCDCDC);
        let strong = rgb(0xFFFFFF);
        let dim = rgb(0xA6A6A6);
        // Rules and borders. Must clear 3:1 - WCAG 1.4.11 asks it of a
        // boundary you are meant to be able to see - and must still be
        // fainter than `dim`, or a separator competes with a label.
        let faint = rgb(0x828282);
        let accent = rgb(0xF0F0F0);
        Self {
            dark: true,
            surface: tint(0x1E1E1E, 0xF0),
            edge: tint(0xC8C8C8, 0x3D),
            text,
            strong,
            dim,
            faint,
            accent,
            input: rgb(0xFAFAFA),
            caret: accent,
            selection: tint(0xFFFFFF, 0x26),
            hover: tint(0xFFFFFF, 0x14),
            text_selection: tint(0xFFFFFF, 0x3A),
            // Brightest and bold, rather than a hue of its own. `row::show`
            // already draws a matched run in `Weight::Bold`.
            match_run: strong,
            // Weaker than the light theme's pair. On a low ground the eye has
            // far less headroom above the surface, so the same strength reads
            // as a glow rather than as a shape.
            well: tint(0x141414, 0xF4),
            // A tile lifts *off* a dark ground, so it is lighter than the
            // window; the well below goes the other way, and the gap between
            // the two is what makes a text box read as recessed into a card
            // rather than as another card.
            card: rgb(0x282828),
            control: rgb(0x333333),
            control_hover: rgb(0x3A3A3A),
            control_active: rgb(0x2B2B2B),
            // Near-white and near-black, not a hue. The palette gave up its
            // colours to `TONE_GLYPH` and to weight, and a blue filled bar
            // would be the only saturated thing in the program.
            accent_fill: rgb(0xE6E6E6),
            accent_fg: rgb(0x1A1A1A),
            good: rgb(0x7FC98A),
            lit: tint(0xFFFFFF, 0x10),
            shade: tint(0x000000, 0x2E),
            // Opaque, unlike every other background here. A key cap is the one
            // element that is *not* meant to track its ground.
            chip_bg: rgb(0x3C3C3C),
            chip_fg: rgb(0xF0F0F0),
            // Busy and Good give up their hues to `TONE_GLYPH`; Warn and Bad
            // keep theirs, because a failure has to be unmissable and they are
            // now the only colour on the panel.
            tones: [text, dim, strong, rgb(0xEFC05A), rgb(0xF09184)],
        }
    }

    /// The light palette: warm sand, a sage accent, soft edges.
    ///
    /// Not the dark one inverted - inverting a palette tuned for a dark ground
    /// gives washed-out pastels - but the same *roles* re-picked against a warm
    /// near-white surface at the same contrast ratios, which is what the tests
    /// at the bottom of this file actually check.
    ///
    /// # Why the type is darker than the reference it came from
    ///
    /// The look this is drawn from is soft-UI: surfaces shaded rather than
    /// outlined, everything a step or two from the ground, nothing shouting.
    /// The *surfaces* are exactly that. The type is not, and deliberately.
    ///
    /// A neumorphic mood board picks its secondary greys around `#8F887C` and
    /// its accents around `#6F8F63`; on this ground those are 2.6:1 and 2.7:1,
    /// against the 4.5:1 the tests below hold every one of these to. This is a
    /// tool somebody reads all day, at a glance, over whatever window they had
    /// open - so the shading carries the style and the contrast stays where it
    /// was. Where the two disagree, the lever is the surface, never the text.
    pub fn light() -> Self {
        let text = rgb(0x333333);
        let strong = rgb(0x0D0D0D);
        let dim = rgb(0x565656);
        let faint = rgb(0x787878);
        let accent = rgb(0x1F1F1F);
        Self {
            dark: false,
            surface: tint(0xF0F0F0, 0xF0),
            edge: tint(0x3C3C3C, 0x33),
            text,
            strong,
            dim,
            faint,
            accent,
            input: rgb(0x141414),
            caret: accent,
            selection: tint(0x000000, 0x24),
            hover: tint(0x000000, 0x12),
            text_selection: tint(0x000000, 0x32),
            // Darkest and bold, rather than a hue of its own. `row::show`
            // already draws a matched run in `Weight::Bold`.
            match_run: strong,
            // Genuinely darker than the panel on both grounds. The warm pair
            // this replaces resolved *brighter* than the surface over black,
            // and passed only because `contrast` is direction-agnostic - so
            // `press`'s "a trough a shade darker than the panel" is true now.
            well: tint(0xD9D9D9, 0xF6),
            // Darker than the window, which is the direction a tile lifts on a
            // light ground, and lighter than the well below it for the reason
            // the dark theme's note gives.
            card: rgb(0xE6E6E6),
            control: rgb(0xFBFBFB),
            control_hover: rgb(0xF4F4F4),
            control_active: rgb(0xECECEC),
            accent_fill: rgb(0x2B2B2B),
            accent_fg: rgb(0xFAFAFA),
            good: rgb(0x1E6B33),
            lit: tint(0xFFFFFF, 0x9A),
            shade: tint(0xB4B4B4, 0x76),
            // Opaque, unlike every other background here. A key cap is the one
            // element that is *not* meant to track its ground.
            chip_bg: rgb(0xFFFFFF),
            chip_fg: rgb(0x2A2A2A),
            // Busy and Good give up their hues to `TONE_GLYPH`; Warn and Bad
            // keep theirs, because a failure has to be unmissable and they are
            // now the only colour on the panel.
            tones: [text, dim, strong, rgb(0x8A4B00), rgb(0xA32820)],
        }
    }

    pub fn of(dark: bool) -> Self {
        if dark { Self::dark() } else { Self::light() }
    }

    pub fn tone(self, tone: Tone) -> Color32 {
        self.tones[tone as usize]
    }

    pub fn glyph(self, tone: Tone) -> char {
        TONE_GLYPH[tone as usize]
    }

    /// What a [`Run`](crate::view::Run) from `src/view/` is drawn in.
    ///
    /// This is the whole of the contract between the words and their
    /// appearance: `view` names a meaning, and exactly one function turns it
    /// into a colour.
    pub fn emphasis(self, emphasis: Emphasis) -> Color32 {
        match emphasis {
            Emphasis::Body => self.text,
            Emphasis::Dim => self.dim,
            Emphasis::Strong => self.strong,
            Emphasis::Accent => self.accent,
            Emphasis::Tone(tone) => self.tone(tone),
        }
    }

    /// And what weight it is drawn in.
    ///
    /// The other half of the same contract, and load-bearing since the palette
    /// went grey: brightness alone carries about three steps before two of them
    /// are the same step to anybody not looking for the difference. `Accent` in
    /// particular is one shade off `text` now, and would mean nothing at all
    /// without this.
    pub const fn weight(self, emphasis: Emphasis) -> Weight {
        match emphasis {
            Emphasis::Body | Emphasis::Dim => Weight::Regular,
            Emphasis::Strong | Emphasis::Accent => Weight::Bold,
            // The two lines that have to land, in the one place where a glyph
            // and a colour were already not quite enough.
            Emphasis::Tone(Tone::Warn | Tone::Bad) => Weight::Bold,
            Emphasis::Tone(_) => Weight::Regular,
        }
    }
}

/// Scales a colour's alpha, for fading a whole body in or out.
///
/// Multiplicative rather than absolute, so a colour that was already
/// translucent - the surface, the edge - stays in proportion instead of
/// becoming opaque halfway through a fade.
pub fn faded(color: Color32, alpha: f32) -> Color32 {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha >= 1.0 {
        return color;
    }
    color.linear_multiply(alpha)
}

/// How far a raised surface is lifted off the panel, in points.
///
/// Small. The whole idea is an element a step out of the ground, not a card
/// floating over it, and a shadow long enough to notice is a shadow that reads
/// as a drop shadow instead.
pub const LIFT: f32 = 2.0;

/// And how soft the lift is.
pub const BLUR: f32 = 6.0;

/// The pair of shadows, in the given order, behind `rect`.
///
/// `Shadow::as_shape` fills the rectangle as well as feathering around it, so
/// both callers below paint a surface over the top afterwards. Without that the
/// two stack into a muddy patch and whatever wash goes on it reads as dirt.
fn shadows(painter: &eframe::egui::Painter, rect: Rect, r: u8, pairs: [(f32, Color32); 2]) {
    for (offset, colour) in pairs {
        painter.add(
            Shadow {
                offset: [offset as i8, offset as i8],
                blur: BLUR as u8,
                spread: 0,
                color: colour,
            }
            .as_shape(rect, radius(r)),
        );
    }
}

/// Draws `rect` as a surface pressed out of the panel.
///
/// A light shadow up and to the left, a dark one down and to the right, and
/// then the panel's own colour over the top: a soft-UI element is the *same*
/// colour as its ground and is legible only by the shading at its edges. Take
/// either shadow away and it stops reading as a thing and becomes a smudge.
///
/// This used to take the panel's entrance fade, so the shading arrived with
/// everything else rather than appearing once the panel had landed. There is
/// no entrance now, so there is nothing to arrive with.
pub fn raise(painter: &eframe::egui::Painter, theme: &Theme, rect: Rect, r: u8) {
    shadows(painter, rect, r, [(-LIFT, theme.lit), (LIFT, theme.shade)]);
    painter.rect_filled(rect, radius(r), theme.surface);
}

/// And `rect` as a surface pressed *into* it: the same pair, swapped, over a
/// slightly recessed fill.
///
/// The fill is what carries it. `Shadow` casts outwards and there is no inset
/// form, so the shading alone would put the light on the wrong side of an
/// element that is otherwise identical to its ground - and a trough that is a
/// shade darker than the panel is what says "put something here" anyway.
/// A key cap: [`raise`]'s shading over a fill of its own.
///
/// `raise` paints the panel's *own* colour, because a soft-UI surface is the
/// colour of its ground and is legible only by the shading at its edges. A key
/// cap is the one element here that is not - a key on a keyboard is a different
/// piece of plastic from the case - and with the palette down to greys a
/// tenth-alpha wash over the panel is not a piece of plastic, it is a stain.
pub fn cap(painter: &eframe::egui::Painter, theme: &Theme, rect: Rect, r: u8) {
    shadows(painter, rect, r, [(-LIFT, theme.lit), (LIFT, theme.shade)]);
    painter.rect_filled(rect, radius(r), theme.chip_bg);
}

pub fn press(painter: &eframe::egui::Painter, theme: &Theme, rect: Rect, r: u8) {
    shadows(painter, rect, r, [(LIFT, theme.lit), (-LIFT, theme.shade)]);
    painter.rect_filled(rect, radius(r), theme.well);
}

pub fn radius(r: u8) -> CornerRadius {
    CornerRadius::same(r)
}

/// The two weights, by role.
pub fn font(size: f32, weight: Weight) -> FontId {
    FontId::new(size, eframe::egui::FontFamily::Name(weight.family().into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Bold,
}

impl Weight {
    pub const fn family(self) -> &'static str {
        match self {
            Self::Regular => "segoe",
            Self::Bold => "segoe-bold",
        }
    }
}

pub mod icons;
pub mod style;
pub use icons::{ICON_INLINE, ICON_NAV, Icon, has_icon, icon_font};
pub use style::{
    CARD_PAD, CARD_RADIUS, CONTENT_PAD, CONTROL_H, CONTROL_RADIUS, GROUP_GAP, NAV_W, ROW_GAP,
    apply_style,
};

#[cfg(test)]
mod tests;
