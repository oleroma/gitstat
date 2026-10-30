//! Anti-aliased strokes for the graph.
//!
//! GPUI fills paths with 4x MSAA, which leaves a curve one or two px wide
//! visibly stair-stepped, while its quads, rounded ones included, get exact
//! anti-aliasing from their distance field. So a curve here is a run of
//! small round quads, close enough together that their union is the stroke
//! with round caps, and its edges are as smooth as a dot's.
//!
//! The quads overlap, so a translucent colour would darken where they do:
//! strokes are opaque, their colour first laid over the background.

use gpui::{Bounds, Hsla, Rgba, Window, fill, point, px, size};

/// Px between stamp centres along a stroke; small against the thinnest
/// stroke's radius, so the edge has no scallops.
const SPACING: f32 = 0.3;
/// Pieces a curve is cut into to measure it; its stamps follow the pieces.
const PIECES: usize = 24;

type P = (f32, f32);

/// `color`, with its alpha, laid over `bg`: the opaque colour it shows as.
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

/// A cubic Bézier `[from, c1, c2, to]`, `width` px wide, in an opaque
/// `color` (see [`over`]).
pub fn cubic(window: &mut Window, c: [P; 4], width: f32, color: Hsla, dash: Option<Dash>) {
    let mut pts = [(0.0, 0.0); PIECES + 1];
    for (i, p) in pts.iter_mut().enumerate() {
        *p = bezier(c, i as f32 / PIECES as f32);
    }
    polyline(window, &pts, width, color, dash);
}

/// A straight stroke from `a` to `b` with round caps.
pub fn line(window: &mut Window, a: P, b: P, width: f32, color: Hsla, dash: Option<Dash>) {
    polyline(window, &[a, b], width, color, dash);
}

fn bezier([p0, p1, p2, p3]: [P; 4], t: f32) -> P {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
        a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
    )
}

/// Stamps round quads at even steps along `pts`.
fn polyline(window: &mut Window, pts: &[P], width: f32, color: Hsla, dash: Option<Dash>) {
    let r = width / 2.0;
    for at in stamps(pts, r, dash) {
        window.paint_quad(
            fill(
                Bounds::new(point(px(at.0 - r), px(at.1 - r)), size(px(width), px(width))),
                color,
            )
            .corner_radii(px(r)),
        );
    }
}

/// Where the stamps of a stroke of radius `r` along `pts` go. A dash's
/// stamps stop `2r` short of its end, so its caps keep it `on` px long.
fn stamps(pts: &[P], r: f32, dash: Option<Dash>) -> Vec<P> {
    let mut out = Vec::new();
    let Some(&first) = pts.first() else {
        return out;
    };
    let period = dash.map(|d| d.on + d.off).filter(|&p| p > 0.0);
    let drawn = |s: f32| match (dash, period) {
        (Some(d), Some(p)) => s.rem_euclid(p) <= (d.on - 2.0 * r).max(0.0),
        _ => true,
    };
    // Arc length walked so far, and where the next stamp is due.
    let (mut walked, mut next) = (0.0f32, 0.0f32);
    let mut prev = first;
    if drawn(0.0) {
        out.push(first);
    }
    next += SPACING;
    for &p in &pts[1..] {
        let len = (p.0 - prev.0).hypot(p.1 - prev.1);
        while next <= walked + len && len > 0.0 {
            let t = (next - walked) / len;
            if drawn(next) {
                out.push((prev.0 + (p.0 - prev.0) * t, prev.1 + (p.1 - prev.1) * t));
            }
            next += SPACING;
        }
        walked += len;
        prev = p;
    }
    // The far end gets its cap even between steps.
    if drawn(walked) && out.last() != Some(&prev) {
        out.push(prev);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_cover_the_whole_line_evenly() {
        let s = stamps(&[(0.0, 0.0), (3.0, 0.0)], 0.75, None);
        assert_eq!(s.first(), Some(&(0.0, 0.0)));
        assert_eq!(s.last(), Some(&(3.0, 0.0)));
        for w in s.windows(2) {
            let gap = w[1].0 - w[0].0;
            assert!(gap > 0.0 && gap <= SPACING + 1e-4, "{gap}");
        }
    }

    #[test]
    fn dashes_leave_gaps() {
        let d = Some(Dash { on: 2.0, off: 2.0 });
        let s = stamps(&[(0.0, 0.0), (8.0, 0.0)], 0.5, d);
        // Stamps cover [0, 1] and [4, 5], so the caps make 2 px dashes.
        assert!(s.iter().all(|p| p.0.rem_euclid(4.0) <= 1.0 + 1e-4));
        assert!(s.iter().any(|p| p.0 > 4.0));
    }

    #[test]
    fn bezier_ends_on_its_ends() {
        let c = [(0.0, 0.0), (5.0, 0.0), (5.0, 10.0), (10.0, 10.0)];
        assert_eq!(bezier(c, 0.0), (0.0, 0.0));
        assert_eq!(bezier(c, 1.0), (10.0, 10.0));
    }
}
