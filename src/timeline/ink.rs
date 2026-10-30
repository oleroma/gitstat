//! Crisp strokes for the graph.
//!
//! GPUI fills paths with 4x MSAA, which leaves a curve one or two px wide
//! stair-stepped. So a curve here is drawn by tiny-skia, whose coverage
//! anti-aliasing is exact, into a picture at the window's own device px,
//! painted 1:1 on whole device px. The pictures are kept by shape and
//! colour, and the graph has few distinct shapes (the bends between
//! neighbouring lanes), so each is drawn once.
//!
//! Straight runs are quads on whole device px, `width` rounded to whole
//! device px. Callers place lanes with [`pos`], so a run's edges fall on
//! pixel edges and a bend starts exactly where its run ends.

use gpui::{Bounds, Hsla, RenderImage, Rgba, Window, fill, point, px, size};
use resvg::tiny_skia;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

/// Pictures kept past this many are all let go, so an unusual run of
/// shapes can't grow the cache for ever.
const MAX_KEPT: usize = 4096;

type P = (f32, f32);

thread_local! {
    static KEPT: RefCell<HashMap<u64, Arc<RenderImage>>> = RefCell::new(HashMap::new());
}

/// `color`, with its alpha, laid over `bg`: the opaque colour it shows as.
/// Strokes are opaque so a bend's round end over its run doesn't show.
pub fn over(color: Hsla, bg: Hsla) -> Hsla {
    let (c, b) = (color.to_rgb(), bg.to_rgb());
    let a = c.a;
    let mix = |c: f32, b: f32| c * a + b * (1.0 - a);
    Rgba {
        r: mix(c.r, b.r),
        g: mix(c.g, b.g),
        b: mix(c.b, b.b),
        a: 1.0,
    }
    .into()
}

/// Dashes along a stroke: px drawn, then px left out.
#[derive(Clone, Copy)]
pub struct Dash {
    pub on: f32,
    pub off: f32,
}

/// A stroke `width` logical px wide, in whole device px.
fn device_width(width: f32, scale: f32) -> f32 {
    (width * scale).round().max(1.0)
}

/// Where to put the centre line of a `width`-px stroke near `v`, so that
/// its edges fall on device pixel edges: on a pixel's middle for an odd
/// number of device px, between pixels for an even one.
/// `s` is the window's scale factor.
pub fn pos(s: f32, v: f32, width: f32) -> f32 {
    let w = device_width(width, s);
    let d = v * s;
    let d = if w as u32 % 2 == 1 {
        d.floor() + 0.5
    } else {
        d.round()
    };
    d / s
}

/// A straight stroke from `a` to `b`, which share an x or a y. Its ends
/// are square; a bend's round end covers the joint.
pub fn line(window: &mut Window, a: P, b: P, width: f32, color: Hsla, dash: Option<Dash>) {
    if dash.is_some() || (a.0 != b.0 && a.1 != b.1) {
        stroke(window, &[a, a, b, b], width, color, dash);
        return;
    }
    let s = window.scale_factor();
    let w = device_width(width, s);
    let (x0, x1) = ((a.0.min(b.0) * s).round(), (a.0.max(b.0) * s).round());
    let (y0, y1) = ((a.1.min(b.1) * s).round(), (a.1.max(b.1) * s).round());
    let (l, r, t, btm) = if a.0 == b.0 {
        let l = (a.0 * s - w / 2.0).round();
        (l, l + w, y0, y1)
    } else {
        let t = (a.1 * s - w / 2.0).round();
        (x0, x1, t, t + w)
    };
    window.paint_quad(fill(
        Bounds::new(
            point(px(l / s), px(t / s)),
            size(px((r - l) / s), px((btm - t) / s)),
        ),
        color,
    ));
}

/// A cubic Bézier `[from, c1, c2, to]`, `width` px wide.
pub fn cubic(window: &mut Window, c: [P; 4], width: f32, color: Hsla, dash: Option<Dash>) {
    stroke(window, &c, width, color, dash);
}

/// An arc of the circle of radius `r` around `center`, from angle `start`
/// to `end` in radians, clockwise from the top.
pub fn arc(window: &mut Window, center: P, r: f32, start: f32, end: f32, width: f32, color: Hsla) {
    // Cubics of at most a quarter turn each, which stay within 0.03% of
    // the circle.
    let n = ((end - start).abs() / std::f32::consts::FRAC_PI_2)
        .ceil()
        .max(1.0) as usize;
    let step = (end - start) / n as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let at = |a: f32| (center.0 + r * a.sin(), center.1 - r * a.cos());
    // The tangent, clockwise from the top.
    let dir = |a: f32| (a.cos(), a.sin());
    let mut pts = Vec::with_capacity(3 * n + 1);
    for i in 0..n {
        let (a0, a1) = (start + step * i as f32, start + step * (i + 1) as f32);
        let (p0, p3) = (at(a0), at(a1));
        let (d0, d1) = (dir(a0), dir(a1));
        if i == 0 {
            pts.push(p0);
        }
        pts.push((p0.0 + d0.0 * k * r, p0.1 + d0.1 * k * r));
        pts.push((p3.0 - d1.0 * k * r, p3.1 - d1.1 * k * r));
        pts.push(p3);
    }
    stroke(window, &pts, width, color, None);
}

/// Strokes the cubics through `pts` (a start, then three points per
/// cubic) with round caps, as a picture drawn once per shape.
fn stroke(window: &mut Window, pts: &[P], width: f32, color: Hsla, dash: Option<Dash>) {
    if pts.len() < 4 {
        return;
    }
    let s = window.scale_factor();
    let w = device_width(width, s);
    let dev: Vec<P> = pts.iter().map(|&(x, y)| (x * s, y * s)).collect();
    // The picture's corner: whole device px, a stroke's width clear of
    // every point, so the curve keeps its place within a px.
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in &dev {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let pad = w + 1.0;
    let (ox, oy) = ((x0 - pad).floor(), (y0 - pad).floor());
    let (iw, ih) = (
        ((x1 + pad).ceil() - ox) as u32,
        ((y1 + pad).ceil() - oy) as u32,
    );
    if iw == 0 || ih == 0 || iw > 4096 || ih > 4096 {
        return;
    }
    let local: Vec<P> = dev.iter().map(|&(x, y)| (x - ox, y - oy)).collect();
    let rgba = color.to_rgb();
    let q = |v: f32| (v * 8.0).round() as i32;

    let mut h = DefaultHasher::new();
    for &(x, y) in &local {
        (q(x), q(y)).hash(&mut h);
    }
    (q(w), iw, ih).hash(&mut h);
    dash.map(|d| (q(d.on * s), q(d.off * s))).hash(&mut h);
    [rgba.r, rgba.g, rgba.b, rgba.a]
        .map(|c| (c * 255.0).round() as u8)
        .hash(&mut h);
    let key = h.finish();

    let image = KEPT.with(|kept| kept.borrow().get(&key).cloned());
    let image = match image {
        Some(image) => image,
        None => {
            let Some(image) = draw(&local, w, dash.map(|d| (d.on * s, d.off * s)), rgba, iw, ih)
            else {
                return;
            };
            KEPT.with(|kept| {
                let mut kept = kept.borrow_mut();
                if kept.len() >= MAX_KEPT {
                    kept.clear();
                }
                kept.insert(key, image.clone());
            });
            image
        }
    };
    let bounds = Bounds::new(
        point(px(ox / s), px(oy / s)),
        size(px(iw as f32 / s), px(ih as f32 / s)),
    );
    window
        .paint_image(bounds, Default::default(), image, 0, false)
        .ok();
}

/// The stroke as a straight-alpha BGRA picture, `iw` by `ih` device px.
fn draw(
    pts: &[P],
    width: f32,
    dash: Option<(f32, f32)>,
    color: Rgba,
    iw: u32,
    ih: u32,
) -> Option<Arc<RenderImage>> {
    let mut pixmap = tiny_skia::Pixmap::new(iw, ih)?;
    let mut path = tiny_skia::PathBuilder::new();
    path.move_to(pts[0].0, pts[0].1);
    for c in pts[1..].as_chunks::<3>().0 {
        path.cubic_to(c[0].0, c[0].1, c[1].0, c[1].1, c[2].0, c[2].1);
    }
    let path = path.finish()?;
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_skia::Color::from_rgba(
        color.r, color.g, color.b, color.a,
    )?);
    paint.anti_alias = true;
    let stroke = tiny_skia::Stroke {
        width,
        line_cap: tiny_skia::LineCap::Round,
        // A dash's round caps add half the width at each end.
        dash: dash.and_then(|(on, off)| {
            tiny_skia::StrokeDash::new(vec![(on - width).max(0.01), off + width], 0.0)
        }),
        ..Default::default()
    };
    pixmap.stroke_path(
        &path,
        &paint,
        &stroke,
        tiny_skia::Transform::identity(),
        None,
    );
    let mut data = pixmap.take_demultiplied();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(iw, ih, data)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}
