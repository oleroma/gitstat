//! The timeline strips: each repo's history as a row of dots joined by
//! noodles, newest at the right next to the repo's name, the tail running
//! off the left edge.
//!
//! `Timelines` owns every repo's loaded graph and loads it off the UI
//! thread, only once its strip is on screen: when the panel opens or the
//! row scrolls into view. A check of a repo marks its strip stale, and a
//! stale strip reloads the next time it is drawn. The strip itself is a
//! canvas that paints only the dots and noodles in view and hit-tests the
//! mouse against the dots it painted.

mod column;
pub(crate) mod ink;
mod turn;

pub use column::{COLUMN_WIDTH, row_lines};
pub use turn::{Turning, paint_turning, turned};

use crate::git::RepoStatus;
use crate::graph::{self, Class, PrColors, PrState, Seg};
use crate::history::{self, Ref, Stat};
use crate::repo_graph::{More, RepoGraph};
use crate::state::AppState;
use crate::view::Palette;
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, BorderStyle, Bounds, ClipboardItem,
    ContentMask, Context, Corner, DispatchPhase, Entity, HitboxBehavior, Hsla, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, PathBuilder, Pixels, Point,
    ScrollWheelEvent, Styled, Window, anchored, canvas, deferred, div, fill, linear_color_stop,
    linear_gradient, outline, point, prelude::*, px, rgb, size,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Px between commits along a strip.
const STEP: f32 = 14.0;
/// Px between lanes across a strip.
const LANE: f32 = 5.0;
/// Lanes a strip shows; the rest fold into a dimmed band at its edge. The
/// outermost sit `2 * LANE` from the centre line, so a hovered dot's glow
/// and a tag's diamond above it stay inside the strip.
const LANES: usize = 5;
/// Px a strip may paint past its top and bottom, into its neighbours'
/// empty margins, so nothing near its edge is cut flat.
pub const BLEED: f32 = 6.0;
const DOT: f32 = 3.0;
const DOT_HOVER: f32 = 4.5;
const STROKE: f32 = 1.5;
/// How wide the band under another worktree's line is.
const BAND: f32 = 7.0;
/// Px from the strip's right edge to HEAD.
const PAD: f32 = 12.0;
/// The tail fades out over this many px at the left edge.
const FADE: f32 = 24.0;
/// A dot is hit within this many px.
const HIT: f32 = 7.0;
/// Load the next page while fewer than this many px of history are left
/// past the left edge.
const PRELOAD: f32 = 1200.0;
/// The mouse rests on a dot this long before its card shows, so sweeping
/// across a strip doesn't flash cards.
const CARD_DELAY: Duration = Duration::from_millis(150);
/// How long a dot shows it was copied.
const COPIED_FOR: Duration = Duration::from_millis(900);
/// Px between a hovered dot and its card.
const CARD_GAP: f32 = 8.0;
/// A wheel step glides the strip to its new place over this long.
const GLIDE: Duration = Duration::from_millis(140);
/// A dot grows into its hover size, and back, over this long.
const GROW: Duration = Duration::from_millis(110);
/// A card fades in over this long.
const CARD_FADE: Duration = Duration::from_millis(120);
/// How long a failed page of older history waits before it is asked again.
const MORE_RETRY: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Slot {
    graph: Option<Arc<RepoGraph>>,
    error: Option<String>,
    /// The load under way, so a slower, older load can't land last.
    generation: u64,
    /// The repo's check count the graph, or the load under way, reflects;
    /// `None` until first loaded.
    checks: Option<u64>,
    /// A reload is queued or running.
    loading: bool,
    loading_more: bool,
    /// When a page of older history last failed: the next waits a while.
    more_failed: Option<Instant>,
    /// Px the strip is scrolled into the past from the newest row; HEAD's
    /// row, `RepoGraph::home`, is where it rests.
    scroll: f32,
    /// Where the strip was shown when the wheel last moved it, and when:
    /// it glides from there to `scroll`.
    glide: Option<(f32, Instant, Duration)>,
    /// Big mode's column scrolls on its own, apart from the strip: the row
    /// at its top, 0 for the newest.
    top: f32,
    /// Like `glide`, for `top`.
    top_glide: Option<(f32, Instant, Duration)>,
}

/// Where a glide from `glide` to `to` is at `now`, and whether it is still
/// going.
fn glided(to: f32, glide: Option<(f32, Instant, Duration)>, now: Instant) -> (f32, bool) {
    match glide {
        Some((from, start, over)) => {
            let t = progress(start, now, over);
            (from + (to - from) * t, t < 1.0)
        }
        None => (to, false),
    }
}

impl Slot {
    /// The scroll to draw at `now`, part way along a glide, and whether
    /// the glide is still going.
    fn shown_scroll(&self, now: Instant) -> (f32, bool) {
        glided(self.scroll, self.glide, now)
    }

    /// Moves the scroll to `to`, gliding from wherever it shows now.
    fn glide_to(&mut self, to: f32, now: Instant) {
        self.glide_over(to, now, GLIDE);
    }

    /// Moves the scroll to `to` over `over`, gliding from wherever it
    /// shows now.
    fn glide_over(&mut self, to: f32, now: Instant, over: Duration) {
        let (shown, _) = self.shown_scroll(now);
        self.glide = Some((shown, now, over));
        self.scroll = to;
    }

    /// The column's top row to draw at `now`, and whether it is gliding.
    fn shown_top(&self, now: Instant) -> (f32, bool) {
        glided(self.top, self.top_glide, now)
    }

    /// Moves the column's top to `to`, gliding from wherever it shows now.
    fn glide_top_to(&mut self, to: f32, now: Instant) {
        self.glide_top_over(to, now, GLIDE);
    }

    fn glide_top_over(&mut self, to: f32, now: Instant, over: Duration) {
        let (shown, _) = self.shown_top(now);
        self.top_glide = Some((shown, now, over));
        self.top = to;
    }
}

/// How far an `over`-long transition begun at `start` is at `now`, eased
/// out: 0 to 1, fast at first and settling gently.
fn progress(start: Instant, now: Instant, over: Duration) -> f32 {
    let t = (now.saturating_duration_since(start).as_secs_f32() / over.as_secs_f32()).min(1.0);
    1.0 - (1.0 - t).powi(3)
}

/// How far each dot has grown toward its hover size: the hovered one up,
/// the one the mouse just left down.
#[derive(Clone, Copy, Default)]
struct Grow {
    hovered: Option<(Dot, f32)>,
    left: Option<(Dot, f32)>,
}

impl Grow {
    /// 0 at rest, 1 at full hover size.
    fn of(&self, dot: Dot) -> f32 {
        match (self.hovered, self.left) {
            (Some((d, k)), _) if d == dot => k,
            (_, Some((d, k))) if d == dot => k,
            _ => 0.0,
        }
    }

    fn going(&self) -> bool {
        let moving = |g: Option<(Dot, f32)>| g.is_some_and(|(_, k)| k > 0.0 && k < 1.0);
        moving(self.hovered) || moving(self.left)
    }

    /// The radius of a dot between `rest` and `hover`.
    fn radius(&self, dot: Dot, rest: f32, hover: f32) -> f32 {
        rest + (hover - rest) * self.of(dot)
    }
}

/// A dot on a strip: a commit's row, or the uncommitted changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dot {
    Commit(usize),
    Wip,
}

/// The dot under the mouse.
#[derive(Clone, Debug, PartialEq)]
pub struct Hover {
    pub repo: PathBuf,
    pub dot: Dot,
    /// Window position of the dot, where the card is anchored.
    pub at: Point<Pixels>,
    /// The card goes above the dot, as it sits in the window's lower half.
    pub above: bool,
    /// The tree was dirty: a change moves every dot a step, and the WIP dot
    /// comes or goes, so the hover holds only while it doesn't.
    pub dirty: bool,
    /// The window it is in: only that one shows its card.
    pub window: Option<isize>,
}

/// What a queued load reads.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Load {
    Reload,
    More,
}

/// What a load read.
enum Loaded {
    Graph(Option<Box<RepoGraph>>),
    Page(More),
}

pub struct Timelines {
    state: Entity<AppState>,
    slots: HashMap<PathBuf, Slot>,
    /// Loads waiting for one of the `git::MAX_PARALLEL` places, so a
    /// panel of many repos doesn't tie up every background thread, nor a
    /// Shift+wheel paging them all at once.
    queue: VecDeque<(PathBuf, Load)>,
    running: usize,
    /// Only ever grows, so a load for a slot since dropped and made again
    /// can't pass for the new slot's.
    generation: u64,
    hover: Option<Hover>,
    /// When `hover` began, so its dot grows.
    hover_since: Instant,
    /// The dot the mouse last left, and when, so it shrinks back.
    left: Option<(PathBuf, Dot, Instant)>,
    /// The hover has lasted `CARD_DELAY`, so its card shows.
    card: bool,
    /// Bumped each time a card comes up, so it fades in once rather than
    /// on every dot the mouse slides to.
    card_shown: u64,
    /// Bumped on every hover change, so an older card timer does nothing.
    hover_generation: u64,
    /// The commit whose id was just copied, flashed until its timer ends.
    copied: Option<(PathBuf, usize)>,
    copied_generation: u64,
    /// Line stats by commit id, read on first hover.
    stats: HashMap<String, Stat>,
    stats_pending: HashSet<String>,
    /// A repo whose column big mode should bring into view.
    focus: Option<PathBuf>,
    /// Each repo's GitHub PRs by branch, given to its graph as it lands.
    prs: HashMap<PathBuf, Arc<PrColors>>,
    /// Big mode columns whose header shows its details, not just the
    /// one-line summary. Kept while gitstat runs.
    expanded: HashSet<PathBuf>,
    /// Rows past home that big mode's columns were last scrolled to
    /// together; a column with fewer rows stops at its last.
    table_top: f32,
    /// Full commit messages by id, read on first hover.
    messages: HashMap<String, Arc<str>>,
    messages_pending: HashSet<String>,
}

/// A repo's strip in the panel ([`Timelines::strip`]).
pub struct Strip {
    /// Its tree, as big as the row's graph box.
    pub graph: AnyElement,
    /// Where the mouse finds its label, as big as that.
    pub label: AnyElement,
    /// Its label's second line: what is checked out, and how it stands.
    pub checkout: String,
    pub status: String,
}

/// How many commits' line stats and messages are kept: past that, the
/// cache starts over, and the rows in view ask again.
const MAX_STATS: usize = 50_000;
const MAX_MESSAGES: usize = 2_000;

impl Timelines {
    fn keep_stat(&mut self, id: String, stat: Stat) {
        if self.stats.len() >= MAX_STATS {
            self.stats.clear();
        }
        self.stats.insert(id, stat);
    }

    fn keep_message(&mut self, id: String, message: Arc<str>) {
        if self.messages.len() >= MAX_MESSAGES {
            self.messages.clear();
        }
        self.messages.insert(id, message);
    }

    pub fn new(state: &Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(state, |this, state, cx| this.sync(&state, cx))
            .detach();
        Self {
            state: state.clone(),
            slots: HashMap::new(),
            queue: VecDeque::new(),
            running: 0,
            generation: 0,
            hover: None,
            hover_since: Instant::now(),
            left: None,
            card: false,
            card_shown: 0,
            hover_generation: 0,
            copied: None,
            copied_generation: 0,
            stats: HashMap::new(),
            stats_pending: HashSet::new(),
            focus: None,
            prs: HashMap::new(),
            expanded: HashSet::new(),
            table_top: 0.0,
            messages: HashMap::new(),
            messages_pending: HashSet::new(),
        }
    }

    /// A repo's loaded graph, for the flip.
    pub fn graph_of(&self, path: &Path) -> Option<Arc<RepoGraph>> {
        self.slots.get(path)?.graph.clone()
    }

    /// Whether `path`'s column header shows its details.
    pub fn header_open(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    pub fn toggle_header(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        cx.notify();
    }

    /// The repo's GitHub page, once its graph has loaded.
    pub fn web(&self, path: &Path) -> Option<String> {
        self.slots.get(path)?.graph.as_ref()?.info.web.clone()
    }

    /// Sets the PR states `path`'s graph colours its branches by. Cheap
    /// when nothing changed, so it can be called on every PR refresh.
    pub fn set_prs(&mut self, path: &Path, prs: PrColors, cx: &mut Context<Self>) {
        if self.prs.get(path).map(|p| p.as_ref()) == Some(&prs)
            || (prs.is_empty() && !self.prs.contains_key(path))
        {
            return;
        }
        let prs = Arc::new(prs);
        self.prs.insert(path.to_path_buf(), prs.clone());
        if let Some(g) = self.slots.get_mut(path).and_then(|s| s.graph.as_mut()) {
            Arc::make_mut(g).prs = prs;
            cx.notify();
        }
    }

    /// Forgets the repos gone from the list, and loads the trees of the
    /// rest that are new or checked since.
    fn sync(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let s = state.read(cx);
        let listed: HashSet<&Path> = s
            .statuses
            .iter()
            .chain(&s.remote_statuses)
            .map(|s| s.path.as_path())
            .collect();
        self.slots.retain(|p, _| listed.contains(p.as_path()));
        self.prs.retain(|p, _| listed.contains(p.as_path()));
        self.queue.retain(|(p, _)| listed.contains(p.as_path()));
        // Every tree loads, and reloads after each check, in the
        // background, so the panel and the window open with them all
        // drawn. GitHub-only repos only while their section is open: their
        // history is fetched from GitHub.
        let mut wanted: Vec<PathBuf> = s.statuses.iter().map(|s| s.path.clone()).collect();
        if !s.folded(crate::config::Section::GithubOnly) {
            wanted.extend(s.remote_statuses.iter().map(|s| s.path.clone()));
        }
        for path in wanted {
            self.shown(&path, cx);
        }
    }

    /// `path`'s strip is on screen: loads it if it never was, or reloads it
    /// if the repo was checked since.
    fn shown(&mut self, path: &Path, cx: &mut Context<Self>) {
        let checks = self.state.read(cx).checks(path);
        let slot = self.slots.entry(path.to_path_buf()).or_default();
        // A load under way finishes first; its redraw comes back here.
        if slot.loading || slot.checks == Some(checks) {
            return;
        }
        slot.checks = Some(checks);
        self.reload(path.to_path_buf(), cx);
    }

    fn reload(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.generation += 1;
        let slot = self.slots.entry(path.clone()).or_default();
        slot.generation = self.generation;
        slot.loading = true;
        slot.loading_more = false;
        // A page queued for the old graph would land on the new one.
        self.queue
            .retain(|(p, load)| !(*p == path && *load == Load::More));
        let entry = (path, Load::Reload);
        if !self.queue.contains(&entry) {
            self.queue.push_back(entry);
        }
        self.start_loads(cx);
    }

    /// Starts queued loads while there is room.
    fn start_loads(&mut self, cx: &mut Context<Self>) {
        while self.running < crate::git::MAX_PARALLEL {
            let Some((path, load)) = self.queue.pop_front() else {
                return;
            };
            let Some(slot) = self.slots.get_mut(&path) else {
                continue;
            };
            let generation = slot.generation;
            let prev = slot.graph.clone();
            let repo = path.clone();
            let task = match (load, prev) {
                // A GitHub repo with no clone: its history comes from GitHub,
                // all of it at once.
                (Load::Reload, _) if crate::github::remote_slug(&repo).is_some() => cx
                    .background_spawn(async move {
                        let slug = crate::github::remote_slug(&repo).unwrap_or_default();
                        let h = crate::github::remote_history(slug)?;
                        let web = format!("https://github.com/{slug}");
                        Ok(Loaded::Graph(Some(Box::new(RepoGraph::from_remote(
                            h, web,
                        )))))
                    }),
                (Load::Reload, prev) => cx.background_spawn(async move {
                    RepoGraph::load(&repo, prev.as_deref(), history::PAGE_SIZE)
                        .map(|g| Loaded::Graph(g.map(Box::new)))
                }),
                (Load::More, Some(graph)) => cx.background_spawn(async move {
                    graph.more(&repo, history::PAGE_SIZE).map(Loaded::Page)
                }),
                (Load::More, None) => {
                    slot.loading_more = false;
                    continue;
                }
            };
            self.running += 1;
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |this, cx| {
                    this.running -= 1;
                    this.start_loads(cx);
                    this.landed(path, generation, result, cx);
                })
                .ok();
            })
            .detach();
        }
    }

    fn landed(
        &mut self,
        path: PathBuf,
        generation: u64,
        result: Result<Loaded, String>,
        cx: &mut Context<Self>,
    ) {
        // Never under a flip, or the glide home before one: the trees it
        // turns stay as it planned them, and the new ones land after.
        if crate::flip::running(cx) {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    if !cx.update(|cx| crate::flip::running(cx)).unwrap_or(false) {
                        break;
                    }
                }
                this.update(cx, |this, cx| this.landed(path, generation, result, cx))
                    .ok();
            })
            .detach();
            return;
        }
        let Some(slot) = self.slots.get_mut(&path) else {
            return;
        };
        if slot.generation != generation {
            return;
        }
        match result {
            Ok(Loaded::Graph(Some(g))) => {
                slot.loading = false;
                // Rows shift under a reload; the view keeps its commit.
                let at = slot.scroll / STEP;
                // A first load starts at the newest commits.
                let row = match &slot.graph {
                    Some(old) => g.carry_scroll(old, at),
                    None => 0.0,
                };
                slot.scroll = row * STEP;
                slot.glide = None;
                slot.top = match &slot.graph {
                    Some(old) => column::carry_top(&g, old, slot.top),
                    None => 0.0,
                };
                slot.top_glide = None;
                let mut g = *g;
                if let Some(prs) = self.prs.get(&path) {
                    g.prs = prs.clone();
                }
                slot.graph = Some(Arc::new(g));
                slot.error = None;
                slot.more_failed = None;
                if self.hover.as_ref().is_some_and(|h| h.repo == path) {
                    self.unhover();
                }
            }
            Ok(Loaded::Graph(None)) => {
                slot.loading = false;
                slot.error = None;
            }
            // The last frame's paint has let go of the graph, so this
            // appends in place rather than copying it all.
            Ok(Loaded::Page(More::Page(commits, end))) => {
                slot.loading_more = false;
                if let Some(g) = &mut slot.graph {
                    Arc::make_mut(g).append(commits, end);
                }
            }
            Ok(Loaded::Page(More::Reload)) => self.reload(path, cx),
            // A repo with a graph keeps it, and its status label: a page
            // of older history that failed (or a reload) is no reason to
            // call the whole history unreadable.
            Err(_) if slot.graph.is_some() => {
                if slot.loading_more && !slot.loading {
                    slot.more_failed = Some(Instant::now());
                }
                slot.loading = false;
                slot.loading_more = false;
            }
            Err(e) => {
                slot.loading = false;
                slot.loading_more = false;
                slot.error = Some(e);
            }
        }
        cx.notify();
    }

    fn load_more(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get_mut(path) else {
            return;
        };
        let Some(graph) = &slot.graph else {
            return;
        };
        // A reload under way would land under the page, or the page on it.
        if slot.loading || slot.loading_more || graph.end {
            return;
        }
        if slot.more_failed.is_some_and(|at| at.elapsed() < MORE_RETRY) {
            return;
        }
        slot.loading_more = true;
        self.queue.push_back((path.to_path_buf(), Load::More));
        self.start_loads(cx);
    }

    /// Scrolls one strip, or every strip with `all`, by `dx` px into the
    /// past (negative: toward HEAD), loading more history near the end.
    fn scroll(&mut self, path: &Path, dx: f32, all: bool, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut more = Vec::new();
        for (p, slot) in self.slots.iter_mut() {
            if !all && p != path {
                continue;
            }
            let Some(g) = &slot.graph else { continue };
            let rows = g.commits.len() as f32;
            let max = ((rows - 1.0) * STEP).max(0.0);
            let to = (slot.scroll + dx).clamp(0.0, max);
            if !g.end && rows * STEP - to < PRELOAD {
                more.push(p.clone());
            }
            slot.glide_to(to, now);
        }
        for p in more {
            self.load_more(&p, cx);
        }
        self.unhover();
        cx.notify();
    }

    /// No dot is hovered any more, and its card goes: the view under the
    /// mouse is moving away.
    pub fn drop_hover(&mut self, cx: &mut Context<Self>) {
        if self.hover.is_some() {
            self.unhover();
            cx.notify();
        }
    }

    /// No dot of `window` is hovered any more: the mouse left it.
    pub fn drop_hover_in(&mut self, window: Option<isize>, cx: &mut Context<Self>) {
        if self.hover.as_ref().is_some_and(|h| h.window == window) {
            self.unhover();
            cx.notify();
        }
    }

    fn unhover(&mut self) {
        self.leave(Instant::now());
        self.hover = None;
        self.card = false;
        self.hover_generation += 1;
    }

    /// Every strip and column back at its newest commits, as the panel or
    /// the big window opens: neither ever opens scrolled into the past.
    pub fn show_newest(&mut self, cx: &mut Context<Self>) {
        for slot in self.slots.values_mut() {
            slot.scroll = 0.0;
            slot.glide = None;
            slot.top = 0.0;
            slot.top_glide = None;
        }
        self.table_top = 0.0;
        self.unhover();
        cx.notify();
    }

    /// Whether any strip or column is gliding now.
    pub fn gliding(&self) -> bool {
        let now = Instant::now();
        self.slots
            .values()
            .any(|s| s.shown_scroll(now).1 || s.shown_top(now).1)
    }

    /// Every strip and column scrolls back to its newest commits over
    /// `over`, as the flip starts from them: true when any had to, false
    /// when all are there already.
    pub fn glide_newest(&mut self, over: Duration, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let mut moving = false;
        for slot in self.slots.values_mut() {
            if slot.shown_scroll(now).0.abs() > 0.5 {
                slot.glide_over(0.0, now, over);
                moving = true;
            } else {
                slot.scroll = 0.0;
                slot.glide = None;
            }
            if slot.shown_top(now).0.abs() > 0.01 {
                slot.glide_top_over(0.0, now, over);
                moving = true;
            } else {
                slot.top = 0.0;
                slot.top_glide = None;
            }
        }
        self.table_top = 0.0;
        if moving {
            self.unhover();
            cx.notify();
        }
        moving
    }

    /// Px `path`'s strip is scrolled into the past, as it will rest: for
    /// the self-test.
    pub fn scrolled(&self, path: &Path) -> Option<f32> {
        self.slots.get(path).map(|s| s.scroll)
    }

    pub fn scroll_home(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(slot) = self.slots.get_mut(path) {
            let home = slot.graph.as_ref().map_or(0.0, |g| g.home() * STEP);
            slot.glide_to(home, Instant::now());
            cx.notify();
        }
    }

    /// The hovered dot, if any, starts shrinking back.
    fn leave(&mut self, now: Instant) {
        if let Some(h) = &self.hover {
            self.left = Some((h.repo.clone(), h.dot, now));
        }
    }

    /// How far `path`'s dots have grown into their hover size at `now`.
    fn grow(&self, path: &Path, dirty: bool, now: Instant) -> Grow {
        Grow {
            hovered: self
                .hover
                .as_ref()
                .filter(|h| h.repo == path && h.dirty == dirty)
                .map(|h| (h.dot, progress(self.hover_since, now, GROW))),
            left: self
                .left
                .as_ref()
                .filter(|(p, ..)| p == path)
                .map(|&(_, dot, at)| (dot, 1.0 - progress(at, now, GROW)))
                .filter(|&(_, k)| k > 0.0),
        }
    }

    /// The mouse moved to `pos` over `path`'s strip, onto a dot or off them.
    fn hover_at(
        &mut self,
        path: &Path,
        pos: Point<Pixels>,
        hit: Option<(Dot, Point<Pixels>, bool, bool)>,
        window: Option<isize>,
        cx: &mut Context<Self>,
    ) {
        let new = match hit {
            Some((dot, at, above, dirty)) => Some(Hover {
                repo: path.to_path_buf(),
                dot,
                at,
                above,
                dirty,
                window,
            }),
            // Another strip's hover is not ours to clear.
            None if self.hover.as_ref().is_some_and(|h| h.repo != path) => return,
            // A card snapped over its dot, in a short window, hides the dot
            // from the strip; dropping the hover would hide the card and
            // bring it back on every move.
            None if self.card
                && self.hover.as_ref().is_some_and(|h| {
                    f32::from(h.at.x - pos.x).hypot(f32::from(h.at.y - pos.y)) <= HIT
                }) =>
            {
                return;
            }
            None => None,
        };
        if new == self.hover {
            return;
        }
        // Sliding from dot to dot keeps a card already up; a fresh hover
        // waits for the mouse to settle.
        let sliding = self.card && self.hover.is_some() && new.is_some();
        // The line counts only show on the card, so a mouse passing over
        // dots on its way somewhere doesn't start a git call for each.
        if sliding {
            self.request_hovered_stat(new.as_ref(), cx);
        }
        let now = Instant::now();
        self.leave(now);
        self.hover_since = now;
        self.hover = new;
        self.hover_generation += 1;
        self.card = sliding;
        if self.hover.is_some() && !sliding {
            let generation = self.hover_generation;
            let timer = cx.background_executor().timer(CARD_DELAY);
            cx.spawn(async move |this, cx| {
                timer.await;
                this.update(cx, |this, cx| {
                    if this.hover_generation == generation {
                        this.card = true;
                        this.card_shown += 1;
                        let hover = this.hover.clone();
                        this.request_hovered_stat(hover.as_ref(), cx);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// Flashes `row`'s dot, its id now on the clipboard.
    fn flash_copied(&mut self, path: &Path, row: usize, cx: &mut Context<Self>) {
        self.copied = Some((path.to_path_buf(), row));
        self.copied_generation += 1;
        let generation = self.copied_generation;
        let timer = cx.background_executor().timer(COPIED_FOR);
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| {
                if this.copied_generation == generation {
                    this.copied = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn request_hovered_stat(&mut self, hover: Option<&Hover>, cx: &mut Context<Self>) {
        if let Some(Hover {
            dot: Dot::Commit(row),
            repo,
            ..
        }) = hover
        {
            self.request_stat(repo, *row, cx);
            self.request_message(repo, *row, cx);
        }
    }

    /// Reads the whole message of the commit at `row`, for its card. A
    /// GitHub-only repo's history came with its messages.
    fn request_message(&mut self, path: &Path, row: usize, cx: &mut Context<Self>) {
        if crate::github::remote_slug(path).is_some() {
            return;
        }
        let Some(id) = self.commit_id(path, row) else {
            return;
        };
        if self.messages.contains_key(&id) || !self.messages_pending.insert(id.clone()) {
            return;
        }
        let repo = path.to_path_buf();
        let key = id.clone();
        let task = cx.background_spawn(async move { history::message(&repo, &key) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.messages_pending.remove(&id);
                if let Ok(message) = result {
                    this.keep_message(id, message.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn request_stat(&mut self, path: &Path, row: usize, cx: &mut Context<Self>) {
        // A GitHub-only repo has no clone to ask.
        if crate::github::remote_slug(path).is_some() {
            return;
        }
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
                    this.keep_stat(id, stat);
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

    /// One repo's strip in the panel: what lies under its row's parts (its
    /// tree, and where the mouse finds its label), and its label's words
    /// for the scene to draw. `float`, it is the floating view's: that
    /// window is no bigger than the view, so it says what the mouse rests
    /// on itself, and a click opens the big window through the panel.
    #[allow(clippy::too_many_arguments)]
    pub fn strip(
        this: &Entity<Timelines>,
        index: usize,
        status: &RepoStatus,
        p: &Palette,
        dark: bool,
        tree: bool,
        float: bool,
        cx: &App,
    ) -> Strip {
        let t = this.read(cx);
        let path = status.path.clone();
        let slot = t.slots.get(&path);
        let graph = slot.and_then(|s| s.graph.clone());
        let now = Instant::now();
        let (scroll, gliding) = slot.map_or((0.0, false), |s| s.shown_scroll(now));
        let error = slot.and_then(|s| s.error.clone());
        let dirty = status.dirty;
        let grow = t.grow(&path, dirty, now);
        let moving = gliding || grow.going();
        let copied = t
            .copied
            .as_ref()
            .filter(|(p, _)| *p == path)
            .map(|&(_, row)| row);
        let colors = Colors::new(dark, p);
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
        let (shown, shown_path) = (this.clone(), path.clone());
        let surface = p.surface;
        let painter = canvas(
            // Only strips inside the list's scrolled viewport load.
            move |bounds, window, cx| {
                if window.content_mask().bounds.intersects(&bounds) {
                    shown.update(cx, |t, cx| t.shown(&shown_path, cx));
                }
                // The mouse handlers act only while this hitbox is hovered,
                // so the hover card, which occludes it, keeps its clicks.
                window.insert_hitbox(bounds, HitboxBehavior::Normal)
            },
            move |bounds, hitbox, window, _cx| {
                let Some(g) = graph else { return };
                if moving {
                    window.request_animation_frame();
                }
                let mask = Bounds::new(
                    point(bounds.left(), bounds.top() - px(BLEED)),
                    size(bounds.size.width, bounds.size.height + px(2.0 * BLEED)),
                );
                let dots = window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                    paint_strip(
                        bounds,
                        &g,
                        scroll,
                        grow,
                        copied,
                        dirty,
                        (tree, true),
                        &colors,
                        window,
                    )
                });
                fade(bounds, surface, window);
                if !tree {
                    return;
                }

                // Only dots that show can be hovered: not ones past the
                // strip's ends, nor ones the list clips away.
                let shown = window.content_mask().bounds.intersect(&mask);
                let dots: Vec<_> = dots
                    .into_iter()
                    .filter(|(at, _)| shown.contains(at))
                    .collect();
                let (e1, e2) = (entity.clone(), entity);
                let (path1, path2) = (paint_path.clone(), paint_path);
                let dots = Arc::new(dots);
                let dots2 = dots.clone();
                // The nearest dot within reach.
                let nearest = |dots: &[(Point<Pixels>, Dot)], pos: Point<Pixels>| {
                    dots.iter()
                        .map(|&(at, dot)| {
                            let d = f32::from(at.x - pos.x).hypot(f32::from(at.y - pos.y));
                            (dot, at, d)
                        })
                        .filter(|&(_, _, d)| d <= HIT)
                        .min_by(|a, b| a.2.total_cmp(&b.2))
                        .map(|(dot, at, _)| (dot, at))
                };
                let hitbox2 = hitbox.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    // Not while the panel slides away under the mouse.
                    let found = (hitbox.is_hovered(window) && !crate::panel::is_leaving(cx))
                        .then(|| nearest(&dots, e.position))
                        .flatten()
                        .map(|(dot, at)| {
                            let above = at.y > window.viewport_size().height / 2.0;
                            (dot, at, above, dirty)
                        });
                    // On the floating view's card, over the tree: the
                    // card holds.
                    if crate::float_tip::holds_mouse(cx) {
                        return;
                    }
                    let win = crate::win::hwnd(window);
                    e1.update(cx, |t, cx| t.hover_at(&path1, e.position, found, win, cx));
                });
                window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || e.button != MouseButton::Left
                        || !hitbox2.is_hovered(window)
                    {
                        return;
                    }
                    if e.click_count >= 2 {
                        e2.update(cx, |t, cx| t.scroll_home(&path2, cx));
                        return;
                    }
                    let Some((Dot::Commit(row), _)) = nearest(&dots2, e.position) else {
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
        .size_full();

        let entity = this.clone();
        let wheel_path = path.clone();
        // A plain wheel scrolls the panel's list, anywhere on the strip.
        // Shift+wheel, which Windows sends sideways, scrolls every strip
        // through time; a sideways swipe or tilt scrolls this one. Up or
        // left goes back in time.
        let wheel = std::rc::Rc::new(move |e: &ScrollWheelEvent, cx: &mut App| {
            let d = e.delta.pixel_delta(px(STEP * 3.0));
            let (x, y) = (f32::from(d.x), f32::from(d.y));
            let (dx, all) = if e.modifiers.shift {
                (if x != 0.0 { x } else { y }, true)
            } else if x.abs() > y.abs() {
                (x, false)
            } else if float {
                // The floating view's trees each go their own way, with
                // the plain wheel too.
                (y, false)
            } else {
                // The strips move under the mouse, and no mouse move
                // follows to say so: the hover and its card go.
                entity.update(cx, |t, cx| {
                    if t.hover.is_some() {
                        t.unhover();
                        cx.notify();
                    }
                });
                return;
            };
            // The list would read a sideways delta as a vertical one.
            cx.stop_propagation();
            entity.update(cx, |t, cx| t.scroll(&wheel_path, dx, all, cx));
        });
        let on_graph = wheel.clone();
        let graph_slot = div()
            .id(("strip-graph", index))
            .size_full()
            .on_scroll_wheel(move |e: &ScrollWheelEvent, _, cx| on_graph(e, cx))
            .child(painter)
            .into_any_element();

        let this_for_label = this.clone();
        let open_path = path.clone();
        let status_text = match &error {
            Some(_) if status.error.is_none() => "history unreadable".to_string(),
            _ => status.label(),
        };
        // Its icon and words are the scene's parts, drawn over this: here
        // only where the mouse finds them, and what it says when it rests.
        let mut lines = vec![(status.name.clone(), p.text)];
        if float {
            lines.push((checkout.clone(), p.muted));
        }
        lines.push((status_text.clone(), status_color));
        lines.extend(crate::view::status_details(status, p));
        // In the floating view the view itself handles an icon's clicks
        // (it is dragged by it too) and the wheel over it, which scrolls
        // its rows.
        let label = div()
            .id(("label", index))
            .size_full()
            .cursor_pointer()
            .hover(|s| s.bg(p.hover))
            .when(!float, |d| {
                let status = status.clone();
                d.on_scroll_wheel(move |e: &ScrollWheelEvent, _, cx| wheel(e, cx))
                    .on_mouse_down(gpui::MouseButton::Right, move |_, window, cx| {
                        cx.stop_propagation();
                        crate::repo_menu::show(&status, window, cx);
                    })
                    // Big mode, scrolled to this repo's column; the folder
                    // when the window is turned off.
                    .on_click(move |_, window, cx| {
                        let big = this_for_label.read(cx).state.read(cx).config.window;
                        if big {
                            crate::flip::open_window(window, Some(open_path.clone()), cx);
                        } else {
                            crate::panel::dismiss(window, cx);
                            crate::view::open_repo(&open_path, cx);
                        }
                    })
            })
            .when(!float, |d| d.tooltip(crate::view::tip(lines, p)))
            .into_any_element();
        Strip {
            graph: graph_slot,
            label,
            checkout,
            status: status_text,
        }
    }

    /// The card for the dot under the mouse, drawn above everything once
    /// the mouse has settled, beside the dot rather than over it.
    pub fn hover_card(
        &self,
        p: &Palette,
        dark: bool,
        window: Option<isize>,
        cx: &App,
    ) -> Option<AnyElement> {
        let (card, h) = self.card_for(p, dark, window, cx)?;
        let gap = px(DOT_HOVER + CARD_GAP);
        let x = h.at.x - px(12.);
        let (corner, y) = if h.above {
            (Corner::BottomLeft, h.at.y - gap)
        } else {
            (Corner::TopLeft, h.at.y + gap)
        };
        Some(
            deferred(
                anchored()
                    .anchor(corner)
                    .position(point(x, y))
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// How far from its dot a card sits, logical px: beside the dot's
    /// hover ring, and that far to the left of it.
    pub const CARD_OFFSET: (f32, f32) = (12.0, DOT_HOVER + CARD_GAP);

    /// Where in `window` the dot is whose card shows, once the mouse has
    /// settled on it.
    pub fn card_at(&self, window: Option<isize>) -> Option<Point<Pixels>> {
        self.hover
            .as_ref()
            .filter(|h| self.card && h.window == window)
            .map(|h| h.at)
    }

    /// The card for the dot under the mouse in `window`, unplaced, and
    /// that hover, once the mouse has settled on it.
    pub fn card_for(
        &self,
        p: &Palette,
        dark: bool,
        window: Option<isize>,
        cx: &App,
    ) -> Option<(AnyElement, Hover)> {
        let h = self
            .hover
            .as_ref()
            .filter(|h| self.card && h.window == window)?;
        // Gone with the dot when the tree turned clean or dirty under it.
        let state = self.state.read(cx);
        let dirty = state
            .statuses
            .iter()
            .chain(&state.remote_statuses)
            .find(|s| s.path == h.repo)?
            .dirty;
        if dirty != h.dirty {
            return None;
        }
        let g = self.slots.get(&h.repo)?.graph.as_ref()?;
        let colors = Colors::new(dark, p);
        let card = match h.dot {
            Dot::Commit(row) => self.commit_card(g, row, &h.repo, &colors, p)?,
            Dot::Wip => wip_card(g, &colors, p),
        };
        let card = div()
            .flex()
            .flex_col()
            .gap_1()
            .max_w(px(420.))
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(p.border)
            .bg(p.bg)
            .shadow_md()
            .text_xs()
            .text_color(p.text)
            // The strips under it don't see the mouse, so a click here
            // copies nothing.
            .occlude()
            .children(card)
            .with_animation(
                ("card", self.card_shown),
                Animation::new(CARD_FADE).with_easing(gpui::ease_out_quint()),
                |card, k| card.opacity(k),
            );
        Some((card.into_any_element(), h.clone()))
    }

    /// A commit card's lines.
    fn commit_card(
        &self,
        g: &RepoGraph,
        row: usize,
        repo: &Path,
        colors: &Colors,
        p: &Palette,
    ) -> Option<Vec<AnyElement>> {
        let c = g.commits.get(row)?;
        let node = g.layout.nodes.get(row)?;
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
            .chain(g.stashes_at(row).map(|s| s.name.clone()))
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
            Class::Branch => g
                .prs
                .of_node(node, &g.layout.branches)
                .map_or("", PrState::label),
        };
        let copied = self
            .copied
            .as_ref()
            .is_some_and(|(p, r)| p == repo && *r == row);
        // The whole message once read, else the subject until it is.
        let message = self
            .messages
            .get(&c.id)
            .map(|m| m.to_string())
            .or_else(|| (!c.message.is_empty()).then(|| c.message.clone()))
            .unwrap_or_else(|| c.subject.clone());
        let (subject, body) = message_parts(&message);
        let mut lines = vec![
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .font_family("Consolas")
                        .text_color(colors.node(g, row))
                        .child(c.short().to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(subject),
                )
                .when(copied, |d| {
                    d.child(div().flex_none().text_color(p.muted).child("copied"))
                })
                .into_any_element(),
        ];
        if !body.is_empty() {
            lines.push(
                div()
                    .flex()
                    .flex_col()
                    .pb_1()
                    .children(body.into_iter().map(|line| {
                        // A blank line between paragraphs keeps its height.
                        div().min_h(px(6.)).child(line).into_any_element()
                    }))
                    .into_any_element(),
            );
        }
        lines.extend([
            div()
                .text_color(p.muted)
                .child(format!(
                    "{} · {} ago · {} UTC",
                    c.author,
                    history::ago(now, c.time),
                    history::utc(c.time)
                ))
                .into_any_element(),
            div().text_color(p.muted).child(stat).into_any_element(),
        ]);
        if head || !what.is_empty() || !refs.is_empty() {
            let mut parts = Vec::new();
            if head {
                parts.push("HEAD".to_string());
            }
            parts.extend(refs);
            if !what.is_empty() {
                parts.push(what.to_string());
            }
            lines.push(
                div()
                    .text_color(p.muted)
                    .child(parts.join(" · "))
                    .into_any_element(),
            );
        }
        for b in g.bands_at(row) {
            let what = match &b.branch {
                Some(branch) => format!("Checked out in worktree {} on {branch}", b.name),
                None => format!("Checked out in worktree {}, detached", b.name),
            };
            lines.push(
                div()
                    .text_color(colors.node(g, row))
                    .child(what)
                    .into_any_element(),
            );
            lines.push(
                div()
                    .text_color(p.muted)
                    .truncate()
                    .child(b.path.display().to_string())
                    .into_any_element(),
            );
        }
        Some(lines)
    }
}

/// Lines of a message body a card shows at most; longer ones end in "…".
const BODY_LINES: usize = 24;
/// Characters of one body line a card shows at most.
const BODY_LINE_CHARS: usize = 400;

/// A message's subject, its first paragraph joined into one line as git
/// does, and its body's lines after the blank line, at most `BODY_LINES`.
fn message_parts(message: &str) -> (String, Vec<String>) {
    let message = message.trim();
    let (subject, body) = message.split_once("\n\n").unwrap_or((message, ""));
    let subject = subject.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    let mut lines: Vec<String> = body
        .trim_matches('\n')
        .lines()
        .map(|l| {
            let l = l.trim_end();
            match l.char_indices().nth(BODY_LINE_CHARS) {
                Some((cut, _)) => format!("{}…", &l[..cut]),
                None => l.to_string(),
            }
        })
        .collect();
    // Runs of blank lines show as one.
    lines.dedup_by(|a, b| a.is_empty() && b.is_empty());
    if lines.len() > BODY_LINES {
        lines.truncate(BODY_LINES);
        lines.push("…".to_string());
    }
    (subject, lines)
}

/// The work-in-progress card's lines. The repo's status says only that the
/// tree is dirty, not how many files, so the card can't count them.
fn wip_card(g: &RepoGraph, colors: &Colors, p: &Palette) -> Vec<AnyElement> {
    let on = match &g.info.checkout {
        crate::repo_info::Checkout::Branch(b) | crate::repo_info::Checkout::Unborn(b) => {
            format!("on {b}")
        }
        crate::repo_info::Checkout::Detached(h) => format!("on @{h}"),
    };
    vec![
        div()
            .text_color(colors.unpushed)
            .child("Uncommitted changes")
            .into_any_element(),
        div().text_color(p.muted).child(on).into_any_element(),
    ]
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
    added: Hsla,
    removed: Hsla,
    /// Branches with an open or a draft PR; closed PRs use `removed`,
    /// merged ones `merged`.
    pr_open: Hsla,
    pr_draft: Hsla,
    hues: [Hsla; 6],
    /// What the graph is drawn on.
    bg: Hsla,
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
                added: c(0x3fb950),
                removed: c(0xf85149),
                pr_open: c(0x3fb950),
                // Darker than the trunk's grey, so the two stay apart.
                pr_draft: c(0x6e7681),
                // blue, orange, pink, lemon, teal, coral
                hues: [
                    c(0x4493f8),
                    c(0xf0883e),
                    c(0xf778ba),
                    c(0xe3b341),
                    c(0x39c5cf),
                    c(0xff7b72),
                ],
                bg: p.surface,
            }
        } else {
            Self {
                merged: c(0x8250df),
                unpushed: c(0x9a6700),
                unpulled: c(0x0969da),
                trunk: c(0x656d76),
                head: c(0x1f2328),
                stash: p.muted,
                added: c(0x1a7f37),
                removed: c(0xcf222e),
                pr_open: c(0x1a7f37),
                // Lighter than the trunk's grey, so the two stay apart.
                pr_draft: c(0xafb8c1),
                hues: [
                    c(0x0969da),
                    c(0xbc4c00),
                    c(0xbf3989),
                    c(0x9a6700),
                    c(0x1b7c83),
                    c(0xc4432b),
                ],
                bg: p.surface,
            }
        }
    }

    /// The band under a worktree's line: its tip's colour, faint.
    /// `folded` when the lane is past the ones shown, so dimmed.
    fn band(&self, g: &RepoGraph, b: &crate::repo_graph::Band, folded: bool) -> Hsla {
        let alpha = if folded { 0.12 } else { 0.24 };
        ink::over(self.node(g, b.top).opacity(alpha), self.bg)
    }

    fn node(&self, g: &RepoGraph, row: usize) -> Hsla {
        let node = &g.layout.nodes[row];
        if let Some(pr) = g.prs.of_node(node, &g.layout.branches) {
            return match pr {
                PrState::Open => self.pr_open,
                PrState::Draft => self.pr_draft,
                PrState::Merged => self.merged,
                PrState::Closed => self.removed,
            };
        }
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

/// The work-in-progress dot's centre, and the ends of the dashed noodle
/// that joins it to HEAD (at HEAD, at the dot) when there is a HEAD commit.
pub(crate) type Wip = ((f32, f32), Option<[(f32, f32); 2]>);

/// Where a strip in `bounds` puts its tree: every row and lane on the
/// device pixel grid, so runs are crisp and bends meet them exactly.
#[derive(Clone, Copy)]
pub(crate) struct StripGeo {
    s: f32,
    /// The newest row's x; at home HEAD sits PAD in, one step further when
    /// the work-in-progress dot leads it.
    top_x: f32,
    cy: f32,
    /// The work-in-progress dot takes a column of its own right of HEAD:
    /// rows newer than HEAD move a step further right to make room.
    gap: f32,
    left: f32,
    right: f32,
}

impl StripGeo {
    pub(crate) fn new(
        bounds: Bounds<Pixels>,
        g: &RepoGraph,
        scroll: f32,
        dirty: bool,
        s: f32,
    ) -> Self {
        let rows = g.layout.nodes.len();
        let (left, right) = (f32::from(bounds.left()), f32::from(bounds.right()));
        let head = g.head_row.filter(|&row| row < rows);
        let gap = head
            .filter(|&row| dirty && row > 0)
            .map_or(0.0, |row| row as f32);
        Self {
            s,
            top_x: right - PAD - if dirty { STEP } else { 0.0 } + scroll,
            cy: f32::from(bounds.center().y),
            gap,
            left,
            right,
        }
    }

    pub(crate) fn x(&self, row: f32) -> f32 {
        let x = self.top_x - row * STEP + if row < self.gap - 0.5 { STEP } else { 0.0 };
        ink::pos(self.s, x, STROKE)
    }

    pub(crate) fn y(&self, lane: f32) -> f32 {
        ink::pos(self.s, self.cy + lane_offset(lane.round() as usize), STROKE)
    }

    /// The rows whose dots are in view, one step of slack each side.
    pub(crate) fn rows(&self, rows: usize) -> (usize, usize) {
        let lo = (((self.top_x - self.right) / STEP).floor() - 1.0).max(0.0) as usize;
        let hi = ((((self.top_x - self.left) / STEP).ceil() + 1.0).max(0.0) as usize).min(rows);
        (lo, hi)
    }

    /// The work-in-progress dot: its centre, and the ends of the dashed
    /// noodle that joins it to HEAD (at HEAD, at the dot), when there is
    /// a HEAD commit. `r` is the dot's radius.
    pub(crate) fn wip(&self, g: &RepoGraph, r: f32) -> Wip {
        let rows = g.layout.nodes.len();
        let head = g.head_row.filter(|&row| row < rows);
        // Right of HEAD, on a lane no noodle crosses that column in; with no
        // HEAD commit (unborn, or not loaded yet) right of the newest row.
        let (hx, hy, lane) = match head {
            Some(row) => (
                self.x(row as f32),
                self.y(g.layout.nodes[row].lane as f32),
                g.layout.free_lane_above(row),
            ),
            None => (self.x(0.0), self.cy, 0),
        };
        let center = (ink::pos(self.s, hx + STEP, STROKE), self.y(lane as f32));
        let noodle = head.map(|_| [(hx + DOT, hy), (hx + STEP - r, center.1)]);
        (center, noodle)
    }
}

/// Paints the visible part of a strip; returns where each painted dot is
/// and what it is, for hit-testing. Without `tree`, only its tag and
/// stash marks (when `marks`): what the flip fades with the panel while it
/// draws the tree itself.
#[allow(clippy::too_many_arguments)]
fn paint_strip(
    bounds: Bounds<Pixels>,
    g: &RepoGraph,
    scroll: f32,
    grow: Grow,
    copied: Option<usize>,
    dirty: bool,
    (tree, marks): (bool, bool),
    colors: &Colors,
    window: &mut Window,
) -> Vec<(Point<Pixels>, Dot)> {
    let rows = g.layout.nodes.len();
    let left = f32::from(bounds.left());
    let s = window.scale_factor();
    let geo = StripGeo::new(bounds, g, scroll, dirty, s);
    let x = |row: f32| geo.x(row);
    let y = |lane: f32| geo.y(lane);
    let at = |pos: graph::Pos| point(px(x(pos.row)), px(y(pos.lane)));
    let folded = |lane: f32| lane.round() as usize >= LANES;

    // Rows whose dots are in view, one step of slack each side.
    let (lo, hi) = geo.rows(rows);
    if lo >= hi && rows > 0 && lo >= rows {
        return Vec::new();
    }

    // Under everything, a soft band along each other worktree's line.
    for b in g
        .bands
        .iter()
        .filter(|b| tree && b.top < hi && b.bottom + 1 >= lo)
    {
        let color = colors.band(g, b, b.lane >= LANES);
        let yb = y(b.lane as f32);
        let r = BAND / 2.0;
        let x0 = x(b.top as f32);
        let x1 = if b.open { left - r } else { x(b.bottom as f32) };
        let x1 = x1.max(left - r);
        ink::line(window, (x1, yb), (x0, yb), BAND, color, None);
        ink::circle(window, (x0, yb), r, Some(color), None);
        if !b.open {
            ink::circle(window, (x1, yb), r, Some(color), None);
        }
    }

    let mut dots = Vec::with_capacity(hi.saturating_sub(lo) + 1);
    for e in g.layout.visible_edges(lo, hi).filter(|_| tree) {
        let base = if e.first {
            colors.node(g, e.child.0)
        } else {
            match e.parent {
                Some((row, _)) => colors.node(g, row),
                None => colors.node(g, e.child.0),
            }
        };
        for seg in e.segments(rows) {
            let (a, b) = match seg {
                Seg::Straight(a, b) | Seg::Curve(a, b) => (a, b),
            };
            let alpha = if folded(a.lane) || folded(b.lane) {
                0.35
            } else {
                0.8
            };
            let color = ink::over(base.opacity(alpha), colors.bg);
            let (pa, pb) = (at(a), at(b));
            let (pa, pb) = (
                (f32::from(pa.x), f32::from(pa.y)),
                (f32::from(pb.x), f32::from(pb.y)),
            );
            match seg {
                Seg::Straight(..) => ink::line(window, pa, pb, STROKE, color, None),
                Seg::Curve(..) => {
                    let c = graph::noodle(pa, pb, (-1.0, 0.0));
                    ink::cubic(window, c, STROKE, color, None);
                }
            }
        }
    }

    if dirty {
        let r = grow.radius(Dot::Wip, DOT + 0.5, DOT_HOVER);
        let (c, noodle) = geo.wip(g, r);
        dots.push((point(px(c.0), px(c.1)), Dot::Wip));
        if let Some([from, to]) = noodle.filter(|_| tree) {
            // Joined to HEAD by a short dashed noodle.
            let [a, c1, c2, b] = graph::noodle(to, from, (-1.0, 0.0));
            ink::cubic(
                window,
                [b, c2, c1, a],
                1.0,
                ink::over(colors.unpushed.opacity(0.8), colors.bg),
                Some(ink::Dash { on: 2.0, off: 2.0 }),
            );
        }
        if tree {
            ink::circle(
                window,
                c,
                r,
                Some(colors.bg),
                Some((1.0, colors.unpushed, true)),
            );
        }
    }

    for row in lo..hi {
        let node = &g.layout.nodes[row];
        let center = point(px(x(row as f32)), px(y(node.lane as f32)));
        dots.push((center, Dot::Commit(row)));
        let color = colors.node(g, row);
        let color = if node.lane >= LANES {
            color.opacity(0.5)
        } else {
            color
        };
        let id = &g.commits[row].id;
        let head = g.ctx.refs.is_head(id);
        let r = grow.radius(Dot::Commit(row), DOT, DOT_HOVER);
        let c = (f32::from(center.x), f32::from(center.y));
        if copied == Some(row) && tree {
            // Its id just went to the clipboard.
            let ring = r + 3.0;
            ink::circle(window, c, ring, None, Some((1.0, colors.head, false)));
        }
        if !tree {
        } else if head {
            ink::circle(window, c, r + 3.0, Some(colors.head.opacity(0.22)), None);
            ink::circle(window, c, r, Some(colors.head), None);
        } else if node.class == Class::Unpulled || node.merge {
            // Merges and not-yet-pulled commits are rings; a merge's is smaller.
            // Filled with the background, so the noodles under it don't
            // show through the ring.
            let r = if node.merge { r - 0.5 } else { r };
            ink::circle(window, c, r, Some(colors.bg), Some((1.5, color, false)));
        } else {
            ink::circle(window, c, r, Some(color), None);
        }
        if !marks {
            continue;
        }
        if g.ctx.refs.at(id).iter().any(|r| matches!(r, Ref::Tag(_))) {
            diamond(point(center.x, center.y - px(r + 4.0)), 2.5, color, window);
        }
        if g.stashes_at(row).next().is_some() {
            stash(
                center,
                r,
                ink::over(colors.stash.opacity(0.8), colors.bg),
                colors.stash,
                window,
            );
        }
    }
    dots
}

/// A hollow square for the stashes made on the commit at `base`, newer than
/// it so to its right, joined to it by a dotted noodle.
/// `line` is `color` laid over the background, for the dotted noodle.
fn stash(base: Point<Pixels>, r: f32, line: Hsla, color: Hsla, window: &mut Window) {
    let s = 2.5;
    let at = point(base.x + px(STEP * 0.6), base.y + px(r + 5.0));
    let c = graph::noodle(
        (f32::from(at.x) - s, f32::from(at.y)),
        (f32::from(base.x), f32::from(base.y) + r),
        (-1.0, 0.0),
    );
    ink::cubic(window, c, 1.0, line, Some(ink::Dash { on: 1.0, off: 1.5 }));
    window.paint_quad(outline(
        Bounds::new(
            point(at.x - px(s), at.y - px(s)),
            size(px(2.0 * s), px(2.0 * s)),
        ),
        color,
        BorderStyle::Solid,
    ));
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

/// A strip's tree at rest, for the flip: exactly as the panel draws it
/// with its newest commits home and nothing hovered, without the fade at
/// its tail and the tag and stash marks, which are the panel's own.
/// `bounds` are the strip's graph box, logical px.
fn paint_strip_tree(
    bounds: Bounds<Pixels>,
    g: &RepoGraph,
    dirty: bool,
    colors: &Colors,
    window: &mut Window,
) {
    let mask = Bounds::new(
        point(bounds.left(), bounds.top() - px(BLEED)),
        size(bounds.size.width, bounds.size.height + px(2.0 * BLEED)),
    );
    window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
        paint_strip(
            bounds,
            g,
            0.0,
            Grow::default(),
            None,
            dirty,
            (true, false),
            colors,
            window,
        );
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_parts_split_subject_and_body() {
        let (s, b) = message_parts("Add the thing\n\nWhy it was needed.\n\n\n\nMore.\n");
        assert_eq!(s, "Add the thing");
        assert_eq!(b, ["Why it was needed.", "", "More."]);
        let (s, b) = message_parts("One line");
        assert_eq!(s, "One line");
        assert!(b.is_empty());
        let (s, _) = message_parts("Wrapped\nsubject\n\nbody");
        assert_eq!(s, "Wrapped subject");
    }

    #[test]
    fn message_parts_cap_long_bodies() {
        let long = format!("s\n\n{}", vec!["x"; BODY_LINES + 5].join("\n"));
        let (_, b) = message_parts(&long);
        assert_eq!(b.len(), BODY_LINES + 1);
        assert_eq!(b.last().unwrap(), "…");
        let wide = format!("s\n\n{}", "é".repeat(BODY_LINE_CHARS + 3));
        let (_, b) = message_parts(&wide);
        assert_eq!(b[0].chars().count(), BODY_LINE_CHARS + 1);
    }
}
