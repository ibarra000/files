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

use eframe::egui::{Color32, CornerRadius, FontId, Rect, pos2};

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

/// The air above and below a row's text, which is Ueli's `padding: 8`.
///
/// Never painted and never positioned against - a row's text is centred as a
/// block, so the padding falls out rather than being applied. It is here
/// because it is the term the two row heights below are derived from, and a
/// height that is derived is a height nobody has to trust.
const ROW_PAD_Y: f32 = 8.0;

/// A group's caption, and the air under it.
///
/// One `caption1` line plus Fluent's `paddingBottom: 5` on a group header.
pub const HEADING_H: f32 = 21.0;
const _: () = assert!(HEADING_H == line_h(SIZE_CAPTION) + 5.0);

/// One result, on one line: an icon, a name and a badge.
pub const ROW_COMPACT_H: f32 = 36.0;
const _: () = assert!(ROW_COMPACT_H == ROW_PAD_Y * 2.0 + line_h(SIZE_BODY));

/// One result, with the folder under the name.
///
/// The two lines sit directly on each other with no gap between them, which
/// is what a flex column of two `Text`s does and is why 20 + 16 lands exactly
/// on Ueli's 52.
pub const ROW_DETAILED_H: f32 = 52.0;
const _: () = assert!(ROW_DETAILED_H == ROW_PAD_Y * 2.0 + line_h(SIZE_BODY) + line_h(SIZE_CAPTION));

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

// -- corners ----------------------------------------------------------------

// Fluent's radius ramp, named by size rather than by role - which is what
// replaced five role-named constants, four of which were the same number.
// `ROW_RADIUS`, `CHIP_RADIUS`, `CONTROL_RADIUS` and `CARD_RADIUS` were all 4,
// and a vocabulary where four words mean one thing is four chances to change
// one of them and not the others.
//
// The ramp stops at six. Fluent has a `borderRadiusXLarge` of 8 and nothing
// here may use it, because 8 is the radius Windows 11 gives a window and this
// program no longer draws window corners at all. See `paint_surface`.

/// A hairline indicator, and anything else too small for a real curve.
pub const RADIUS_SMALL: u8 = 2;
/// A row, a tile, a text box, a button, a key cap, a menu.
pub const RADIUS_MEDIUM: u8 = 4;
/// The accent bar down a selected row, which this fully rounds.
pub const RADIUS_LARGE: u8 = 6;

/// Six on a three-point bar is a capsule, which is what Ueli's is.
const _: () = assert!(RADIUS_LARGE as f32 * 2.0 >= MARKER_W);

// -- type -------------------------------------------------------------------

/// The system's own typeface, in the two weights Fluent draws with.
///
/// Loaded from disk rather than bundled: shipping Segoe UI would be a licence
/// violation, and a Windows without it is a Windows that cannot boot.
///
/// # Static, and not Segoe UI Variable
///
/// There used to be a second list preferred over this one - Windows 11's
/// `SegUIVar.ttf` and `SegUIVarB.ttf`, on the argument that the variable cut
/// stays crisp across sizes where the original has one set of outlines doing
/// both jobs. It was wrong three times over, and each is checkable.
///
/// `SegUIVarB.ttf` does not exist. Not "is missing on this machine" - Windows
/// does not ship a bold variable file, so the pair could never be one
/// typeface and the bold slot always fell through to `segoeuib.ttf` in
/// silence.
///
/// `SegUIVar.ttf` is a *variable* font, and epaint's rasteriser cannot select
/// a `wght` or `opsz` axis. It draws the default instance, which is Segoe UI
/// Variable **Display** - the cut meant for headlines, tighter and more
/// closely spaced than the Text cut a fourteen-point row wants. So the panel
/// was already setting its regular text in a display face and its bold in a
/// text face: two typefaces on one line.
///
/// And Fluent's own `fontFamilyBase` is `'Segoe UI'`, the static family.
/// Matching the thing being copied means matching that.
pub const FONT_FILES: [(&str, &str); 2] = [
    ("segoe", r"C:\Windows\Fonts\segoeui.ttf"),
    // Semibold, not Bold. Fluent's ramp is 400 and 600; 700 appears nowhere
    // in what this is drawn from. See [`Weight`].
    ("segoe-semibold", r"C:\Windows\Fonts\seguisb.ttf"),
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

// -- type -------------------------------------------------------------------

// Fluent's ramp, and nothing between its rungs. Each of the five is a real
// token out of `@fluentui/tokens`, and what used to be a sixth size is now a
// change of [`Weight`]: `body1` against `body1Strong` is one size and two
// weights, which is how the thing being copied does it.
//
// Four of the five moved. The loudest is the search box, which was 22 - very
// nearly a window title - against Fluent's 16 for an input at `size="large"`.
// The old numbers were a ramp of one program's own invention, and every one
// of them was a point or two off a rung of the ramp underneath it.

/// A badge over a row's icon. Fluent `caption2`.
pub const SIZE_BADGE: f32 = 10.0;
/// The folder under a name, the status line, a group caption, a key cap and a
/// menu row. Fluent `caption1`.
pub const SIZE_CAPTION: f32 = 12.0;
/// A filename, and everything else meant to be read rather than glanced at.
/// Fluent `body1`.
pub const SIZE_BODY: f32 = 14.0;
/// What the user is typing. Fluent `body2`, which is what a large input is
/// set in.
pub const SIZE_LARGE: f32 = 16.0;
/// A settings page's title. Fluent `subtitle1`.
pub const SIZE_TITLE: f32 = 20.0;

/// The room one line of `size` takes, which is Fluent's `lineHeight` for it.
///
/// A table rather than a factor, because the ramp is deliberately not
/// proportional: 10/14 is 1.40, 12/16 is 1.33, 14/20 is 1.43, 16/22 is 1.375,
/// 20/28 is 1.40. Any single multiplier is wrong at one end of that, and
/// being wrong by a point is how a row ends up a point short of the height it
/// was declared at.
///
/// A size that is not on the ramp does not compile. That is the point: the
/// only way to get a line height here is to add the rung with the number
/// Microsoft publishes for it, rather than to guess one at a call site.
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

/// A colour that lets what is behind it through.
///
/// Takes straight (un-premultiplied) channels, because that is how a colour is
/// picked and written down. `Color32` stores them premultiplied, and writing
/// the premultiplied form by hand is how a surface ends up brighter than its
/// own alpha allows - which is not a colour at all.
pub fn tint(hex: u32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, alpha)
}

/// Every colour the panel is drawn with, and every one of them is Fluent's.
///
/// # The names are roles, the values are tokens
///
/// The field names stay semantic - a caller asks for "the ground under a
/// selected row", not for `NeutralBackground1Selected` - because that is the
/// bargain the rest of this module keeps: one place turns a meaning into a
/// picture. What changed is that every value below is now a transcription
/// rather than a decision. Each doc comment names the token it came from, so
/// a disagreement with Fluent is a thing you can look up rather than argue
/// about.
///
/// They come from `webLightTheme` and `webDarkTheme` in `@fluentui/tokens`,
/// which is what Ueli uses with no customisation whatsoever - the entire
/// contents of its theme file is a choice between those two objects.
///
/// # What this cost
///
/// The palette this replaces was a hand-tuned warm grey scheme carried over
/// from a terminal build, and it was *better* by some measures: every pair in
/// it cleared a ratio somebody had picked on purpose. Fluent's does not, in
/// two places, and both are noted where they happen - a subtle divider is
/// 1.3:1 against its own ground, and in the dark theme a hovered row is
/// slightly louder than a selected one. Those are Microsoft's numbers in
/// Microsoft's theme, and the alternative to living with them is inventing a
/// palette again, which is the thing this is undoing.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub dark: bool,

    // -- grounds ------------------------------------------------------------
    /// The window's own ground. `NeutralBackground1`.
    ///
    /// Opaque, which is new. It used to be a tint at alpha `0xF0` so that the
    /// painted fallback path still read as an overlay rather than as a dialog.
    /// Ueli's `None` material is a flat opaque background and its acrylic path
    /// paints [`Self::wash`] over the compositor's blur instead, and copying
    /// that arrangement is what makes every ratio below arithmetic: the ground
    /// under this program's text is now a colour this program picked, not a
    /// blur of somebody's spreadsheet.
    ///
    /// Translucency did not go away - it moved to the path that can actually
    /// do it well. See [`Self::wash`].
    pub surface: Color32,

    /// What goes over a compositor backdrop instead of [`Self::surface`].
    ///
    /// Black at 60% in the dark theme, white at 60% in the light one, exactly
    /// as Ueli's acrylic path does it. DWM's acrylic is already a blur tinted
    /// hard toward the system theme; this drives it the rest of the way to a
    /// ground the palette can predict, while leaving enough through that the
    /// panel still reads as a sheet over the desktop rather than as a box on
    /// it.
    pub wash: Color32,

    /// A tile in the settings window, and the trough a text box sits in.
    /// `NeutralBackground3`.
    ///
    /// One token for both, which is Fluent's arrangement and was not ours: a
    /// filled input really is the same colour as the card around it, and it
    /// reads as a box because of the rule under it rather than because of a
    /// step in the fill. See [`Self::accessible`].
    pub card: Color32,
    /// The search field, and a text box in the settings window.
    /// `NeutralBackground3` - the same value as [`Self::card`], deliberately.
    pub well: Color32,
    /// Behind a scroll-bar's thumb. `NeutralBackground4`.
    pub track: Color32,

    // -- rows ---------------------------------------------------------------
    /// The row the arrows are on. `NeutralBackground1Selected`.
    ///
    /// Opaque, where it used to be a tint. The tint existed because the panel
    /// was translucent and an opaque fill would stop tracking its ground; with
    /// the ground opaque there is nothing to track, and a published value
    /// beats a computed one.
    pub selection: Color32,
    /// The row under the pointer. `NeutralBackground1Hover`.
    ///
    /// **Not** strictly weaker than [`Self::selection`], which it used to be
    /// required to be. In stock Fluent dark, hover `#3d3d3d` is a shade
    /// *louder* than selected `#383838`, and no amount of tuning inside the
    /// ramp changes that. What keeps Enter from opening the wrong file is the
    /// accent bar, which only the selected row has. See
    /// `the_selected_row_is_never_confusable_with_the_hovered_one`.
    pub hover: Color32,
    /// The run the user is about to copy out of the search field.
    ///
    /// Still a tint, and the last one here. Fluent publishes no selected-text
    /// colour - a browser supplies it - and a tint is the only form that is
    /// right over both a field and a label.
    pub text_selection: Color32,

    // -- text ---------------------------------------------------------------
    /// A filename, and body text generally. `NeutralForeground1`.
    pub text: Color32,
    /// A heading, or the name on the selected row. `NeutralForeground1`.
    ///
    /// The same value as [`Self::text`], which is the point: Fluent separates
    /// `body1` from `body1Strong` by weight and not by colour. A selected
    /// row's name no longer brightens, because there is nowhere brighter for
    /// it to go and it never needed to - the fill and the bar say which row it
    /// is.
    pub strong: Color32,
    /// The code being typed. `NeutralForeground1`.
    pub input: Color32,
    /// The matched substring. `NeutralForeground1`, carried by weight.
    pub match_run: Color32,
    /// Labels, folders, the status line. `NeutralForeground3`.
    pub dim: Color32,
    /// A group caption and a placeholder: present, and not asking to be read
    /// first. `NeutralForeground4`.
    pub caption: Color32,
    /// Where the caret is. `NeutralForeground1`.
    ///
    /// It used to be [`Self::accent`], on the reasoning that the caret is the
    /// one thing that says where the keyboard is going. A Fluent caret is
    /// `currentColor` - the colour of the text it sits in - and the focus
    /// signal is the rule under the field, not a coloured bar in the middle of
    /// a word.
    pub caret: Color32,

    // -- boundaries ---------------------------------------------------------
    /// The rule between two bands. `NeutralStroke2`, which is what Fluent's
    /// `Divider` uses.
    ///
    /// Faint - 1.3:1 against its own ground in the light theme - and that is
    /// the published value for a divider rather than an oversight. A rule
    /// between two bands is a hint about structure; it is not a boundary
    /// anybody has to find. [`Self::accessible`] is the one that is.
    pub edge: Color32,
    /// The outline of a control at rest. `NeutralStroke1`.
    pub stroke: Color32,
    /// The rule that makes a filled input a box. `NeutralStrokeAccessible`.
    ///
    /// The load-bearing one, and the reason [`Self::well`] is allowed to be
    /// the same colour as [`Self::card`]. Fluent's filled text input has no
    /// step in its fill at all; what says "type here" is a 1 px rule along the
    /// bottom edge at this colour, which clears 3:1 in both themes. Take it
    /// away and the box genuinely disappears.
    pub accessible: Color32,
    /// A scroll-bar's thumb at rest. `NeutralForeground4`.
    pub grip: Color32,

    // -- brand --------------------------------------------------------------
    /// The one hue that means "this". `BrandForeground1`.
    ///
    /// The accent bar down a selected row, the nav indicator, a link. Not
    /// small body text on the panel: `#479ef5` on the dark theme's ground
    /// comes to 5.2:1, which is fine, but on the acrylic path over a bright
    /// desktop it can fall to about 4:1, and a twelve-point line is the wrong
    /// place to spend that margin. Where a run used to be accented it is now
    /// full-strength and Semibold, which is how Fluent emphasises anyway.
    pub accent: Color32,
    /// A surface that means "this one": a switch that is on, a primary button.
    /// `CompoundBrandBackground`.
    pub accent_fill: Color32,
    /// And what reads on it: white, in both themes, as Fluent specifies.
    pub accent_fg: Color32,

    // -- controls -----------------------------------------------------------
    /// The fill behind a control. `NeutralBackground1{,Hover,Pressed}`.
    ///
    /// Three of them rather than one, because egui asks for a colour per
    /// interaction state up front rather than tinting one at paint time, and a
    /// hover that is computed by compositing would have to be computed
    /// identically in four places.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,

    // -- small surfaces -----------------------------------------------------
    /// The pill at the end of a row that names its drive.
    ///
    /// `NeutralBackground3` and `NeutralForeground1`. Fluent's own Badge fills
    /// with `NeutralBackground1`, which works there because a badge sits *on*
    /// an icon; ours sits on the row's own ground, where `Background1` would
    /// be the same colour as the ground and the pill would not exist. One
    /// step off, which is the smallest move that keeps it a pill.
    pub badge_bg: Color32,
    pub badge_fg: Color32,

    /// A key name: ` Enter `. `NeutralBackground5` and `NeutralForeground1`.
    ///
    /// Background5 is the far end of the neutral ramp - near-black in the dark
    /// theme, and a light grey in the light one - which is why a key cap is
    /// the one small surface here that is unmistakably a different piece of
    /// material from what it sits on. That is what a key cap is.
    pub key_bg: Color32,
    pub key_fg: Color32,

    tones: [Color32; 5],
}

/// The small marks that say what a line means without using colour.
pub const TONE_GLYPH: [char; 5] = ['\u{2022}', '\u{2219}', '\u{2713}', '\u{25B2}', '\u{00D7}'];

impl Theme {
    /// `webDarkTheme`, transcribed.
    ///
    /// Every value here is a token out of `@fluentui/tokens`, named in the
    /// comment beside it. Nothing is tuned and nothing is ours; where two
    /// fields hold the same number that is Fluent holding them the same, not
    /// a copy-paste.
    pub fn dark() -> Self {
        let fg1 = rgb(0xFFFFFF); // NeutralForeground1
        let fg3 = rgb(0xADADAD); // NeutralForeground3
        let fg4 = rgb(0x999999); // NeutralForeground4
        let bg1 = rgb(0x292929); // NeutralBackground1
        let bg3 = rgb(0x1F1F1F); // NeutralBackground3
        Self {
            dark: true,
            surface: bg1,
            // Black at 60%, over whatever the compositor produced.
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
            // Warn and Bad keep hues of our own rather than Fluent's.
            // `colorPaletteRedForeground1` is 4.16:1 on this ground, under the
            // 4.5 the drive-unreachable line has always been held to, and that
            // line is the one that has to land. Busy and Good give up their
            // hues to `TONE_GLYPH`.
            tones: [fg1, fg3, fg1, rgb(0xEFC05A), rgb(0xF09184)],
        }
    }

    /// `webLightTheme`, transcribed. See [`Self::dark`].
    pub fn light() -> Self {
        let fg1 = rgb(0x242424); // NeutralForeground1
        let fg3 = rgb(0x616161); // NeutralForeground3
        let fg4 = rgb(0x707070); // NeutralForeground4
        let bg1 = rgb(0xFFFFFF); // NeutralBackground1
        let bg3 = rgb(0xF5F5F5); // NeutralBackground3
        Self {
            dark: false,
            surface: bg1,
            // White at 60%, over whatever the compositor produced.
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
            // See the dark theme's note.
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
            Emphasis::Strong | Emphasis::Accent => Weight::Semibold,
            // The two lines that have to land, in the one place where a glyph
            // and a colour were already not quite enough.
            Emphasis::Tone(Tone::Warn | Tone::Bad) => Weight::Semibold,
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

// -- depth ------------------------------------------------------------------

// There is none, and that is the whole section.
//
// What used to be here was a soft-UI kit: a `LIFT` of two points, a `BLUR`
// of six, a `lit` and a `shade` in the palette, and three helpers - `raise`,
// `press`, `cap` - each of which drew a pair of offset shadows and then a
// fill over the top. Six call sites used them: the search box, the selected
// row in three places, the drive badge and every key cap.
//
// Ueli's search window has no shadow anywhere in it. Not a subtle one, not
// one on the selected row - none. Every surface in it is legible by its fill
// alone, which is what Fluent's neutral ramp is *for*: the whole point of
// having `Background1Hover` and `Background1Selected` as published values is
// that a row does not need shading to be distinguishable from the row above
// it. Shading it as well is answering a question the palette already
// answered, in a second voice, at a second angle.
//
// So the shadows went, and nothing replaced them. The selected row is its
// fill and its accent bar. The search box is its fill and, shortly, the rule
// under it. A key cap is a different colour from the ground and that is all
// a key cap ever needed to be.

/// The rule along the bottom of a text box, which is what makes it one.
///
/// Fluent's filled input has no step in its fill - it is `NeutralBackground3`
/// and so is the card around it, the same colour, 1.000:1 - and no outline
/// worth the name on three sides. What says "type here" is a rule along the
/// bottom edge: one point of [`Theme::accessible`] at rest, two points of the
/// brand colour while the box has the keyboard. Take it away and a text box
/// is a rectangle of card drawn on card, which is the state this program was
/// in for exactly as long as it took to transcribe the palette.
///
/// This is also the whole of the focus indication. There is no ring, no glow
/// and no border change, because there is none in the thing being copied: a
/// Fluent input goes from a hairline to a brand bar and that is the event.
///
/// Drawn over the bottom edge rather than under it, and with the bottom
/// corners rounded to the box's own - a square rule under a rounded box is a
/// rule with two small horns on it.
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

/// A size off the ramp above, in one of the two weights.
pub fn font(size: f32, weight: Weight) -> FontId {
    FontId::new(size, eframe::egui::FontFamily::Name(weight.family().into()))
}

/// The two weights, by role.
///
/// Fluent's ramp uses 400 and 600 and nothing else: `body1` against
/// `body1Strong`, `caption1` against `caption1Strong`. There is no Bold here
/// because there is no 700 in the thing this is drawn from, and a third
/// weight with no legitimate caller is a third font to load and the one a
/// hand reaches for by habit.
///
/// Semibold is also what Windows resolves Fluent's `fontWeightMedium` (500)
/// to, because Segoe UI ships no Medium - so the group captions Ueli sets at
/// 500 land here too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Semibold,
}

impl Weight {
    /// Every one, so the enum and [`FONT_FILES`] cannot drift apart. The two
    /// tests in [`crate::gui::fonts`] are what hold them together.
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
