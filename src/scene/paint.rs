//! The painters of the parts that aren't words: filled and rounded
//! boxes and the health bar, on the whole device px their layout gives.

use crate::layout::Rect;
use gpui::{Bounds, Hsla, Pixels, Window, point, px, size};

/// `r`, device px from a view's corner, as GPUI's bounds in a window
/// whose scale is `s`, the view's corner at device px `at`.
pub fn bounds(r: Rect, at: (i32, i32), s: f32) -> Bounds<Pixels> {
    Bounds::new(
        point(
            px((r.left + at.0) as f32 / s),
            px((r.top + at.1) as f32 / s),
        ),
        size(
            px(r.width().max(0) as f32 / s),
            px(r.height().max(0) as f32 / s),
        ),
    )
}

/// Fills `b` with `color`.
pub fn fill(window: &mut Window, b: Bounds<Pixels>, color: Hsla) {
    if color.a > 0.0 && b.size.width > px(0.0) && b.size.height > px(0.0) {
        window.paint_quad(gpui::fill(b, color));
    }
}

/// Fills `b` with `color`, its corners rounded by `radii`, logical px.
pub fn round(window: &mut Window, b: Bounds<Pixels>, radii: gpui::Corners<Pixels>, color: Hsla) {
    if color.a > 0.0 && b.size.width > px(0.0) && b.size.height > px(0.0) {
        window.paint_quad(gpui::fill(b, color).corner_radii(radii));
    }
}

/// The health bar in `bar` (device px from the corner at `at`): a track,
/// and over it the repos' tones, each as wide as its share of `counts`,
/// their edges on whole device px. Dimmed while a check runs.
pub fn health(
    window: &mut Window,
    bar: Rect,
    at: (i32, i32),
    counts: [usize; 3],
    colors: [Hsla; 3],
    track: Hsla,
    checking: bool,
) {
    if bar.width() <= 0 || bar.height() <= 0 {
        return;
    }
    let s = window.scale_factor();
    let dim = |c: Hsla| if checking { c.opacity(0.6) } else { c };
    let all = bounds(bar, at, s);
    round(
        window,
        all,
        gpui::Corners::all(all.size.height / 2.0),
        dim(track),
    );
    let total: usize = counts.iter().sum();
    if total == 0 {
        return;
    }
    let edge = |n: usize| bar.left + (bar.width() as f32 * n as f32 / total as f32).round() as i32;
    let mut done = 0;
    for (n, color) in counts.into_iter().zip(colors) {
        let part = Rect::new(edge(done), bar.top, edge(done + n), bar.bottom);
        done += n;
        fill(window, bounds(part, at, s), dim(color));
    }
}
