//! Glyphs read with ab_glyph from Segoe UI (Segoe UI Symbol for the marks
//! it lacks), for text drawn turned (`turned_text`).

use ab_glyph::{Font, FontVec, GlyphId};
use std::sync::OnceLock;

/// Segoe UI and Segoe UI Symbol, as the views' text uses them.
fn fonts() -> &'static [FontVec] {
    static FONTS: OnceLock<Vec<FontVec>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let Some(dir) = std::env::var_os("WINDIR") else {
            return Vec::new();
        };
        let dir = std::path::PathBuf::from(dir).join("Fonts");
        ["segoeui.ttf", "seguisym.ttf"]
            .iter()
            .filter_map(|name| FontVec::try_from_vec(std::fs::read(dir.join(name)).ok()?).ok())
            .collect()
    })
}

/// The font that has `c`, and its glyph there.
pub(crate) fn glyph(c: char) -> Option<(&'static FontVec, GlyphId)> {
    let fonts = fonts();
    fonts
        .iter()
        .find_map(|f| {
            let id = f.glyph_id(c);
            (id.0 != 0).then_some((f, id))
        })
        .or_else(|| fonts.first().map(|f| (f, f.glyph_id(c))))
}

/// Px per font unit at `size` px (the em, as CSS sizes text).
fn scale(font: &FontVec, size: f32) -> f32 {
    size / font.units_per_em().unwrap_or(2048.0)
}

/// How long `text` runs at `size` px.
pub fn advance(text: &str, size: f32) -> f32 {
    let mut pen = 0.0;
    let mut prev: Option<(&FontVec, GlyphId)> = None;
    for c in text.chars() {
        let Some((font, id)) = glyph(c) else { continue };
        let k = scale(font, size);
        if let Some((pf, pid)) = prev
            && std::ptr::eq(pf, font)
        {
            pen += font.kern_unscaled(pid, id) * k;
        }
        pen += font.h_advance_unscaled(id) * k;
        prev = Some((font, id));
    }
    pen
}
