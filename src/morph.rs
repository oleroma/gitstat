//! The flip animation: opening the big window from the panel turns the
//! panel into it, and closing the big window turns it back.
//!
//! The panel and the big window are two OS windows, and resizing a window
//! every frame flickers, so neither moves. A third window, borderless,
//! transparent, click-through and topmost, covers both their places for
//! the length of the flip and draws it. Opening:
//!
//! 1. Fade: a sheet the colour of the strips fades in over the panel,
//!    hiding its words, while each repo's tree is drawn over its strip.
//! 2. Turn: the panel closes; the sheet grows from the panel's place to
//!    the big window's, each tree turns a quarter turn from its strip to
//!    where its column will be (`timeline::paint_tree`), and each section's
//!    heading stretches into its spine.
//! 3. Hold: the big window opens underneath, still covered, and gets a
//!    moment to draw.
//! 4. Reveal: the sheet and the trees fade out, showing the big window
//!    with its words, then the flip's window closes.
//!
//! Closing or minimising runs the same backwards: the sheet fades in over
//! the big window, which then closes, everything turns back, the panel
//! opens underneath, the sheet fades to show it, and the panel slides away.
//!
//! Where things land comes from what each window drew last time (`seen_*`),
//! so the first flip after starting gitstat may land a few px off; the
//! reveal's fade hides that.

use crate::config::Section;
use crate::layout::Rect;
use crate::timeline::{COLUMN_WIDTH, Ends};
use crate::view::{self, Palette, Ui};
use crate::win;
use gpui::{
    App, AppContext, Bounds, Context, Global, Hsla, IntoElement, ParentElement, Pixels, Render,
    Styled, Window, WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, canvas, div, point, px, quad, size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FADE: Duration = Duration::from_millis(90);
const TURN: Duration = Duration::from_millis(280);
const HOLD: Duration = Duration::from_millis(80);
const REVEAL: Duration = Duration::from_millis(110);
/// The panel's and the sheet's rounded corners, logical px.
const RADIUS: f32 = 8.0;
/// Until the big window has been drawn once: its column header's height,
/// and the table's top and bottom margins, logical px.
const HEADER_GUESS: f32 = 110.0;
const TABLE_TOP_GUESS: f32 = 60.0;
const TABLE_BOTTOM_GUESS: f32 = 40.0;

thread_local! {
    /// Each strip in view, by repo, as the panel last drew it: bounds in
    /// the panel's logical px.
    static STRIPS: RefCell<HashMap<PathBuf, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    /// Each section heading in view, as the panel last drew it.
    static HEADERS: RefCell<HashMap<SectionKey, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    /// The column body's top below the big window's client top, physical
    /// px, as last drawn.
    static BODY_TOP: Cell<Option<f32>> = const { Cell::new(None) };
    /// A spine's top, and its bottom's distance above the client bottom,
    /// physical px, as last drawn.
    static SPINE: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// The big window's client area minus the bounds it was opened with,
    /// physical px per edge (left, top, right, bottom).
    static CLIENT_DELTA: Cell<(i32, i32, i32, i32)> = const { Cell::new((0, 0, 0, 0)) };
}

/// `Section` as a map key.
type SectionKey = u8;

fn key(section: Section) -> SectionKey {
    match section {
        Section::NeedsYou => 0,
        Section::Synced => 1,
        Section::PullRequests => 2,
        Section::GithubOnly => 3,
    }
}

/// Notes a strip the panel drew, for the flip.
pub fn seen_strip(path: &Path, bounds: Bounds<Pixels>) {
    STRIPS.with(|s| {
        s.borrow_mut().insert(path.to_path_buf(), bounds);
    });
}

/// Notes a section heading the panel drew, for the flip.
pub fn seen_header(section: Section, bounds: Bounds<Pixels>) {
    HEADERS.with(|h| {
        h.borrow_mut().insert(key(section), bounds);
    });
}

/// Forgets the strips and headings; the panel is opening afresh.
pub fn clear_strips() {
    STRIPS.with(|s| s.borrow_mut().clear());
    HEADERS.with(|h| h.borrow_mut().clear());
}

/// Notes a column body the big window drew, for the flip.
pub fn seen_column(body: Bounds<Pixels>, window: &Window) {
    let top = f32::from(body.top()) * window.scale_factor();
    BODY_TOP.with(|t| t.set(Some(top)));
}

/// Notes a section spine the big window drew, for the flip.
pub fn seen_spine(spine: Bounds<Pixels>, window: &Window) {
    let s = window.scale_factor();
    let below = f32::from(window.viewport_size().height - spine.bottom()) * s;
    SPINE.with(|t| t.set(Some((f32::from(spine.top()) * s, below))));
}

/// Notes where the big window's client area came out for the bounds it
/// was opened with, both physical px.
pub fn seen_client(requested: Rect, client: Rect) {
    CLIENT_DELTA.with(|d| {
        d.set((
            client.left - requested.left,
            client.top - requested.top,
            client.right - requested.right,
            client.bottom - requested.bottom,
        ))
    });
}

#[derive(Default)]
struct MorphSlot {
    handle: Option<WindowHandle<MorphView>>,
}

impl Global for MorphSlot {}

/// Whether a flip is drawing now.
pub fn running(cx: &App) -> bool {
    cx.try_global::<MorphSlot>()
        .and_then(|s| s.handle)
        .is_some_and(|h| view::is_live(h.into(), cx))
}

fn enabled(cx: &App) -> bool {
    cx.try_global::<Ui>().is_some_and(|ui| {
        let cfg = &ui.state.read(cx).config;
        cfg.flip_animation && cfg.window && cfg.panel
    })
}

/// Opens the big window from the panel, scrolled to `focus` if given, with
/// the flip when it's on. Called from the panel's own handlers; closes the
/// panel either way.
pub fn open_window(panel: &mut Window, focus: Option<PathBuf>, cx: &mut App) {
    if let Some(path) = &focus
        && let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone())
    {
        t.update(cx, |t, _| t.focus(path));
    }
    if running(cx) {
        return;
    }
    let flip = enabled(cx) && !view::window_open(cx);
    let started = flip
        && win::hwnd(panel)
            .and_then(win::client_rect)
            .and_then(|rect| {
                let plan = plan(rect, panel.scale_factor(), None, focus.as_deref(), cx)?;
                Some(launch(plan, Direction::Open, cx))
            })
            .unwrap_or(false);
    if !started {
        crate::panel::dismiss(panel, cx);
        view::show_window(cx);
    }
}

/// Opens the big window from the tray or the badge: through the panel at
/// `anchor` when the flip is on, else straight away.
pub fn show_window(anchor: crate::panel::Anchor, cx: &mut App) {
    if !enabled(cx) || view::window_open(cx) || running(cx) {
        if !view::window_open(cx) {
            crate::panel::close(cx);
        }
        view::show_window(cx);
        return;
    }
    crate::panel::then_window(anchor, cx);
}

/// Closing the big window: starts the flip back into the panel and returns
/// true, leaving the window for the flip to close; false when the flip is
/// off or can't run, so the caller closes it.
pub fn flip_back(big: &mut Window, cx: &mut App) -> bool {
    if running(cx) {
        // Mid-flip; the flip closes it.
        return true;
    }
    if !enabled(cx) {
        return false;
    }
    let Some(panel_rect) = crate::panel::last_rect(cx) else {
        return false;
    };
    let Some(big_rect) = win::hwnd(big).and_then(win::client_rect) else {
        return false;
    };
    // The panel draws at the primary monitor's scale, like the big window.
    let ps = big.scale_factor();
    let scroll = view::table_scroll(cx) * ps;
    let Some(plan) = plan(panel_rect, ps, Some((big_rect, scroll)), None, cx) else {
        return false;
    };
    launch(plan, Direction::Close, cx)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Panel to big window.
    Open,
    /// Big window to panel.
    Close,
}

/// One repo's tree in the flip.
struct Tree {
    graph: Arc<crate::repo_graph::RepoGraph>,
    dirty: bool,
    /// Physical px, from the flip window's top-left corner.
    ends: Ends,
    /// Its column is past the big window's edge: it fades as it turns.
    gone: bool,
}

/// A section's heading in the panel and its spine in the big window,
/// physical px from the flip window's top-left corner.
struct Spine {
    section: Section,
    from: Bounds<f32>,
    to: Bounds<f32>,
}

/// Everything a flip draws, laid out.
struct Plan {
    /// The flip window's place, physical px.
    union: Rect,
    /// The panel's and the big window's client areas, physical px from the
    /// flip window's top-left corner.
    from: Bounds<f32>,
    to: Bounds<f32>,
    trees: Vec<Tree>,
    spines: Vec<Spine>,
}

/// Lays out a flip between the panel at `panel_rect` (client area,
/// physical px, drawn at `ps`) and the big window: at `big`'s client rect
/// with its table scrolled that far (physical px), or where show_window
/// will open it, scrolled to `focus`. None when nothing would turn.
fn plan(
    panel_rect: Rect,
    ps: f32,
    big: Option<(Rect, f32)>,
    focus: Option<&Path>,
    cx: &App,
) -> Option<Plan> {
    let ui = cx.try_global::<Ui>()?;
    let state = ui.state.read(cx);
    let timelines = ui.timelines.read(cx);
    let strips = STRIPS.with(|s| s.borrow().clone());
    let headers = HEADERS.with(|h| h.borrow().clone());

    let cells = view::table_cells(state);
    let lefts = view::cell_lefts(&cells);
    let repos = cells
        .iter()
        .filter(|c| matches!(c, view::Cell::Repo(_)))
        .count();
    let (target, ws) = match big {
        Some((rect, _)) => (rect, ps),
        None => {
            // Where show_window will put it.
            let (bounds, ws) = view::window_place(repos);
            let at = |v: Pixels| (f32::from(v) * ws).round() as i32;
            let (dl, dt, dr, db) = CLIENT_DELTA.with(Cell::get);
            (
                Rect::new(
                    at(bounds.origin.x) + dl,
                    at(bounds.origin.y) + dt,
                    at(bounds.origin.x + bounds.size.width) + dr,
                    at(bounds.origin.y + bounds.size.height) + db,
                ),
                ws,
            )
        }
    };

    let union = Rect::new(
        panel_rect.left.min(target.left),
        panel_rect.top.min(target.top),
        panel_rect.right.max(target.right),
        panel_rect.bottom.max(target.bottom),
    );
    let local = |r: Rect| Bounds {
        origin: point((r.left - union.left) as f32, (r.top - union.top) as f32),
        size: size(r.width() as f32, r.height() as f32),
    };
    let (from, to) = (local(panel_rect), local(target));
    // In the panel, logical px to the flip window's physical px.
    let in_panel = |b: &Bounds<Pixels>| Bounds {
        origin: point(
            from.origin.x + f32::from(b.left()) * ps,
            from.origin.y + f32::from(b.top()) * ps,
        ),
        size: size(f32::from(b.size.width) * ps, f32::from(b.size.height) * ps),
    };

    // The table's scroll: as it is, or bringing the focused column in.
    let scroll = match big {
        Some((_, scroll)) => scroll,
        None => focus
            .and_then(|f| view::cell_of(&cells, f))
            .map_or(0.0, |i| {
                ((lefts[i] + COLUMN_WIDTH) * ws - to.size.width).max(0.0)
            }),
    };
    let body_top = BODY_TOP.with(Cell::get).unwrap_or(HEADER_GUESS * ws);
    let (spine_top, spine_below) = SPINE
        .with(Cell::get)
        .unwrap_or((TABLE_TOP_GUESS * ws, TABLE_BOTTOM_GUESS * ws));

    let mut trees = Vec::new();
    let mut spines = Vec::new();
    for (cell, left) in cells.iter().zip(&lefts) {
        let x = to.origin.x + left * ws - scroll;
        let gone = x + cell.width() * ws <= to.origin.x || x >= to.origin.x + to.size.width;
        match *cell {
            view::Cell::Repo(status) => {
                let (Some(strip), Some(graph)) =
                    (strips.get(&status.path), timelines.graph_of(&status.path))
                else {
                    continue;
                };
                let strip = in_panel(strip);
                let newest = strip.origin.x + strip.size.width - crate::timeline::STRIP_PAD * ps;
                trees.push(Tree {
                    graph,
                    dirty: status.dirty,
                    ends: Ends {
                        strip: (newest, strip.origin.y + strip.size.height / 2.0),
                        strip_len: newest - strip.origin.x,
                        column: (x, to.origin.y + body_top),
                        column_len: (to.size.height - body_top).max(0.0),
                    },
                    gone,
                });
            }
            view::Cell::Spine(section, _) => {
                let Some(header) = headers.get(&key(section)) else {
                    continue;
                };
                if gone {
                    continue;
                }
                spines.push(Spine {
                    section,
                    from: in_panel(header),
                    to: Bounds {
                        origin: point(x, to.origin.y + spine_top),
                        size: size(
                            view::SPINE_WIDTH * ws,
                            (to.size.height - spine_top - spine_below).max(0.0),
                        ),
                    },
                });
            }
        }
    }
    (!trees.is_empty() || !spines.is_empty()).then_some(Plan {
        union,
        from,
        to,
        trees,
        spines,
    })
}

/// Opens the flip's window for `plan`. False if it couldn't.
fn launch(plan: Plan, direction: Direction, cx: &mut App) -> bool {
    let union = plan.union;
    let Ok(handle) = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(crate::panel::logical(union))),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            if let Some(h) = win::hwnd(window) {
                cx.spawn(async move |_| unsafe {
                    win::style_morph(h);
                    win::place_morph(h, union);
                })
                .detach();
            }
            cx.new(|_| MorphView {
                start: Instant::now(),
                plan,
                direction,
                hwnd: win::hwnd(window),
                left: false,
                arrived: false,
            })
        },
    ) else {
        return false;
    };
    cx.default_global::<MorphSlot>().handle = Some(handle);
    true
}

pub struct MorphView {
    start: Instant,
    plan: Plan,
    direction: Direction,
    hwnd: Option<isize>,
    /// The window the flip starts from has closed.
    left: bool,
    /// The window it ends in has opened.
    arrived: bool,
}

/// `t` eased in and out.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// How far `elapsed` is into the phase that starts `at` and lasts `len`.
fn phase(elapsed: Duration, at: Duration, len: Duration) -> f32 {
    (elapsed.saturating_sub(at).as_secs_f32() / len.as_secs_f32()).clamp(0.0, 1.0)
}

fn lerp_bounds(a: Bounds<f32>, b: Bounds<f32>, e: f32, s: f32) -> Bounds<Pixels> {
    let l = |a: f32, b: f32| (a + (b - a) * e) / s;
    Bounds::new(
        point(px(l(a.origin.x, b.origin.x)), px(l(a.origin.y, b.origin.y))),
        size(
            px(l(a.size.width, b.size.width)),
            px(l(a.size.height, b.size.height)),
        ),
    )
}

impl Render for MorphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let elapsed = self.start.elapsed();
        let turn_at = FADE;
        let hold_at = turn_at + TURN;
        let reveal_at = hold_at + HOLD;
        let end = reveal_at + REVEAL;
        let open = self.direction == Direction::Open;

        if elapsed >= turn_at && !self.left {
            self.left = true;
            if open {
                cx.defer(crate::panel::close);
            } else {
                cx.defer(view::close_window);
            }
        }
        if elapsed >= hold_at && !self.arrived {
            self.arrived = true;
            if open {
                cx.defer(view::show_window);
            } else {
                let me = self.hwnd;
                cx.defer(move |cx| {
                    crate::panel::reopen(cx);
                    // The panel goes topmost as it opens; the flip stays
                    // over it until the reveal.
                    if let Some(h) = me {
                        cx.spawn(async move |_| unsafe { win::reassert_topmost(h) })
                            .detach();
                    }
                });
            }
        }
        if elapsed >= end {
            window.remove_window();
            if !open {
                // Back in the panel, which then goes the way it came.
                cx.defer(crate::panel::slide_away);
            }
            return div();
        }

        let sheet_alpha = ease(phase(elapsed, Duration::ZERO, FADE));
        let turned = ease(phase(elapsed, turn_at, TURN));
        // 0 is the panel's layout, 1 the big window's.
        let e = if open { turned } else { 1.0 - turned };
        let reveal = 1.0 - ease(phase(elapsed, reveal_at, REVEAL));
        let appearance = window.appearance();
        let dark = matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let p = Palette::for_appearance(appearance);
        let s = window.scale_factor();
        let sheet = lerp_bounds(self.plan.from, self.plan.to, e, s);
        let trees: Vec<(Arc<crate::repo_graph::RepoGraph>, bool, Ends, f32)> = self
            .plan
            .trees
            .iter()
            .map(|t| {
                let k = |(x, y): (f32, f32)| (x / s, y / s);
                let ends = Ends {
                    strip: k(t.ends.strip),
                    strip_len: t.ends.strip_len / s,
                    column: k(t.ends.column),
                    column_len: t.ends.column_len / s,
                };
                let alpha = if t.gone { 1.0 - e } else { 1.0 };
                (t.graph.clone(), t.dirty, ends, alpha * reveal)
            })
            .collect();
        let spines: Vec<(Bounds<Pixels>, Hsla)> = self
            .plan
            .spines
            .iter()
            .map(|sp| {
                let color = match sp.section {
                    Section::NeedsYou => p.red,
                    Section::Synced => p.green,
                    _ => p.muted,
                };
                (lerp_bounds(sp.from, sp.to, e, s), color)
            })
            .collect();

        let painter = canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.request_animation_frame();
                let bg = p.surface.opacity(sheet_alpha * reveal);
                window.paint_quad(quad(
                    sheet,
                    px(RADIUS),
                    bg,
                    px(0.0),
                    gpui::transparent_black(),
                    gpui::BorderStyle::Solid,
                ));
                window.with_content_mask(Some(gpui::ContentMask { bounds: sheet }), |window| {
                    // A section's heading stretching into its spine: its
                    // colour's edge on the side the name reads from.
                    for (b, color) in &spines {
                        window.paint_quad(quad(
                            *b,
                            px(4.0),
                            p.bg.opacity(reveal),
                            gpui::Edges {
                                left: px(3.0),
                                ..Default::default()
                            },
                            color.opacity(0.8 * reveal),
                            gpui::BorderStyle::Solid,
                        ));
                    }
                    for (graph, dirty, ends, alpha) in &trees {
                        crate::timeline::paint_tree(
                            graph, *dirty, *ends, e, *alpha, dark, &p, window,
                        );
                    }
                });
            },
        )
        .size_full();
        div().size_full().child(painter)
    }
}
