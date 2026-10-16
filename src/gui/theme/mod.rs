//! Colours, type and measurements, with light and dark variants that follow the
//! system theme.
//!
//! Colour is never the only carrier: every tone also has a glyph, and the
//! selected row has an accent bar as well as a fill. Every contrast ratio
//! claimed here is asserted in `tests.rs`.

use eframe::egui::{Color32, CornerRadius, FontId, Rect, pos2};

use crate::config::ResultLayout;
use crate::view::Emphasis;
use crate::view::status::Tone;

// -- measurements -----------------------------------------------------------

/// The panel's fixed size, in points. The list scrolls inside it.
pub const PANEL_W: f32 = 600.0;
pub const PANEL_H: f32 = 400.0;

/// Padding inside the header and around the list.
pub const BAND_PAD: f32 = 10.0;

/// Height of the search box: Fluent's `large` input.
pub const INPUT_H: f32 = 40.0;

/// The header: the search box with padding above and below it.
pub const HEADER_H: f32 = BAND_PAD * 2.0 + INPUT_H;

/// Height of the footer.
pub const FOOTER_H: f32 = 40.0;
/// Padding inside the footer.
pub const FOOTER_PAD: f32 = 8.0;

/// Thickness of the rule between two bands.
///
/// Both painted and subtracted from the content band's height, so the two must
/// use this constant.
pub const DIVIDER: f32 = 1.0;

/// Vertical padding around a row's text.
///
/// Not applied directly (row text is centred as a block); it is the term the
/// row heights below are derived from.
const ROW_PAD_Y: f32 = 8.0;

/// A group caption plus the space under it: one `caption1` line and Fluent's
/// `paddingBottom: 5`.
pub const HEADING_H: f32 = 21.0;
const _: () = assert!(HEADING_H == line_h(SIZE_CAPTION) + 5.0);

/// One result on one line: an icon, a name and a badge.
pub const ROW_COMPACT_H: f32 = 36.0;
const _: () = assert!(ROW_COMPACT_H == ROW_PAD_Y * 2.0 + line_h(SIZE_BODY));

/// One result with the folder under the name. The two lines have no gap
/// between them.
pub const ROW_DETAILED_H: f32 = 52.0;
const _: () = assert!(ROW_DETAILED_H == ROW_PAD_Y * 2.0 + line_h(SIZE_BODY) + line_h(SIZE_CAPTION));

/// Horizontal gap between a row's edge and its text.
pub const ROW_PAD_X: f32 = 12.0;

/// Returns a row's height in the given layout.
pub const fn row_h(layout: ResultLayout) -> f32 {
    match layout {
        ResultLayout::Compact => ROW_COMPACT_H,
        ResultLayout::Detailed => ROW_DETAILED_H,
    }
}

/// Returns a row's height plus the gap under it.
pub const fn row_pitch(layout: ResultLayout) -> f32 {
    row_h(layout) + ROW_GAP
}

/// Height between the two dividers, which holds only the scroller.
pub const CONTENT_H: f32 = PANEL_H - HEADER_H - FOOTER_H - DIVIDER * 2.0;

/// Returns the height of a full page of rows under its caption.
///
/// A page must fit the content band, or `PageDown` scrolls past unseen rows;
/// the assertions below check this at compile time. The gap is counted between
/// rows, not after the last one.
const fn page_h(layout: ResultLayout, rows: usize) -> f32 {
    BAND_PAD * 2.0 + HEADING_H + row_pitch(layout) * rows as f32 - ROW_GAP
}

const _: () = assert!(page_h(ResultLayout::Compact, crate::config::VISIBLE_ROWS) <= CONTENT_H);
const _: () =
    assert!(page_h(ResultLayout::Detailed, crate::config::VISIBLE_ROWS_DETAILED) <= CONTENT_H);

/// A page must also leave less than a row of unused space, so `PageDown`
/// moves a full screen.
const _: () =
    assert!(CONTENT_H - page_h(ResultLayout::Compact, crate::config::VISIBLE_ROWS) < ROW_COMPACT_H);
const _: () = assert!(
    CONTENT_H - page_h(ResultLayout::Detailed, crate::config::VISIBLE_ROWS_DETAILED)
        < ROW_DETAILED_H
);

/// Width of the accent bar on the selected row.
pub const MARKER_W: f32 = 3.0;

/// Scroll-bar width in both windows.
///
/// The bar takes layout space rather than floating over content, so scroll
/// position stays visible while the wheel is idle.
pub const SCROLL_W: f32 = 6.0;

// -- corners ----------------------------------------------------------------

// Fluent's radius ramp, named by size rather than role. It stops at 6:
// Fluent's `borderRadiusXLarge` (8) is the Windows 11 window radius, and this
// program does not draw window corners. See `paint_surface` in
// `src/gui/panel/mod.rs`.

/// A hairline indicator, and anything too small for a real curve.
pub const RADIUS_SMALL: u8 = 2;
/// A row, a tile, a text box, a button, a key cap, a menu.
pub const RADIUS_MEDIUM: u8 = 4;
/// The accent bar on a selected row, which this fully rounds.
pub const RADIUS_LARGE: u8 = 6;

/// A fully round shape, such as a badge (Fluent's `shape="circular"`).
///
/// The `u8` equivalent of Fluent's `borderRadiusCircular` (10000px). It is
/// separate from the ramp so that
/// `nothing_this_program_paints_is_rounded_like_a_window` is not tripped by it.
pub const RADIUS_CIRCULAR: u8 = u8::MAX;

/// Must stay at the maximum so it remains circular on any height.
const _: () = assert!(RADIUS_CIRCULAR == u8::MAX);

/// Six on a three-point bar yields a capsule.
const _: () = assert!(RADIUS_LARGE as f32 * 2.0 >= MARKER_W);

// -- type -------------------------------------------------------------------

/// The system typeface, in Fluent's two weights.
///
/// Loaded from disk rather than bundled, because Segoe UI may not be
/// redistributed. The static family is used, matching Fluent's
/// `fontFamilyBase`: Windows ships no bold Segoe UI Variable file, and epaint
/// cannot select a variable font's `wght` or `opsz` axis, so `SegUIVar.ttf`
/// would render only its Display cut.
pub const FONT_FILES: [(&str, &str); 2] = [
    ("segoe", r"C:\Windows\Fonts\segoeui.ttf"),
    // Semibold, not Bold: Fluent's ramp uses only 400 and 600. See [`Weight`].
    ("segoe-semibold", r"C:\Windows\Fonts\seguisb.ttf"),
];

/// Fallback fonts for glyphs Segoe UI lacks, such as emoji in filenames.
///
/// Read from `C:\Windows\Fonts` for the same licensing reason as
/// [`FONT_FILES`]. Symbol comes before emoji so a character covered by both is
/// drawn as a glyph rather than a pictograph.
pub const FALLBACK_FILES: [(&str, &str); 2] = [
    ("segoe-symbol", r"C:\Windows\Fonts\seguisym.ttf"),
    ("segoe-emoji", r"C:\Windows\Fonts\seguiemj.ttf"),
];

// -- type -------------------------------------------------------------------

// Fluent's type ramp from `@fluentui/tokens`, with no sizes between its rungs.
// Emphasis within a size is a change of [`Weight`], as with `body1` and
// `body1Strong`.

/// A badge over a row's icon. Fluent `caption2`.
pub const SIZE_BADGE: f32 = 10.0;
/// The folder under a name, the status line, a group caption, a key cap and a
/// menu row. Fluent `caption1`.
pub const SIZE_CAPTION: f32 = 12.0;
/// A filename, and other text meant to be read. Fluent `body1`.
pub const SIZE_BODY: f32 = 14.0;
/// The search text. Fluent `body2`, used by a large input.
pub const SIZE_LARGE: f32 = 16.0;
/// A settings page's title. Fluent `subtitle1`.
pub const SIZE_TITLE: f32 = 20.0;

/// Returns Fluent's published `lineHeight` for a size on the ramp.
///
/// A table rather than a factor, because the ratios differ per rung (10/14,
/// 12/16, 14/20, 16/22, 20/28). A size off the ramp fails const evaluation.
pub const fn line_h(size: f32) -> f32 {
    if size == SIZE_BADGE {
        14.0
    } else if size == SIZE_CAPTION {
        16.0
    } else if size == SIZE_BODY {
        20.0
    } else if size == SIZE_LARGE {
        22.0
    } else if size == SIZE_TITLE {
        28.0
    } else {
        panic!("a size off Fluent's ramp has no published line height")
    }
}

// -- colour -----------------------------------------------------------------

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Returns a translucent colour from straight (un-premultiplied) channels.
///
/// `Color32` stores premultiplied values; writing those by hand risks a colour
/// brighter than its alpha allows.
pub fn tint(hex: u32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, alpha)
}

/// The panel's palette, taken from Fluent's `webLightTheme` and `webDarkTheme`
/// in `@fluentui/tokens`.
///
/// Field names are semantic roles; each doc comment names the source token.
/// Two published values fall short of usual contrast targets and are noted
/// where they occur: the divider is 1.3:1 against its ground, and in the dark
/// theme the hovered row is slightly louder than the selected one.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub dark: bool,

    // -- grounds ------------------------------------------------------------
    /// The window's ground. `NeutralBackground1`.
    ///
    /// Opaque, so every contrast ratio is computed against a known colour. On
    /// the acrylic path [`Self::wash`] is painted instead.
    pub surface: Color32,

    /// Painted over a compositor backdrop instead of [`Self::surface`].
    ///
    /// Black at 60% (dark) or white at 60% (light). It pushes DWM's tinted blur
    /// toward a predictable ground while leaving the desktop faintly visible.
    pub wash: Color32,

    /// A settings tile, and the trough a text box sits in. `NeutralBackground3`.
    ///
    /// A filled input is the same colour as the card around it; the rule under
    /// it is what makes it read as a box. See [`Self::accessible`].
    pub card: Color32,
    /// The search field, and a text box in the settings window.
    /// `NeutralBackground3`, the same value as [`Self::card`].
    pub well: Color32,
    /// Behind a scroll-bar's thumb. `NeutralBackground4`.
    pub track: Color32,

    // -- rows ---------------------------------------------------------------
    /// The keyboard-selected row. `NeutralBackground1Selected`.
    pub selection: Color32,
    /// The row under the pointer. `NeutralBackground1Hover`.
    ///
    /// Not necessarily weaker than [`Self::selection`]: in Fluent dark, hover
    /// `#3d3d3d` is louder than selected `#383838`. The accent bar, shown only
    /// on the selected row, is what distinguishes them. See
    /// `the_selected_row_is_never_confusable_with_the_hovered_one`.
    pub hover: Color32,
    /// Selected text in the search field.
    ///
    /// A tint, because Fluent publishes no selected-text colour and a tint
    /// works over both a field and a label.
    pub text_selection: Color32,

    // -- text ---------------------------------------------------------------
    /// A filename, and body text generally. `NeutralForeground1`.
    pub text: Color32,
    /// A heading, or the name on the selected row. `NeutralForeground1`.
    ///
    /// Same value as [`Self::text`]; Fluent separates `body1` from
    /// `body1Strong` by weight, not colour.
    pub strong: Color32,
    /// The search text. `NeutralForeground1`.
    pub input: Color32,
    /// The matched substring. `NeutralForeground1`, emphasised by weight.
    pub match_run: Color32,
    /// Labels, folders, the status line. `NeutralForeground3`.
    pub dim: Color32,
    /// A group caption and a placeholder. `NeutralForeground4`.
    pub caption: Color32,
    /// The caret. `NeutralForeground1`.
    ///
    /// Matches the text colour, as Fluent's `currentColor` caret does; focus is
    /// shown by the rule under the field.
    pub caret: Color32,

    // -- boundaries ---------------------------------------------------------
    /// The rule between two bands. `NeutralStroke2`, as used by Fluent's
    /// `Divider`.
    ///
    /// 1.3:1 against its ground in the light theme, which is the published
    /// value. It is a structural hint; [`Self::accessible`] is the boundary
    /// that must be found.
    pub edge: Color32,
    /// The outline of a control at rest. `NeutralStroke1`.
    pub stroke: Color32,
    /// The rule that makes a filled input a box. `NeutralStrokeAccessible`.
    ///
    /// A 1 px rule along the bottom edge, clearing 3:1 in both themes. Without
    /// it a text box is indistinguishable from its card.
    pub accessible: Color32,
    /// A scroll-bar's thumb at rest. `NeutralForeground4`.
    pub grip: Color32,

    // -- brand --------------------------------------------------------------
    /// The brand hue. `BrandForeground1`.
    ///
    /// Used for the selected row's accent bar, the nav indicator and links,
    /// not for small body text: `#479ef5` is 5.2:1 on the dark ground but can
    /// fall to about 4:1 on the acrylic path over a bright desktop.
    pub accent: Color32,
    /// A surface that means "this one": a switch that is on, a primary button.
    /// `CompoundBrandBackground`.
    pub accent_fill: Color32,
    /// Text on [`Self::accent_fill`]: white in both themes, per Fluent.
    pub accent_fg: Color32,

    // -- controls -----------------------------------------------------------
    /// The fill behind a control. `NeutralBackground1{,Hover,Pressed}`.
    ///
    /// One field per state, because egui takes a colour per interaction state
    /// up front.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,

    // -- small surfaces -----------------------------------------------------
    /// The pill at the end of a row that names its drive.
    ///
    /// `NeutralBackground3` and `NeutralForeground1`. Fluent's Badge uses
    /// `NeutralBackground1`, but this badge sits on the row's own ground, where
    /// that value would make it invisible.
    pub badge_bg: Color32,
    pub badge_fg: Color32,

    /// A key cap, such as ` Enter `. `NeutralBackground5` and
    /// `NeutralForeground1`.
    ///
    /// `NeutralBackground5` is the far end of the neutral ramp, so a key cap
    /// stands apart from what it sits on.
    pub key_bg: Color32,
    pub key_fg: Color32,

    tones: [Color32; 5],
}

/// Glyphs that convey each tone without relying on colour.
pub const TONE_GLYPH: [char; 5] = ['\u{2022}', '\u{2219}', '\u{2713}', '\u{25B2}', '\u{00D7}'];

impl Theme {
    /// Fluent's `webDarkTheme`.
    ///
    /// Each value is a token from `@fluentui/tokens`, named beside it. Fields
    /// that share a value do so because Fluent's tokens do.
    pub fn dark() -> Self {
        let fg1 = rgb(0xFFFFFF); // NeutralForeground1
        let fg3 = rgb(0xADADAD); // NeutralForeground3
        let fg4 = rgb(0x999999); // NeutralForeground4
        let bg1 = rgb(0x292929); // NeutralBackground1
        let bg3 = rgb(0x1F1F1F); // NeutralBackground3
        Self {
            dark: true,
            surface: bg1,
            // Black at 60%, over the compositor backdrop.
            wash: tint(0x000000, 0x99),
            card: bg3,
            well: bg3,
            track: rgb(0x141414),     // NeutralBackground4
            selection: rgb(0x383838), // NeutralBackground1Selected
            hover: rgb(0x3D3D3D),     // NeutralBackground1Hover
            text_selection: tint(0xFFFFFF, 0x3A),
            text: fg1,
            strong: fg1,
            input: fg1,
            match_run: fg1,
            dim: fg3,
            caption: fg4,
            caret: fg1,
            edge: rgb(0x333333),       // NeutralStroke2
            stroke: rgb(0x666666),     // NeutralStroke1
            accessible: rgb(0xADADAD), // NeutralStrokeAccessible
            grip: fg4,
            accent: rgb(0x479EF5),      // BrandForeground1
            accent_fill: rgb(0x115EA3), // CompoundBrandBackground
            accent_fg: rgb(0xFFFFFF),
            control: bg1,
            control_hover: rgb(0x3D3D3D),  // NeutralBackground1Hover
            control_active: rgb(0x333333), // NeutralBackground1Pressed
            badge_bg: bg3,
            badge_fg: fg1,
            key_bg: rgb(0x0A0A0A), // NeutralBackground5
            key_fg: fg1,
            // Warn and Bad use custom hues: `colorPaletteRedForeground1` is
            // 4.16:1 on this ground, below the 4.5:1 required for the
            // drive-unreachable line. Busy and Good rely on `TONE_GLYPH`.
            tones: [fg1, fg3, fg1, rgb(0xEFC05A), rgb(0xF09184)],
        }
    }

    /// Fluent's `webLightTheme`. See [`Self::dark`].
    pub fn light() -> Self {
        let fg1 = rgb(0x242424); // NeutralForeground1
        let fg3 = rgb(0x616161); // NeutralForeground3
        let fg4 = rgb(0x707070); // NeutralForeground4
        let bg1 = rgb(0xFFFFFF); // NeutralBackground1
        let bg3 = rgb(0xF5F5F5); // NeutralBackground3
        Self {
            dark: false,
            surface: bg1,
            // White at 60%, over the compositor backdrop.
            wash: tint(0xFFFFFF, 0x99),
            card: bg3,
            well: bg3,
            track: rgb(0xF0F0F0),     // NeutralBackground4
            selection: rgb(0xEBEBEB), // NeutralBackground1Selected
            hover: rgb(0xF5F5F5),     // NeutralBackground1Hover
            text_selection: tint(0x000000, 0x32),
            text: fg1,
            strong: fg1,
            input: fg1,
            match_run: fg1,
            dim: fg3,
            caption: fg4,
            caret: fg1,
            edge: rgb(0xE0E0E0),       // NeutralStroke2
            stroke: rgb(0xD1D1D1),     // NeutralStroke1
            accessible: rgb(0x616161), // NeutralStrokeAccessible
            grip: fg4,
            accent: rgb(0x0F6CBD),      // BrandForeground1
            accent_fill: rgb(0x0F6CBD), // CompoundBrandBackground
            accent_fg: rgb(0xFFFFFF),
            control: bg1,
            control_hover: rgb(0xF5F5F5),  // NeutralBackground1Hover
            control_active: rgb(0xE0E0E0), // NeutralBackground1Pressed
            badge_bg: bg3,
            badge_fg: fg1,
            key_bg: rgb(0xEBEBEB), // NeutralBackground5
            key_fg: fg1,
            // See the note in `dark`.
            tones: [fg1, fg3, fg1, rgb(0x8A4B00), rgb(0xA32820)],
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

    /// Returns the colour for a [`Run`](crate::view::Run) from `src/view/`.
    ///
    /// The single mapping from a view-layer emphasis to a colour.
    pub fn emphasis(self, emphasis: Emphasis) -> Color32 {
        match emphasis {
            Emphasis::Body => self.text,
            Emphasis::Dim => self.dim,
            Emphasis::Strong => self.strong,
            Emphasis::Accent => self.accent,
            Emphasis::Tone(tone) => self.tone(tone),
        }
    }

    /// Returns the font weight for an emphasis.
    ///
    /// Required alongside [`Self::emphasis`]: the neutral palette has too few
    /// distinguishable brightness steps, and `Accent` is close to `text`.
    pub const fn weight(self, emphasis: Emphasis) -> Weight {
        match emphasis {
            Emphasis::Body | Emphasis::Dim => Weight::Regular,
            Emphasis::Strong | Emphasis::Accent => Weight::Semibold,
            // Warnings and errors get weight in addition to glyph and colour.
            Emphasis::Tone(Tone::Warn | Tone::Bad) => Weight::Semibold,
            Emphasis::Tone(_) => Weight::Regular,
        }
    }
}

/// Scales a colour's alpha, for fading a whole body in or out.
///
/// Multiplicative, so an already translucent colour stays in proportion.
pub fn faded(color: Color32, alpha: f32) -> Color32 {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha >= 1.0 {
        return color;
    }
    color.linear_multiply(alpha)
}

// -- depth ------------------------------------------------------------------

// No shadows. Surfaces are distinguished by Fluent's neutral fills alone.

/// Paints the rule along the bottom of a text box.
///
/// A Fluent filled input has the same fill as its card (1.000:1), so this rule
/// is what marks it: one point of [`Theme::accessible`] at rest, two points of
/// the accent colour when focused. It is also the only focus indication.
/// Drawn over the bottom edge with the box's bottom corner radius.
pub fn focus_rule(painter: &eframe::egui::Painter, theme: &Theme, rect: Rect, focused: bool) {
    let (thickness, colour) = if focused {
        (2.0, theme.accent)
    } else {
        (1.0, theme.accessible)
    };
    let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - thickness), rect.max);
    painter.rect_filled(
        bar,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: RADIUS_MEDIUM,
            se: RADIUS_MEDIUM,
        },
        colour,
    );
}

pub fn radius(r: u8) -> CornerRadius {
    CornerRadius::same(r)
}

/// Returns a font at a size from the ramp, in the given weight.
pub fn font(size: f32, weight: Weight) -> FontId {
    FontId::new(size, eframe::egui::FontFamily::Name(weight.family().into()))
}

/// The two font weights.
///
/// Fluent's ramp uses only 400 and 600. Windows also resolves Fluent's
/// `fontWeightMedium` (500) to Semibold, because Segoe UI has no Medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Semibold,
}

impl Weight {
    /// Every weight. Tests in [`crate::gui::fonts`] keep this in step with
    /// [`FONT_FILES`].
    pub const ALL: [Self; 2] = [Self::Regular, Self::Semibold];

    pub const fn family(self) -> &'static str {
        match self {
            Self::Regular => "segoe",
            Self::Semibold => "segoe-semibold",
        }
    }
}

pub mod icons;
pub mod style;
pub use icons::{ICON_INLINE, ICON_NAV, Icon, has_icon, icon_font};
pub use style::{CARD_PAD, CONTENT_PAD, CONTROL_H, GROUP_GAP, NAV_W, ROW_GAP, apply_style};

#[cfg(test)]
mod tests;
