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

use crate::view::Emphasis;
use crate::view::status::Tone;

// -- measurements -----------------------------------------------------------

/// How wide the panel is, in points. Wide enough for a long job-code filename
/// and its folder side by side, narrow enough to read in one fixation.
pub const PANEL_W: f32 = 720.0;

/// The search field.
pub const FIELD_H: f32 = 64.0;
/// One result.
pub const ROW_H: f32 = 40.0;
/// The status and key hints.
pub const FOOTER_H: f32 = 44.0;

/// The most rows shown at once.
///
/// Re-exported rather than defined here: the arrows scroll a window over the
/// result list and the state machine owns that window, so the number belongs
/// where the state machine can reach it. See [`crate::config::VISIBLE_ROWS`].
pub use crate::config::VISIBLE_ROWS as MAX_ROWS;

/// Breathing room at the panel's edge.
pub const PAD_X: f32 = 16.0;
pub const PAD_Y: f32 = 8.0;
/// The gap between a row's edge and its text.
pub const ROW_PAD_X: f32 = 12.0;

/// The accent bar down the selected row.
pub const MARKER_W: f32 = 3.0;

/// Matches the radius Windows 11 gives an ordinary window.
pub const PANEL_RADIUS: u8 = 8;
pub const ROW_RADIUS: u8 = 6;
pub const CHIP_RADIUS: u8 = 4;

/// The tallest the panel ever gets: field, a full list, footer and the rules
/// between them.
pub const PANEL_MAX_H: f32 = FIELD_H + ROW_H * MAX_ROWS as f32 + FOOTER_H + PAD_Y * 2.0 + 2.0;

/// The pane beside the list that says what a result is.
///
/// Narrower than half the list, deliberately. It holds a file name, two short
/// facts and a handful of page names, and giving it more room would only let
/// the folder line run on - while taking width from the column the names
/// themselves are read in, which is the one that has to stay legible.
pub const PREVIEW_W: f32 = 380.0;

/// The panel with the pane attached.
pub const PANEL_WIDE_W: f32 = PANEL_W + PREVIEW_W;

/// The narrowest monitor that gets the pane rather than the popup.
///
/// The wide panel plus a margin, so the pane is only offered where the panel
/// still reads as something summoned over the work rather than something that
/// has taken the screen over. Below this the same words arrive as a popup
/// beside the row instead, which costs no width at all.
///
/// Measured against the monitor rather than the work area because it is a
/// question about the display, and a taskbar moving does not change whether a
/// 1920-wide screen can hold an 1100-wide panel.
pub const ROOM_FOR_PANE: f32 = PANEL_WIDE_W + 260.0;

/// How the panel is laid out, decided once per summon.
///
/// Per summon rather than per frame, and the difference is the whole reason
/// this is a stored decision rather than a function of the current width:
/// recomputed every frame it would flip while the panel is up, and recomputed
/// per selection it would change the *window's* width as the pointer moved,
/// which is the class of jitter `gui::frame` and `tests/jitter.rs` exist to
/// refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// List only. A hover brings the pane up over the rows.
    #[default]
    List,
    /// List with the pane permanently beside it.
    Pane,
}

impl Layout {
    /// Which layout a monitor of this width gets.
    ///
    /// `None` - no monitor reported - takes [`Layout::List`], which is the
    /// answer that fits everywhere.
    pub fn for_monitor(width: Option<f32>) -> Self {
        match width {
            Some(w) if w >= ROOM_FOR_PANE => Self::Pane,
            _ => Self::List,
        }
    }

    /// How wide the panel is in this layout.
    pub const fn width(self) -> f32 {
        match self {
            Self::List => PANEL_W,
            Self::Pane => PANEL_WIDE_W,
        }
    }

    pub const fn has_pane(self) -> bool {
        matches!(self, Self::Pane)
    }
}

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
/// A key name in a hint chip.
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
fn tint(hex: u32, alpha: u8) -> Color32 {
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

// -- the toolkit's own style ------------------------------------------------

/// A control's corner. Windows 11 rounds a button and a text box by four.
pub const CONTROL_RADIUS: u8 = 4;

/// The tile a setting is drawn on, and the padding inside it.
pub const CARD_RADIUS: u8 = 4;
pub const CARD_PAD: f32 = 12.0;

/// Between one setting and the next.
pub const ROW_GAP: f32 = 5.0;
/// Between one group of settings and the next. Large, and that is the point:
/// it is the only thing separating two groups, because a group heading is
/// smaller than the labels under it.
pub const GROUP_GAP: f32 = 40.0;
/// The margin the settings window keeps around its content.
pub const CONTENT_PAD: f32 = 20.0;
/// How wide the list of pages is.
pub const NAV_W: f32 = 260.0;

/// The shortest a control may be, so a row's right-hand column does not change
/// height with what is in it.
pub const CONTROL_H: f32 = 28.0;

/// Dresses egui's own widgets in this palette.
///
/// # Why there was no such function until now
///
/// Because until now nothing needed one. The panel does not use a single egui
/// widget: `gui::overlay`, `gui::row` and `gui::preview` between them call
/// `ui.interact` and then paint, and every colour they use comes from
/// [`Theme`] directly. A `Style` would have configured nothing.
///
/// The settings window is the opposite. It is a form, it is made of buttons,
/// boxes and drop-downs, and until this function existed every one of them was
/// drawn in egui's stock blue-grey - which is why a window in this program did
/// not look like this program.
///
/// # It is safe to apply to everything
///
/// Applied to the whole context rather than scoped to the settings viewport,
/// and that is sound precisely because of the paragraph above: there is no
/// egui widget on the panel for this to reach. `tests/panel.rs` and the image
/// snapshots beside it are the standing proof. Scoping it would mean applying
/// it inside the child viewport's own pass, which is a second place to keep in
/// step for no benefit anybody can see.
///
/// Both themes, like [`crate::gui::fonts::sharpen`]: egui keeps a `Style` per
/// light and dark and this program switches between them at runtime, so
/// setting only the active one would dress the window until somebody changed
/// theme.
pub fn apply_style(ctx: &eframe::egui::Context, theme: &Theme) {
    use eframe::egui::{Margin, Stroke, TextStyle, Vec2};

    let solid = |c: Color32| Color32::from_rgb(c.r(), c.g(), c.b());
    let hairline = Stroke::new(1.0, theme.faint);

    // A control's states, which differ in the fill and in how firm the outline
    // is. The text on all of them is the same colour: a button whose label
    // brightens under the pointer is a button that looks disabled until you
    // touch it.
    //
    // `weak_bg_fill` and `bg_fill` are two different jobs and are given two
    // different colours, which is the one thing about this struct that is easy
    // to get wrong. `weak_bg_fill` is what a button and a drop-down paint.
    // `bg_fill` is the slider rail, the *scroll-bar handle* and the box of a
    // checkbox - so filling it with the control colour puts a scroll handle the
    // same shade as a text box, which reads as a gutter rather than as a grip.
    //
    // The checkbox is the third of those and this deliberately does not suit
    // it: the settings window draws switches, by hand, and calls `ui.checkbox`
    // nowhere. Anybody who reaches for one will get a grey-filled box, and this
    // paragraph is why.
    let widget = |fill: Color32, grip: Color32, stroke: Stroke| {
        eframe::egui::style::WidgetVisuals {
            weak_bg_fill: fill,
            bg_fill: grip,
            bg_stroke: stroke,
            corner_radius: radius(CONTROL_RADIUS),
            fg_stroke: Stroke::new(1.0, theme.text),
            // Zero, deliberately. egui's default grows a hovered widget by a
            // point; in a column of tiles that have to line up, that reads as
            // jitter rather than as feedback.
            expansion: 0.0,
        }
    };

    ctx.all_styles_mut(|style| {
        // Widget text in the same typeface and at the same sizes as everything
        // this program writes by hand. Without this a button is 14 pt and the
        // label beside it is 15, which reads as a mistake because it is one.
        style.text_styles = [
            (TextStyle::Small, font(SIZE_CHIP, Weight::Regular)),
            (TextStyle::Body, font(SIZE_ROW, Weight::Regular)),
            (TextStyle::Button, font(SIZE_ROW, Weight::Regular)),
            (TextStyle::Heading, font(SIZE_HEADLINE, Weight::Bold)),
            (TextStyle::Monospace, font(SIZE_SMALL, Weight::Regular)),
        ]
        .into();

        let spacing = &mut style.spacing;
        spacing.item_spacing = Vec2::new(8.0, ROW_GAP);
        spacing.button_padding = Vec2::new(12.0, 6.0);
        spacing.interact_size = Vec2::new(40.0, CONTROL_H);
        spacing.menu_margin = Margin::same(4);
        spacing.window_margin = Margin::same(CARD_PAD as i8);
        spacing.combo_width = 140.0;
        spacing.text_edit_width = 240.0;

        // Ueli's scrollbar, which is thinner than egui's and always there. A
        // form this long needs a position indicator; it does not need a
        // twelve-point gutter taken out of the reading column to get one.
        let scroll = &mut spacing.scroll;
        scroll.bar_width = 6.0;
        scroll.bar_inner_margin = 4.0;
        scroll.bar_outer_margin = 0.0;
        scroll.floating = false;

        let v = &mut style.visuals;
        v.dark_mode = theme.dark;
        // Opaque, unlike the panel's. These windows are documents: they sit
        // over other programs for minutes at a time, and text on a translucent
        // ground is harder to read the longer you read it.
        v.panel_fill = solid(theme.surface);
        v.window_fill = solid(theme.surface);
        v.window_stroke = Stroke::new(1.0, solid(theme.edge));
        v.window_corner_radius = radius(PANEL_RADIUS);
        v.menu_corner_radius = radius(CONTROL_RADIUS);
        v.faint_bg_color = theme.card;
        // The scroll-bar *track*, which is what this field actually is - the
        // text box below only falls back to it when `text_edit_bg_color` is
        // unset, and it is not. A track the colour of a tile is a groove in
        // the page; a track the colour of the well would be a black stripe
        // down the edge of a light window.
        v.extreme_bg_color = theme.card;
        // What a text box is sunk into. The same trough the search field uses,
        // so a box in the settings window and the one on the panel are the
        // same idea rather than two.
        v.text_edit_bg_color = Some(solid(theme.well));
        v.code_bg_color = solid(theme.well);
        // Selected *text*, which is what this is: egui uses it inside a
        // `TextEdit` and behind a selectable label. The window's own "this
        // one" is `accent_fill`, painted by hand where it is meant.
        v.selection = eframe::egui::style::Selection {
            bg_fill: theme.text_selection,
            stroke: Stroke::new(1.0, theme.strong),
        };
        v.text_cursor.stroke = Stroke::new(1.0, theme.caret);
        // A pinned setting is drawn disabled rather than hidden, so its value
        // has to stay readable while it is refused. egui's default fades
        // harder than that.
        v.disabled_alpha = 0.45;
        v.striped = false;
        v.warn_fg_color = theme.tone(Tone::Warn);
        v.error_fg_color = theme.tone(Tone::Bad);
        v.hyperlink_color = theme.accent;
        v.button_frame = true;
        // Left to egui elsewhere, off here: a window this program draws has no
        // shadow of its own, because the compositor already gives it one.
        v.window_shadow = Shadow::NONE;
        v.popup_shadow = Shadow::default();

        let w = &mut v.widgets;
        // A label, a group heading, a panel separator: not interactive, so not
        // outlined in anything firmer than a rule. Its `bg_stroke` is what
        // `SidePanel` draws its dividing line with.
        w.noninteractive = widget(theme.card, theme.faint, Stroke::new(1.0, theme.edge));
        w.inactive = widget(theme.control, theme.faint, hairline);
        w.hovered = widget(theme.control_hover, theme.dim, Stroke::new(1.0, theme.dim));
        w.active = widget(
            theme.control_active,
            theme.dim,
            Stroke::new(1.0, theme.accent),
        );
        w.open = widget(theme.control_active, theme.dim, hairline);

        // Re-asserted rather than assumed. `gui::fonts::sharpen` turns this
        // off once at startup, and this function runs after it and again on
        // every theme change; it mutates a `Style` in place rather than
        // replacing one, so the setting does survive - but "does survive"
        // is a property of how this is written, and writing it down here
        // costs a line and outlives whoever knew.
        style.visuals.text_options.subpixel_binning = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance.
    fn luminance(c: Color32) -> f64 {
        fn channel(v: u8) -> f64 {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }

    /// WCAG contrast ratio, 1.0 to 21.0.
    fn contrast(a: Color32, b: Color32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Premultiplied source-over: what `top` looks like painted on `bottom`.
    fn over(top: Color32, bottom: Color32) -> Color32 {
        let a = top.a() as f32 / 255.0;
        let mix = |t: u8, b: u8| (t as f32 + b as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        Color32::from_rgb(
            mix(top.r(), bottom.r()),
            mix(top.g(), bottom.g()),
            mix(top.b(), bottom.b()),
        )
    }

    /// What the panel's translucent surface actually resolves to.
    ///
    /// Contrast is a property of what reaches the eye, and behind the panel is
    /// a blur of the desktop, not a known colour. Composited over the darkest
    /// and the lightest thing it could possibly sit on, so the ratios below
    /// hold over a black terminal *and* a white spreadsheet rather than over a
    /// convenient average.
    ///
    /// This is not hypothetical: the first version of this palette used opaque
    /// row fills, and they passed every ratio over black while the hover tint
    /// came to 1.03:1 - invisible - over white.
    fn grounds(theme: Theme) -> [(&'static str, Color32); 2] {
        [
            ("over black", over(theme.surface, Color32::BLACK)),
            ("over white", over(theme.surface, Color32::WHITE)),
        ]
    }

    fn both() -> [(&'static str, Theme); 2] {
        [("dark", Theme::dark()), ("light", Theme::light())]
    }

    /// Body text at AAA, which is the bar for a tool read for hours a day by
    /// people of every age. This is the rule the original complaint was about.
    #[test]
    fn body_text_is_legible_to_the_aaa_standard() {
        for (name, theme) in both() {
            for (ground, bg) in grounds(theme) {
                for (role, fg) in [
                    ("text", theme.text),
                    ("strong", theme.strong),
                    ("input", theme.input),
                ] {
                    let ratio = contrast(fg, bg);
                    assert!(
                        ratio >= 7.0,
                        "{name}/{role} {ground}: {ratio:.2}:1, want 7:1"
                    );
                }
            }
        }
    }

    /// Secondary text - folders, the status line, hint labels - at AA. It is
    /// smaller and it is supporting, but it is still text somebody reads.
    #[test]
    fn secondary_text_is_legible_to_the_aa_standard() {
        for (name, theme) in both() {
            for (ground, bg) in grounds(theme) {
                for (role, fg) in [
                    ("dim", theme.dim),
                    ("accent", theme.accent),
                    ("match_run", theme.match_run),
                ] {
                    let ratio = contrast(fg, bg);
                    assert!(
                        ratio >= 4.5,
                        "{name}/{role} {ground}: {ratio:.2}:1, want 4.5:1"
                    );
                }
            }
        }
    }

    /// A tinted row must not cost the text on it its legibility - and the
    /// selected row is the one the user is looking hardest at.
    #[test]
    fn text_stays_legible_on_every_row_background() {
        for (name, theme) in both() {
            for (ground, surface) in grounds(theme) {
                for (role, fill, fg) in [
                    ("selection", theme.selection, theme.strong),
                    ("hover", theme.hover, theme.text),
                    ("chip", theme.chip_bg, theme.chip_fg),
                    ("text_selection", theme.text_selection, theme.strong),
                ] {
                    let ratio = contrast(fg, over(fill, surface));
                    assert!(
                        ratio >= 4.5,
                        "{name}/{role} {ground}: {ratio:.2}:1, want 4.5:1"
                    );
                }
            }
        }
    }

    /// The code being typed is the biggest text on the panel and sits in a
    /// recessed well rather than on the panel itself, so the well is a text
    /// ground and is held to the AAA ratio the rest of the body text is.
    #[test]
    fn the_code_is_legible_in_the_well_it_is_typed_into() {
        for (name, theme) in both() {
            for (ground, surface) in grounds(theme) {
                let well = over(theme.well, surface);
                for (role, fg) in [("input", theme.input), ("dim", theme.dim)] {
                    let want = if role == "input" { 7.0 } else { 4.5 };
                    let ratio = contrast(fg, well);
                    assert!(
                        ratio >= want,
                        "{name}/{role} {ground}: {ratio:.2}:1 in the well, want {want}:1"
                    );
                }
            }
        }
    }

    /// The settings window is opaque, so its grounds are known colours rather
    /// than a composite over an unknown desktop.
    ///
    /// That is the whole difference between this group of tests and the ones
    /// above it, and it is worth being explicit about: `grounds` exists
    /// because the panel is translucent and its real background is a blur of
    /// whatever was behind it. A document window has no such problem, and
    /// pretending it does would hold these colours to a ratio against black
    /// that nothing will ever draw them over.
    fn opaque(c: Color32) -> Color32 {
        Color32::from_rgb(c.r(), c.g(), c.b())
    }

    /// A setting's label and its sentence of help both sit on the tile, not on
    /// the window, so the tile is a text ground and answers as one.
    #[test]
    fn a_setting_is_legible_on_the_tile_it_is_drawn_on() {
        for (name, theme) in both() {
            let card = theme.card;
            for (role, fg, want) in [
                ("text", theme.text, 7.0),
                ("strong", theme.strong, 7.0),
                ("dim", theme.dim, 4.5),
                ("accent", theme.accent, 4.5),
                ("good", theme.good, 4.5),
                ("warn", theme.tone(Tone::Warn), 4.5),
                ("bad", theme.tone(Tone::Bad), 4.5),
            ] {
                let ratio = contrast(fg, card);
                assert!(
                    ratio >= want,
                    "{name}/{role} on the card: {ratio:.2}:1, want {want}:1"
                );
            }
        }
    }

    /// And on whatever a control is filled with, which is a different colour
    /// again and is where the value actually is.
    #[test]
    fn a_value_is_legible_in_the_control_that_holds_it() {
        for (name, theme) in both() {
            for (state, fill) in [
                ("control", theme.control),
                ("hover", theme.control_hover),
                ("active", theme.control_active),
            ] {
                for (role, fg, want) in [("text", theme.text, 7.0), ("dim", theme.dim, 4.5)] {
                    let ratio = contrast(fg, fill);
                    assert!(
                        ratio >= want,
                        "{name}/{role} on {state}: {ratio:.2}:1, want {want}:1"
                    );
                }
            }
        }
    }

    /// A filled accent carries text of its own - a primary button's label, and
    /// the knob of a switch that is on.
    #[test]
    fn the_filled_accent_carries_what_is_drawn_on_it() {
        for (name, theme) in both() {
            let ratio = contrast(theme.accent_fg, theme.accent_fill);
            assert!(ratio >= 7.0, "{name}: accent {ratio:.2}:1, want 7:1");
        }
    }

    /// A tile that is not a shade off the window is not a tile, and one that
    /// is a long way off it is a second window inside the first. The same rule
    /// the well answers to, in the other direction.
    #[test]
    fn a_tile_is_a_shade_off_the_window_and_no_more() {
        for (name, theme) in both() {
            let window = opaque(theme.surface);
            let ratio = contrast(theme.card, window);
            assert!(
                (1.02..1.35).contains(&ratio),
                "{name}: the card is {ratio:.3}:1 against the window"
            );
        }
    }

    /// The tile and the trough are the two surfaces a setting is made of, and
    /// a text box sits in the second one *inside* the first. If they resolve
    /// to the same colour the box stops looking like a box.
    #[test]
    fn a_text_box_is_visibly_sunk_into_the_tile_around_it() {
        for (name, theme) in both() {
            let well = opaque(theme.well);
            let ratio = contrast(well, theme.card);
            assert!(
                ratio >= 1.15,
                "{name}: the well is {ratio:.3}:1 against the card"
            );
        }
    }

    /// A control's outline and the bar beside the selected page are boundaries
    /// rather than text, so WCAG 1.4.11 and its 3:1 - but they are the only
    /// thing saying where a control ends.
    #[test]
    fn a_control_has_an_edge_you_can_see() {
        for (name, theme) in both() {
            for (role, colour, ground) in [
                ("border on the card", theme.faint, theme.card),
                ("border on a control", theme.faint, theme.control),
                ("accent on the card", theme.accent_fill, theme.card),
                (
                    "accent on the window",
                    theme.accent_fill,
                    opaque(theme.surface),
                ),
            ] {
                let ratio = contrast(colour, ground);
                assert!(ratio >= 3.0, "{name}/{role}: {ratio:.2}:1, want 3:1");
            }
        }
    }

    /// Both of egui's styles, because this program picks its palette from the
    /// configuration and Windows picks egui's.
    ///
    /// The two disagree the moment somebody sets `theme = "light"` on a
    /// machine in dark mode, and a style written into only the active one is a
    /// window that is dressed until the system theme moves. The same argument
    /// [`crate::gui::fonts::sharpen`] makes, and it is here as a test because
    /// `all_styles_mut` is one word away from `style_mut`.
    #[test]
    fn a_style_is_written_into_both_of_the_toolkits_themes() {
        let ctx = eframe::egui::Context::default();
        let theme = Theme::dark();
        apply_style(&ctx, &theme);

        for which in [eframe::egui::Theme::Light, eframe::egui::Theme::Dark] {
            let style = ctx.style_of(which);
            assert_eq!(
                style.visuals.panel_fill,
                Color32::from_rgb(theme.surface.r(), theme.surface.g(), theme.surface.b()),
                "{which:?} was left undressed"
            );
            assert!(
                !style.visuals.text_options.subpixel_binning,
                "{which:?} lost the sharpening that `fonts::sharpen` set"
            );
        }
    }

    /// The scroll-bar track and a text box are two different surfaces, and
    /// egui will make them one if `text_edit_bg_color` is left unset.
    ///
    /// Worth a test rather than a comment because the failure is quiet: the
    /// box still works, it just stops looking like a box.
    #[test]
    fn a_text_box_and_a_scroll_track_are_not_the_same_colour() {
        let ctx = eframe::egui::Context::default();
        for theme in [Theme::dark(), Theme::light()] {
            apply_style(&ctx, &theme);
            let v = &ctx.style_of(eframe::egui::Theme::Dark).visuals;
            assert_eq!(
                v.text_edit_bg_color,
                Some(opaque(theme.well)),
                "a text box is not sunk into the well"
            );
            assert_ne!(
                v.extreme_bg_color,
                v.text_edit_bg_color.unwrap(),
                "the scroll track and the text box resolved to one colour"
            );
        }
    }

    /// `weak_bg_fill` is a button; `bg_fill` is a scroll handle and a slider
    /// rail. Setting them together is the easy mistake, and it puts a scroll
    /// grip the same shade as a text box.
    #[test]
    fn a_scroll_handle_is_not_the_colour_of_a_button() {
        let ctx = eframe::egui::Context::default();
        for theme in [Theme::dark(), Theme::light()] {
            apply_style(&ctx, &theme);
            let w = &ctx.style_of(eframe::egui::Theme::Dark).visuals.widgets;
            for (state, v) in [
                ("inactive", &w.inactive),
                ("hovered", &w.hovered),
                ("active", &w.active),
                ("open", &w.open),
            ] {
                assert_ne!(
                    v.bg_fill, v.weak_bg_fill,
                    "{state}: the handle and the button share a fill"
                );
            }
        }
    }

    /// A widget that grows under the pointer no longer lines up with the one
    /// beside it, and this window is a column of tiles that have to.
    #[test]
    fn nothing_swells_when_the_pointer_is_over_it() {
        let ctx = eframe::egui::Context::default();
        apply_style(&ctx, &Theme::light());
        let w = &ctx.style_of(eframe::egui::Theme::Dark).visuals.widgets;
        for (state, v) in [
            ("noninteractive", &w.noninteractive),
            ("inactive", &w.inactive),
            ("hovered", &w.hovered),
            ("active", &w.active),
            ("open", &w.open),
        ] {
            assert_eq!(v.expansion, 0.0, "{state} expands");
        }
    }

    /// A recessed surface that is not recessed is a flat panel with extra
    /// drawing in it, and one that is recessed too far is a hole.
    #[test]
    fn the_well_is_a_shade_off_the_panel_and_no_more() {
        for (name, theme) in both() {
            for (ground, surface) in grounds(theme) {
                let well = over(theme.well, surface);
                let ratio = contrast(well, surface);
                assert!(
                    (1.02..1.35).contains(&ratio),
                    "{name} {ground}: the well is {ratio:.3}:1 against the panel"
                );
            }
        }
    }

    /// The highlight has to survive the selection: a match drawn in amber on a
    /// tinted row is exactly where a hue stops carrying and nobody notices,
    /// because the unselected rows still look right.
    #[test]
    fn a_matched_run_is_legible_on_the_row_it_is_most_likely_to_be_on() {
        for (name, theme) in both() {
            for (ground, surface) in grounds(theme) {
                let ratio = contrast(theme.match_run, over(theme.selection, surface));
                assert!(
                    ratio >= 4.5,
                    "{name} {ground}: the match run is {ratio:.2}:1 on the                      selected row"
                );
            }
        }
    }

    /// Rules and borders are not text, so they answer to WCAG 1.4.11 rather
    /// than 1.4.3 - but they must still be visible, or the panel has no edge.
    #[test]
    fn rules_and_borders_are_visible_without_being_text() {
        for (name, theme) in both() {
            for (ground, bg) in grounds(theme) {
                let ratio = contrast(theme.faint, bg);
                assert!(
                    ratio >= 3.0,
                    "{name}/faint {ground}: {ratio:.2}:1, want 3:1"
                );
                assert!(
                    ratio < contrast(theme.dim, bg),
                    "{name}/faint {ground} is no fainter than dim, so one of \
                     them is pointless"
                );
            }
        }
    }

    /// Enter opens the selection. A hover that looked like a selection would
    /// get a file opened by accident - the mistake this codebase has already
    /// made once, when mono mode drew them identically.
    #[test]
    fn the_selected_row_is_never_confusable_with_the_hovered_one() {
        for (name, theme) in both() {
            assert_ne!(
                theme.selection, theme.hover,
                "{name}: the selected and hovered rows are drawn the same"
            );
            for (ground, bg) in grounds(theme) {
                let selected = contrast(over(theme.selection, bg), bg);
                let hovered = contrast(over(theme.hover, bg), bg);
                assert!(
                    selected > hovered,
                    "{name} {ground}: hover ({hovered:.2}) stands out at least \
                     as much as the selection ({selected:.2})"
                );
                assert!(
                    hovered > 1.05,
                    "{name} {ground}: the hover tint is invisible at \
                     {hovered:.2}:1"
                );
            }
        }
    }

    /// Every tone must read, including the one that says a drive is
    /// unreachable - which is the line that has to land.
    #[test]
    fn every_tone_is_legible_on_every_ground() {
        for (name, theme) in both() {
            for (ground, bg) in grounds(theme) {
                for tone in [Tone::Normal, Tone::Busy, Tone::Good, Tone::Warn, Tone::Bad] {
                    let ratio = contrast(theme.tone(tone), bg);
                    assert!(
                        ratio >= 4.5,
                        "{name}/{tone:?} {ground}: {ratio:.2}:1, want 4.5:1"
                    );
                }
            }
        }
    }

    /// Roughly one man in twelve cannot tell the green from the red.
    #[test]
    fn no_tone_depends_on_colour_alone() {
        let mut seen: Vec<char> = TONE_GLYPH.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), TONE_GLYPH.len(), "two tones share a glyph");
    }

    /// The mapping from meaning to colour is total, which is what lets
    /// `src/view/` name emphasis without naming a palette.
    #[test]
    fn every_emphasis_has_a_distinct_appearance() {
        for (name, theme) in both() {
            // Appearance, not colour. In greyscale two emphases can sit a
            // couple of values apart and satisfy a colour-only check while
            // being the same thing to anybody not looking for the difference,
            // so the weight is half the identity.
            let mut looks = vec![];
            for emphasis in [
                Emphasis::Body,
                Emphasis::Dim,
                Emphasis::Strong,
                Emphasis::Accent,
            ] {
                looks.push((
                    theme.emphasis(emphasis).to_array(),
                    theme.weight(emphasis) as u8,
                ));
            }
            let mut colours: Vec<_> = looks.iter().map(|(c, _)| *c).collect();
            colours.sort();
            colours.dedup();
            assert_eq!(
                colours.len(),
                4,
                "{name}: two emphases are the same colour, so one says nothing"
            );

            looks.sort();
            looks.dedup();
            assert_eq!(looks.len(), 4, "{name}: two emphases are drawn identically");
            assert_eq!(
                theme.emphasis(Emphasis::Tone(Tone::Bad)),
                theme.tone(Tone::Bad)
            );
        }
    }

    /// A hover is a tint over a row, not a repaint of it.
    #[test]
    fn fading_a_colour_scales_it_rather_than_replacing_it() {
        let opaque = Color32::from_rgb(0x80, 0x40, 0x20);
        assert_eq!(faded(opaque, 1.0), opaque);
        assert_eq!(faded(opaque, 0.0), Color32::TRANSPARENT);
        assert!(faded(opaque, 0.5).a() < opaque.a());
        // Out-of-range input is clamped rather than wrapping into nonsense.
        assert_eq!(faded(opaque, 2.0), opaque);
        assert_eq!(faded(opaque, -1.0), Color32::TRANSPARENT);
    }

    /// The panel is a sheet over whatever is behind it, on both paths: with a
    /// compositor backdrop *and* with the fill it paints itself.
    #[test]
    fn the_surface_is_translucent_in_both_themes() {
        for (name, theme) in both() {
            assert!(
                theme.surface.a() < 255,
                "{name}: an opaque panel reads as a dialog, not an overlay"
            );
            assert!(
                theme.surface.a() > 200,
                "{name}: too transparent to read text on"
            );
        }
    }

    /// The height the window is created at has to hold everything the panel
    /// can grow to, or the bottom of a full list is clipped by the window.
    #[test]
    fn the_panel_can_never_want_more_room_than_the_window_has() {
        let tallest = FIELD_H + ROW_H * MAX_ROWS as f32 + FOOTER_H + PAD_Y * 2.0;
        assert!(PANEL_MAX_H >= tallest, "{PANEL_MAX_H} < {tallest}");
    }
}
