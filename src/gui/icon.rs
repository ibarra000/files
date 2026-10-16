//! The application icon as pixels, for the tray and the window title bars.
//!
//! The artwork is a single-colour glyph on transparency, so it is tinted to
//! stay visible: black on light surfaces, white on dark ones. The .exe and
//! installer icon is `assets/files.ico`; both come from `assets/open.ico`
//! via `tools/make_icon.py`.

use std::sync::Arc;

use eframe::egui;

/// The icon at [`SIZE`]x[`SIZE`], as straight (not premultiplied) RGBA.
///
/// Embedded so the tray icon cannot go missing with a misplaced file.
pub const RGBA: &[u8] = include_bytes!("../../assets/files-32.rgba");
pub const SIZE: u32 = 32;

/// The icon's pixels, white for a dark surface and black for a light one.
///
/// Alpha is kept, so antialiased edges survive the tint.
pub fn pixels(on_dark: bool) -> Vec<u8> {
    let ink = if on_dark { 0xFF } else { 0x00 };
    RGBA.chunks_exact(4)
        .flat_map(|p| [ink, ink, ink, p[3]])
        .collect()
}

/// The icon for a window whose title bar is dark or light.
pub fn window_icon(dark: bool) -> Arc<egui::IconData> {
    Arc::new(egui::IconData {
        rgba: pixels(dark),
        width: SIZE,
        height: SIZE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wrong length is rejected by the tray and window APIs at run time.
    #[test]
    fn the_embedded_icon_is_the_size_it_claims_to_be() {
        assert_eq!(
            RGBA.len(),
            (SIZE * SIZE * 4) as usize,
            "assets/files-32.rgba does not match SIZE; re-run tools/make_icon.py"
        );
    }

    #[test]
    fn the_embedded_icon_is_not_blank() {
        let drawn = RGBA.chunks_exact(4).filter(|p| p[3] > 0).count();
        let total = RGBA.len() / 4;
        assert!(
            drawn * 20 > total,
            "only {drawn} of {total} pixels are drawn"
        );
    }

    /// Tinting replaces colour, so the artwork must be a single colour.
    #[test]
    fn the_embedded_icon_is_one_colour() {
        let mut colours: Vec<&[u8]> = RGBA
            .chunks_exact(4)
            .filter(|p| p[3] > 0)
            .map(|p| &p[..3])
            .collect();
        colours.sort_unstable();
        colours.dedup();
        assert_eq!(colours.len(), 1, "{colours:?}");
    }

    #[test]
    fn tinting_changes_colour_and_keeps_alpha() {
        for on_dark in [false, true] {
            let tinted = pixels(on_dark);
            let ink = if on_dark { 0xFF } else { 0x00 };
            assert_eq!(tinted.len(), RGBA.len());
            for (out, src) in tinted.chunks_exact(4).zip(RGBA.chunks_exact(4)) {
                assert_eq!(out, [ink, ink, ink, src[3]]);
            }
        }
    }
}
