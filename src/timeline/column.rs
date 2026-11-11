//! Big mode: each repo a column of the table, headed by its checkout and
//! stats, its history running down as a vertical tree with each commit's
//! hash, subject and line stats beside its dot.
//!
//! A column shares its repo's slot with the strip, so the same graph, but
//! scrolls on its own: wheeling the panel's strips leaves the table where
//! it was. The column's top rests on the newest row rather than on HEAD, so
//! commits not yet pulled stay in sight above it.

use super::{Colors, Dot, Grow, STEP, Timelines, ink};
use crate::git::RepoStatus;
use crate::graph::{self, Class, Seg};
use crate::history::{self, Ref};
use crate::repo_graph::RepoGraph;
use crate::repo_info::Checkout;
use crate::view::Palette;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, ContentMask, Context, DispatchPhase, Entity,
    HitboxBehavior, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement,
    Pixels, Point, ScrollWheelEvent, Styled, Window, canvas, div, point, prelude::*, px,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A column's width.
pub const COLUMN_WIDTH: f32 = 380.0;
/// Px between commits down a column.
pub(super) const ROW: f32 = 24.0;
/// Px between lanes across a column.
pub(super) const LANE: f32 = 12.0;
/// Lanes a column shows; the rest fold into a dimmed last lane.
pub(super) const LANES: usize = 8;
pub(super) const DOT: f32 = 4.0;
const DOT_HOVER: f32 = 5.5;
pub(super) const STROKE: f32 = 1.75;
/// How wide the band under another worktree's line is.
pub(super) const BAND: f32 = 10.0;
/// Px from the column's left edge to lane 0.
pub(super) const PAD: f32 = 14.0;
/// Commits whose stats one git call reads.
const STATS_BATCH: usize = 64;
/// How long after a failed batch its line stats are asked for again.
const STATS_RETRY: Duration = Duration::from_secs(10);

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
    /// or gliding, and whether it is gliding.
    fn column_top(&self, path: &Path, now: Instant) -> (f32, bool) {
        self.slots
            .get(path)
            .map_or((0.0, false), |slot| slot.shown_top(now))
    }

    /// Scrolls a column `rows` rows down (negative: up), loading more
    /// history near the end. With `together`, every column moves to the
    /// same row, as far as it has rows.
    fn scroll_column(&mut self, path: &Path, rows: f32, together: bool, cx: &mut Context<Self>) {
        let Some(own) = self.slots.get(path).and_then(|s| {
            let g = s.graph.as_ref()?;
            Some((s.top, last_top(g)))
        }) else {
            return;
        };
        let to = if together {
            // Go on from where the columns were scrolled together, unless
            // this column shows something else (scrolled before the setting
            // was on, or moved by a reload): then the rest line up with it.
            let base = if (self.table_top.min(own.1) - own.0).abs() < 1e-3 {
                self.table_top
            } else {
                own.0
            };
            let longest = self
                .slots
                .values()
                .filter_map(|s| s.graph.as_deref().map(last_top))
                .fold(0.0, f32::max);
            (base + rows).clamp(0.0, longest)
        } else {
            (own.0 + rows).clamp(0.0, own.1)
        };
        if together {
            self.table_top = to;
        }
        let now = Instant::now();
        let mut more = Vec::new();
        for (p, slot) in self.slots.iter_mut() {
            if !together && p != path {
                continue;
            }
            let Some(g) = &slot.graph else { continue };
            let top = to.min(last_top(g));
            if !g.end && (g.commits.len() as f32 - top) * STEP < super::PRELOAD {
                more.push(p.clone());
            }
            slot.glide_top_to(top, now);
        }
        for p in more {
            self.load_more(&p, cx);
        }
        self.unhover();
        cx.notify();
    }

    /// A double click on a column: back to its top, or every column's when
    /// they scroll together.
    fn column_home(&mut self, path: &Path, together: bool, cx: &mut Context<Self>) {
        if together {
            self.table_top = 0.0;
        }
        let now = Instant::now();
        for (p, slot) in self.slots.iter_mut() {
            if together || p == path {
                slot.glide_top_to(0.0, now);
            }
        }
        cx.notify();
    }

    /// Reads the line stats of the rows in view that haven't been read.
    fn want_stats(&mut self, path: &Path, rows: std::ops::Range<usize>, cx: &mut Context<Self>) {
        // No clone to ask git; GitHub's line counts aren't read.
        if crate::github::remote_slug(path).is_some() {
            return;
        }
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
        let retry = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let found = match task.await {
                Ok(found) => found,
                Err(_) => {
                    // Git failed as a whole (timed out, busy): ask again a
                    // little later instead of keeping no change for good.
                    retry.timer(STATS_RETRY).await;
                    this.update(cx, |this, _| {
                        for id in &ids {
                            this.stats_pending.remove(id);
                        }
                    })
                    .ok();
                    return;
                }
            };
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
        let together = t.state.read(cx).config.scroll_together;
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
            &colors,
            &path,
            (t.header_open(&path), this.clone()),
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
        // Rows scrolled together line up across the table, so they get
        // lines like the columns'.
        let row_lines = together.then_some(p.border);
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
                // Where the flip lands the trees next time.
                crate::morph::seen_column(bounds, window);
                if let Some(color) = row_lines {
                    paint_row_lines(bounds, top, color, window);
                }
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
                        e2.update(cx, |t, cx| t.column_home(&path2, together, cx));
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
                entity.update(cx, |t, cx| {
                    t.scroll_column(&wheel_path, -y / ROW, together, cx)
                });
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
            .chain(
                g.bands_at(row)
                    .map(|b| (format!("⌂ {}", b.name), colors.node(g, row))),
            )
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

/// The column's header: name, checkout and status, then the repo at a
/// glance: its branches by fate (merged, in review, hanging), worktrees and
/// last merge. Folded, that is one row of coloured counts; open, a bar,
/// a legend and the names behind the counts. A click on the name opens the
/// repo, a click on the counts folds or opens the details.
#[allow(clippy::too_many_arguments)]
fn header(
    index: usize,
    status: &RepoStatus,
    g: Option<&RepoGraph>,
    error: Option<&str>,
    p: &Palette,
    colors: &Colors,
    path: &Path,
    (open, timelines): (bool, Entity<Timelines>),
    cx: &App,
) -> AnyElement {
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
    let repo = path.to_path_buf();
    let title = div()
        .id(("column-title", index))
        .flex()
        .flex_col()
        .gap(px(2.))
        .cursor_pointer()
        .on_click(move |_, _, cx| crate::view::open_repo(&repo, cx))
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
        );
    let head = div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(6.))
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(p.border)
        .child(title);
    let Some(g) = g else {
        return head.into_any_element();
    };
    let glance = Glance::new(g);
    let toggle_path = path.to_path_buf();
    let summary = glance
        .summary_row(index, open, p, colors)
        .cursor_pointer()
        .rounded_sm()
        .hover(|s| s.bg(p.hover))
        .on_click(move |_, _, cx| {
            timelines.update(cx, |t, cx| t.toggle_header(&toggle_path, cx));
        });
    head.child(summary)
        .when(open, |d| d.child(glance.details(p, colors)))
        .into_any_element()
}

/// What a column header shows about a repo beyond its status.
struct Glance {
    branches: crate::repo_info::BranchSummary,
    /// The default branch's short name, for the words: `main`.
    trunk: String,
    local: usize,
    remote: usize,
    /// Other worktrees than the main one, as their lines.
    worktrees: Vec<String>,
    /// How long ago the last merge into the default branch was, and its line.
    merge: Option<(String, String)>,
    /// The PR state per branch, for the names in the details.
    prs: Arc<graph::PrColors>,
}

impl Glance {
    fn new(g: &RepoGraph) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let i = &g.info;
        let trunk = i
            .default_branch
            .as_deref()
            .map(|d| d.split_once('/').map_or(d, |(_, n)| n).to_string())
            .unwrap_or_else(|| "the default branch".to_string());
        let worktrees = i
            .worktrees
            .iter()
            .skip(1)
            .map(|w| {
                let name = w.path.file_name().map_or_else(
                    || w.path.display().to_string(),
                    |n| n.to_string_lossy().into(),
                );
                match &w.branch {
                    Some(b) => format!("{name} on {b}"),
                    None => format!("{name}, detached"),
                }
            })
            .collect();
        let merge = match (&i.last_merge, &i.default_branch) {
            (Some(m), Some(b)) => {
                let ago = history::ago(now, m.time);
                Some((
                    ago.clone(),
                    format!("Last merge into {b}, {ago} ago: {}", m.subject),
                ))
            }
            _ => None,
        };
        Self {
            branches: crate::repo_info::summarize(&i.branches, |name| g.prs.of(name)),
            trunk,
            local: i.local_branches,
            remote: i.remote_branches,
            worktrees,
            merge,
            prs: g.prs.clone(),
        }
    }

    /// (count, colour, words) for merged, in review and hanging.
    fn fates(&self, colors: &Colors) -> [(usize, Hsla, String); 3] {
        let b = &self.branches;
        [
            (
                b.merged.len(),
                colors.merged,
                format!("merged into {}", self.trunk),
            ),
            (
                b.in_review.len(),
                colors.pr_open,
                "in open pull requests".to_string(),
            ),
            (
                b.hanging.len(),
                colors.unpushed,
                "hanging: not merged, no open PR".to_string(),
            ),
        ]
    }

    /// The folded view: a small bar of the fates, the branch count, each
    /// fate's count, worktrees and the last merge's age, as icons with
    /// numbers that never wrap. Each part says what it is on hover.
    fn summary_row(
        &self,
        index: usize,
        open: bool,
        p: &Palette,
        colors: &Colors,
    ) -> gpui::Stateful<gpui::Div> {
        let total = self.branches.total();
        let fates = self.fates(colors);
        let mut tip_lines = vec![(
            format!(
                "{total} branch{} besides {} ({} local, {} remote)",
                if total == 1 { "" } else { "es" },
                self.trunk,
                self.local,
                self.remote
            ),
            p.text,
        )];
        tip_lines.extend(
            fates
                .iter()
                .map(|(n, c, words)| (format!("{n} {words}"), *c)),
        );
        if !self.worktrees.is_empty() {
            tip_lines.push((
                format!("{} more worktree(s)", self.worktrees.len()),
                p.muted,
            ));
        }
        if let Some((_, line)) = &self.merge {
            tip_lines.push((line.clone(), p.muted));
        }
        tip_lines.push((
            if open {
                "Click to fold"
            } else {
                "Click for details"
            }
            .to_string(),
            p.muted,
        ));
        let count = |glyph: &'static str, n: String, c: Hsla, dim: bool| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(3.))
                .when(dim, |d| d.opacity(0.45))
                .child(div().text_color(c).child(glyph))
                .child(div().text_color(p.text).child(n))
        };
        div()
            .id(("column-glance", index))
            .flex()
            .items_center()
            .gap(px(10.))
            .px_1()
            .py(px(2.))
            .overflow_hidden()
            .text_xs()
            .tooltip(crate::view::tip(tip_lines, p))
            .child(fate_bar(&fates, total, px(36.).into(), p).flex_none())
            .child(
                div()
                    .flex_none()
                    .text_color(p.muted)
                    .child(format!("{total}")),
            )
            .children(
                fates
                    .iter()
                    .map(|(n, c, _)| count("●", n.to_string(), *c, *n == 0)),
            )
            .when(!self.worktrees.is_empty(), |d| {
                d.child(count(
                    "⧉",
                    (self.worktrees.len() + 1).to_string(),
                    p.muted,
                    false,
                ))
            })
            .when_some(self.merge.as_ref(), |d, (ago, _)| {
                d.child(count("◷", ago.clone(), p.muted, false))
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_color(p.muted)
                    .child(if open { "▾" } else { "▸" }),
            )
    }

    /// The open view: a full-width bar, a legend line per fate with the
    /// branch names behind it, then worktrees and the last merge.
    fn details(&self, p: &Palette, colors: &Colors) -> gpui::Div {
        /// Names listed under a fate before "and N more".
        const NAMES: usize = 5;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let total = self.branches.total();
        let fates = self.fates(colors);
        let lists = [
            &self.branches.merged,
            &self.branches.in_review,
            &self.branches.hanging,
        ];
        let line = |text: String, c: Hsla| div().truncate().text_color(c).child(text);
        let mut out = div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .px_1()
            .text_xs()
            .child(fate_bar(&fates, total, gpui::relative(1.), p).mb_1());
        for (i, ((n, c, words), list)) in fates.iter().zip(lists).enumerate() {
            out = out.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_none().size(px(8.)).rounded_full().bg(*c))
                    .child(line(format!("{n} {words}"), p.text)),
            );
            // Merged ones (the first) are done with: their names matter least.
            let shown = if i == 0 { 0 } else { NAMES };
            for b in list.iter().take(shown) {
                let mut text = format!("{} · {}", b.name, history::ago(now, b.time));
                match (b.local, b.remote) {
                    (true, false) => text.push_str(" · local only"),
                    (false, true) => text.push_str(" · remote only"),
                    _ => {}
                }
                if let Some(pr) = self.prs.of(&b.name) {
                    text.push_str(&format!(" · {}", pr.label()));
                }
                out = out.child(div().pl(px(16.)).child(line(text, p.muted)));
            }
            if shown > 0 && list.len() > shown {
                out = out.child(
                    div()
                        .pl(px(16.))
                        .child(line(format!("and {} more", list.len() - shown), p.muted)),
                );
            }
        }
        out = out.child(line(
            format!(
                "{} local · {} remote branch{}",
                self.local,
                self.remote,
                if self.remote == 1 { "" } else { "es" }
            ),
            p.muted,
        ));
        for w in &self.worktrees {
            out = out.child(line(format!("↳ worktree {w}"), p.muted));
        }
        out.child(line(
            match &self.merge {
                Some((_, l)) => l.clone(),
                None => format!("No merges into {}", self.trunk),
            },
            p.muted,
        ))
    }
}

/// A rounded bar cut into the fates, each part as long as its share; an
/// empty track when there are no branches.
fn fate_bar(
    fates: &[(usize, Hsla, String); 3],
    total: usize,
    width: gpui::DefiniteLength,
    p: &Palette,
) -> gpui::Div {
    div()
        .flex()
        .w(width)
        .h(px(6.))
        .rounded_full()
        .overflow_hidden()
        .bg(p.border)
        .children(fates.iter().filter(|(n, _, _)| *n > 0).map(|(n, c, _)| {
            div()
                .h_full()
                .w(gpui::relative(*n as f32 / total.max(1) as f32))
                .bg(*c)
        }))
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

/// The rows' thin lines across a column, one between each two rows, the
/// same colour and width as the lines between the columns.
fn paint_row_lines(bounds: Bounds<Pixels>, top: f32, color: Hsla, window: &mut Window) {
    let s = window.scale_factor();
    let (ox, oy) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let (x1, h) = (f32::from(bounds.right()), f32::from(bounds.size.height));
    // The line under row k sits where row k + 1 starts.
    let mut y = (1.0 - top.fract()) * ROW;
    while y < h {
        // On the device pixel grid, so a line stays one crisp pixel.
        let at = ink::pos(s, oy + y, 1.0);
        ink::line(window, (ox, at), (x1, at), 1.0, color, None);
        y += ROW;
    }
}

/// A column's highest top: its last row.
fn last_top(g: &RepoGraph) -> f32 {
    (g.commits.len() as f32 - 1.0).max(0.0)
}

/// Where a column whose top was `top` in `old` goes in this reload of it:
/// still the newest row if it was there, else the same commit, so new
/// commits don't slide what was being looked at.
pub(super) fn carry_top(g: &RepoGraph, old: &RepoGraph, top: f32) -> f32 {
    if top < 1e-3 || old.commits.is_empty() {
        return 0.0;
    }
    let row = (top.round() as usize).min(old.commits.len() - 1);
    let id = &old.commits[row].id;
    let at = match g.commits.iter().position(|c| &c.id == id) {
        Some(n) => n as f32 + top - row as f32,
        None => top,
    };
    at.clamp(0.0, last_top(g))
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
    let s = window.scale_factor();
    let (ox, oy) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let at = |row: f32, lane: f32| {
        // Fractional rows only come from segment ends, which are whole.
        let pos = if row >= rows as f32 {
            row + if geo.dirty { 1.0 } else { 0.0 }
        } else {
            geo.pos(row as usize)
        };
        // On the device pixel grid, so runs are crisp and bends meet them
        // exactly.
        point(
            px(ink::pos(s, ox + geo.x(lane.round() as usize), STROKE)),
            px(ink::pos(s, oy + geo.y(pos), STROKE)),
        )
    };
    let folded = |lane: f32| lane.round() as usize >= LANES;
    let span = geo.rows_in(bounds.size.height);
    let (lo, hi) = (span.start.saturating_sub(1), (span.end + 1).min(rows));

    // Under everything, a soft band along each other worktree's line.
    let bottom = oy + f32::from(bounds.size.height);
    for b in g.bands.iter().filter(|b| b.top < hi && b.bottom + 1 >= lo) {
        let color = colors.band(g, b, b.lane >= LANES);
        let r = BAND / 2.0;
        let top = at(b.top as f32, b.lane as f32);
        let (xb, y0) = (f32::from(top.x), f32::from(top.y));
        let y1 = if b.open {
            bottom + r
        } else {
            f32::from(at(b.bottom as f32, b.lane as f32).y)
        };
        let y1 = y1.min(bottom + r);
        ink::line(window, (xb, y0), (xb, y1), BAND, color, None);
        ink::circle(window, (xb, y0), r, Some(color), None);
        if !b.open {
            ink::circle(window, (xb, y1), r, Some(color), None);
        }
    }

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
        let center = point(
            px(ink::pos(s, ox + geo.x(geo.wip_lane), STROKE)),
            px(ink::pos(s, oy + geo.y(geo.wip_pos), STROKE)),
        );
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
        let c = (f32::from(center.x), f32::from(center.y));
        ink::circle(
            window,
            c,
            r,
            Some(colors.bg),
            Some((1.0, colors.unpushed, true)),
        );
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
        let c = (f32::from(center.x), f32::from(center.y));
        if copied == Some(row) {
            let ring = r + 3.0;
            ink::circle(window, c, ring, None, Some((1.0, colors.head, false)));
        }
        if head {
            ink::circle(window, c, r + 3.5, Some(colors.head.opacity(0.22)), None);
            ink::circle(window, c, r, Some(colors.head), None);
        } else if node.class == Class::Unpulled || node.merge {
            let r = if node.merge { r - 0.5 } else { r };
            ink::circle(window, c, r, Some(colors.bg), Some((1.75, color, false)));
        } else {
            ink::circle(window, c, r, Some(color), None);
        }
    }
    dots
}
