//! The flip between the panel and the big window: a repo's tree part way
//! through its quarter turn, from its strip (newest at the right, lanes
//! above and below HEAD's line) to its column (newest at the top, lanes
//! side by side).
//!
//! Both ends are the views' own: where the strip puts each dot and bend,
//! on its device px, and where the column does. Part way, each point is
//! taken in the tree's own frame (along its history, across its lanes)
//! from both, blended, and turned by a quarter turn times the progress
//! about the tree's newest row, which itself goes from where the strip
//! has it to where the column does. So at 0 every point is the strip's
//! and at 1 the column's, and there the tree is painted by the strip's
//! and the column's own code: a tree come to rest is the view's, px for
//! px.
//!
//! On the way its history is as long as the strip's box at first and the
//! column's at last, and is cut there, in its own frame.

use super::column::{self, ColumnGeo};
use super::{Colors, StripGeo, ink};
use crate::graph::{self, Class, Seg};
use crate::repo_graph::RepoGraph;
use crate::scene::parts::lerp_color;
use crate::view::Palette;
use gpui::{Bounds, Pixels, Window};
use std::sync::Arc;

type P = (f32, f32);

/// A tree the flip turns: where its strip's graph box is in the panel and
/// its column's body in the big window, logical px of the window that
/// draws the flip.
pub struct Turning {
    pub graph: Arc<RepoGraph>,
    pub dirty: bool,
    pub strip: Bounds<Pixels>,
    pub column: Bounds<Pixels>,
}

/// Paints `trees` at progress `e`: 0 the strips, as the panel draws them
/// at their newest commits with nothing hovered (less the fade at their
/// tail and their tag and stash marks, which are the panel's own), 1 the
/// columns, as the big window does.
pub fn paint_turning(trees: &[Turning], e: f32, dark: bool, p: &Palette, window: &mut Window) {
    let colors = Colors::new(dark, p);
    if e <= 0.0 {
        for t in trees {
            super::paint_strip_tree(t.strip, &t.graph, t.dirty, &colors, window);
        }
        return;
    }
    if e >= 1.0 {
        for t in trees {
            column::paint_column_tree(t.column, &t.graph, t.dirty, &colors, window);
        }
        return;
    }
    let s = window.scale_factor();
    let mut batch = ink::Batch::new(window);
    for t in trees {
        turn(t, e, &colors, &mut batch, s);
    }
    batch.paint(window);
}

/// The turning is over: lets go of what its last frame drew with.
pub fn turned(window: &mut Window) {
    ink::Batch::done(window);
}

/// The tree's own frame at progress `e`: x runs back along its history
/// (older to the left, as in the strip), y across its lanes.
struct Frame {
    e: f32,
    /// The newest row on HEAD's line: in the strip, in the column, and
    /// now.
    strip: P,
    column: P,
    now: P,
    sin: f32,
    cos: f32,
    /// How much history shows, px.
    len: f32,
}

impl Frame {
    /// A point the strip has at `s` and the column at `c`, in the frame.
    fn local(&self, s: P, c: P) -> P {
        let a = (s.0 - self.strip.0, s.1 - self.strip.1);
        // The column's turned back a quarter turn: down its history is
        // left along the strip's, right across its lanes is down.
        let b = (-(c.1 - self.column.1), c.0 - self.column.0);
        (a.0 + (b.0 - a.0) * self.e, a.1 + (b.1 - a.1) * self.e)
    }

    /// Where a point of the frame is drawn.
    fn screen(&self, (x, y): P) -> P {
        (
            self.now.0 + x * self.cos - y * self.sin,
            self.now.1 + x * self.sin + y * self.cos,
        )
    }

    fn shows(&self, l: P) -> bool {
        l.0 >= -self.len
    }
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// A point of the cubic `c`.
fn along(c: &[P; 4], t: f32) -> P {
    let u = 1.0 - t;
    let k = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    (
        k[0] * c[0].0 + k[1] * c[1].0 + k[2] * c[2].0 + k[3] * c[3].0,
        k[0] * c[0].1 + k[1] * c[1].1 + k[2] * c[2].1 + k[3] * c[3].1,
    )
}

/// The cubic `c` from `t0` to `t1`.
fn piece(c: &[P; 4], t0: f32, t1: f32) -> [P; 4] {
    // The tangent at t, a third of the piece's length long.
    let d = |t: f32| {
        let u = 1.0 - t;
        let k = [3.0 * u * u, 6.0 * u * t, 3.0 * t * t];
        let v =
            |a: f32, b: f32, c_: f32, d: f32| k[0] * (b - a) + k[1] * (c_ - b) + k[2] * (d - c_);
        (
            v(c[0].0, c[1].0, c[2].0, c[3].0) * (t1 - t0) / 3.0,
            v(c[0].1, c[1].1, c[2].1, c[3].1) * (t1 - t0) / 3.0,
        )
    };
    let (a, b) = (along(c, t0), along(c, t1));
    let (da, db) = (d(t0), d(t1));
    [a, (a.0 + da.0, a.1 + da.1), (b.0 - db.0, b.1 - db.1), b]
}

/// What of the cubic `c` lies at x of `min` or more: a noodle only ever
/// crosses a given x once.
fn cut(c: [P; 4], min: f32) -> Option<[P; 4]> {
    let (a, b) = (c[0].0 >= min, c[3].0 >= min);
    match (a, b) {
        (true, true) => Some(c),
        (false, false) => None,
        _ => {
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..20 {
                let mid = (lo + hi) / 2.0;
                if (along(&c, mid).0 >= min) == a {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let t = (lo + hi) / 2.0;
            Some(if a {
                piece(&c, 0.0, t)
            } else {
                piece(&c, t, 1.0)
            })
        }
    }
}

/// What of the run from `a` to `b` lies at x of `min` or more.
fn cut_run(a: P, b: P, min: f32) -> Option<(P, P)> {
    let at = |from: P, to: P| {
        let t = (min - from.0) / (to.0 - from.0);
        (min, from.1 + (to.1 - from.1) * t)
    };
    match (a.0 >= min, b.0 >= min) {
        (true, true) => Some((a, b)),
        (false, false) => None,
        (true, false) => Some((a, at(a, b))),
        (false, true) => Some((at(b, a), b)),
    }
}

fn turn(t: &Turning, e: f32, colors: &Colors, batch: &mut ink::Batch, s: f32) {
    let g = &*t.graph;
    let rows = g.layout.nodes.len();
    let sg = StripGeo::new(t.strip, g, 0.0, t.dirty, s);
    let cg = ColumnGeo::new(t.column, g, t.dirty, s);
    let (strip, col) = ((sg.x(0.0), sg.y(0.0)), cg.at(0.0, 0.0));
    let (sin, cos) = (-std::f32::consts::FRAC_PI_2 * e).sin_cos();
    let f = Frame {
        e,
        strip,
        column: col,
        // Along the same path as everything else that travels.
        now: {
            let (x, y) = crate::scene::parts::sway(e);
            (mix(strip.0, col.0, x), mix(strip.1, col.1, y))
        },
        sin,
        cos,
        len: mix(strip.0 - f32::from(t.strip.left()), cg.bottom() - col.1, e),
    };
    let at = |row: f32, lane: f32| f.local((sg.x(row), sg.y(lane)), cg.at(row, lane));
    let hi = sg.rows(rows).1.max(cg.rows().1).min(rows);
    let wide =
        |strip: f32, column: f32| mix(ink::device_width(strip, s), ink::device_width(column, s), e);
    let stroke = wide(super::STROKE, column::STROKE);
    let folds = |lane: f32| {
        let lane = lane.round() as usize;
        (lane >= super::LANES, lane >= column::LANES)
    };

    // Under everything, a soft band along each other worktree's line.
    let band = wide(super::BAND, column::BAND);
    let r = mix(super::BAND, column::BAND, e) / 2.0;
    for b in g.bands.iter().filter(|b| b.top < hi) {
        let color = lerp_color(
            colors.band(g, b, b.lane >= super::LANES),
            colors.band(g, b, b.lane >= column::LANES),
            e,
        );
        let top = at(b.top as f32, b.lane as f32);
        let end = if b.open {
            (-f.len - r, top.1)
        } else {
            at(b.bottom as f32, b.lane as f32)
        };
        let Some((from, to)) = cut_run(top, end, -f.len) else {
            continue;
        };
        batch.line(f.screen(from), f.screen(to), band, color);
        if f.shows(top) {
            batch.circle(f.screen(top), r, Some(color), None);
        }
        if !b.open && f.shows(end) {
            batch.circle(f.screen(end), r, Some(color), None);
        }
    }

    for edge in g.layout.visible_edges(0, hi) {
        let base = if edge.first {
            colors.node(g, edge.child.0)
        } else {
            match edge.parent {
                Some((row, _)) => colors.node(g, row),
                None => colors.node(g, edge.child.0),
            }
        };
        for seg in edge.segments(rows) {
            let (a, b, curve) = match seg {
                Seg::Straight(a, b) => (a, b, false),
                Seg::Curve(a, b) => (a, b, true),
            };
            let (fa, fb) = (folds(a.lane), folds(b.lane));
            let alpha = mix(
                if fa.0 || fb.0 { 0.35 } else { 0.8 },
                if fa.1 || fb.1 { 0.35 } else { 0.85 },
                e,
            );
            let color = ink::over(base.opacity(alpha), colors.bg);
            let (pa, pb) = (at(a.row, a.lane), at(b.row, b.lane));
            if curve {
                if let Some(c) = cut(graph::noodle(pa, pb, (-1.0, 0.0)), -f.len) {
                    batch.cubic(c.map(|p| f.screen(p)), stroke, color, None);
                }
            } else if let Some((from, to)) = cut_run(pa, pb, -f.len) {
                batch.line(f.screen(from), f.screen(to), stroke, color);
            }
        }
    }

    if t.dirty {
        let (rs, rc) = (super::DOT + 0.5, column::DOT);
        let ((cs, ns), (cc, nc)) = (sg.wip(g, rs), cg.wip(g, rc));
        if let (Some([head_s, dot_s]), Some([head_c, dot_c])) = (ns, nc) {
            // Dashed from the end each view draws it from, whichever is
            // nearer.
            let [a, c1, c2, b] =
                graph::noodle(f.local(dot_s, dot_c), f.local(head_s, head_c), (-1.0, 0.0))
                    .map(|p| f.screen(p));
            batch.cubic(
                if e < 0.5 {
                    [b, c2, c1, a]
                } else {
                    [a, c1, c2, b]
                },
                ink::device_width(1.0, s),
                ink::over(colors.unpushed.opacity(0.8), colors.bg),
                Some(ink::Dash { on: 2.0, off: 2.0 }),
            );
        }
        batch.circle(
            f.screen(f.local(cs, cc)),
            mix(rs, rc, e),
            Some(colors.bg),
            Some((1.0, colors.unpushed, true)),
        );
    }

    let dot = mix(super::DOT, column::DOT, e);
    for row in 0..hi {
        let node = &g.layout.nodes[row];
        let l = at(row as f32, node.lane as f32);
        if !f.shows(l) {
            continue;
        }
        let c = f.screen(l);
        let color = colors.node(g, row);
        let dim = |folded: bool| if folded { color.opacity(0.5) } else { color };
        let color = lerp_color(
            dim(node.lane >= super::LANES),
            dim(node.lane >= column::LANES),
            e,
        );
        if g.ctx.refs.is_head(&g.commits[row].id) {
            batch.circle(
                c,
                dot + mix(3.0, 3.5, e),
                Some(colors.head.opacity(0.22)),
                None,
            );
            batch.circle(c, dot, Some(colors.head), None);
        } else if node.class == Class::Unpulled || node.merge {
            let r = if node.merge { dot - 0.5 } else { dot };
            batch.circle(
                c,
                r,
                Some(colors.bg),
                Some((mix(1.5, 1.75, e), color, false)),
            );
        } else {
            batch.circle(c, dot, Some(color), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: P, b: P) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    fn frame(e: f32) -> Frame {
        let (sin, cos) = (-std::f32::consts::FRAC_PI_2 * e).sin_cos();
        let (strip, column) = ((300.0, 50.0), (54.0, 212.0));
        Frame {
            e,
            strip,
            column,
            now: (mix(strip.0, column.0, e), mix(strip.1, column.1, e)),
            sin,
            cos,
            len: 100.0,
        }
    }

    #[test]
    fn a_point_starts_where_the_strip_has_it_and_ends_where_the_column_does() {
        // Three rows back and a lane out, in each view.
        let (s, c) = ((258.0, 45.0), (66.0, 284.0));
        let at = |e: f32| {
            let f = frame(e);
            f.screen(f.local(s, c))
        };
        assert!(near(at(0.0), s));
        assert!(near(at(1.0), c));
        // Half way it has swung down and to the left of the strip's.
        let mid = at(0.5);
        assert!(mid.0 < s.0 && mid.1 > s.1);
    }

    #[test]
    fn a_noodle_is_cut_where_the_history_ends() {
        let c = graph::noodle((-90.0, 0.0), (-104.0, 5.0), (-1.0, 0.0));
        let kept = cut(c, -100.0).unwrap();
        assert!(near(kept[0], c[0]));
        assert!((kept[3].0 + 100.0).abs() < 1e-2);
        // The other way round, its far part is what shows.
        let back = cut([c[3], c[2], c[1], c[0]], -100.0).unwrap();
        assert!(near(back[3], c[0]));
        assert!(cut(c, -80.0).is_none());
        assert!(near(cut(c, -200.0).unwrap()[3], c[3]));
    }
}
