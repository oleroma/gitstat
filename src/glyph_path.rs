//! Text drawn as filled outlines, at any angle: for words that turn with
//! the flip. GPUI lays text out only upright, so the glyphs' outlines are
//! read with ab_glyph from Segoe UI (Segoe UI Symbol for the marks it
//! lacks) and filled as paths along a direction.

use ab_glyph::{Font, FontVec, GlyphId, OutlineCurve};
use gpui::{Hsla, PathBuilder, Window, point, px};
use std::sync::OnceLock;

/// A point in logical px.
pub type P = (f32, f32);

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

/// How far the middle of a line's height (between its ascent and
/// descent) sits above its baseline, at `size` px.
pub fn middle(size: f32) -> f32 {
    fonts().first().map_or(size * 0.35, |f| {
        (f.ascent_unscaled() + f.descent_unscaled()) / 2.0 * scale(f, size)
    })
}

/// The middle of `c`'s own outline above the baseline and along the line,
/// at `size` px: to centre a mark on a point.
pub fn glyph_centre(c: char, size: f32) -> P {
    glyph(c)
        .and_then(|(f, id)| {
            let r = f.outline(id)?.bounds;
            let k = scale(f, size);
            Some(((r.min.x + r.max.x) / 2.0 * k, (r.min.y + r.max.y) / 2.0 * k))
        })
        .unwrap_or((size / 2.0, size / 2.0))
}

/// Fills `text` at `size` px, its baseline starting at `start` and running
/// along the unit vector `dir`, its glyphs' tops towards the unit vector
/// `up`.
pub fn paint(window: &mut Window, text: &str, size: f32, start: P, dir: P, up: P, color: Hsla) {
    let mut pen = 0.0;
    let mut prev: Option<(&FontVec, GlyphId)> = None;
    let mut path = PathBuilder::fill();
    let mut any = false;
    for c in text.chars() {
        let Some((font, id)) = glyph(c) else { continue };
        let k = scale(font, size);
        if let Some((pf, pid)) = prev
            && std::ptr::eq(pf, font)
        {
            pen += font.kern_unscaled(pid, id) * k;
        }
        if let Some(outline) = font.outline(id) {
            // A font unit point (x right, y up) to the screen.
            let at = |p: ab_glyph::Point| {
                let (u, v) = (pen + p.x * k, p.y * k);
                point(
                    px(start.0 + u * dir.0 + v * up.0),
                    px(start.1 + u * dir.1 + v * up.1),
                )
            };
            let mut last: Option<ab_glyph::Point> = None;
            for curve in &outline.curves {
                let (from, to) = match curve {
                    OutlineCurve::Line(a, b) => (*a, *b),
                    OutlineCurve::Quad(a, _, b) => (*a, *b),
                    OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
                };
                // A new contour wherever the last didn't end here.
                if last.is_none_or(|l| (l.x - from.x).abs() > 0.01 || (l.y - from.y).abs() > 0.01) {
                    if last.is_some() {
                        path.close();
                    }
                    path.move_to(at(from));
                }
                match curve {
                    OutlineCurve::Line(_, b) => path.line_to(at(*b)),
                    OutlineCurve::Quad(_, c, b) => path.curve_to(at(*b), at(*c)),
                    OutlineCurve::Cubic(_, c1, c2, b) => {
                        path.cubic_bezier_to(at(*b), at(*c1), at(*c2))
                    }
                }
                last = Some(to);
                any = true;
            }
            if last.is_some() {
                path.close();
            }
        }
        pen += font.h_advance_unscaled(id) * k;
        prev = Some((font, id));
    }
    if any && let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
