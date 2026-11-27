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
pub(super) fn device_width(width: f32, scale: f32) -> f32 {
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
    if iw == 0 || ih == 0 || iw > 4096 || ih > 4096 || masked(window, (ox, oy), (iw, ih)) {
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

    let Some(image) = kept(key, || {
        draw(&local, w, dash.map(|d| (d.on * s, d.off * s)), rgba, iw, ih)
    }) else {
        return;
    };
    paint(window, (ox, oy), (iw, ih), image);
}

/// Whether a picture with its corner at device px `at` falls wholly
/// outside what `window` draws into now: then it isn't made or kept at
/// all. A picture painted many times under small masks (the flip's cuts)
/// costs nothing where it doesn't show.
fn masked(window: &Window, at: P, (iw, ih): (u32, u32)) -> bool {
    let s = window.scale_factor();
    let bounds = Bounds::new(
        point(px(at.0 / s), px(at.1 / s)),
        size(px(iw as f32 / s), px(ih as f32 / s)),
    );
    !window.content_mask().bounds.intersects(&bounds)
}

/// The picture kept under `key`, made with `make` the first time.
fn kept(key: u64, make: impl FnOnce() -> Option<Arc<RenderImage>>) -> Option<Arc<RenderImage>> {
    if let Some(image) = KEPT.with(|kept| kept.borrow().get(&key).cloned()) {
        return Some(image);
    }
    let image = make()?;
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.len() >= MAX_KEPT {
            kept.clear();
        }
        kept.insert(key, image.clone());
    });
    Some(image)
}

/// Paints a picture 1:1 with its corner at device px `at`.
fn paint(window: &mut Window, at: P, (iw, ih): (u32, u32), image: Arc<RenderImage>) {
    let s = window.scale_factor();
    let bounds = Bounds::new(
        point(px(at.0 / s), px(at.1 / s)),
        size(px(iw as f32 / s), px(ih as f32 / s)),
    );
    window
        .paint_image(bounds, Default::default(), image, 0, false)
        .ok();
}

/// A dot of radius `r` around `center`, filled with `fill`, with a ring
/// `(width, colour, dashed)` just inside its edge. Drawn like the bends:
/// GPUI's own circles take their coverage at pixel centres, which leaves
/// a dot a few px wide visibly flat-sided.
pub fn circle(
    window: &mut Window,
    center: P,
    r: f32,
    fill: Option<Hsla>,
    ring: Option<(f32, Hsla, bool)>,
) {
    let s = window.scale_factor();
    let (cx, cy, rd) = (center.0 * s, center.1 * s, r * s);
    if rd <= 0.0 {
        return;
    }
    let (ox, oy) = ((cx - rd - 1.0).floor(), (cy - rd - 1.0).floor());
    let (iw, ih) = (
        ((cx + rd + 1.0).ceil() - ox) as u32,
        ((cy + rd + 1.0).ceil() - oy) as u32,
    );
    if masked(window, (ox, oy), (iw, ih)) {
        return;
    }
    let (lx, ly) = (cx - ox, cy - oy);
    let ring = ring.map(|(w, c, dashed)| (w * s, c.to_rgb(), dashed));
    let fill = fill.map(|c| c.to_rgb());
    let q = |v: f32| (v * 8.0).round() as i32;
    let byte = |c: Rgba| [c.r, c.g, c.b, c.a].map(|v| (v * 255.0).round() as u8);
    let mut h = DefaultHasher::new();
    ("circle", q(lx), q(ly), q(rd), iw, ih).hash(&mut h);
    fill.map(byte).hash(&mut h);
    ring.map(|(w, c, d)| (q(w), byte(c), d)).hash(&mut h);
    let Some(image) = kept(h.finish(), || {
        let mut pixmap = tiny_skia::Pixmap::new(iw, ih)?;
        let solid = |c: Rgba| {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color(tiny_skia::Color::from_rgba(c.r, c.g, c.b, c.a)?);
            paint.anti_alias = true;
            Some(paint)
        };
        let id = tiny_skia::Transform::identity();
        if let Some(c) = fill {
            let path = tiny_skia::PathBuilder::from_circle(lx, ly, rd)?;
            pixmap.fill_path(&path, &solid(c)?, tiny_skia::FillRule::Winding, id, None);
        }
        if let Some((w, c, dashed)) = ring {
            let rr = (rd - w / 2.0).max(0.1);
            let path = tiny_skia::PathBuilder::from_circle(lx, ly, rr)?;
            // Eight dashes around, whatever the size.
            let arc = std::f32::consts::TAU * rr / 16.0;
            let stroke = tiny_skia::Stroke {
                width: w,
                dash: if dashed {
                    tiny_skia::StrokeDash::new(vec![arc, arc], 0.0)
                } else {
                    None
                },
                ..Default::default()
            };
            pixmap.stroke_path(&path, &solid(c)?, &stroke, id, None);
        }
        to_image(pixmap)
    }) else {
        return;
    };
    paint(window, (ox, oy), (iw, ih), image);
}

/// A tiny-skia picture as the straight-alpha BGRA GPUI takes.
pub(crate) fn to_image(pixmap: tiny_skia::Pixmap) -> Option<Arc<RenderImage>> {
    let (iw, ih) = (pixmap.width(), pixmap.height());
    let mut data = pixmap.take_demultiplied();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(iw, ih, data)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
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
    to_image(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_land_on_pixel_edges() {
        // 2 device px wide: centred between pixels.
        assert_eq!(pos(1.0, 14.3, 1.75), 14.0);
        // 3 device px wide at 150%: centred on a pixel's middle.
        let x = pos(1.5, 14.3, 1.75) * 1.5;
        assert_eq!(x.fract(), 0.5);
        // At least one device px, however thin.
        assert_eq!(device_width(0.2, 1.0), 1.0);
    }
}

/// One frame's bends and dots of trees that are turning, where no two
/// frames draw the same shape: instead of a picture kept per shape, all
/// of them are drawn into one picture, each in a tile of its own, which is
/// painted tile by tile where the shapes go, 1:1 on whole device px, and
/// let go when the next frame's comes. Drawn by the same code as the
/// pictures kept at rest, so a shape that has come to rest looks as it
/// does there. Straight runs, which GPUI draws well wherever they are,
/// are painted in their turn among them.
pub struct Batch {
    s: f32,
    ops: Vec<Op>,
}

enum Op {
    Line {
        a: P,
        b: P,
        width: f32,
        color: Hsla,
    },
    Stroke {
        /// Device px.
        pts: Vec<P>,
        width: f32,
        dash: Option<(f32, f32)>,
        color: Rgba,
    },
    Circle {
        /// Its centre and radius, device px.
        c: P,
        r: f32,
        fill: Option<Rgba>,
        ring: Option<(f32, Rgba, bool)>,
    },
}

thread_local! {
    /// The last frame's picture, let go as the next is painted.
    static LAST: RefCell<Option<Arc<RenderImage>>> = const { RefCell::new(None) };
}

/// A shape's tile: where it goes in the window, its size, and where it is
/// in the picture; device px.
type Tile = (P, (u32, u32), (u32, u32));

/// The packed picture's width, device px.
const SHEET: u32 = 2048;

impl Batch {
    pub fn new(window: &Window) -> Self {
        Self {
            s: window.scale_factor(),
            ops: Vec::new(),
        }
    }

    /// A straight stroke from `a` to `b`, logical px, `width` device px
    /// wide.
    pub fn line(&mut self, a: P, b: P, width: f32, color: Hsla) {
        self.ops.push(Op::Line { a, b, width, color });
    }

    /// A cubic Bézier, logical px, `width` device px wide; `dash` in
    /// logical px.
    pub fn cubic(&mut self, c: [P; 4], width: f32, color: Hsla, dash: Option<Dash>) {
        let s = self.s;
        self.ops.push(Op::Stroke {
            pts: c.iter().map(|&(x, y)| (x * s, y * s)).collect(),
            width,
            dash: dash.map(|d| (d.on * s, d.off * s)),
            color: color.to_rgb(),
        });
    }

    /// A dot, as [`circle`] draws it.
    pub fn circle(
        &mut self,
        center: P,
        r: f32,
        fill: Option<Hsla>,
        ring: Option<(f32, Hsla, bool)>,
    ) {
        let s = self.s;
        if r <= 0.0 {
            return;
        }
        self.ops.push(Op::Circle {
            c: (center.0 * s, center.1 * s),
            r: r * s,
            fill: fill.map(|c| c.to_rgb()),
            ring: ring.map(|(w, c, dashed)| (w * s, c.to_rgb(), dashed)),
        });
    }

    /// Paints everything, in the order it was added.
    pub fn paint(self, window: &mut Window) {
        let s = self.s;
        // Each shape's tile: where it goes in the window, its size, and
        // where it is in the picture.
        let mut tiles: Vec<Option<Tile>> = Vec::with_capacity(self.ops.len());
        let (mut x, mut y, mut shelf) = (0u32, 0u32, 0u32);
        for op in &self.ops {
            let (lo, hi) = match op {
                Op::Line { .. } => {
                    tiles.push(None);
                    continue;
                }
                Op::Stroke { pts, width, .. } => {
                    let pad = width + 1.0;
                    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                    for &(px, py) in pts {
                        (x0, y0, x1, y1) = (x0.min(px), y0.min(py), x1.max(px), y1.max(py));
                    }
                    ((x0 - pad, y0 - pad), (x1 + pad, y1 + pad))
                }
                Op::Circle { c, r, .. } => (
                    (c.0 - r - 1.0, c.1 - r - 1.0),
                    (c.0 + r + 1.0, c.1 + r + 1.0),
                ),
            };
            let at = (lo.0.floor(), lo.1.floor());
            let (w, h) = ((hi.0.ceil() - at.0) as u32, (hi.1.ceil() - at.1) as u32);
            if w == 0 || h == 0 || w > SHEET || h > 4096 || masked(window, at, (w, h)) {
                tiles.push(None);
                continue;
            }
            if x + w > SHEET {
                (x, y, shelf) = (0, y + shelf, 0);
            }
            tiles.push(Some((at, (w, h), (x, y))));
            x += w;
            shelf = shelf.max(h);
        }
        let height = y + shelf;
        let image = (height > 0 && height <= 16384)
            .then(|| tiny_skia::Pixmap::new(SHEET, height))
            .flatten()
            .and_then(|mut pixmap| {
                for (op, tile) in self.ops.iter().zip(&tiles) {
                    let Some((at, _, place)) = tile else { continue };
                    let shift = tiny_skia::Transform::from_translate(
                        place.0 as f32 - at.0,
                        place.1 as f32 - at.1,
                    );
                    match op {
                        Op::Line { .. } => {}
                        Op::Stroke {
                            pts,
                            width,
                            dash,
                            color,
                        } => stroke_into(&mut pixmap, pts, *width, *dash, *color, shift),
                        Op::Circle { c, r, fill, ring } => {
                            circle_into(&mut pixmap, *c, *r, *fill, *ring, shift)
                        }
                    }
                }
                to_image(pixmap)
            });
        if let Some(old) = LAST.with(|last| last.replace(image.clone())) {
            window.drop_image(old).ok();
        }
        let whole = size(px(SHEET as f32 / s), px(height as f32 / s));
        for (op, tile) in self.ops.iter().zip(&tiles) {
            if let Op::Line { a, b, width, color } = op {
                let mut path = gpui::PathBuilder::stroke(px(width / s));
                path.move_to(point(px(a.0), px(a.1)));
                path.line_to(point(px(b.0), px(b.1)));
                if let Ok(path) = path.build() {
                    window.paint_path(path, *color);
                }
                continue;
            }
            let (Some((at, (w, h), place)), Some(image)) = (tile, &image) else {
                continue;
            };
            // The whole picture, placed so that this tile lands where its
            // shape goes, and cut to the tile.
            let tile = Bounds::new(
                point(px(at.0 / s), px(at.1 / s)),
                size(px(*w as f32 / s), px(*h as f32 / s)),
            );
            let all = Bounds::new(
                point(
                    px((at.0 - place.0 as f32) / s),
                    px((at.1 - place.1 as f32) / s),
                ),
                whole,
            );
            window.with_content_mask(Some(gpui::ContentMask { bounds: tile }), |window| {
                window
                    .paint_image(all, Default::default(), image.clone(), 0, false)
                    .ok();
            });
        }
    }

    /// Lets go of the last frame's picture: the turning is over.
    pub fn done(window: &mut Window) {
        if let Some(old) = LAST.with(|last| last.take()) {
            window.drop_image(old).ok();
        }
    }
}

/// Strokes the cubics through `pts` into `pixmap`, as [`draw`] does.
fn stroke_into(
    pixmap: &mut tiny_skia::Pixmap,
    pts: &[P],
    width: f32,
    dash: Option<(f32, f32)>,
    color: Rgba,
    shift: tiny_skia::Transform,
) {
    let mut path = tiny_skia::PathBuilder::new();
    path.move_to(pts[0].0, pts[0].1);
    for c in pts[1..].as_chunks::<3>().0 {
        path.cubic_to(c[0].0, c[0].1, c[1].0, c[1].1, c[2].0, c[2].1);
    }
    let Some(path) = path.finish() else { return };
    let mut paint = tiny_skia::Paint::default();
    let Some(color) = tiny_skia::Color::from_rgba(color.r, color.g, color.b, color.a) else {
        return;
    };
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = tiny_skia::Stroke {
        width,
        line_cap: tiny_skia::LineCap::Round,
        dash: dash.and_then(|(on, off)| {
            tiny_skia::StrokeDash::new(vec![(on - width).max(0.01), off + width], 0.0)
        }),
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, shift, None);
}

/// Draws a dot into `pixmap`, as [`circle`] does.
fn circle_into(
    pixmap: &mut tiny_skia::Pixmap,
    (cx, cy): P,
    rd: f32,
    fill: Option<Rgba>,
    ring: Option<(f32, Rgba, bool)>,
    shift: tiny_skia::Transform,
) {
    let solid = |c: Rgba| {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(tiny_skia::Color::from_rgba(c.r, c.g, c.b, c.a)?);
        paint.anti_alias = true;
        Some(paint)
    };
    if let Some(c) = fill
        && let (Some(path), Some(paint)) =
            (tiny_skia::PathBuilder::from_circle(cx, cy, rd), solid(c))
    {
        pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, shift, None);
    }
    if let Some((w, c, dashed)) = ring {
        let rr = (rd - w / 2.0).max(0.1);
        let arc = std::f32::consts::TAU * rr / 16.0;
        let stroke = tiny_skia::Stroke {
            width: w,
            dash: if dashed {
                tiny_skia::StrokeDash::new(vec![arc, arc], 0.0)
            } else {
                None
            },
            ..Default::default()
        };
        if let (Some(path), Some(paint)) =
            (tiny_skia::PathBuilder::from_circle(cx, cy, rr), solid(c))
        {
            pixmap.stroke_path(&path, &paint, &stroke, shift, None);
        }
    }
}
