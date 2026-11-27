//! Where the panel's parts go: its top bar, its sections' headings and
//! rows, what follows them and its bottom bar, in whole device px from
//! the panel's own top-left corner.
//!
//! Lengths are set in logical px and every edge is rounded once, from
//! the panel's corner, so neighbours share their edge: no gap and no
//! overlap at any scale.

pub use super::px;
use crate::config::Section;
use crate::layout::Rect;

/// The panel's width, logical px: wide enough for a timeline strip next to
/// each repo's name.
pub const WIDTH: f32 = 480.0;
/// A row's height.
pub const STRIP: f32 = 36.0;
/// The label column at a row's right: icon, name, checkout and status.
pub const LABEL: f32 = 168.0;
/// The label column of a row that is its repo's icon alone: the floating
/// view's.
pub const ICON_LABEL: f32 = 40.0;
/// A repo icon beside its name, and one alone.
pub const MARK: f32 = 16.0;
pub const ICON_MARK: f32 = 22.0;
/// A section's heading.
pub const HEADING: f32 = 35.0;
/// Room under an open section's heading, so a tag's diamond over a dot in
/// the first strip's top lane stays clear of the heading.
pub const HEADING_GAP: f32 = 4.0;
/// The top bar's buttons, square.
pub const BUTTON: f32 = 28.0;
/// The status edge on a row that needs something: how wide, and how far
/// in from the row's top and bottom.
pub const EDGE_WIDTH: f32 = 3.0;
pub const EDGE_INSET: f32 = 3.0;

/// The words' sizes, logical px.
pub const HEADLINE: f32 = 16.0;
pub const SMALL: f32 = 14.0;
pub const TINY: f32 = 12.0;

/// The bars' padding at their sides, and the top bar's above and below.
const PAD_X: f32 = 16.0;
const PAD_Y: f32 = 12.0;
const GAP: f32 = 8.0;
/// The most a row's status takes of its label's second line.
const STATUS_MAX: f32 = LABEL * 0.6;

/// A line's height for words `size` px high, as GPUI sets it: the golden
/// ratio, on whole px.
// GPUI's own figure for it, digit for digit.
#[allow(clippy::approx_constant)]
pub fn line_height(size: f32) -> f32 {
    (size * 1.618_034).round()
}

/// What the panel shows, as far as its layout goes. Widths and heights
/// here are measured, in device px.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spec {
    pub scale: f32,
    /// The panel's width and the most it may be tall, device px.
    pub width: i32,
    pub max_height: i32,
    /// The health row's three counts' widths, when it shows.
    pub health: Option<[i32; 3]>,
    /// How many buttons the top bar has.
    pub buttons: usize,
    pub sections: Vec<SectionSpec>,
    /// What follows the rows in the list, top to bottom: each one's height.
    pub tail: Vec<i32>,
    /// The bottom bar's height.
    pub footer: Option<i32>,
    /// How far the list is scrolled.
    pub scroll: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SectionSpec {
    pub section: Section,
    /// How many repos it holds, shown or folded away.
    pub count: usize,
    /// Its fold arrow's width.
    pub arrow: i32,
    /// Its rows; none when folded.
    pub rows: Option<Vec<RowSpec>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowSpec {
    /// Its checkout's and its status's widths, uncut.
    pub checkout: i32,
    pub status: i32,
    /// It has the status edge.
    pub edge: bool,
}

/// The top bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub rect: Rect,
    /// Its bottom edge line.
    pub line: Rect,
    /// The headline's and the subline's line boxes.
    pub headline: Rect,
    pub subline: Rect,
    /// The health bar, and its counts' line boxes.
    pub bar: Option<Rect>,
    pub counts: [Rect; 3],
    /// The buttons, left to right.
    pub buttons: Vec<Rect>,
}

/// A section's heading.
#[derive(Clone, Debug, PartialEq)]
pub struct Heading {
    pub section: Section,
    pub rect: Rect,
    /// Its words' line box, and its fold arrow's.
    pub label: Rect,
    pub arrow: Rect,
}

/// A repo's row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Which section it is in, by that section's place in the spec, and
    /// its place among all the rows.
    pub section: usize,
    pub index: usize,
    pub rect: Rect,
    /// Its tree's strip, and the label column beside it.
    pub strip: Rect,
    pub label: Rect,
    pub icon: Rect,
    /// The line boxes of its name, checkout and status.
    pub name: Rect,
    pub checkout: Rect,
    pub status: Rect,
    pub edge: Option<Rect>,
}

/// The panel, laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Its width and height.
    pub size: (i32, i32),
    pub header: Header,
    /// The list's window on its rows, between the bars.
    pub list: Rect,
    /// How tall everything in the list is, how far it is scrolled and how
    /// far it can be.
    pub content: i32,
    pub scroll: i32,
    pub max_scroll: i32,
    pub headings: Vec<Heading>,
    pub rows: Vec<Row>,
    pub tail: Vec<Rect>,
    pub footer: Option<Rect>,
}

/// Lays the panel out.
pub fn layout(spec: &Spec) -> Layout {
    let s = if spec.scale.is_finite() && spec.scale > 0.0 {
        spec.scale
    } else {
        1.0
    };
    let e = |v: f32| px(v, s);
    let w = spec.width.max(0);

    // The top bar: the titles, then the buttons at its right.
    let titles_h = line_height(HEADLINE)
        + line_height(TINY)
        + if spec.health.is_some() {
            4.0 + line_height(TINY)
        } else {
            0.0
        };
    let header_h = PAD_Y + titles_h + PAD_Y + 1.0;
    let n = spec.buttons as i32;
    let button = e(BUTTON);
    let buttons_left = w - e(PAD_X) - n * button - (n - 1).max(0) * e(GAP);
    let button_top = e(PAD_Y + (titles_h - BUTTON) / 2.0);
    let buttons = (0..n)
        .map(|i| {
            let left = buttons_left + i * (button + e(GAP));
            Rect::new(left, button_top, left + button, button_top + button)
        })
        .collect();
    let titles_left = e(PAD_X);
    let titles_right = (buttons_left - if n > 0 { e(GAP) } else { 0 }).max(titles_left);
    let line_box = |top: f32, size: f32| {
        Rect::new(
            titles_left,
            e(top),
            titles_right,
            e(top) + e(line_height(size)),
        )
    };
    let mut counts = [Rect::new(0, 0, 0, 0); 3];
    let (headline, subline, bar) = {
        let health_top = PAD_Y + line_height(HEADLINE) + line_height(TINY) + 4.0;
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
            let top = e(health_top + (line_height(TINY) - 4.0) / 2.0);
            Rect::new(titles_left, top, right, top + e(4.0))
        });
        (
            line_box(PAD_Y, HEADLINE),
            line_box(PAD_Y + line_height(HEADLINE), TINY),
            bar,
        )
    };
    let header = Header {
        rect: Rect::new(0, 0, w, e(header_h)),
        line: Rect::new(0, e(header_h - 1.0), w, e(header_h)),
        headline,
        subline,
        bar,
        counts,
        buttons,
    };

    // The list, unscrolled, from the top bar down: logical px, so every
    // edge is rounded once from the panel's corner.
    struct At {
        section: usize,
        heading: f32,
        rows: Option<(f32, Vec<RowSpec>)>,
    }
    let mut y = 0.0f32;
    let mut placed = Vec::new();
    for (i, sec) in spec.sections.iter().enumerate() {
        let heading = y;
        y += HEADING;
        let rows = sec.rows.as_ref().map(|rows| {
            y += HEADING_GAP;
            let top = y;
            y += STRIP * rows.len() as f32;
            (top, rows.clone())
        });
        placed.push(At {
            section: i,
            heading,
            rows,
        });
    }
    let tail_tops: Vec<f32> = spec
        .tail
        .iter()
        .map(|&h| {
            let top = y;
            y += h.max(0) as f32 / s;
            top
        })
        .collect();
    let content = e(header_h + y) - e(header_h);

    // The bottom bar, and the list between the bars: as tall as what it
    // holds, until the panel is as tall as it may be.
    let footer_h = spec.footer.unwrap_or(0).max(0);
    let list_top = e(header_h);
    let room = (spec.max_height - list_top - footer_h).max(0);
    let list_h = content.min(room);
    let list = Rect::new(0, list_top, w, list_top + list_h);
    let footer = spec
        .footer
        .map(|_| Rect::new(0, list.bottom, w, list.bottom + footer_h));
    let max_scroll = (content - list_h).max(0);
    let scroll = spec.scroll.clamp(0, max_scroll);

    // Where the list's parts are now, scrolled.
    let at = |v: f32| e(header_h + v) - scroll;
    let label_w = e(LABEL);
    let label_left = (w - label_w).max(0);
    let mut headings = Vec::new();
    let mut rows = Vec::new();
    for p in &placed {
        let sec = &spec.sections[p.section];
        let (top, bottom) = (at(p.heading), at(p.heading + HEADING));
        let text_top = at(p.heading + 6.0);
        let text_bottom = text_top + e(line_height(SMALL));
        let arrow_left = w - e(PAD_X) - sec.arrow;
        let label_right = (arrow_left - e(10.0)).max(e(PAD_X));
        headings.push(Heading {
            section: sec.section,
            rect: Rect::new(0, top, w, bottom),
            label: Rect::new(e(PAD_X), text_top, label_right, text_bottom),
            arrow: Rect::new(arrow_left, text_top, arrow_left + sec.arrow, text_bottom),
        });
        let Some((rows_top, specs)) = &p.rows else {
            continue;
        };
        for (i, row) in specs.iter().enumerate() {
            let top_l = rows_top + STRIP * i as f32;
            let (top, bottom) = (at(top_l), at(top_l + STRIP));
            let rect = Rect::new(0, top, w, bottom);
            let label = Rect::new(label_left, top, w, bottom);
            let (icon, name, checkout, status) = {
                let side = e(MARK);
                let left = label_left + e(GAP);
                let icon_top = at(top_l + (STRIP - MARK) / 2.0);
                // The two lines of words, centred on the row as one block.
                let lines = line_height(SMALL) + line_height(TINY);
                let words_top = top_l + (STRIP - lines) / 2.0;
                let words_left = left + side + e(GAP);
                let words_right = (w - e(PAD_X)).max(words_left);
                let name_top = at(words_top);
                let second_top = at(words_top + line_height(SMALL));
                let second_bottom = second_top + e(line_height(TINY));
                // The status keeps its width, up to its share; the
                // checkout takes what is left.
                let room = words_right - words_left;
                let status_w = row.status.min(e(STATUS_MAX)).min(room).max(0);
                let checkout_w = row.checkout.min(room - e(4.0) - status_w).max(0);
                let status_left = words_left + checkout_w + e(4.0);
                (
                    Rect::new(left, icon_top, left + side, icon_top + side),
                    Rect::new(
                        words_left,
                        name_top,
                        words_right,
                        name_top + e(line_height(SMALL)),
                    ),
                    Rect::new(
                        words_left,
                        second_top,
                        words_left + checkout_w,
                        second_bottom,
                    ),
                    Rect::new(
                        status_left,
                        second_top,
                        (status_left + status_w).min(words_right),
                        second_bottom,
                    ),
                )
            };
            rows.push(Row {
                section: p.section,
                index: rows.len(),
                rect,
                strip: Rect::new(0, top, label_left, bottom),
                label,
                icon,
                name,
                checkout,
                status,
                edge: row.edge.then(|| {
                    Rect::new(
                        0,
                        at(top_l + EDGE_INSET),
                        e(EDGE_WIDTH),
                        at(top_l + STRIP - EDGE_INSET),
                    )
                }),
            });
        }
    }
    let tail = tail_tops
        .iter()
        .zip(&spec.tail)
        .map(|(&top, &h)| {
            let top = at(top);
            Rect::new(0, top, w, top + h.max(0))
        })
        .collect();

    Layout {
        size: (w, list.bottom + footer_h),
        header,
        list,
        content,
        scroll,
        max_scroll,
        headings,
        rows,
        tail,
        footer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALES: [f32; 6] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5];

    fn spec(scale: f32) -> Spec {
        let e = |v: f32| px(v, scale);
        Spec {
            scale,
            width: e(WIDTH),
            max_height: e(900.0),
            health: Some([e(22.0), e(20.0), e(14.0)]),
            buttons: 3,
            sections: vec![
                SectionSpec {
                    section: Section::NeedsYou,
                    count: 3,
                    arrow: e(9.0),
                    rows: Some(vec![
                        RowSpec {
                            checkout: e(40.0),
                            status: e(60.0),
                            edge: true,
                        },
                        RowSpec {
                            checkout: e(300.0),
                            status: e(300.0),
                            edge: true,
                        },
                        RowSpec::default(),
                    ]),
                },
                SectionSpec {
                    section: Section::Synced,
                    count: 12,
                    arrow: e(9.0),
                    rows: None,
                },
                SectionSpec {
                    section: Section::GithubOnly,
                    count: 2,
                    arrow: e(9.0),
                    rows: Some(vec![RowSpec::default(); 2]),
                },
            ],
            tail: vec![e(36.0) + 3 * e(38.0)],
            footer: Some(e(36.0)),
            scroll: 0,
        }
    }

    fn inside(inner: Rect, outer: Rect) -> bool {
        inner.left >= outer.left
            && inner.top >= outer.top
            && inner.right <= outer.right
            && inner.bottom <= outer.bottom
    }

    #[test]
    fn the_list_has_no_gaps_and_no_overlaps() {
        for scale in SCALES {
            let l = layout(&spec(scale));
            // Top to bottom: every heading, row and tail piece, in order.
            let mut pieces: Vec<Rect> = l
                .headings
                .iter()
                .map(|h| h.rect)
                .chain(l.rows.iter().map(|r| r.rect))
                .chain(l.tail.iter().copied())
                .collect();
            pieces.sort_by_key(|r| r.top);
            assert_eq!(pieces[0].top, l.list.top, "scale {scale}");
            let gap = px(HEADING_GAP, scale);
            for pair in pieces.windows(2) {
                let between = pair[1].top - pair[0].bottom;
                // Nothing overlaps; the only room is under a heading.
                assert!(
                    between == 0 || (between - gap).abs() <= 1,
                    "scale {scale}: {pair:?}"
                );
            }
            assert_eq!(
                pieces.last().unwrap().bottom - l.list.top,
                l.content,
                "scale {scale}"
            );
        }
    }

    #[test]
    fn the_bars_and_the_list_fill_the_panel() {
        for scale in SCALES {
            let l = layout(&spec(scale));
            assert_eq!(l.header.rect.top, 0);
            assert_eq!(l.header.rect.bottom, l.list.top);
            assert_eq!(l.header.line.bottom, l.header.rect.bottom);
            assert!(l.header.line.height() >= 1);
            let footer = l.footer.unwrap();
            assert_eq!(l.list.bottom, footer.top);
            assert_eq!(footer.bottom, l.size.1);
            assert_eq!(l.max_scroll, 0, "it all fits: nothing to scroll");
        }
    }

    #[test]
    fn every_part_is_inside_its_row() {
        for scale in SCALES {
            let l = layout(&spec(scale));
            let whole = Rect::new(0, 0, l.size.0, l.size.1);
            for r in &l.rows {
                assert!(inside(r.rect, whole), "scale {scale}");
                assert_eq!(r.strip.right, r.label.left);
                assert!(inside(r.icon, r.label), "scale {scale}: {r:?}");
                // The words' line boxes may reach a little past the row,
                // above and below: they are taller than the words.
                for part in [r.name, r.checkout, r.status] {
                    assert!(part.left >= r.icon.right && part.right <= r.label.right);
                    assert!(
                        part.top >= r.rect.top - px(4.0, scale) - 1,
                        "scale {scale}: {r:?}"
                    );
                    assert!(
                        part.bottom <= r.rect.bottom + px(4.0, scale) + 1,
                        "scale {scale}: {r:?}"
                    );
                }
                assert!(r.checkout.right <= r.status.left);
                assert_eq!(r.icon.width(), px(MARK, scale));
                assert_eq!(r.icon.height(), px(MARK, scale));
            }
            for h in &l.headings {
                assert!(inside(h.label, h.rect) && inside(h.arrow, h.rect));
                assert!(h.label.right <= h.arrow.left);
            }
            let bar = l.header.bar.unwrap();
            assert!(inside(bar, l.header.rect));
            assert!(bar.right <= l.header.counts[0].left);
            for b in &l.header.buttons {
                assert!(inside(*b, l.header.rect));
                assert!(b.left >= l.header.headline.right);
            }
        }
    }

    #[test]
    fn long_words_are_cut_to_the_label() {
        let l = layout(&spec(1.25));
        let r = &l.rows[1];
        assert_eq!(r.status.width(), px(STATUS_MAX, 1.25));
        assert!(r.checkout.width() > 0);
        assert!(r.status.right <= r.name.right);
    }

    #[test]
    fn a_tall_list_scrolls_inside_the_panel() {
        for scale in SCALES {
            let mut s = spec(scale);
            s.max_height = px(300.0, scale);
            let unscrolled = layout(&s);
            assert_eq!(unscrolled.size.1, s.max_height);
            assert!(unscrolled.max_scroll > 0);
            assert_eq!(
                unscrolled.list.height() + unscrolled.max_scroll,
                unscrolled.content
            );
            // Scrolled, everything is that much higher and nothing else
            // changes; past the end, it stops at the end.
            s.scroll = 20;
            let scrolled = layout(&s);
            for (a, b) in unscrolled.rows.iter().zip(&scrolled.rows) {
                assert_eq!(a.rect.top - 20, b.rect.top);
                assert_eq!(a.name.top - 20, b.name.top);
                assert_eq!(a.icon.left, b.icon.left);
            }
            assert_eq!(unscrolled.header, scrolled.header);
            assert_eq!(unscrolled.footer, scrolled.footer);
            s.scroll = 1_000_000;
            assert_eq!(layout(&s).scroll, unscrolled.max_scroll);
        }
    }
}
