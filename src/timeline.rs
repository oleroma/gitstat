//! The timeline strips: each repo's history as a row of dots joined by
//! noodles, newest at the right next to the repo's name, the tail running
//! off the left edge.
//!
//! `Timelines` owns every repo's loaded graph, loads it off the UI thread
//! and reloads it whenever a status check of that repo finishes. The strip
//! itself is a canvas that paints only the dots and noodles in view and
//! hit-tests the mouse against the dots it painted.

use crate::git::RepoStatus;
use crate::graph::{self, Class, Seg};
use crate::history::{self, Ref, Stat};
use crate::repo_graph::{More, RepoGraph};
use crate::state::AppState;
use crate::view::Palette;
use gpui::{
    AnyElement, App, AppContext, BorderStyle, Bounds, ClipboardItem, ContentMask, Context,
    DispatchPhase, Entity, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, PathBuilder, Pixels, Point, ScrollWheelEvent, Styled, Window, anchored, canvas,
    deferred, div, fill, linear_color_stop, linear_gradient, outline, point, prelude::*, px, rgb,
    size,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A strip's height; the panel's row height in `layout`.
pub const STRIP_HEIGHT: f32 = 36.0;
/// The label column at the strip's right.
pub const LABEL_WIDTH: f32 = 168.0;
/// Px between commits along a strip.
const STEP: f32 = 14.0;
/// Px between lanes across a strip.
const LANE: f32 = 5.0;
/// Lanes a strip shows; the rest fold into a dimmed band at its edge.
const LANES: usize = 6;
const DOT: f32 = 3.0;
const DOT_HOVER: f32 = 4.5;
const STROKE: f32 = 1.5;
/// Px from the strip's right edge to HEAD.
const PAD: f32 = 12.0;
/// The tail fades out over this many px at the left edge.
const FADE: f32 = 24.0;
/// A dot is hit within this many px.
const HIT: f32 = 7.0;
/// Load the next page while fewer than this many px of history are left
/// past the left edge.
const PRELOAD: f32 = 1200.0;

#[derive(Default)]
struct Slot {
    graph: Option<Arc<RepoGraph>>,
    error: Option<String>,
    /// Bumped on every load, so a slower, older load can't land last.
    generation: u64,
    loading_more: bool,
    /// Px the strip is scrolled into the past; 0 shows HEAD.
    scroll: f32,
}

/// The dot under the mouse.
#[derive(Clone, Debug, PartialEq)]
pub struct Hover {
    pub repo: PathBuf,
    pub row: usize,
    /// Window position of the dot, where the card is anchored.
    pub at: Point<Pixels>,
}

pub struct Timelines {
    slots: HashMap<PathBuf, Slot>,
    /// The repos being checked as of the last state change; a repo leaving
    /// this set has just been checked, so its history may have changed.
    in_flight: HashSet<PathBuf>,
    hover: Option<Hover>,
    /// Line stats by commit id, read on first hover.
    stats: HashMap<String, Stat>,
    stats_pending: HashSet<String>,
}

impl Timelines {
    pub fn new(state: &Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(state, |this, state, cx| this.sync(&state, cx))
            .detach();
        Self {
            slots: HashMap::new(),
            in_flight: HashSet::new(),
            hover: None,
            stats: HashMap::new(),
            stats_pending: HashSet::new(),
        }
    }

    /// Loads repos that are new, reloads the ones a check just finished
    /// with, and forgets the ones gone from the list.
    fn sync(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let (paths, in_flight) = {
            let s = state.read(cx);
            let paths: Vec<PathBuf> = s.statuses.iter().map(|s| s.path.clone()).collect();
            (paths, s.in_flight.clone())
        };
        self.slots.retain(|p, _| paths.contains(p));
        for path in paths {
            let checked = self.in_flight.contains(&path) && !in_flight.contains(&path);
            if checked || !self.slots.contains_key(&path) {
                self.reload(path, cx);
            }
        }
        self.in_flight = in_flight;
    }

    fn reload(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let slot = self.slots.entry(path.clone()).or_default();
        slot.generation += 1;
        slot.loading_more = false;
        let generation = slot.generation;
        let prev = slot.graph.clone();
        let repo = path.clone();
        let task = cx.background_spawn(async move {
            RepoGraph::load(&repo, prev.as_deref(), history::PAGE_SIZE)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(slot) = this.slots.get_mut(&path) else {
                    return;
                };
                if slot.generation != generation {
                    return;
                }
                match result {
                    Ok(g) => {
                        slot.graph = Some(Arc::new(g));
                        slot.error = None;
                    }
                    Err(e) => slot.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_more(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get_mut(path) else {
            return;
        };
        let Some(graph) = slot.graph.clone() else {
            return;
        };
        if slot.loading_more || graph.end {
            return;
        }
        slot.loading_more = true;
        let generation = slot.generation;
        let repo = path.to_path_buf();
        let path = path.to_path_buf();
        let task = cx.background_spawn(async move {
            match graph.more(&repo, history::PAGE_SIZE) {
                Ok(More::Page(commits, end)) => {
                    let mut g = (*graph).clone();
                    g.append(commits, end);
                    Ok(Some(g))
                }
                Ok(More::Reload) => Ok(None),
                Err(e) => Err(e),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(slot) = this.slots.get_mut(&path) else {
                    return;
                };
                if slot.generation != generation {
                    return;
                }
                slot.loading_more = false;
                match result {
                    Ok(Some(g)) => slot.graph = Some(Arc::new(g)),
                    Ok(None) => this.reload(path, cx),
                    Err(e) => slot.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Scrolls one strip, or every strip with `all`, by `dx` px into the
    /// past (negative: toward HEAD), loading more history near the end.
    fn scroll(&mut self, path: &Path, dx: f32, all: bool, cx: &mut Context<Self>) {
        let mut more = Vec::new();
        for (p, slot) in self.slots.iter_mut() {
            if !all && p != path {
                continue;
            }
            let Some(g) = &slot.graph else { continue };
            let rows = g.commits.len() as f32;
            let max = ((rows - 1.0) * STEP).max(0.0);
            slot.scroll = (slot.scroll + dx).clamp(0.0, max);
            if !g.end && rows * STEP - slot.scroll < PRELOAD {
                more.push(p.clone());
            }
        }
        for p in more {
            self.load_more(&p, cx);
        }
        self.hover = None;
        cx.notify();
    }

    fn scroll_home(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(slot) = self.slots.get_mut(path) {
            slot.scroll = 0.0;
            cx.notify();
        }
    }

    /// The mouse moved over `path`'s strip, onto a dot or off them.
    fn hover_at(
        &mut self,
        path: &Path,
        hit: Option<(usize, Point<Pixels>)>,
        cx: &mut Context<Self>,
    ) {
        let new = match hit {
            Some((row, at)) => Some(Hover {
                repo: path.to_path_buf(),
                row,
                at,
            }),
            // Another strip's hover is not ours to clear.
            None if self.hover.as_ref().is_some_and(|h| h.repo != path) => return,
            None => None,
        };
        if new == self.hover {
            return;
        }
        if let Some(h) = &new {
            self.request_stat(&h.repo, h.row, cx);
        }
        self.hover = new;
        cx.notify();
    }

    fn request_stat(&mut self, path: &Path, row: usize, cx: &mut Context<Self>) {
        let Some(id) = self.commit_id(path, row) else {
            return;
        };
        if self.stats.contains_key(&id) || !self.stats_pending.insert(id.clone()) {
            return;
        }
        let repo = path.to_path_buf();
        let key = id.clone();
        let task = cx.background_spawn(async move { history::stat(&repo, &key) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.stats_pending.remove(&id);
                if let Ok(stat) = result {
                    this.stats.insert(id, stat);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn commit_id(&self, path: &Path, row: usize) -> Option<String> {
        let g = self.slots.get(path)?.graph.as_ref()?;
        Some(g.commits.get(row)?.id.clone())
    }

    /// One repo's strip: the graph, then the name, checkout and status.
    pub fn strip(
        this: &Entity<Timelines>,
        index: usize,
        status: &RepoStatus,
        p: &Palette,
        dark: bool,
        cx: &App,
    ) -> AnyElement {
        let t = this.read(cx);
        let path = status.path.clone();
        let slot = t.slots.get(&path);
        let graph = slot.and_then(|s| s.graph.clone());
        let scroll = slot.map_or(0.0, |s| s.scroll);
        let error = slot.and_then(|s| s.error.clone());
        let hovered = t.hover.as_ref().filter(|h| h.repo == path).map(|h| h.row);
        let colors = Colors::new(dark, p);
        let dirty = status.dirty;
        let web = graph.as_ref().and_then(|g| g.info.web.clone());

        let checkout = match graph.as_ref().map(|g| &g.info.checkout) {
            Some(crate::repo_info::Checkout::Branch(b) | crate::repo_info::Checkout::Unborn(b)) => {
                b.clone()
            }
            Some(crate::repo_info::Checkout::Detached(h)) => format!("@{h}"),
            None => String::new(),
        };
        let status_color = crate::view::status_color(status, p);

        let entity = this.clone();
        let paint_path = path.clone();
        let surface = p.surface;
        let painter = canvas(
            |bounds, _, _| bounds,
            move |bounds, _, window, _cx| {
                let Some(g) = graph else { return };
                let dots = window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    paint_strip(bounds, &g, scroll, hovered, dirty, &colors, window)
                });
                fade(bounds, surface, window);

                let (e1, e2) = (entity.clone(), entity);
                let (path1, path2) = (paint_path.clone(), paint_path);
                let dots2 = dots.clone();
                let hit = move |pos: Point<Pixels>| -> Option<(usize, Point<Pixels>)> {
                    if !bounds.contains(&pos) {
                        return None;
                    }
                    dots.iter()
                        .map(|&(at, row)| {
                            (
                                row,
                                at,
                                f32::from(at.x - pos.x).hypot(f32::from(at.y - pos.y)),
                            )
                        })
                        .filter(|&(_, _, d)| d <= HIT)
                        .min_by(|a, b| a.2.total_cmp(&b.2))
                        .map(|(row, at, _)| (row, at))
                };
                let hit2 = {
                    let dots = dots2;
                    move |pos: Point<Pixels>| -> Option<usize> {
                        dots.iter()
                            .find(|(at, _)| {
                                f32::from(at.x - pos.x).hypot(f32::from(at.y - pos.y)) <= HIT
                            })
                            .map(|&(_, row)| row)
                    }
                };
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _window, cx| {
                    if phase == DispatchPhase::Bubble {
                        let found = hit(e.position);
                        e1.update(cx, |t, cx| t.hover_at(&path1, found, cx));
                    }
                });
                window.on_mouse_event(move |e: &MouseDownEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble
                        || e.button != MouseButton::Left
                        || !bounds.contains(&e.position)
                    {
                        return;
                    }
                    if e.click_count >= 2 {
                        e2.update(cx, |t, cx| t.scroll_home(&path2, cx));
                        return;
                    }
                    let Some(row) = hit2(e.position) else { return };
                    let Some(id) = e2.read(cx).commit_id(&path2, row) else {
                        return;
                    };
                    match (&web, e.modifiers.control) {
                        (Some(web), true) => cx.open_url(&format!("{web}/commit/{id}")),
                        _ => cx.write_to_clipboard(ClipboardItem::new_string(id)),
                    }
                });
            },
        )
        .flex_1()
        .h_full();

        let entity = this.clone();
        let wheel_path = path.clone();
        let open_path = path.clone();
        div()
            .id(("strip", index))
            .flex()
            .items_center()
            .h(px(STRIP_HEIGHT))
            .on_scroll_wheel(move |e: &ScrollWheelEvent, _, cx| {
                let d = e.delta.pixel_delta(px(STEP * 3.0));
                // Up or left goes back in time.
                let dx = if f32::from(d.x) != 0.0 {
                    f32::from(d.x)
                } else {
                    f32::from(d.y)
                };
                let all = e.modifiers.shift;
                entity.update(cx, |t, cx| t.scroll(&wheel_path, dx, all, cx));
            })
            .child(painter)
            .child(
                div()
                    .id(("label", index))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .flex_none()
                    .w(px(LABEL_WIDTH))
                    .h_full()
                    .pl_2()
                    .pr_4()
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .on_click(move |_, window, cx| {
                        cx.open_with_system(&open_path);
                        crate::panel::dismiss(window, cx);
                    })
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(p.text)
                            .child(status.name.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .text_xs()
                            .child(
                                div()
                                    .flex_shrink()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(p.muted)
                                    .child(checkout),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(LABEL_WIDTH * 0.6))
                                    .truncate()
                                    .text_color(status_color)
                                    .child(match &error {
                                        Some(_) if status.error.is_none() => {
                                            "history unreadable".to_string()
                                        }
                                        _ => status.label(),
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The card for the dot under the mouse, drawn above everything.
    pub fn hover_card(&self, p: &Palette, dark: bool) -> Option<AnyElement> {
        let h = self.hover.as_ref()?;
        let g = self.slots.get(&h.repo)?.graph.as_ref()?;
        let c = g.commits.get(h.row)?;
        let node = g.layout.nodes.get(h.row)?;
        let colors = Colors::new(dark, p);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let refs: Vec<String> = g
            .ctx
            .refs
            .at(&c.id)
            .iter()
            .map(|r| match r {
                Ref::Tag(t) => format!("tag {t}"),
                r => r.name().to_string(),
            })
            .chain(g.stashes_at(h.row).map(|s| s.name.clone()))
            .collect();
        let stat = self.stats.get(&c.id).map_or_else(
            || "…".to_string(),
            |s| {
                format!(
                    "+{} −{} in {} file{}",
                    s.added,
                    s.removed,
                    s.files,
                    if s.files == 1 { "" } else { "s" }
                )
            },
        );
        let head = g.ctx.refs.is_head(&c.id);
        let what = match node.class {
            Class::Unpushed => "not pushed",
            Class::Unpulled => "not pulled",
            Class::Trunk => "on the default branch",
            Class::Merged => "merged",
            Class::Branch => "",
        };
        let card = div()
            .flex()
            .flex_col()
            .gap_1()
            .max_w(px(320.))
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(p.border)
            .bg(p.bg)
            .shadow_md()
            .text_xs()
            .text_color(p.text)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .font_family("Consolas")
                            .text_color(colors.node(g, h.row))
                            .child(c.short().to_string()),
                    )
                    .child(div().min_w_0().truncate().child(c.subject.clone())),
            )
            .child(div().text_color(p.muted).child(format!(
                "{} · {} ago · {} UTC",
                c.author,
                history::ago(now, c.time),
                history::utc(c.time)
            )))
            .child(div().text_color(p.muted).child(stat))
            .when(head || !what.is_empty() || !refs.is_empty(), |d| {
                let mut parts = Vec::new();
                if head {
                    parts.push("HEAD".to_string());
                }
                parts.extend(refs);
                if !what.is_empty() {
                    parts.push(what.to_string());
                }
                d.child(div().text_color(p.muted).child(parts.join(" · ")))
            });
        Some(
            deferred(
                anchored()
                    .position(point(h.at.x - px(12.), h.at.y + px(10.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

/// The strip's colours: GitHub's Primer palette.
#[derive(Clone)]
struct Colors {
    merged: Hsla,
    unpushed: Hsla,
    unpulled: Hsla,
    trunk: Hsla,
    head: Hsla,
    stash: Hsla,
    hues: [Hsla; 6],
}

impl Colors {
    fn new(dark: bool, p: &Palette) -> Self {
        let c = |v: u32| -> Hsla { rgb(v).into() };
        if dark {
            Self {
                merged: c(0xa371f7),
                unpushed: c(0xd29922),
                unpulled: c(0x58a6ff),
                trunk: c(0x8b949e),
                head: c(0xe6edf3),
                stash: p.muted,
                // blue, orange, pink, lemon, teal, coral
                hues: [
                    c(0x4493f8),
                    c(0xf0883e),
                    c(0xf778ba),
                    c(0xe3b341),
                    c(0x39c5cf),
                    c(0xff7b72),
                ],
            }
        } else {
            Self {
                merged: c(0x8250df),
                unpushed: c(0x9a6700),
                unpulled: c(0x0969da),
                trunk: c(0x656d76),
                head: c(0x1f2328),
                stash: p.muted,
                hues: [
                    c(0x0969da),
                    c(0xbc4c00),
                    c(0xbf3989),
                    c(0x9a6700),
                    c(0x1b7c83),
                    c(0xc4432b),
                ],
            }
        }
    }

    fn node(&self, g: &RepoGraph, row: usize) -> Hsla {
        let node = &g.layout.nodes[row];
        match node.class {
            Class::Unpushed => self.unpushed,
            Class::Unpulled => self.unpulled,
            Class::Trunk => self.trunk,
            Class::Merged => self.merged,
            Class::Branch => {
                let i = match node.branch {
                    Some(b) => graph::hue(&g.layout.branches[b], self.hues.len()),
                    None => node.lane % self.hues.len(),
                };
                self.hues[i]
            }
        }
    }
}

/// Px from the strip's centre line to a lane: HEAD's lane on the line,
/// the rest alternating above and below it, the overflow on the edge.
fn lane_offset(lane: usize) -> f32 {
    let lane = lane.min(LANES - 1);
    let step = lane.div_ceil(2) as f32 * LANE;
    if lane % 2 == 1 { -step } else { step }
}

/// Paints the visible part of a strip; returns where each painted dot is
/// and its row, for hit-testing.
fn paint_strip(
    bounds: Bounds<Pixels>,
    g: &RepoGraph,
    scroll: f32,
    hovered: Option<usize>,
    dirty: bool,
    colors: &Colors,
    window: &mut Window,
) -> Vec<(Point<Pixels>, usize)> {
    let rows = g.layout.nodes.len();
    let (left, right) = (f32::from(bounds.left()), f32::from(bounds.right()));
    // HEAD's x, one step further in when the work-in-progress dot leads.
    let head_x = right - PAD - if dirty { STEP } else { 0.0 } + scroll;
    let cy = f32::from(bounds.center().y);
    let x = |row: f32| head_x - row * STEP;
    let y = |lane: f32| cy + lane_offset(lane.round() as usize);
    let at = |pos: graph::Pos| point(px(x(pos.row)), px(y(pos.lane)));
    let folded = |lane: f32| lane.round() as usize >= LANES;

    // Rows whose dots are in view, one step of slack each side.
    let lo = (((head_x - right) / STEP).floor() - 1.0).max(0.0) as usize;
    let hi = ((((head_x - left) / STEP).ceil() + 1.0).max(0.0) as usize).min(rows);
    if lo >= hi && rows > 0 && lo >= rows {
        return Vec::new();
    }

    for e in &g.layout.edges {
        let end = e.parent.map_or(rows, |p| p.0);
        if end < lo || e.child.0 > hi {
            continue;
        }
        let base = if e.first {
            colors.node(g, e.child.0)
        } else {
            match e.parent {
                Some((row, _)) => colors.node(g, row),
                None => colors.node(g, e.child.0),
            }
        };
        for seg in e.segments(rows) {
            match seg {
                Seg::Straight(a, b) => {
                    let color = if folded(a.lane) {
                        base.opacity(0.35)
                    } else {
                        base.opacity(0.8)
                    };
                    let (x0, x1) = (x(b.row), x(a.row));
                    window.paint_quad(fill(
                        Bounds::new(
                            point(px(x0), px(y(a.lane) - STROKE / 2.0)),
                            size(px(x1 - x0), px(STROKE)),
                        ),
                        color,
                    ));
                }
                Seg::Curve(a, b) => {
                    let color = if folded(a.lane) || folded(b.lane) {
                        base.opacity(0.35)
                    } else {
                        base.opacity(0.8)
                    };
                    let (pa, pb) = (at(a), at(b));
                    let [_, c1, c2, _] = graph::noodle(
                        (f32::from(pa.x), f32::from(pa.y)),
                        (f32::from(pb.x), f32::from(pb.y)),
                        (-1.0, 0.0),
                    );
                    let mut path = PathBuilder::stroke(px(STROKE));
                    path.move_to(pa);
                    path.cubic_bezier_to(pb, point(px(c1.0), px(c1.1)), point(px(c2.0), px(c2.1)));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                }
            }
        }
    }

    if dirty {
        let center = point(px(head_x + STEP), px(cy));
        let r = DOT + 0.5;
        window.paint_quad(
            outline(circle(center, r), colors.unpushed, BorderStyle::Dashed).corner_radii(px(r)),
        );
        // Joined to HEAD by a short dashed line.
        let mut path = PathBuilder::stroke(px(1.0)).dash_array(&[px(2.0), px(2.0)]);
        path.move_to(point(px(head_x + DOT), px(cy)));
        path.line_to(point(px(head_x + STEP - r), px(cy)));
        if let Ok(path) = path.build() {
            window.paint_path(path, colors.unpushed.opacity(0.8));
        }
    }

    let mut dots = Vec::with_capacity(hi.saturating_sub(lo));
    for row in lo..hi {
        let node = &g.layout.nodes[row];
        let center = point(px(x(row as f32)), px(y(node.lane as f32)));
        dots.push((center, row));
        let color = colors.node(g, row);
        let color = if node.lane >= LANES {
            color.opacity(0.5)
        } else {
            color
        };
        let id = &g.commits[row].id;
        let head = g.ctx.refs.is_head(id);
        let r = if hovered == Some(row) { DOT_HOVER } else { DOT };
        if head {
            let glow = r + 3.0;
            window.paint_quad(
                fill(circle(center, glow), colors.head.opacity(0.22)).corner_radii(px(glow)),
            );
            window.paint_quad(fill(circle(center, r), colors.head).corner_radii(px(r)));
        } else if node.class == Class::Unpulled || node.merge {
            // Merges and not-yet-pulled commits are rings; a merge's is smaller.
            let r = if node.merge { r - 0.5 } else { r };
            window.paint_quad(
                outline(circle(center, r), color, BorderStyle::Solid)
                    .corner_radii(px(r))
                    .border_widths(px(1.5)),
            );
        } else {
            window.paint_quad(fill(circle(center, r), color).corner_radii(px(r)));
        }
        if g.ctx.refs.at(id).iter().any(|r| matches!(r, Ref::Tag(_))) {
            diamond(point(center.x, center.y - px(r + 4.0)), 2.5, color, window);
        }
        if g.stashes_at(row).next().is_some() {
            let s = 2.5;
            let at = point(center.x, center.y + px(r + 5.0));
            window.paint_quad(outline(
                Bounds::new(
                    point(at.x - px(s), at.y - px(s)),
                    size(px(2.0 * s), px(2.0 * s)),
                ),
                colors.stash,
                BorderStyle::Solid,
            ));
        }
    }
    dots
}

fn circle(center: Point<Pixels>, r: f32) -> Bounds<Pixels> {
    Bounds::new(
        point(center.x - px(r), center.y - px(r)),
        size(px(2.0 * r), px(2.0 * r)),
    )
}

fn diamond(c: Point<Pixels>, r: f32, color: Hsla, window: &mut Window) {
    let mut path = PathBuilder::fill();
    path.move_to(point(c.x, c.y - px(r)));
    path.line_to(point(c.x + px(r), c.y));
    path.line_to(point(c.x, c.y + px(r)));
    path.line_to(point(c.x - px(r), c.y));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// Fades the tail into the background at the strip's left edge.
fn fade(bounds: Bounds<Pixels>, bg: Hsla, window: &mut Window) {
    let w = FADE.min(f32::from(bounds.size.width));
    window.paint_quad(fill(
        Bounds::new(bounds.origin, size(px(w), bounds.size.height)),
        linear_gradient(
            90.,
            linear_color_stop(bg, 0.),
            linear_color_stop(bg.opacity(0.), 1.),
        ),
    ));
}
