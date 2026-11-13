//! The flip from the panel to the big window: one repo's tree part way
//! through its quarter turn, from a strip (newest at the right, lanes above
//! and below HEAD's line) to a column (newest at the top, lanes side by
//! side).
//!
//! Every point is laid out in the tree's own frame, along its history and
//! across its lanes, with the spacing of both ends blended, then turned by
//! a quarter turn times the progress about its newest commit, which itself
//! slides from where the strip had it to where the column will. At 0 the
//! drawing is the strip's, at 1 the column's.
//!
//! The tree is drawn with GPUI's own paths and quads, which can sit
//! anywhere: `ink`'s crisp pictures are kept per shape and would be made
//! afresh every frame. Moving, the softer edges don't show.

use super::{Colors, column, lane_offset};
use crate::graph::{self, Class, Seg};
use crate::repo_graph::RepoGraph;
use crate::view::Palette;
use gpui::{BorderStyle, Bounds, Hsla, PathBuilder, Window, point, px, quad, size};

/// Where a tree is at the two ends of the turn, in the drawing window's
/// logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ends {
    /// The strip's newest position: `PAD` in from its graph's right edge,
    /// on its centre line.
    pub strip: (f32, f32),
    /// How much history the strip shows, px left of `strip`.
    pub strip_len: f32,
    /// The column body's top-left corner.
    pub column: (f32, f32),
    /// The column body's height.
    pub column_len: f32,
}

type P = (f32, f32);

/// The tree's frame at progress `e`: its newest position, the step between
/// two rows, the unit vectors along its history and across its lanes, and
/// how much history is drawn.
#[derive(Clone, Copy, Debug)]
struct Frame {
    origin: P,
    step: f32,
    along: P,
    across: P,
    len: f32,
    e: f32,
}

impl Frame {
    fn new(ends: Ends, e: f32) -> Self {
        let lerp = |a: f32, b: f32| a + (b - a) * e;
        // A quarter turn anticlockwise on screen (y grows down): the strip's
        // past, to the left, swings down; its lanes, up and down, swing
        // out to the right.
        let (sin, cos) = (-std::f32::consts::FRAC_PI_2 * e).sin_cos();
        let turn = |(x, y): P| (x * cos - y * sin, x * sin + y * cos);
        let column = (
            ends.column.0 + column::PAD,
            ends.column.1 + column::ROW / 2.0,
        );
        Frame {
            origin: (lerp(ends.strip.0, column.0), lerp(ends.strip.1, column.1)),
            step: lerp(super::STEP, column::ROW),
            along: turn((-1.0, 0.0)),
            across: turn((0.0, 1.0)),
            len: lerp(ends.strip_len, ends.column_len),
            e,
        }
    }

    /// Px from the history line to `lane`: the strip's alternating lanes
    /// blended into the column's side-by-side ones.
    fn lane(&self, lane: f32) -> f32 {
        let lane = lane.round().max(0.0) as usize;
        let strip = lane_offset(lane);
        let column = lane.min(column::LANES - 1) as f32 * column::LANE;
        strip + (column - strip) * self.e
    }

    /// Where position `pos` (rows from the newest) on `lane` is drawn.
    fn at(&self, pos: f32, lane: f32) -> P {
        let (u, v) = (pos * self.step, self.lane(lane));
        (
            self.origin.0 + u * self.along.0 + v * self.across.0,
            self.origin.1 + u * self.along.1 + v * self.across.1,
        )
    }

    fn blend(&self, strip: f32, column: f32) -> f32 {
        strip + (column - strip) * self.e
    }
}

/// Paints `g` at progress `e` (0 the strip, 1 the column), every colour at
/// `alpha`.
#[allow(clippy::too_many_arguments)]
pub fn paint_tree(
    g: &RepoGraph,
    dirty: bool,
    ends: Ends,
    e: f32,
    alpha: f32,
    dark: bool,
    p: &Palette,
    window: &mut Window,
) {
    if alpha <= 0.0 {
        return;
    }
    let f = Frame::new(ends, e.clamp(0.0, 1.0));
    let colors = Colors::new(dark, p);
    let rows = g.layout.nodes.len();
    let head = g.head_row.filter(|&r| r < rows);
    // As both the strip and the column place them: with uncommitted work,
    // HEAD and older move one position on, and the work-in-progress dot
    // takes HEAD's.
    let pos = |row: f32| -> f32 {
        if row >= rows as f32 {
            return row + if dirty { 1.0 } else { 0.0 };
        }
        let r = row.max(0.0) as usize;
        let shifted = dirty && head.is_none_or(|h| r >= h);
        r as f32 + if shifted { 1.0 } else { 0.0 }
    };
    // The rows within the drawn length, one more for a noodle leaving it.
    let hi = (((f.len / f.step).ceil() as usize) + 2).min(rows);
    let stroke = f.blend(super::STROKE, column::STROKE);
    let dot = f.blend(super::DOT, column::DOT);
    let a = |c: Hsla| c.opacity(c.a * alpha);
    let folded = |lane: f32| lane.round() as usize >= super::LANES;

    // Under everything, a soft band along each other worktree's line.
    let band = f.blend(super::BAND, column::BAND);
    for b in g.bands.iter().filter(|b| b.top < hi) {
        let color = a(colors.band(g, b, b.lane >= super::LANES));
        let from = f.at(pos(b.top as f32), b.lane as f32);
        let to = if b.open || b.bottom >= hi {
            f.at(f.len / f.step + 1.0, b.lane as f32)
        } else {
            f.at(pos(b.bottom as f32), b.lane as f32)
        };
        line(window, from, to, band, color);
        disc(window, from, band / 2.0, Some(color), None);
        if !b.open && b.bottom < hi {
            disc(window, to, band / 2.0, Some(color), None);
        }
    }

    for e in g.layout.visible_edges(0, hi) {
        let base = if e.first {
            colors.node(g, e.child.0)
        } else {
            match e.parent {
                Some((row, _)) => colors.node(g, row),
                None => colors.node(g, e.child.0),
            }
        };
        for seg in e.segments(rows) {
            let (s, t, curve) = match seg {
                Seg::Straight(s, t) => (s, t, false),
                Seg::Curve(s, t) => (s, t, true),
            };
            let opacity = if folded(s.lane) || folded(t.lane) {
                0.35
            } else {
                0.8
            };
            let color = a(super::ink::over(base.opacity(opacity), colors.bg));
            let (ps, pt) = (f.at(pos(s.row), s.lane), f.at(pos(t.row), t.lane));
            if curve {
                cubic(window, graph::noodle(ps, pt, f.along), stroke, color);
            } else {
                line(window, ps, pt, stroke, color);
            }
        }
    }

    if dirty {
        let (wip, lane) = match head {
            Some(h) => (h as f32, g.layout.free_lane_above(h)),
            None => (0.0, 0),
        };
        let c = f.at(wip, lane as f32);
        if let Some(h) = head {
            let to = f.at(pos(h as f32), g.layout.nodes[h].lane as f32);
            let color = a(super::ink::over(colors.unpushed.opacity(0.8), colors.bg));
            cubic(window, graph::noodle(c, to, f.along), 1.0, color);
        }
        disc(
            window,
            c,
            dot + 0.5,
            Some(a(colors.bg)),
            Some((1.0, a(colors.unpushed))),
        );
    }

    for row in 0..hi {
        let node = &g.layout.nodes[row];
        let c = f.at(pos(row as f32), node.lane as f32);
        let color = colors.node(g, row);
        let color = a(if node.lane >= super::LANES {
            color.opacity(0.5)
        } else {
            color
        });
        if g.ctx.refs.is_head(&g.commits[row].id) {
            disc(
                window,
                c,
                dot + 3.0,
                Some(a(colors.head.opacity(0.22))),
                None,
            );
            disc(window, c, dot, Some(a(colors.head)), None);
        } else if node.class == Class::Unpulled || node.merge {
            let r = if node.merge { dot - 0.5 } else { dot };
            disc(window, c, r, Some(a(colors.bg)), Some((1.5, color)));
        } else {
            disc(window, c, dot, Some(color), None);
        }
    }
}

fn pt((x, y): P) -> gpui::Point<gpui::Pixels> {
    point(px(x), px(y))
}

fn line(window: &mut Window, from: P, to: P, width: f32, color: Hsla) {
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(pt(from));
    path.line_to(pt(to));
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn cubic(window: &mut Window, [from, c1, c2, to]: [P; 4], width: f32, color: Hsla) {
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(pt(from));
    path.cubic_bezier_to(pt(to), pt(c1), pt(c2));
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// A dot of radius `r` around `c`, with a ring `(width, colour)` inside
/// its edge.
fn disc(window: &mut Window, c: P, r: f32, fill: Option<Hsla>, ring: Option<(f32, Hsla)>) {
    if r <= 0.0 {
        return;
    }
    let bounds = Bounds::new(pt((c.0 - r, c.1 - r)), size(px(2.0 * r), px(2.0 * r)));
    let (width, ring_color) = ring.unwrap_or((0.0, gpui::transparent_black()));
    window.paint_quad(quad(
        bounds,
        px(r),
        fill.unwrap_or(gpui::transparent_black()),
        px(width),
        ring_color,
        BorderStyle::Solid,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENDS: Ends = Ends {
        strip: (300.0, 50.0),
        strip_len: 280.0,
        column: (40.0, 200.0),
        column_len: 600.0,
    };

    fn near(a: P, b: P) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    #[test]
    fn starts_as_the_strip() {
        let f = Frame::new(ENDS, 0.0);
        // Older rows to the left, a step apart; lanes up and down.
        assert!(near(f.at(0.0, 0.0), (300.0, 50.0)));
        assert!(near(
            f.at(2.0, 0.0),
            (300.0 - 2.0 * super::super::STEP, 50.0)
        ));
        assert!(near(f.at(0.0, 1.0), (300.0, 50.0 + lane_offset(1))));
    }

    #[test]
    fn ends_as_the_column() {
        let f = Frame::new(ENDS, 1.0);
        let top = (40.0 + column::PAD, 200.0 + column::ROW / 2.0);
        // Older rows downwards, a row apart; lanes side by side.
        assert!(near(f.at(0.0, 0.0), top));
        assert!(near(f.at(3.0, 0.0), (top.0, top.1 + 3.0 * column::ROW)));
        assert!(near(f.at(0.0, 2.0), (top.0 + 2.0 * column::LANE, top.1)));
    }
}
