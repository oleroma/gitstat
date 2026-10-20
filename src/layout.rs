//! Pure window geometry, all in physical px unless a name says logical.

use crate::config::Corner;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub const fn centre(&self) -> (i32, i32) {
        (self.left + self.width() / 2, self.top + self.height() / 2)
    }

    const fn sized(left: i32, top: i32, w: i32, h: i32) -> Self {
        Self::new(left, top, left + w, top + h)
    }

    /// Grown by `d` on every side.
    pub const fn outset(&self, d: i32) -> Self {
        Self::new(self.left - d, self.top - d, self.right + d, self.bottom + d)
    }

    /// Shrunk by `d` on every side, but never past its own centre.
    fn inset(&self, d: i32) -> Self {
        let dx = d.max(0).min(self.width().max(0) / 2);
        let dy = d.max(0).min(self.height().max(0) / 2);
        Self::new(
            self.left + dx,
            self.top + dy,
            self.right - dx,
            self.bottom - dy,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

/// The side of the monitor the taskbar sits on: the side where `work` leaves
/// the widest gap in `monitor`. An auto-hide taskbar leaves no gap, so then
/// it is the monitor edge nearest to `anchor` (the tray icon).
pub fn taskbar_edge(monitor: Rect, work: Rect, anchor: Rect) -> Edge {
    // Bottom last: max_by_key keeps the last of equal gaps.
    let gaps = [
        (Edge::Left, work.left - monitor.left),
        (Edge::Top, work.top - monitor.top),
        (Edge::Right, monitor.right - work.right),
        (Edge::Bottom, monitor.bottom - work.bottom),
    ];
    if let Some(&(edge, gap)) = gaps.iter().max_by_key(|(_, gap)| *gap)
        && gap > 0
    {
        return edge;
    }
    let (x, y) = anchor.centre();
    // Bottom first: min_by_key keeps the first of equal distances.
    let distances = [
        (Edge::Bottom, monitor.bottom - y),
        (Edge::Left, x - monitor.left),
        (Edge::Top, y - monitor.top),
        (Edge::Right, monitor.right - x),
    ];
    distances
        .iter()
        .min_by_key(|(_, d)| *d)
        .map_or(Edge::Bottom, |&(edge, _)| edge)
}

/// A `w`×`h` flyout against the taskbar `edge`, centred on `anchor` and kept
/// `gap` inside `work`, shrunk if `work` is too small for it.
pub fn flyout(anchor: Rect, work: Rect, edge: Edge, w: i32, h: i32, gap: i32) -> Rect {
    let area = work.inset(gap);
    let (w, h) = fit(w, h, area);
    let (cx, cy) = anchor.centre();
    // min/max with the anchor keeps an auto-hide taskbar, which overlaps
    // the work area while shown, uncovered.
    let (x, y) = match edge {
        Edge::Bottom => (cx - w / 2, anchor.top.min(work.bottom) - gap - h),
        Edge::Top => (cx - w / 2, anchor.bottom.max(work.top) + gap),
        Edge::Left => (anchor.right.max(work.left) + gap, cy - h / 2),
        Edge::Right => (anchor.left.min(work.right) - gap - w, cy - h / 2),
    };
    clamp_into(x, y, w, h, area)
}

/// A `w`×`h` flyout growing inward from the overlay badge in `corner`, beside
/// it rather than over it: above or below when there is room, else sideways.
pub fn flyout_from_corner(
    overlay: Rect,
    corner: Corner,
    work: Rect,
    w: i32,
    h: i32,
    gap: i32,
) -> Rect {
    let area = work.inset(gap);
    let (w, h) = fit(w, h, area);
    let (left, top) = sides(corner);
    let vertical = if top {
        area.bottom - overlay.bottom - gap
    } else {
        overlay.top - gap - area.top
    };
    let horizontal = if left {
        area.right - overlay.right - gap
    } else {
        overlay.left - gap - area.left
    };
    if vertical >= h || vertical >= horizontal {
        let h = h.min(vertical.max(h / 2));
        let x = if left {
            overlay.left
        } else {
            overlay.right - w
        };
        let y = if top {
            overlay.bottom + gap
        } else {
            overlay.top - gap - h
        };
        clamp_into(x, y, w, h, area)
    } else {
        let w = w.min(horizontal.max(w / 2));
        let x = if left {
            overlay.right + gap
        } else {
            overlay.left - gap - w
        };
        let y = if top { overlay.top } else { overlay.bottom - h };
        clamp_into(x, y, w, h, area)
    }
}

/// A `d`×`d` square `offset` (x, y) in from the two edges of `work` that
/// meet at `corner`, shrunk to fit and shifted to lie fully inside `work`.
pub fn corner(work: Rect, corner: Corner, d: i32, offset: (i32, i32)) -> Rect {
    let d = d.min(work.width()).min(work.height()).max(0);
    let (left, top) = sides(corner);
    let x = if left {
        work.left + offset.0
    } else {
        work.right - offset.0 - d
    };
    let y = if top {
        work.top + offset.1
    } else {
        work.bottom - offset.1 - d
    };
    clamp_into(x, y, d, d, work)
}

/// The corner of `work` whose quadrant holds the centre of `r`.
pub fn nearest_corner(r: Rect, work: Rect) -> Corner {
    let (x, y) = r.centre();
    let (cx, cy) = work.centre();
    match (x < cx, y < cy) {
        (true, true) => Corner::TopLeft,
        (false, true) => Corner::TopRight,
        (true, false) => Corner::BottomLeft,
        (false, false) => Corner::BottomRight,
    }
}

/// Where a badge dropped at `r` is kept: the nearest corner of `work` and
/// `r`'s distance from that corner's two edges in logical px (physical over
/// `scale`), to a tenth of a px, so [`corner`] puts it back at `r`. A rect
/// partly outside `work` counts as touching its edge.
pub fn drop_position(r: Rect, work: Rect, scale: f32) -> (Corner, f32, f32) {
    let c = nearest_corner(r, work);
    let (left, top) = sides(c);
    let dx = if left {
        r.left - work.left
    } else {
        work.right - r.right
    };
    let dy = if top {
        r.top - work.top
    } else {
        work.bottom - r.bottom
    };
    let logical = |v: i32| (v.max(0) as f32 / scale * 10.0).round() / 10.0;
    (c, logical(dx), logical(dy))
}

/// Physical px the cursor must travel before a press on the badge is a
/// drag rather than a click: Windows' own default drag threshold.
pub const DRAG_THRESHOLD: i32 = 4;

/// A left-button press on the badge, from mouse-down to mouse-up. Works in
/// screen coordinates: the window moves under the cursor, so positions
/// relative to the window would drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drag {
    start: (i32, i32),
    origin: Rect,
    moved: bool,
    /// A timer pass already saw the button up.
    seen_up: bool,
}

impl Drag {
    /// `cursor` is where the press happened, `origin` the badge's rect then.
    pub fn new(cursor: (i32, i32), origin: Rect) -> Self {
        Self {
            start: cursor,
            origin,
            moved: false,
            seen_up: false,
        }
    }

    /// The badge's rect for the cursor at `cursor`, kept inside `bounds`, or
    /// `None` while the press is still a click: it hasn't yet gone
    /// [`DRAG_THRESHOLD`] px from where it started. Once a drag, always one.
    pub fn track(&mut self, cursor: (i32, i32), bounds: Rect) -> Option<Rect> {
        let (dx, dy) = (cursor.0 - self.start.0, cursor.1 - self.start.1);
        if !self.moved && dx.abs() < DRAG_THRESHOLD && dy.abs() < DRAG_THRESHOLD {
            return None;
        }
        self.moved = true;
        let o = self.origin;
        Some(clamp_into(
            o.left + dx,
            o.top + dy,
            o.width(),
            o.height(),
            bounds,
        ))
    }

    /// Whether the press became a drag; if not, its release is a click.
    pub fn is_drag(&self) -> bool {
        self.moved
    }

    /// A timer pass found the button up: whether to end the press without
    /// its release. A drag ends at once. A click waits for a second pass,
    /// since the button reads up before its mouse-up is handled, and that
    /// mouse-up (still queued behind the timer) is what opens the panel.
    pub fn stale(&mut self) -> bool {
        let stale = self.moved || self.seen_up;
        self.seen_up = true;
        stale
    }
}

/// Transparent physical px around the badge's disc, inside its window and
/// its window region: the region's edge is aliased (1-bit), so the disc's
/// anti-aliased edge and its soft shadow must end before it. 3 logical px,
/// at least 2 physical.
pub fn badge_pad(scale: f32) -> i32 {
    ((3.0 * scale).round() as i32).max(2)
}

const HEADER: f32 = 64.0;
const ROW: f32 = 36.0;
const FOOTER: f32 = 48.0;
const MIN_PANEL: f32 = 200.0;
const MAX_SHARE: f32 = 0.6;

/// Panel height in logical px for `rows` repo rows: room for every row, at
/// least `MIN_PANEL`, at most 60% of the work area (the screen wins on a
/// tiny one). Extra rows scroll.
pub fn panel_height(rows: usize, work_h_logical: f32) -> f32 {
    let wanted = (HEADER + rows as f32 * ROW + FOOTER).max(MIN_PANEL);
    let cap = MAX_SHARE * work_h_logical;
    if cap > 0.0 { wanted.min(cap) } else { wanted }
}

/// (left, top) for a corner.
fn sides(corner: Corner) -> (bool, bool) {
    match corner {
        Corner::TopLeft => (true, true),
        Corner::TopRight => (false, true),
        Corner::BottomLeft => (true, false),
        Corner::BottomRight => (false, false),
    }
}

fn fit(w: i32, h: i32, area: Rect) -> (i32, i32) {
    (w.min(area.width()).max(0), h.min(area.height()).max(0))
}

/// The `w`×`h` rect at (x, y), shifted to lie inside `area`.
fn clamp_into(x: i32, y: i32, w: i32, h: i32, area: Rect) -> Rect {
    Rect::sized(
        shift(x, w, area.left, area.right),
        shift(y, h, area.top, area.bottom),
        w,
        h,
    )
}

fn shift(start: i32, len: i32, lo: i32, hi: i32) -> i32 {
    if len >= hi - lo {
        lo
    } else {
        start.clamp(lo, hi - len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORNERS: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    fn inside(r: Rect, area: Rect) -> bool {
        r.left >= area.left && r.top >= area.top && r.right <= area.right && r.bottom <= area.bottom
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    fn scaled(v: i32, scale: f32) -> i32 {
        (v as f32 * scale).round() as i32
    }

    /// A 1920×1080 (logical) monitor at `origin` with a 48 px taskbar on
    /// `edge`, and a 24 px tray icon near the taskbar's far end.
    fn screen(origin: (i32, i32), edge: Edge, scale: f32) -> (Rect, Rect, Rect) {
        let (ox, oy) = origin;
        let s = |v| scaled(v, scale);
        let monitor = Rect::new(ox, oy, ox + s(1920), oy + s(1080));
        let bar = s(48);
        let icon = s(24);
        let (work, anchor) = match edge {
            Edge::Bottom => (
                Rect {
                    bottom: monitor.bottom - bar,
                    ..monitor
                },
                Rect::sized(monitor.right - s(200), monitor.bottom - s(36), icon, icon),
            ),
            Edge::Top => (
                Rect {
                    top: monitor.top + bar,
                    ..monitor
                },
                Rect::sized(monitor.right - s(200), monitor.top + s(12), icon, icon),
            ),
            Edge::Left => (
                Rect {
                    left: monitor.left + bar,
                    ..monitor
                },
                Rect::sized(monitor.left + s(12), monitor.bottom - s(200), icon, icon),
            ),
            Edge::Right => (
                Rect {
                    right: monitor.right - bar,
                    ..monitor
                },
                Rect::sized(monitor.right - s(36), monitor.bottom - s(200), icon, icon),
            ),
        };
        (monitor, work, anchor)
    }

    #[test]
    fn flyout_sits_against_each_taskbar_edge_at_every_scale() {
        for origin in [(0, 0), (-1920, -300)] {
            for scale in [1.0, 1.5, 2.0] {
                for edge in [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom] {
                    let (monitor, work, anchor) = screen(origin, edge, scale);
                    assert_eq!(taskbar_edge(monitor, work, anchor), edge);
                    let (w, h, gap) = (scaled(360, scale), scaled(400, scale), scaled(12, scale));
                    let r = flyout(anchor, work, edge, w, h, gap);
                    let ctx = format!("{edge:?} at {scale} from {origin:?}: {r:?}");
                    assert_eq!((r.width(), r.height()), (w, h), "{ctx}");
                    assert!(inside(r, work.inset(gap)), "{ctx}");
                    assert!(!overlaps(r, anchor), "{ctx}");
                    let (ax, ay) = anchor.centre();
                    let (cx, cy) = r.centre();
                    match edge {
                        Edge::Bottom => assert_eq!(r.bottom, work.bottom - gap, "{ctx}"),
                        Edge::Top => assert_eq!(r.top, work.top + gap, "{ctx}"),
                        Edge::Left => assert_eq!(r.left, work.left + gap, "{ctx}"),
                        Edge::Right => assert_eq!(r.right, work.right - gap, "{ctx}"),
                    }
                    // Centred on the icon unless that would cross the edge.
                    match edge {
                        Edge::Bottom | Edge::Top => {
                            assert!((cx - ax).abs() <= 1 || r.right == work.right - gap, "{ctx}")
                        }
                        Edge::Left | Edge::Right => {
                            assert!(
                                (cy - ay).abs() <= 1 || r.bottom == work.bottom - gap,
                                "{ctx}"
                            )
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn flyout_centres_on_an_icon_with_room_around_it() {
        let work = Rect::new(0, 0, 1920, 1032);
        let anchor = Rect::sized(1000, 1040, 24, 24);
        let r = flyout(anchor, work, Edge::Bottom, 360, 400, 12);
        assert_eq!(r, Rect::new(832, 620, 1192, 1020));
    }

    #[test]
    fn auto_hide_taskbar_uses_the_nearest_edge_and_stays_clear_of_it() {
        let monitor = Rect::new(0, 0, 1920, 1080);
        // Auto-hide: no gap, and the shown taskbar overlaps the work area.
        let bottom_icon = Rect::sized(1700, 1050, 24, 24);
        assert_eq!(taskbar_edge(monitor, monitor, bottom_icon), Edge::Bottom);
        let r = flyout(bottom_icon, monitor, Edge::Bottom, 360, 400, 12);
        assert_eq!(r.bottom, bottom_icon.top - 12);
        assert!(inside(r, monitor));

        let top_icon = Rect::sized(1700, 8, 24, 24);
        assert_eq!(taskbar_edge(monitor, monitor, top_icon), Edge::Top);
        let left_icon = Rect::sized(8, 700, 24, 24);
        assert_eq!(taskbar_edge(monitor, monitor, left_icon), Edge::Left);
        let right_icon = Rect::sized(1888, 700, 24, 24);
        assert_eq!(taskbar_edge(monitor, monitor, right_icon), Edge::Right);
    }

    #[test]
    fn oversized_flyout_shrinks_to_fit() {
        let (monitor, work, anchor) = screen((-1280, -1024), Edge::Bottom, 1.0);
        let small = Rect::new(work.left, work.top, work.left + 300, work.top + 250);
        for (area, edge) in [
            (work, taskbar_edge(monitor, work, anchor)),
            (small, Edge::Bottom),
        ] {
            let r = flyout(anchor, area, edge, 5000, 5000, 12);
            assert!(inside(r, area.inset(12)), "{r:?} in {area:?}");
            assert_eq!(r.width(), area.width() - 24);
            assert_eq!(r.height(), area.height() - 24);
        }
        // Degenerate areas give an empty rect, not a panic.
        let r = flyout(
            anchor,
            Rect::new(10, 10, 10, 10),
            Edge::Bottom,
            360,
            400,
            12,
        );
        assert_eq!((r.width(), r.height()), (0, 0));
    }

    #[test]
    fn corner_badge_sits_margin_from_each_corner() {
        let work = Rect::new(-1920, -100, 0, 932);
        for (c, expected) in [
            (Corner::TopLeft, Rect::new(-1904, -84, -1860, -40)),
            (Corner::TopRight, Rect::new(-60, -84, -16, -40)),
            (Corner::BottomLeft, Rect::new(-1904, 872, -1860, 916)),
            (Corner::BottomRight, Rect::new(-60, 872, -16, 916)),
        ] {
            assert_eq!(corner(work, c, 44, (16, 16)), expected, "{c:?}");
        }
        // Too big for the work area: shrinks and stays inside.
        let tiny = Rect::new(0, 0, 50, 40);
        let r = corner(tiny, Corner::BottomRight, 44, (16, 16));
        assert!(inside(r, tiny) && r.width() == r.height(), "{r:?}");
        // Offsets past the far edge, or negative, are clamped onto the area.
        for c in CORNERS {
            for offset in [(5000, 5000), (-50, -50), (i32::MAX / 2, 0)] {
                let r = corner(work, c, 44, offset);
                assert!(
                    inside(r, work) && r.width() == 44,
                    "{c:?} {offset:?}: {r:?}"
                );
            }
        }
    }

    #[test]
    fn nearest_corner_round_trips() {
        for work in [Rect::new(0, 0, 1920, 1032), Rect::new(-2560, -400, 0, 1000)] {
            for scale in [1.0, 1.5, 2.0] {
                for c in CORNERS {
                    let m = scaled(16, scale);
                    let r = corner(work, c, scaled(44, scale), (m, m));
                    assert_eq!(nearest_corner(r, work), c, "{work:?} at {scale}");
                }
            }
        }
    }

    #[test]
    fn flyout_from_corner_stays_inside_and_off_the_badge() {
        for work in [Rect::new(0, 0, 1920, 1032), Rect::new(-1920, -300, 0, 780)] {
            for c in CORNERS {
                let badge = corner(work, c, 44, (16, 16));
                let r = flyout_from_corner(badge, c, work, 360, 400, 12);
                let ctx = format!("{c:?} in {work:?}: {r:?}");
                assert_eq!((r.width(), r.height()), (360, 400), "{ctx}");
                assert!(inside(r, work.inset(12)), "{ctx}");
                assert!(!overlaps(r, badge), "{ctx}");
                // Lined up with the badge's outer side.
                let (left, _) = sides(c);
                if left {
                    assert_eq!(r.left, badge.left, "{ctx}");
                } else {
                    assert_eq!(r.right, badge.right, "{ctx}");
                }
            }
        }
        // A short, wide work area: no room above the badge, so it goes beside.
        let wide = Rect::new(0, 0, 1920, 300);
        let badge = corner(wide, Corner::BottomRight, 44, (16, 16));
        let r = flyout_from_corner(badge, Corner::BottomRight, wide, 360, 400, 12);
        assert!(inside(r, wide.inset(12)) && !overlaps(r, badge), "{r:?}");
        assert_eq!(r.right, badge.left - 12);
        // Larger than the work area: shrinks and stays inside.
        let small = Rect::new(0, 0, 300, 200);
        let badge = corner(small, Corner::TopLeft, 44, (16, 16));
        let r = flyout_from_corner(badge, Corner::TopLeft, small, 5000, 5000, 12);
        assert!(inside(r, small.inset(12)), "{r:?}");
    }

    #[test]
    fn dropped_badge_round_trips_through_corner_and_offset() {
        for work in [Rect::new(0, 0, 1920, 1032), Rect::new(-2560, -400, 0, 1000)] {
            for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 3.0] {
                let d = scaled(44, scale);
                // Every corner's quadrant, the edges and the centre.
                for (fx, fy) in [
                    (0.0, 0.0),
                    (0.1, 0.9),
                    (0.37, 0.41),
                    (0.5, 0.5),
                    (0.8, 0.2),
                    (1.0, 1.0),
                ] {
                    let x = work.left + ((work.width() - d) as f32 * fx) as i32;
                    let y = work.top + ((work.height() - d) as f32 * fy) as i32;
                    let r = Rect::sized(x, y, d, d);
                    let (c, ox, oy) = drop_position(r, work, scale);
                    assert_eq!(c, nearest_corner(r, work));
                    assert!(ox >= 0.0 && oy >= 0.0);
                    // What config stores (logical) puts it back exactly.
                    let back = corner(work, c, d, (scaled_f(ox, scale), scaled_f(oy, scale)));
                    assert_eq!(back, r, "{work:?} at {scale}, ({fx}, {fy})");
                }
            }
        }
        // Dropped against the bottom-right edges: offsets 0, not negative.
        let work = Rect::new(0, 0, 1920, 1032);
        let r = Rect::new(1900, 1010, 1944, 1054);
        assert_eq!(drop_position(r, work, 1.0), (Corner::BottomRight, 0.0, 0.0));
        // A default-placed badge reads back as its margin.
        for scale in [1.0, 1.5, 2.0] {
            let m = scaled(16, scale);
            let r = corner(work, Corner::TopLeft, scaled(44, scale), (m, m));
            assert_eq!(drop_position(r, work, scale), (Corner::TopLeft, 16.0, 16.0));
        }
    }

    fn scaled_f(v: f32, scale: f32) -> i32 {
        (v * scale).round() as i32
    }

    #[test]
    fn press_is_a_click_until_it_moves_four_px() {
        let bounds = Rect::new(0, 0, 1920, 1032);
        let origin = Rect::sized(1860, 972, 44, 44);
        let mut drag = Drag::new((1880, 990), origin);
        // Wobbling within 3 px either way stays a click.
        for p in [
            (1880, 990),
            (1883, 990),
            (1877, 993),
            (1880, 987),
            (1883, 993),
        ] {
            assert_eq!(drag.track(p, bounds), None, "{p:?}");
        }
        assert!(!drag.is_drag());
        // 4 px on one axis starts the drag; the badge follows the cursor.
        assert_eq!(
            drag.track((1876, 990), bounds),
            Some(Rect::sized(1856, 972, 44, 44))
        );
        assert!(drag.is_drag());
        // Back near the start, it is still a drag, at the original place.
        assert_eq!(drag.track((1880, 990), bounds), Some(origin));
        // Dragged past the work area: kept fully inside it.
        let r = drag.track((5000, 5000), bounds).unwrap();
        assert_eq!(r, Rect::new(1876, 988, 1920, 1032));
        let r = drag.track((-5000, -5000), bounds).unwrap();
        assert_eq!(r, Rect::sized(0, 0, 44, 44));
        let mut down = Drag::new((10, 10), origin);
        assert!(down.track((10, 14), bounds).is_some());
    }

    #[test]
    fn a_click_outlives_one_timer_pass() {
        let origin = Rect::sized(1860, 972, 44, 44);
        // The first pass may run before the queued mouse-up: keep the click.
        let mut click = Drag::new((1880, 990), origin);
        assert!(!click.stale());
        assert!(click.stale());
        // A drag whose release was lost ends at the first pass.
        let mut drag = Drag::new((1880, 990), origin);
        drag.track((1800, 990), Rect::new(0, 0, 1920, 1032));
        assert!(drag.stale());
    }

    #[test]
    fn badge_pad_covers_the_anti_aliased_edge_and_shadow() {
        for scale in [0.5, 1.0, 1.25, 1.5, 2.0, 3.0] {
            let pad = badge_pad(scale);
            assert!(pad >= 2, "{scale}");
            // The shadow (0.5 px down, 0.75 px blur, invisible past 3
            // blurs) ends inside the pad.
            assert!(pad as f32 >= (0.5 + 3.0 * 0.75) * scale, "{scale}: {pad}");
        }
        let r = Rect::new(10, 20, 54, 64).outset(3);
        assert_eq!(r, Rect::new(7, 17, 57, 67));
        assert_eq!(r.centre(), (32, 42));
    }

    #[test]
    fn panel_height_fits_rows_within_bounds() {
        assert_eq!(panel_height(0, 1000.0), 200.0);
        assert_eq!(panel_height(5, 1000.0), 64.0 + 5.0 * 36.0 + 48.0);
        assert_eq!(panel_height(100, 1000.0), 600.0);
        // The screen wins over the minimum on a tiny work area.
        assert_eq!(panel_height(3, 250.0), 150.0);
        assert_eq!(panel_height(3, f32::NAN), 220.0);
        assert_eq!(panel_height(3, 0.0), 220.0);
    }
}
