//! Where the big window's parts go: its title bar, which gitstat draws
//! itself, its top bar, the table of spines and columns, the pull
//! requests under it and its bottom bar, in whole device px from the
//! window's own top-left corner.
//!
//! As in [`super::panel`], lengths are set in logical px and every edge
//! is rounded once, from the window's corner.

use super::panel::{BUTTON, SMALL, TINY, line_height};
pub use super::px;
use crate::layout::Rect;

/// The title bar's height, and each of its three buttons' width, logical
/// px: Windows 11's own.
pub const CAPTION: f32 = 32.0;
pub const CAPTION_BUTTON: f32 = 46.0;
/// A repo's column, and a section's spine.
pub const COLUMN: f32 = 380.0;
pub const SPINE: f32 = 30.0;
/// The headline's size here.
pub const HEADLINE: f32 = 18.0;
/// The most the pull requests take under the table: their heading and
/// about six rows.
pub const PRS_MAX: f32 = 230.0;

const PAD_X: f32 = 16.0;
const PAD_Y: f32 = 12.0;
const GAP: f32 = 8.0;

/// A column's header: its icon's side, its name's size, and the size of
/// the line under it.
pub const HEAD_ICON: f32 = 20.0;
pub const HEAD_NAME: f32 = 16.0;
pub const HEAD_LINE: f32 = 14.0;
const HEAD_PAD_X: f32 = 12.0;
const HEAD_PAD_Y: f32 = 8.0;
const HEAD_GAP: f32 = 6.0;

/// One cell of the table, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellSpec {
    Spine,
    Column(HeadSpec),
}

/// What of a column's header is measured, device px: how wide its
/// checkout and its status run, and how tall the glance under them is,
/// when it has one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeadSpec {
    pub checkout: i32,
    pub status: i32,
    pub glance: Option<i32>,
}

/// A column, laid out: its header's parts, then its tree's room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    pub icon: Rect,
    pub name: Rect,
    pub checkout: Rect,
    pub status: Rect,
    /// The name and the line under it, as far across as the header goes:
    /// where the mouse takes them.
    pub title: Rect,
    pub glance: Option<Rect>,
    /// The line under the header, and the one down the column's right.
    pub line: Rect,
    pub edge: Rect,
    pub body: Rect,
}

/// Lays a column's header out in `cell`, and its body under it.
pub fn column(cell: Rect, head: &HeadSpec, scale: f32) -> Column {
    let e = |v: f32| px(v, scale);
    let (x, y) = (cell.left, cell.top);
    let rim = e(1.0).max(1);
    let inner_right = (cell.right - rim - e(HEAD_PAD_X)).max(x + e(HEAD_PAD_X));
    let left = x + e(HEAD_PAD_X);
    // The icon and the name on one line, the icon in its middle; under
    // them the checkout, then the status.
    let name_h = line_height(HEAD_NAME);
    let icon_top = y + e(HEAD_PAD_Y + (name_h - HEAD_ICON) / 2.0);
    let icon = Rect::new(left, icon_top, left + e(HEAD_ICON), icon_top + e(HEAD_ICON));
    let name_left = (icon.right + e(GAP)).min(inner_right);
    let name_top = y + e(HEAD_PAD_Y);
    let name = Rect::new(name_left, name_top, inner_right, name_top + e(name_h));
    let line_top = y + e(HEAD_PAD_Y + name_h + 2.0);
    let line_bottom = line_top + e(line_height(HEAD_LINE));
    let status_w = head.status.max(0).min(inner_right - left);
    let checkout_w = head
        .checkout
        .max(0)
        .min((inner_right - left - e(GAP) - status_w).max(0));
    let checkout = Rect::new(left, line_top, left + checkout_w, line_bottom);
    let status_left = (checkout.right + e(GAP)).min(inner_right - status_w);
    let status = Rect::new(status_left, line_top, status_left + status_w, line_bottom);
    let title = Rect::new(left, name_top, inner_right, line_bottom);
    let glance = head.glance.map(|tall| {
        let top = line_bottom + e(HEAD_GAP);
        Rect::new(left, top, inner_right, top + tall.max(0))
    });
    let under = glance.map_or(line_bottom, |g| g.bottom) + e(HEAD_PAD_Y);
    let under = under.min(cell.bottom);
    let line = Rect::new(x, under, cell.right - rim, (under + rim).min(cell.bottom));
    let edge = Rect::new(cell.right - rim, y, cell.right, cell.bottom);
    let body = Rect::new(x, line.bottom, cell.right - rim, cell.bottom);
    Column {
        icon,
        name,
        checkout,
        status,
        title,
        glance,
        line,
        edge,
        body,
    }
}

/// What the big window shows, as far as its layout goes. Widths and
/// heights here are measured, in device px.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spec {
    pub scale: f32,
    /// The window inside its rim, title bar and all.
    pub size: (i32, i32),
    /// The health row's three counts' widths, when it shows.
    pub health: Option<[i32; 3]>,
    /// The refresh button's words' width.
    pub refresh: i32,
    pub cells: Vec<CellSpec>,
    /// How far the table is scrolled sideways.
    pub scroll: i32,
    /// How tall the pull requests are, all of them; none without any.
    pub prs: Option<i32>,
    /// The bottom bar's height.
    pub footer: i32,
}

/// The title bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    pub rect: Rect,
    /// The app's icon and its name's line box.
    pub icon: Rect,
    pub title: Rect,
    /// Minimise, maximise or restore, close.
    pub buttons: [Rect; 3],
}

/// The top bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub rect: Rect,
    pub line: Rect,
    pub headline: Rect,
    pub subline: Rect,
    pub bar: Option<Rect>,
    pub counts: [Rect; 3],
    /// The settings button, and the refresh button with its words' line
    /// box.
    pub settings: Rect,
    pub refresh: Rect,
    pub refresh_words: Rect,
}

/// The big window, laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub size: (i32, i32),
    pub caption: Caption,
    pub header: Header,
    /// The table's window on its cells.
    pub table: Rect,
    /// Each cell, top to bottom of the table, where it is now, scrolled.
    pub cells: Vec<Rect>,
    /// Each cell's column, for the cells that are one.
    pub columns: Vec<Option<Column>>,
    /// How wide all the cells are, how far the table is scrolled and how
    /// far it can be.
    pub content: i32,
    pub scroll: i32,
    pub max_scroll: i32,
    pub prs: Option<Rect>,
    pub footer: Rect,
}

/// Each cell's left edge in the unscrolled table, device px, and the
/// table's whole width: every edge rounded once from the table's left.
pub fn cell_edges(cells: &[CellSpec], scale: f32) -> Vec<i32> {
    let mut x = 0.0f32;
    let mut edges = vec![0];
    for cell in cells {
        x += match cell {
            CellSpec::Spine => SPINE,
            CellSpec::Column(_) => COLUMN,
        };
        edges.push(px(x, scale));
    }
    edges
}

/// Lays the big window out.
pub fn layout(spec: &Spec) -> Layout {
    let s = if spec.scale.is_finite() && spec.scale > 0.0 {
        spec.scale
    } else {
        1.0
    };
    let e = |v: f32| px(v, s);
    let (w, h) = (spec.size.0.max(0), spec.size.1.max(0));

    // The title bar: the icon and the name at its left, the three buttons
    // at its right.
    let cap_h = e(CAPTION);
    let button = e(CAPTION_BUTTON);
    let buttons = [3, 2, 1].map(|i| Rect::new(w - i * button, 0, w - (i - 1) * button, cap_h));
    let icon_side = e(16.0);
    let icon_top = e((CAPTION - 16.0) / 2.0);
    let icon = Rect::new(
        e(PAD_X),
        icon_top,
        e(PAD_X) + icon_side,
        icon_top + icon_side,
    );
    let title_top = e((CAPTION - line_height(TINY)) / 2.0);
    let title = Rect::new(
        icon.right + e(GAP),
        title_top,
        (buttons[0].left - e(GAP)).max(icon.right + e(GAP)),
        title_top + e(line_height(TINY)),
    );
    let caption = Caption {
        rect: Rect::new(0, 0, w, cap_h),
        icon,
        title,
        buttons,
    };

    // The top bar, from under the title bar: logical px from the window's
    // corner, so every edge is rounded once.
    let titles_h = line_height(HEADLINE)
        + line_height(TINY)
        + if spec.health.is_some() {
            4.0 + line_height(TINY)
        } else {
            0.0
        };
    let top = CAPTION;
    let header_h = PAD_Y + titles_h + PAD_Y + 1.0;
    // The refresh button: its words with room around them.
    let refresh_h = line_height(SMALL) + 8.0;
    let refresh_w = spec.refresh.max(0) + 2 * e(12.0);
    let refresh_top = e(top + PAD_Y + (titles_h - refresh_h) / 2.0);
    let refresh_right = w - e(PAD_X);
    let refresh = Rect::new(
        refresh_right - refresh_w,
        refresh_top,
        refresh_right,
        refresh_top + e(refresh_h),
    );
    let words_top = refresh_top + e(4.0);
    let refresh_words = Rect::new(
        refresh.left + e(12.0),
        words_top,
        refresh.right - e(12.0),
        words_top + e(line_height(SMALL)),
    );
    let settings_top = e(top + PAD_Y + (titles_h - BUTTON) / 2.0);
    let settings_right = refresh.left - e(GAP);
    let settings = Rect::new(
        settings_right - e(BUTTON),
        settings_top,
        settings_right,
        settings_top + e(BUTTON),
    );
    let titles_left = e(PAD_X);
    let titles_right = (settings.left - e(GAP)).max(titles_left);
    let line_box = |at: f32, size: f32| {
        Rect::new(
            titles_left,
            e(at),
            titles_right,
            e(at) + e(line_height(size)),
        )
    };
    let mut counts = [Rect::new(0, 0, 0, 0); 3];
    let health_top = top + PAD_Y + line_height(HEADLINE) + line_height(TINY) + 4.0;
    let bar = spec.health.map(|widths| {
        let taken: i32 = widths.iter().map(|cw| cw + e(GAP)).sum();
        let right = (titles_right - taken).max(titles_left);
        let mut x = right;
        for (count, cw) in counts.iter_mut().zip(widths) {
            x += e(GAP);
            *count = Rect::new(
                x,
                e(health_top),
                x + cw,
                e(health_top) + e(line_height(TINY)),
            );
            x += cw;
        }
        let bar_top = e(health_top + (line_height(TINY) - 4.0) / 2.0);
        Rect::new(titles_left, bar_top, right, bar_top + e(4.0))
    });
    let header_bottom = e(top + header_h);
    let header = Header {
        rect: Rect::new(0, cap_h, w, header_bottom),
        line: Rect::new(0, e(top + header_h - 1.0), w, header_bottom),
        headline: line_box(top + PAD_Y, HEADLINE),
        subline: line_box(top + PAD_Y + line_height(HEADLINE), TINY),
        bar,
        counts,
        settings,
        refresh,
        refresh_words,
    };

    // From the bottom up: the bottom bar, the pull requests, and the
    // table with what room is left.
    let footer_h = spec.footer.max(0).min((h - header_bottom).max(0));
    let footer = Rect::new(0, h - footer_h, w, h);
    let room = (footer.top - header_bottom).max(0);
    let prs = spec.prs.map(|all| {
        let tall = all.max(0).min(e(PRS_MAX)).min(room);
        Rect::new(0, footer.top - tall, w, footer.top)
    });
    let table_bottom = prs.map_or(footer.top, |r| r.top);
    let table = Rect::new(0, header_bottom, w, table_bottom.max(header_bottom));

    let edges = cell_edges(&spec.cells, s);
    let content = edges.last().copied().unwrap_or(0);
    let max_scroll = (content - w).max(0);
    let scroll = spec.scroll.clamp(0, max_scroll);
    let cells: Vec<Rect> = edges
        .windows(2)
        .map(|pair| Rect::new(pair[0] - scroll, table.top, pair[1] - scroll, table.bottom))
        .collect();
    let columns = cells
        .iter()
        .zip(&spec.cells)
        .map(|(cell, kind)| match kind {
            CellSpec::Spine => None,
            CellSpec::Column(head) => Some(column(*cell, head, s)),
        })
        .collect();

    Layout {
        size: (w, h),
        caption,
        header,
        table,
        cells,
        columns,
        content,
        scroll,
        max_scroll,
        prs,
        footer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALES: [f32; 6] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5];

    fn head(scale: f32) -> HeadSpec {
        HeadSpec {
            checkout: px(90.0, scale),
            status: px(120.0, scale),
            glance: Some(px(22.0, scale)),
        }
    }

    fn spec(scale: f32) -> Spec {
        let e = |v: f32| px(v, scale);
        Spec {
            scale,
            size: (e(1200.0), e(800.0)),
            health: Some([e(22.0), e(20.0), e(14.0)]),
            refresh: e(78.0),
            cells: vec![
                CellSpec::Spine,
                CellSpec::Column(head(scale)),
                CellSpec::Column(head(scale)),
                CellSpec::Spine,
                CellSpec::Spine,
                CellSpec::Column(HeadSpec::default()),
                CellSpec::Column(head(scale)),
            ],
            scroll: 0,
            prs: Some(e(400.0)),
            footer: e(33.0),
        }
    }

    fn inside(inner: Rect, outer: Rect) -> bool {
        inner.left >= outer.left
            && inner.top >= outer.top
            && inner.right <= outer.right
            && inner.bottom <= outer.bottom
    }

    #[test]
    fn its_bands_fill_the_window_top_to_bottom() {
        for scale in SCALES {
            let l = layout(&spec(scale));
            let prs = l.prs.unwrap();
            assert_eq!(l.caption.rect.top, 0);
            assert_eq!(l.caption.rect.bottom, l.header.rect.top);
            assert_eq!(l.header.rect.bottom, l.table.top);
            assert_eq!(l.table.bottom, prs.top);
            assert_eq!(prs.bottom, l.footer.top);
            assert_eq!(l.footer.bottom, l.size.1);
            assert_eq!(prs.height(), px(PRS_MAX, scale), "scale {scale}");
            assert!(l.header.line.height() >= 1);
            assert_eq!(l.header.line.bottom, l.header.rect.bottom);
        }
    }

    #[test]
    fn the_cells_share_their_edges() {
        for scale in SCALES {
            let l = layout(&spec(scale));
            assert_eq!(l.cells[0].left, 0);
            for pair in l.cells.windows(2) {
                assert_eq!(pair[0].right, pair[1].left, "scale {scale}");
            }
            assert_eq!(l.cells.last().unwrap().right, l.content);
            for (cell, kind) in l.cells.iter().zip(&spec(scale).cells) {
                let want = match kind {
                    CellSpec::Spine => SPINE,
                    CellSpec::Column(_) => COLUMN,
                };
                assert!((cell.width() - px(want, scale)).abs() <= 1);
                assert_eq!((cell.top, cell.bottom), (l.table.top, l.table.bottom));
            }
        }
    }

    #[test]
    fn the_table_scrolls_sideways_to_its_end() {
        for scale in SCALES {
            let mut s = spec(scale);
            let still = layout(&s);
            assert_eq!(still.max_scroll, still.content - still.size.0);
            s.scroll = 100;
            let moved = layout(&s);
            for (a, b) in still.cells.iter().zip(&moved.cells) {
                assert_eq!(a.left - 100, b.left);
                assert_eq!(a.width(), b.width());
            }
            assert_eq!(still.header, moved.header);
            s.scroll = 1_000_000;
            let end = layout(&s);
            assert_eq!(end.scroll, still.max_scroll);
            assert_eq!(end.cells.last().unwrap().right, end.size.0);
        }
    }

    #[test]
    fn the_bars_parts_stay_inside_them_and_apart() {
        for scale in SCALES {
            let l = layout(&spec(scale));
            let c = &l.caption;
            assert!(inside(c.icon, c.rect) && inside(c.title, c.rect));
            assert!(c.icon.right <= c.title.left);
            assert!(c.title.right <= c.buttons[0].left);
            assert_eq!(c.buttons[0].right, c.buttons[1].left);
            assert_eq!(c.buttons[1].right, c.buttons[2].left);
            assert_eq!(c.buttons[2].right, l.size.0);
            for b in c.buttons {
                assert_eq!((b.top, b.bottom), (c.rect.top, c.rect.bottom));
            }
            let h = &l.header;
            for part in [h.headline, h.subline, h.settings, h.refresh] {
                assert!(inside(part, h.rect), "scale {scale}: {part:?}");
            }
            assert!(inside(h.refresh_words, h.refresh));
            assert!(h.headline.right <= h.settings.left);
            assert!(h.settings.right <= h.refresh.left);
            let bar = h.bar.unwrap();
            assert!(inside(bar, h.rect));
            assert!(bar.right <= h.counts[0].left);
            assert!(h.counts[2].right <= h.settings.left);
        }
    }

    #[test]
    fn a_small_window_keeps_every_band_in_it() {
        for scale in SCALES {
            let mut s = spec(scale);
            s.size = (px(300.0, scale), px(200.0, scale));
            let l = layout(&s);
            let whole = Rect::new(0, 0, l.size.0, l.size.1);
            assert!(inside(l.table, whole));
            assert!(inside(l.prs.unwrap(), whole));
            assert!(inside(l.footer, whole));
            assert!(l.table.height() >= 0 && l.prs.unwrap().height() >= 0);
            assert_eq!(l.table.bottom, l.prs.unwrap().top);
        }
    }

    #[test]
    fn a_columns_header_stays_in_it_and_its_tree_under_it() {
        for scale in SCALES {
            let l = layout(&spec(scale));
            for (cell, column) in l.cells.iter().zip(&l.columns) {
                let Some(c) = column else { continue };
                for part in [c.icon, c.name, c.checkout, c.status, c.title] {
                    assert!(inside(part, *cell), "scale {scale}: {part:?}");
                }
                assert!(c.icon.right <= c.name.left);
                assert!(c.name.bottom <= c.checkout.top);
                assert!(c.checkout.right <= c.status.left);
                assert_eq!(c.icon.width(), px(HEAD_ICON, scale));
                if let Some(g) = c.glance {
                    assert!(c.status.bottom <= g.top && g.bottom <= c.line.top);
                }
                assert_eq!(c.line.bottom, c.body.top);
                assert_eq!(c.body.bottom, cell.bottom);
                assert_eq!(c.body.right, c.edge.left);
                assert_eq!(c.edge.right, cell.right);
            }
            // A long checkout gives way to the status.
            let long = HeadSpec {
                checkout: px(900.0, scale),
                status: px(120.0, scale),
                glance: None,
            };
            let c = column(l.cells[1], &long, scale);
            assert!(inside(c.status, l.cells[1]));
            assert_eq!(c.status.width(), px(120.0, scale));
            assert!(c.checkout.right <= c.status.left);
        }
    }

    #[test]
    fn without_pull_requests_the_table_reaches_the_bottom_bar() {
        let mut s = spec(1.5);
        s.prs = None;
        let l = layout(&s);
        assert!(l.prs.is_none());
        assert_eq!(l.table.bottom, l.footer.top);
    }
}
