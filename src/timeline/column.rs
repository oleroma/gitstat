//! Big mode: each repo a column of the table, headed by its checkout and
//! stats, its history running down as a vertical tree with each commit's
//! hash, subject and line stats beside its dot.
//!
//! A column shares its repo's slot with the strip: the same graph, and the
//! same scroll, measured in strip px, so the strip and the column show the
//! same commit. The column's top rests on the newest row rather than on
//! HEAD, so commits not yet pulled stay in sight above it.

use super::{Colors, Dot, Grow, STEP, Timelines, circle, ink, ring};
use crate::git::RepoStatus;
use crate::graph::{self, Class, Seg};
use crate::history::{self, Ref};
use crate::repo_graph::RepoGraph;
use crate::repo_info::Checkout;
use crate::view::Palette;
use gpui::{
    AnyElement, App, BorderStyle, Bounds, ClipboardItem, ContentMask, Context, DispatchPhase,
    Entity, HitboxBehavior, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, Pixels, Point, ScrollWheelEvent, Styled, Window, canvas, div, fill, outline,
    point, prelude::*, px,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// A column's width.
pub const COLUMN_WIDTH: f32 = 380.0;
/// Px between commits down a column.
const ROW: f32 = 24.0;
/// Px between lanes across a column.
const LANE: f32 = 12.0;
/// Lanes a column shows; the rest fold into a dimmed last lane.
const LANES: usize = 8;
const DOT: f32 = 4.0;
const DOT_HOVER: f32 = 5.5;
const STROKE: f32 = 1.75;
/// Px from the column's left edge to lane 0.
const PAD: f32 = 14.0;
/// Commits whose stats one git call reads.
const STATS_BATCH: usize = 64;

impl Timelines {
    /// The repo to bring into view when big mode next draws, once.
    pub fn take_focus(&mut self) -> Option<PathBuf> {
        self.focus.take()
    }

    /// Asks big mode to show `path`'s column.
    pub fn focus(&mut self, path: &Path) {
        self.focus = Some(path.to_path_buf());
    }

    /// The first row a column shows at `now`, fractional while between rows
    /// or gliding, and whether it is gliding. The column rests on the newest
    /// row; HEAD's row is where the strip rests.
    fn column_top(&self, path: &Path, now: Instant) -> (f32, bool) {
        let Some(slot) = self.slots.get(path) else {
            return (0.0, false);
        };
        let home = slot.graph.as_ref().map_or(0.0, |g| g.home());
        let (scroll, gliding) = slot.shown_scroll(now);
        ((scroll / STEP - home).max(0.0), gliding)
    }

    /// Scrolls a column `rows` rows down (negative: up), loading more
    /// history near the end. Below home the strip and column agree; above
    /// it the column has nothing to show, so it starts from home.
    fn scroll_column(&mut self, path: &Path, rows: f32, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get_mut(path) else {
            return;
        };
        let Some(g) = &slot.graph else { return };
        let n = g.commits.len() as f32;
        let home = g.home() * STEP;
        let max = ((n - 1.0) * STEP).max(home);
        let to = (slot.scroll.max(home) + rows * STEP).clamp(home, max);
        let near_end = !g.end && n * STEP - to < super::PRELOAD;
        // Gliding up from above home would sweep through rows the column
        // never shows.
        if slot.scroll < home {
            slot.scroll = home;
            slot.glide = None;
        }
        slot.glide_to(to, Instant::now());
        if near_end {
            self.load_more(path, cx);
        }
        self.unhover();
        cx.notify();
    }

    /// Reads the line stats of the rows in view that haven't been read.
    fn want_stats(&mut self, path: &Path, rows: std::ops::Range<usize>, cx: &mut Context<Self>) {
        let Some(g) = self.slots.get(path).and_then(|s| s.graph.as_ref()) else {
            return;
        };
        let ids: Vec<String> = g.commits
            [rows.start.min(g.commits.len())..rows.end.min(g.commits.len())]
            .iter()
            .map(|c| c.id.clone())
            .filter(|id| !self.stats.contains_key(id) && !self.stats_pending.contains(id))
            .take(STATS_BATCH)
            .collect();
        if ids.is_empty() {
            return;
        }
        self.stats_pending.extend(ids.iter().cloned());
        let repo = path.to_path_buf();
        let asked = ids.clone();
        let task = cx.background_spawn(async move { history::stats(&repo, &asked) });
        cx.spawn(async move |this, cx| {
            let found = task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                for id in ids {
                    this.stats_pending.remove(&id);
                    // A commit git couldn't show counts as no change, so it
                    // isn't asked for again on every frame.
                    let stat = found.get(&id).copied().unwrap_or_default();
                    this.stats.insert(id, stat);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// One repo's column: its header, then its tree.
    pub fn column(
        this: &Entity<Timelines>,
        index: usize,
        status: &RepoStatus,
        p: &Palette,
        dark: bool,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let t = this.read(cx);
        let path = status.path.clone();
        let slot = t.slots.get(&path);
        let graph = slot.and_then(|s| s.graph.clone());
        let error = slot.and_then(|s| s.error.clone());
        let now = Instant::now();
        let (top, gliding) = t.column_top(&path, now);
        let dirty = status.dirty;
        let grow = t.grow(&path, dirty, now);
        let moving = gliding || grow.going();
        let copied = t
            .copied
            .as_ref()
            .filter(|(p, _)| *p == path)
            .map(|&(_, row)| row);
        let colors = Colors::new(dark, p);
        let geo = graph
            .as_ref()
            .map(|g| Geometry::new(g, dirty, top))
            .unwrap_or_default();

        let header = header(
            index,
            status,
            graph.as_deref(),
            error.as_deref(),
            p,
            &path,
            cx,
        );

        // The rows' text, for the rows that fit the window; the body clips
        // the rest.
        let visible = (f32::from(window.viewport_size().height) / ROW).ceil() as usize + 2;
        let mut texts: Vec<AnyElement> = Vec::new();
        if let Some(g) = &graph {
            let first = top.floor() as usize;
            let left = geo.text_left();
            if dirty {
                texts.push(
                    row_box(geo.y(geo.wip_pos), left)
                        .text_color(colors.unpushed)
                        .child("Uncommitted changes")
                        .into_any_element(),
                );
            }
            for row in first..(first + visible).min(g.commits.len()) {
                texts.push(t.row_text(g, row, geo.y(geo.pos(row)), left, &colors, p));
            }
        }

        let loaded = graph.is_some();
        let entity = this.clone();
        let (shown, shown_path) = (this.clone(), path.clone());
        let paint_path = path.clone();
        let web = graph.as_ref().and_then(|g| g.info.web.clone());
        let painter = canvas(
            move |bounds, window, cx| {
                if window.content_mask().bounds.intersects(&bounds) {
                    let rows = geo.rows_in(bounds.size.height);
                    shown.update(cx, |t, cx| {
                        t.shown(&shown_path, cx);
                        t.want_stats(&shown_path, rows, cx);
                    });
                }
                window.insert_hitbox(bounds, HitboxBehavior::Normal)
            },
            move |bounds, hitbox, window, _cx| {
                let Some(g) = graph else { return };
                if moving {
                    window.request_animation_frame();
                }
                let dots = window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    paint_column(bounds, &g, &geo, grow, copied, &colors, window)
                });
                let dots = Arc::new(dots);
                let dots2 = dots.clone();
                // A column's rows are hit across their whole width, text too.
                let at_row = move |dots: &[(Point<Pixels>, Dot)], pos: Point<Pixels>| {
                    dots.iter()
                        .find(|(at, _)| f32::from((at.y - pos.y).abs()) <= ROW / 2.0)
                        .copied()
                };
                let (e1, e2) = (entity.clone(), entity);
                let (path1, path2) = (paint_path.clone(), paint_path);
                let hitbox2 = hitbox.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let found = hitbox
                        .is_hovered(window)
                        .then(|| at_row(&dots, e.position))
                        .flatten()
                        .map(|(at, dot)| {
                            let above = at.y > window.viewport_size().height / 2.0;
                            (dot, at, above, dirty)
                        });
                    e1.update(cx, |t, cx| t.hover_at(&path1, e.position, found, cx));
                });
                window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || e.button != MouseButton::Left
                        || !hitbox2.is_hovered(window)
                    {
                        return;
                    }
                    if e.click_count >= 2 {
                        e2.update(cx, |t, cx| {
                            if let Some(slot) = t.slots.get_mut(&path2) {
                                slot.scroll = slot.graph.as_ref().map_or(0.0, |g| g.home() * STEP);
                                cx.notify();
                            }
                        });
                        return;
                    }
                    let Some((_, Dot::Commit(row))) = at_row(&dots2, e.position) else {
                        return;
                    };
                    let Some(id) = e2.read(cx).commit_id(&path2, row) else {
                        return;
                    };
                    match (&web, e.modifiers.control) {
                        (Some(web), true) => cx.open_url(&format!("{web}/commit/{id}")),
                        _ => {
                            cx.write_to_clipboard(ClipboardItem::new_string(id));
                            e2.update(cx, |t, cx| t.flash_copied(&path2, row, cx));
                        }
                    }
                });
            },
        )
        .absolute()
        .inset_0();

        let entity = this.clone();
        let wheel_path = path.clone();
        let body = div()
            .id(("column-body", index))
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(p.surface)
            .on_scroll_wheel(move |e: &ScrollWheelEvent, _, cx| {
                let d = e.delta.pixel_delta(px(ROW));
                let (x, y) = (f32::from(d.x), f32::from(d.y));
                // Sideways and Shift+wheel move the table; up and down the column.
                if e.modifiers.shift || x.abs() > y.abs() {
                    return;
                }
                cx.stop_propagation();
                entity.update(cx, |t, cx| t.scroll_column(&wheel_path, -y / ROW, cx));
            })
            .child(painter)
            .children(texts)
            .when(!loaded, |b| {
                b.child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(p.muted)
                        .child(if error.is_some() {
                            "History unreadable"
                        } else {
                            "Reading history…"
                        }),
                )
            });

        div()
            .id(("column", index))
            .flex()
            .flex_col()
            .flex_none()
            .w(px(COLUMN_WIDTH))
            .h_full()
            .border_r_1()
            .border_color(p.border)
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// A row's text beside its dot: hash, refs, subject, stats, age.
    fn row_text(
        &self,
        g: &RepoGraph,
        row: usize,
        y: f32,
        left: f32,
        colors: &Colors,
        p: &Palette,
    ) -> AnyElement {
        let c = &g.commits[row];
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let head = g.ctx.refs.is_head(&c.id);
        let pills = g
            .ctx
            .refs
            .at(&c.id)
            .iter()
            .map(|r| match r {
                Ref::Tag(t) => (format!("◆ {t}"), colors.node(g, row)),
                Ref::Local(n) => (n.clone(), colors.node(g, row)),
                Ref::Remote(n) => (n.clone(), p.muted),
            })
            .chain(g.stashes_at(row).map(|s| (s.name.clone(), colors.stash)))
            .map(|(text, color)| pill(text, color))
            .collect::<Vec<_>>();
        let stat = self.stats.get(&c.id);
        row_box(y, left)
            .child(
                div()
                    .flex_none()
                    .font_family("Consolas")
                    .text_color(colors.node(g, row))
                    .when(head, |d| d.font_weight(gpui::FontWeight::BOLD))
                    .child(c.short().to_string()),
            )
            .children(pills)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(p.text)
                    .child(c.subject.clone()),
            )
            .when_some(stat.filter(|s| s.files > 0), |d, s| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .gap_1()
                        .child(
                            div()
                                .text_color(colors.added)
                                .child(format!("+{}", s.added)),
                        )
                        .child(
                            div()
                                .text_color(colors.removed)
                                .child(format!("−{}", s.removed)),
                        ),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(30.))
                    .text_right()
                    .text_color(p.muted)
                    .child(history::ago(now, c.time)),
            )
            .into_any_element()
    }
}

/// A row's text box, centred on `y` in the body, right of the graph.
fn row_box(y: f32, left: f32) -> gpui::Div {
    div()
        .absolute()
        .top(px(y - ROW / 2.0))
        .left(px(left))
        .right(px(8.))
        .h(px(ROW))
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
}

/// A branch, tag or stash name beside a hash.
fn pill(text: String, color: Hsla) -> AnyElement {
    div()
        .flex_none()
        .max_w(px(110.))
        .truncate()
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(color.opacity(0.6))
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// The column's header: name, checkout and status, worktrees and branches,
/// and the last merge. A click opens the repo's folder.
fn header(
    index: usize,
    status: &RepoStatus,
    g: Option<&RepoGraph>,
    error: Option<&str>,
    p: &Palette,
    path: &Path,
    cx: &App,
) -> AnyElement {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let checkout = match g.map(|g| &g.info.checkout) {
        Some(Checkout::Branch(b)) => b.clone(),
        Some(Checkout::Unborn(b)) => format!("{b} (no commits)"),
        Some(Checkout::Detached(h)) => format!("detached at {h}"),
        None => String::new(),
    };
    let label = match error {
        Some(_) if status.error.is_none() => "history unreadable".to_string(),
        _ => status.label(),
    };
    let stats = g.map(|g| {
        let i = &g.info;
        let wt = i.worktrees.len().max(1);
        format!(
            "{wt} worktree{} · {} local · {} remote branch{}",
            if wt == 1 { "" } else { "s" },
            i.local_branches,
            i.remote_branches,
            if i.remote_branches == 1 { "" } else { "es" }
        )
    });
    let merge = g.map(|g| match (&g.info.last_merge, &g.info.default_branch) {
        (Some(m), Some(b)) => format!(
            "last merge into {b} {} ago: {}",
            history::ago(now, m.time),
            m.subject
        ),
        (None, Some(b)) => format!("no merges into {b}"),
        _ => "no default branch".to_string(),
    });
    let worktrees: Vec<String> = g
        .map(|g| {
            g.info
                .worktrees
                .iter()
                .skip(1)
                .map(|w| {
                    let name = w.path.file_name().map_or_else(
                        || w.path.display().to_string(),
                        |n| n.to_string_lossy().into(),
                    );
                    match &w.branch {
                        Some(b) => format!("↳ {name} on {b}"),
                        None => format!("↳ {name}, detached"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let open = path.to_path_buf();
    div()
        .id(("column-header", index))
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(2.))
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(p.border)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .on_click(move |_, _, cx| cx.open_with_system(&open))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(crate::repo_marks::global_mark(
                    status,
                    20.0,
                    crate::view::status_color(status, p),
                    cx,
                ))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(p.text)
                        .child(status.name.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(p.text)
                        .child(checkout),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(crate::view::status_color(status, p))
                        .child(label),
                ),
        )
        .children(stats.map(|s| div().truncate().text_xs().text_color(p.muted).child(s)))
        .children(
            worktrees
                .into_iter()
                .map(|w| div().truncate().text_xs().text_color(p.muted).child(w)),
        )
        .children(merge.map(|m| div().truncate().text_xs().text_color(p.muted).child(m)))
        .into_any_element()
}

/// Where a column's rows and lanes sit. Rows are counted in "positions":
/// with uncommitted changes, HEAD and everything below it move down one, and
/// the work-in-progress dot takes the position HEAD left.
#[derive(Clone, Copy, Default)]
struct Geometry {
    top: f32,
    dirty: bool,
    head: Option<usize>,
    wip_pos: f32,
    wip_lane: usize,
    lanes: usize,
}

impl Geometry {
    fn new(g: &RepoGraph, dirty: bool, top: f32) -> Self {
        let rows = g.layout.nodes.len();
        let head = g.head_row.filter(|&r| r < rows);
        Geometry {
            top,
            dirty,
            head,
            wip_pos: head.map_or(0.0, |h| h as f32),
            wip_lane: head.map_or(0, |h| g.layout.free_lane_above(h)),
            lanes: g.layout.width.clamp(1, LANES),
        }
    }

    fn pos(&self, row: usize) -> f32 {
        let shifted = self.dirty && self.head.is_none_or(|h| row >= h);
        row as f32 + if shifted { 1.0 } else { 0.0 }
    }

    /// y of a position, from the body's top.
    fn y(&self, pos: f32) -> f32 {
        (pos - self.top) * ROW + ROW / 2.0
    }

    fn x(&self, lane: usize) -> f32 {
        PAD + lane.min(LANES - 1) as f32 * LANE
    }

    fn text_left(&self) -> f32 {
        self.x(self.lanes.max(1) - 1) + PAD
    }

    /// The rows in view for a body this tall.
    fn rows_in(&self, height: Pixels) -> std::ops::Range<usize> {
        let first = self.top.floor().max(0.0) as usize;
        let n = (f32::from(height) / ROW).ceil() as usize + 1;
        first..first + n
    }
}

/// Paints the visible part of a column; returns each painted dot's centre.
fn paint_column(
    bounds: Bounds<Pixels>,
    g: &RepoGraph,
    geo: &Geometry,
    grow: Grow,
    copied: Option<usize>,
    colors: &Colors,
    window: &mut Window,
) -> Vec<(Point<Pixels>, Dot)> {
    let rows = g.layout.nodes.len();
    let (ox, oy) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let at = |row: f32, lane: f32| {
        // Fractional rows only come from segment ends, which are whole.
        let pos = if row >= rows as f32 {
            row + if geo.dirty { 1.0 } else { 0.0 }
        } else {
            geo.pos(row as usize)
        };
        point(px(ox + geo.x(lane.round() as usize)), px(oy + geo.y(pos)))
    };
    let folded = |lane: f32| lane.round() as usize >= LANES;
    let span = geo.rows_in(bounds.size.height);
    let (lo, hi) = (span.start.saturating_sub(1), (span.end + 1).min(rows));

    let mut dots = Vec::new();
    for e in g.layout.visible_edges(lo, hi) {
        let base = if e.first {
            colors.node(g, e.child.0)
        } else {
            match e.parent {
                Some((row, _)) => colors.node(g, row),
                None => colors.node(g, e.child.0),
            }
        };
        for seg in e.segments(rows) {
            let (a, b, curve) = match seg {
                Seg::Straight(a, b) => (a, b, false),
                Seg::Curve(a, b) => (a, b, true),
            };
            let alpha = if folded(a.lane) || folded(b.lane) {
                0.35
            } else {
                0.85
            };
            let color = ink::over(base.opacity(alpha), colors.bg);
            let (pa, pb) = (at(a.row, a.lane), at(b.row, b.lane));
            let (pa, pb) = (
                (f32::from(pa.x), f32::from(pa.y)),
                (f32::from(pb.x), f32::from(pb.y)),
            );
            if curve {
                let c = graph::noodle(pa, pb, (0.0, 1.0));
                ink::cubic(window, c, STROKE, color, None);
            } else {
                ink::line(window, pa, pb, STROKE, color, None);
            }
        }
    }

    if geo.dirty {
        let center = point(px(ox + geo.x(geo.wip_lane)), px(oy + geo.y(geo.wip_pos)));
        dots.push((center, Dot::Wip));
        let r = grow.radius(Dot::Wip, DOT, DOT_HOVER);
        if let Some(h) = geo.head {
            let to = at(h as f32, g.layout.nodes[h].lane as f32);
            let from = (f32::from(center.x), f32::from(center.y) + r);
            let to = (f32::from(to.x), f32::from(to.y) - DOT);
            ink::cubic(
                window,
                graph::noodle(from, to, (0.0, 1.0)),
                1.0,
                ink::over(colors.unpushed.opacity(0.8), colors.bg),
                Some(ink::Dash { on: 2.0, off: 2.0 }),
            );
        }
        let mut wip = ring(center, r, 1.0, colors.unpushed, colors.bg);
        wip.border_style = BorderStyle::Dashed;
        window.paint_quad(wip);
    }

    for row in lo..hi {
        let node = &g.layout.nodes[row];
        let center = at(row as f32, node.lane as f32);
        dots.push((center, Dot::Commit(row)));
        let color = colors.node(g, row);
        let color = if node.lane >= LANES {
            color.opacity(0.5)
        } else {
            color
        };
        let head = g.ctx.refs.is_head(&g.commits[row].id);
        let r = grow.radius(Dot::Commit(row), DOT, DOT_HOVER);
        if copied == Some(row) {
            let ring = r + 3.0;
            window.paint_quad(
                outline(circle(center, ring), colors.head, BorderStyle::Solid)
                    .corner_radii(px(ring))
                    .border_widths(px(1.0)),
            );
        }
        if head {
            let glow = r + 3.5;
            window.paint_quad(
                fill(circle(center, glow), colors.head.opacity(0.22)).corner_radii(px(glow)),
            );
            window.paint_quad(fill(circle(center, r), colors.head).corner_radii(px(r)));
        } else if node.class == Class::Unpulled || node.merge {
            let r = if node.merge { r - 0.5 } else { r };
            window.paint_quad(ring(center, r, 1.75, color, colors.bg));
        } else {
            window.paint_quad(fill(circle(center, r), color).corner_radii(px(r)));
        }
    }
    dots
}

/// The repos in the table's order, for `ScrollHandle::scroll_to_item`.
pub fn column_index(statuses: &[RepoStatus], path: &Path) -> Option<usize> {
    statuses.iter().position(|s| s.path == path)
}
