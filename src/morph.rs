//! The flip animation: opening the big window from the panel turns the
//! panel into it.
//!
//! The panel and the big window are two OS windows, and resizing a window
//! every frame flickers, so neither moves. A third window, borderless,
//! transparent, click-through and topmost, covers both their places for
//! the length of the flip and draws it:
//!
//! 1. Fade: a sheet the colour of the strips fades in over the panel,
//!    hiding its words, while each repo's tree is drawn over its strip.
//! 2. Turn: the panel closes; the sheet grows from the panel's place to
//!    the big window's, and each tree turns a quarter turn from its strip
//!    to where its column will be (`timeline::paint_tree`).
//! 3. Hold: the big window opens underneath, still covered, and gets a
//!    moment to draw.
//! 4. Reveal: the sheet and the trees fade out, showing the big window
//!    with its words, then the flip's window closes.
//!
//! Where the trees land comes from what the big window drew last time
//! (`seen_column`), so the first flip after starting gitstat may land a
//! few px off; the reveal's fade hides that.

use crate::layout::Rect;
use crate::timeline::{COLUMN_WIDTH, Ends, column_index};
use crate::view::{self, Palette, Ui};
use crate::win;
use gpui::{
    App, AppContext, Bounds, Context, Global, IntoElement, ParentElement, Pixels, Render, Styled,
    Window, WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, canvas, div, point, px, quad, size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FADE: Duration = Duration::from_millis(140);
const TURN: Duration = Duration::from_millis(420);
const HOLD: Duration = Duration::from_millis(90);
const REVEAL: Duration = Duration::from_millis(170);
/// The panel's and the sheet's rounded corners, logical px.
const RADIUS: f32 = 8.0;
/// The column header's height until the big window has been drawn once.
const HEADER_GUESS: f32 = 110.0;

thread_local! {
    /// Each strip in view, by repo, as the panel last drew it: bounds in
    /// the panel's logical px.
    static STRIPS: RefCell<HashMap<PathBuf, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    /// The column body's top below the big window's client top, physical
    /// px, as last drawn.
    static BODY_TOP: Cell<Option<f32>> = const { Cell::new(None) };
    /// The big window's client area minus the bounds it was opened with,
    /// physical px per edge (left, top, right, bottom).
    static CLIENT_DELTA: Cell<(i32, i32, i32, i32)> = const { Cell::new((0, 0, 0, 0)) };
}

/// Notes a strip the panel drew, for the flip's start.
pub fn seen_strip(path: &Path, bounds: Bounds<Pixels>) {
    STRIPS.with(|s| {
        s.borrow_mut().insert(path.to_path_buf(), bounds);
    });
}

/// Forgets the strips; the panel is opening afresh.
pub fn clear_strips() {
    STRIPS.with(|s| s.borrow_mut().clear());
}

/// Notes a column body the big window drew, for the flip's landing.
pub fn seen_column(body: Bounds<Pixels>, window: &Window) {
    let top = f32::from(body.top()) * window.scale_factor();
    BODY_TOP.with(|t| t.set(Some(top)));
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
    let flip = cx.try_global::<Ui>().is_some_and(|ui| {
        let cfg = &ui.state.read(cx).config;
        cfg.flip_animation && cfg.window
    }) && !view::window_open(cx);
    if !(flip && start(panel, focus.as_deref(), cx)) {
        crate::panel::dismiss(panel, cx);
        view::show_window(cx);
    }
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

pub struct MorphView {
    start: Instant,
    trees: Vec<Tree>,
    /// The panel's and the big window's client areas, physical px from the
    /// flip window's top-left corner.
    from: Bounds<f32>,
    to: Bounds<f32>,
    panel_closed: bool,
    window_opened: bool,
}

/// Starts the flip; false when there is nothing to flip from.
fn start(panel: &mut Window, focus: Option<&Path>, cx: &mut App) -> bool {
    let Some((state, timelines)) = cx
        .try_global::<Ui>()
        .map(|ui| (ui.state.clone(), ui.timelines.clone()))
    else {
        return false;
    };
    let Some(panel_rect) = win::hwnd(panel).and_then(win::client_rect) else {
        return false;
    };
    let ps = panel.scale_factor();
    let strips = STRIPS.with(|s| s.borrow().clone());

    // Where the big window will be, as show_window places it.
    let (order, dirty): (Vec<PathBuf>, HashMap<PathBuf, bool>) = {
        let s = state.read(cx);
        let order = s
            .sorted(&s.statuses)
            .into_iter()
            .chain(s.sorted(&s.remote_statuses))
            .collect::<Vec<_>>();
        let dirty = order.iter().map(|r| (r.path.clone(), r.dirty)).collect();
        (order.into_iter().map(|r| r.path.clone()).collect(), dirty)
    };
    let (bounds, ws) = view::window_place(order.len());
    let at = |v: Pixels| (f32::from(v) * ws).round() as i32;
    let requested = Rect::new(
        at(bounds.origin.x),
        at(bounds.origin.y),
        at(bounds.origin.x + bounds.size.width),
        at(bounds.origin.y + bounds.size.height),
    );
    let (dl, dt, dr, db) = CLIENT_DELTA.with(Cell::get);
    let target = Rect::new(
        requested.left + dl,
        requested.top + dt,
        requested.right + dr,
        requested.bottom + db,
    );

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

    // The table scrolls a focused column into view.
    let col_w = COLUMN_WIDTH * ws;
    let scroll = {
        let s = state.read(cx);
        let refs: Vec<&crate::git::RepoStatus> = s
            .sorted(&s.statuses)
            .into_iter()
            .chain(s.sorted(&s.remote_statuses))
            .collect();
        focus
            .and_then(|f| column_index(&refs, f))
            .map_or(0.0, |i| ((i + 1) as f32 * col_w - to.size.width).max(0.0))
    };
    let body_top = BODY_TOP.with(Cell::get).unwrap_or(HEADER_GUESS * ws);

    let t = timelines.read(cx);
    let mut trees = Vec::new();
    for (i, path) in order.iter().enumerate() {
        let (Some(strip), Some(graph)) = (strips.get(path), t.graph_of(path)) else {
            continue;
        };
        let (l, r) = (f32::from(strip.left()) * ps, f32::from(strip.right()) * ps);
        let cy = f32::from(strip.center().y) * ps;
        let newest = r - crate::timeline::STRIP_PAD * ps;
        let column_left = to.origin.x + i as f32 * col_w - scroll;
        trees.push(Tree {
            graph,
            dirty: dirty.get(path).copied().unwrap_or(false),
            ends: Ends {
                strip: (from.origin.x + newest, from.origin.y + cy),
                strip_len: newest - l,
                column: (column_left, to.origin.y + body_top),
                column_len: (to.size.height - body_top).max(0.0),
            },
            gone: column_left + col_w <= to.origin.x || column_left >= to.origin.x + to.size.width,
        });
    }
    if trees.is_empty() {
        return false;
    }

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
                trees,
                from,
                to,
                panel_closed: false,
                window_opened: false,
            })
        },
    ) else {
        return false;
    };
    cx.default_global::<MorphSlot>().handle = Some(handle);
    true
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

impl Render for MorphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let elapsed = self.start.elapsed();
        let turn_at = FADE;
        let hold_at = turn_at + TURN;
        let reveal_at = hold_at + HOLD;
        let end = reveal_at + REVEAL;

        if elapsed >= turn_at && !self.panel_closed {
            self.panel_closed = true;
            cx.defer(crate::panel::close);
        }
        if elapsed >= hold_at && !self.window_opened {
            self.window_opened = true;
            cx.defer(view::show_window);
        }
        if elapsed >= end {
            window.remove_window();
            return div();
        }

        let sheet_alpha = ease(phase(elapsed, Duration::ZERO, FADE));
        let e = ease(phase(elapsed, turn_at, TURN));
        let reveal = 1.0 - ease(phase(elapsed, reveal_at, REVEAL));
        let appearance = window.appearance();
        let dark = matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let p = Palette::for_appearance(appearance);
        let s = window.scale_factor();
        let lerp = |a: f32, b: f32| a + (b - a) * e;
        let sheet = Bounds::new(
            point(
                px(lerp(self.from.origin.x, self.to.origin.x) / s),
                px(lerp(self.from.origin.y, self.to.origin.y) / s),
            ),
            size(
                px(lerp(self.from.size.width, self.to.size.width) / s),
                px(lerp(self.from.size.height, self.to.size.height) / s),
            ),
        );
        let trees: Vec<(Arc<crate::repo_graph::RepoGraph>, bool, Ends, f32)> = self
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
