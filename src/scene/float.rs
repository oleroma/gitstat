//! Where the floating view's parts go: the bar along its top, a row per
//! repo (its tree, then its icon) and what follows them, in whole device
//! px from the view's own top-left corner.
//!
//! The view is the badge grown: it keeps the badge's corners, so what it
//! shows stays clear of them, a little in from every edge.

pub use super::px;
use crate::config::Corner;
use crate::layout::Rect;

/// A row's height, the panel's.
pub const STRIP: f32 = super::panel::STRIP;
/// The column at a row's right: the repo's icon.
pub const LABEL: f32 = super::panel::ICON_LABEL;
/// A repo's icon.
pub const MARK: f32 = super::panel::ICON_MARK;
/// The top bar: the health bar, and what the mouse drags the view by.
pub const TOP: f32 = 18.0;
/// The health bar in it.
const BAR: f32 = 6.0;
/// A top-bar button, square.
pub const BUTTON: f32 = 18.0;
const GAP: f32 = 6.0;
/// The line the view shows while it lists no repo.
pub const EMPTY: f32 = 30.0;
/// What follows the rows, such as the pull requests: a line's height.
pub const TAIL: f32 = 24.0;

/// What the view shows, as far as its layout goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spec {
    pub scale: f32,
    /// The badge's side, the view's width and the most it may be tall,
    /// device px.
    pub badge: i32,
    pub width: i32,
    pub max_height: i32,
    /// Its corners' radius, logical px.
    pub radius: f32,
    pub rows: usize,
    pub health: bool,
    /// How many buttons the top bar has.
    pub buttons: usize,
    /// How many lines follow the rows.
    pub tail: usize,
    /// How far the rows are scrolled.
    pub scroll: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    pub rect: Rect,
    /// Its tree's strip, and the icon's column beside it.
    pub strip: Rect,
    pub label: Rect,
    pub icon: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub size: (i32, i32),
    /// The top bar, edge to edge: where the mouse takes hold of the view.
    pub top: Rect,
    pub bar: Option<Rect>,
    /// The buttons, right to left.
    pub buttons: Vec<Rect>,
    /// The rows' window, and the line shown in it while there are none.
    pub list: Rect,
    pub empty: Option<Rect>,
    pub rows: Vec<Row>,
    pub tail: Vec<Rect>,
    pub content: i32,
    pub scroll: i32,
    pub max_scroll: i32,
}

/// How far in from the view's edges its insides stay, logical px: clear
/// of a corner of `radius`, whose arc cuts 0.29 of it off the corner's
/// square.
pub fn inset(radius: f32) -> f32 {
    (radius.max(0.0) * 0.3).round() + 3.0
}

/// Lays the view out.
pub fn layout(spec: &Spec) -> Layout {
    let s = if spec.scale.is_finite() && spec.scale > 0.0 {
        spec.scale
    } else {
        1.0
    };
    let e = |v: f32| px(v, s);
    let m = e(inset(spec.radius));
    let w = spec.width.max(spec.badge).max(2 * m + e(LABEL) + e(STRIP));
    let (left, right) = (m, w - m);

    let top = Rect::new(0, 0, w, m + e(TOP) + e(GAP) / 2);
    let mut buttons = Vec::new();
    let mut x = right;
    let button_top = m + (e(TOP) - e(BUTTON)) / 2;
    for _ in 0..spec.buttons {
        buttons.push(Rect::new(
            x - e(BUTTON),
            button_top,
            x,
            button_top + e(BUTTON),
        ));
        x -= e(BUTTON) + e(GAP) / 2;
    }
    let bar_right = if spec.buttons > 0 { x - e(GAP) / 2 } else { x };
    let bar = (spec.health && bar_right - left >= e(BAR) * 3).then(|| {
        let bar_top = m + (e(TOP) - e(BAR)) / 2;
        Rect::new(left, bar_top, bar_right, bar_top + e(BAR))
    });

    let list_top = m + e(TOP) + e(GAP);
    let tail_h = spec.tail as i32 * e(TAIL);
    let content = if spec.rows == 0 {
        e(EMPTY)
    } else {
        spec.rows as i32 * e(STRIP)
    };
    // Whole rows show, as many as fit under the top bar and over the
    // tail; the rest scroll.
    let room = (spec.max_height - list_top - tail_h - m).max(e(STRIP));
    let shown = if content <= room {
        content
    } else {
        (room / e(STRIP)).max(1) * e(STRIP)
    };
    let max_scroll = (content - shown).max(0);
    let scroll = spec.scroll.clamp(0, max_scroll);
    let list = Rect::new(left, list_top, right, list_top + shown);
    let rows = (0..spec.rows)
        .map(|i| {
            let y = list_top + i as i32 * e(STRIP) - scroll;
            let rect = Rect::new(left, y, right, y + e(STRIP));
            let label = Rect::new(right - e(LABEL), y, right, y + e(STRIP));
            let side = e(MARK);
            let icon_left = label.left + (label.width() - side) / 2;
            let icon_top = y + (e(STRIP) - side) / 2;
            Row {
                rect,
                strip: Rect::new(left, y, label.left, y + e(STRIP)),
                label,
                icon: Rect::new(icon_left, icon_top, icon_left + side, icon_top + side),
            }
        })
        .collect();
    let empty = (spec.rows == 0).then_some(list);
    let tail = (0..spec.tail as i32)
        .map(|i| {
            let y = list.bottom + i * e(TAIL);
            Rect::new(left, y, right, y + e(TAIL))
        })
        .collect();
    let h = (list.bottom + tail_h + m).max(spec.badge);
    Layout {
        size: (w, h),
        top,
        bar,
        buttons,
        list,
        empty,
        rows,
        tail,
        content,
        scroll,
        max_scroll,
    }
}

/// Where on the screen the view grown from the badge at `badge` lies: it
/// grows away from the screen's corner the badge is kept by, so the
/// badge's two edges nearest that corner stay the view's.
pub fn frame(badge: Rect, size: (i32, i32), corner: Corner) -> Rect {
    let (left, top) = match corner {
        Corner::TopLeft => (true, true),
        Corner::TopRight => (false, true),
        Corner::BottomLeft => (true, false),
        Corner::BottomRight => (false, false),
    };
    let x = if left {
        badge.left
    } else {
        badge.right - size.0
    };
    let y = if top {
        badge.top
    } else {
        badge.bottom - size.1
    };
    Rect::new(x, y, x + size.0, y + size.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(scale: f32, rows: usize) -> Spec {
        Spec {
            scale,
            badge: px(44.0, scale),
            width: px(220.0, scale),
            max_height: px(600.0, scale),
            radius: 22.0,
            rows,
            health: true,
            buttons: 1,
            tail: 0,
            scroll: 0,
        }
    }

    #[test]
    fn rows_stack_on_whole_px_inside_the_corners() {
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            let l = layout(&spec(scale, 4));
            let m = px(inset(22.0), scale);
            assert_eq!(l.rows.len(), 4);
            for pair in l.rows.windows(2) {
                assert_eq!(pair[0].rect.bottom, pair[1].rect.top, "at {scale}");
            }
            for row in &l.rows {
                assert_eq!(row.strip.right, row.label.left);
                assert!(row.rect.left >= m && row.rect.right <= l.size.0 - m);
                assert!(row.icon.left >= row.label.left && row.icon.right <= row.label.right);
            }
            assert_eq!(l.list.bottom, l.rows[3].rect.bottom);
            assert_eq!(l.size.1, l.list.bottom + m);
            let bar = l.bar.unwrap();
            assert!(bar.right <= l.buttons[0].left && bar.left == m);
            assert_eq!(l.max_scroll, 0);
        }
    }

    #[test]
    fn too_many_rows_scroll_by_whole_rows() {
        let mut s = spec(1.25, 40);
        s.scroll = 100_000;
        let l = layout(&s);
        let strip = px(STRIP, 1.25);
        assert!(l.size.1 <= s.max_height);
        assert_eq!(l.list.height() % strip, 0);
        assert_eq!(l.scroll, l.max_scroll);
        assert_eq!(l.max_scroll, 40 * strip - l.list.height());
        assert_eq!(l.rows[39].rect.bottom, l.list.bottom);
    }

    #[test]
    fn no_rows_leave_a_line_and_never_less_than_the_badge() {
        let l = layout(&spec(1.0, 0));
        assert_eq!(l.empty, Some(l.list));
        assert!(l.rows.is_empty());
        let mut s = spec(1.0, 0);
        s.width = 10;
        s.badge = 400;
        let l = layout(&s);
        assert!(l.size.0 >= 400 && l.size.1 >= 400);
    }

    #[test]
    fn grows_away_from_its_corner() {
        let badge = Rect::new(1000, 500, 1044, 544);
        let size = (200, 300);
        assert_eq!(
            frame(badge, size, Corner::TopLeft),
            Rect::new(1000, 500, 1200, 800)
        );
        assert_eq!(
            frame(badge, size, Corner::BottomRight),
            Rect::new(844, 244, 1044, 544)
        );
        assert_eq!(
            frame(badge, size, Corner::TopRight),
            Rect::new(844, 500, 1044, 800)
        );
        assert_eq!(
            frame(badge, size, Corner::BottomLeft),
            Rect::new(1000, 244, 1200, 544)
        );
    }
}
