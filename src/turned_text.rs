//! Text turned a quarter turn anticlockwise, reading upwards, for the big
//! window's section spines. GPUI can't turn text, so the line is drawn with
//! ab_glyph from Segoe UI, turned, and painted as a picture, kept per text,
//! size and colour.

use ab_glyph::{Font, PxScale, ScaleFont};
use gpui::{Bounds, Hsla, Pixels, RenderImage, Window, point, px, size};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// A picture's text, height in device px and colour.
type Key = (String, u32, [u8; 4]);

thread_local! {
    static KEPT: RefCell<HashMap<Key, Option<Arc<RenderImage>>>> =
        RefCell::new(HashMap::new());
}

/// How long `text` runs at `height` logical px: the turned picture's
/// height. 0 without the font.
pub fn length(text: &str, height: f32) -> f32 {
    let Some(font) = crate::icon::system_font() else {
        return 0.0;
    };
    advance(font, text, height).ceil()
}

fn advance(font: &impl Font, text: &str, height: f32) -> f32 {
    let f = font.as_scaled(PxScale::from(height));
    let mut pen = 0.0;
    let mut prev = None;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(prev) = prev {
            pen += f.kern(prev, id);
        }
        pen += f.h_advance(id);
        prev = Some(id);
    }
    pen
}

/// Paints `text`, turned to read upwards, at `height` logical px, centred
/// across `bounds` and starting from its bottom.
pub fn paint(window: &mut Window, bounds: Bounds<Pixels>, text: &str, height: f32, color: Hsla) {
    let s = window.scale_factor();
    let dev = (height * s).round() as u32;
    let rgba = color.to_rgb();
    let key = (
        text.to_string(),
        dev,
        [rgba.r, rgba.g, rgba.b, rgba.a].map(|c| (c * 255.0).round() as u8),
    );
    let image = KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.len() > 64 {
            kept.clear();
        }
        kept.entry(key.clone())
            .or_insert_with(|| draw(text, dev, key.2))
            .clone()
    });
    let Some(image) = image else { return };
    let frame = image.size(0);
    let (iw, ih) = (frame.width.0 as f32 / s, frame.height.0 as f32 / s);
    let left = bounds.left() + (bounds.size.width - px(iw)) / 2.0;
    let top = bounds.bottom() - px(ih);
    // Whole device px, so the letters stay crisp.
    let snap = |v: Pixels| px((f32::from(v) * s).round() / s);
    window
        .paint_image(
            Bounds::new(point(snap(left), snap(top)), size(px(iw), px(ih))),
            Default::default(),
            image,
            0,
            false,
        )
        .ok();
}

/// The line drawn `height` device px tall, then turned: straight-alpha BGRA.
fn draw(text: &str, height: u32, [r, g, b, a]: [u8; 4]) -> Option<Arc<RenderImage>> {
    let font = crate::icon::system_font()?;
    let h = height as f32;
    let f = font.as_scaled(PxScale::from(h));
    let w = advance(font, text, h).ceil() as usize + 2;
    let rows = (f.ascent() - f.descent()).ceil() as usize + 2;
    if w == 0 || rows == 0 || w > 4096 {
        return None;
    }
    // The line as it reads, left to right: coverage per px.
    let mut mask = vec![0f32; w * rows];
    let baseline = f.ascent().round() + 1.0;
    let (mut pen, mut prev) = (1.0f32, None);
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(prev) = prev {
            pen += f.kern(prev, id);
        }
        let glyph = id.with_scale_and_position(PxScale::from(h), ab_glyph::point(pen, baseline));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bb = outlined.px_bounds();
            let (left, top) = (bb.min.x as i32, bb.min.y as i32);
            outlined.draw(|gx, gy, cov| {
                let (x, y) = (left + gx as i32, top + gy as i32);
                if (0..w as i32).contains(&x) && (0..rows as i32).contains(&y) {
                    let m = &mut mask[y as usize * w + x as usize];
                    *m = (*m + cov).min(1.0);
                }
            });
        }
        pen += f.h_advance(id);
        prev = Some(id);
    }
    // Turned anticlockwise: the line's start at the bottom, its top edge
    // to the left. Source (x, y) lands at (y, w - 1 - x).
    let (iw, ih) = (rows, w);
    let mut data = vec![0u8; iw * ih * 4];
    for y in 0..rows {
        for x in 0..w {
            let cov = mask[y * w + x];
            if cov <= 0.0 {
                continue;
            }
            let (dx, dy) = (y, w - 1 - x);
            let i = (dy * iw + dx) * 4;
            data[i..i + 4].copy_from_slice(&[b, g, r, (a as f32 * cov).round() as u8]);
        }
    }
    let buffer = image::RgbaImage::from_raw(iw as u32, ih as u32, data)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}
