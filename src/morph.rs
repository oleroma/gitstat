//! The flip animation: opening the big window from the panel turns the
//! panel into it, and closing the big window turns it back.
//!
//! The panel and the big window are two OS windows, and resizing a window
//! every frame flickers, so neither moves. A third window, borderless,
//! transparent, click-through and topmost, covers both their places for
//! the length of the flip and draws it. Opening:
//!
//! 1. Fade: a sheet in the panel's colours (top bar, body, bottom bar)
//!    fades in over the panel, hiding what only the panel shows. What both
//!    views show (each repo's tree, icon, name and checkout, the top bar's
//!    words and health bar) is drawn whole over its twin from the start.
//! 2. Slide out, with the fade: the panel's section headings slide out of the top or the
//!    bottom of its list, and its pull requests down into its bottom bar,
//!    drawn by the panel's own elements.
//! 3. Turn: the panel closes; the sheet grows from the panel's place to
//!    the big window's, its bars and title bar with it, each tree turns a
//!    quarter turn from its strip to where its column will be
//!    (`timeline::paint_tree`), and the shared parts move and grow to the
//!    window's. Nothing fades while anything moves.
//! 4. Hold: the big window opens underneath, still covered, and gets a
//!    moment to draw, while its spines slide in from its sides and its pull
//!    requests up out of its bottom bar.
//! 5. Reveal: the sheet's colours fade out, showing what only the big
//!    window shows, then the flip's window closes over the shared parts'
//!    twins.
//!
//! Closing or minimising runs the same backwards: the sheet fades in over
//! the big window, which then closes, everything turns back, the panel
//! opens underneath, the sheet fades to show it, and the panel slides away.
//!
//! Where things land comes from what each window drew last time (`seen_*`),
//! so the first flip after starting gitstat may land a few px off and its
//! shared parts settle at the end. The big window's frame (how far its client
//! area sits inside the bounds it was opened with, maximised or not) is
//! measured once the window has settled and kept in window.json, so the
//! sheet lands on it from the first flip.
//!
//! Words both views show move, growing and changing colour and weight;
//! words that differ between them aren't carried but fade with their own
//! view, before or after the turn, like everything only one view shows.

use crate::config::Section;
use crate::layout::Rect;
use crate::timeline::{COLUMN_WIDTH, Ends};
use crate::view::{self, Palette, Ui};
use crate::win;
use gpui::prelude::FluentBuilder;
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

/// The phases at the default speed (`flip_ms` 560), 450 ms in all:
/// `flip_ms` stretches all but the hold, which is the big window's time to
/// draw. The slide out runs with the fade, the slide in during the hold.
const FADE: Duration = Duration::from_millis(40);
const SLIDE: Duration = Duration::from_millis(80);
const TURN: Duration = Duration::from_millis(230);
const HOLD: Duration = Duration::from_millis(80);
const REVEAL: Duration = Duration::from_millis(100);
/// The panel's and the sheet's rounded corners, logical px.
const RADIUS: f32 = 8.0;
/// Until the big window has been measured: its title bar's height,
/// logical px.
const CAPTION_GUESS: f32 = 32.0;
/// Room left around the sheet for its shadow, logical px.
const SHADOW_ROOM: f32 = 72.0;

/// The shadow Windows 11 draws around an active window, logical px:
/// fitted to its own, measured around the panel and the big window, an
/// even one all round and a deep one below. Drawn around the flip's sheet
/// and the clipped panel, so they hand over to Windows' own unseen.
pub(crate) fn window_shadow() -> [gpui::BoxShadow; 2] {
    [
        gpui::BoxShadow {
            color: gpui::black().opacity(0.33),
            offset: point(px(0.0), px(-0.5)),
            blur_radius: px(12.4),
            spread_radius: px(4.4),
        },
        gpui::BoxShadow {
            color: gpui::black().opacity(0.36),
            offset: point(px(0.0), px(45.0)),
            blur_radius: px(20.2),
            spread_radius: px(0.4),
        },
    ]
}

/// Until the big window has been drawn once: its column header's height,
/// logical px.
const HEADER_GUESS: f32 = 110.0;

/// A piece of a repo's row that its column header shows too, and the
/// flip carries from one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Icon,
    Name,
    Checkout,
    Status,
    /// The top bar both views have, and what's on it: the headline, the
    /// line below it, and the settings and refresh buttons. Kept under an
    /// empty path.
    Bar,
    Headline,
    Subline,
    Settings,
    Refresh,
    /// The bottom bar both views have.
    Footer,
    /// The health bar under the headline, and its three counts.
    Health,
    Count(u8),
    /// The pull requests' block, which slides out of and into the bottom
    /// bar.
    Prs,
    /// A panel section heading, and a big-window spine, which slide out of
    /// and into the body.
    Heading(Section),
    Spine(Section),
    /// A heading's or spine's words and arrow, which turn with it.
    Label(Section),
    Arrow(Section),
}

/// The path the top bar's parts are kept under.
pub fn app() -> &'static Path {
    Path::new("")
}

/// A part as one view last drew it: its bounds in that window's logical px
/// and, for words, the words and how they look.
#[derive(Clone, Debug)]
struct Seen {
    bounds: Bounds<Pixels>,
    text: Option<Words>,
    /// The window's height then, logical px: a part kept to the bottom
    /// (the bottom bar, the pull requests) is placed from there.
    height: f32,
    /// The window's scale then.
    scale: f32,
}

/// Where one view draws a part.
#[derive(Clone, Copy)]
pub enum Side {
    Panel,
    Column,
}

/// How a part's words look: size in px and whether bold.
pub struct Look {
    pub color: Hsla,
    pub size: f32,
    pub bold: bool,
}

thread_local! {
    /// The panel's parts, by repo, as last drawn: panel logical px.
    static PANEL_PARTS: RefCell<HashMap<(PathBuf, Part), Seen>> = RefCell::new(HashMap::new());
    /// The big window's header parts, by repo, as last drawn: window
    /// logical px, beside each header's own top-left corner.
    static COLUMN_PARTS: RefCell<HashMap<(PathBuf, Part), Seen>> = RefCell::new(HashMap::new());
    static HEADER_AT: RefCell<HashMap<PathBuf, gpui::Point<Pixels>>> = RefCell::new(HashMap::new());
    /// Each strip in view, by repo, as the panel last drew it: bounds in
    /// the panel's logical px.
    static STRIPS: RefCell<HashMap<PathBuf, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    /// The column body's top below the big window's client top, physical
    /// px, as last drawn.
    static BODY_TOP: Cell<Option<f32>> = const { Cell::new(None) };
    /// The big window's client area minus the bounds it was opened with,
    /// physical px per edge (left, top, right, bottom), once measured.
    static CLIENT_DELTA: Cell<Option<Delta>> = const { Cell::new(None) };
    /// The same for a maximised big window, against its monitor's work area.
    static MAXIMIZED_DELTA: Cell<Option<Delta>> = const { Cell::new(None) };
    /// The big window's title bar height, physical px, normal and
    /// maximised, once measured.
    static CAPTION: Cell<Option<Delta>> = const { Cell::new(None) };
    static MAXIMIZED_CAPTION: Cell<Option<Delta>> = const { Cell::new(None) };
    /// When the big window last reached the place and state it opened
    /// with (a maximised one shows at its normal size first).
    static SETTLED: Cell<Option<Instant>> = const { Cell::new(None) };
    /// How far the panel's list was scrolled and could be, logical px, as
    /// last drawn.
    static LIST_SCROLL: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
}

/// Notes how far the panel's list is scrolled and could be, logical px.
pub fn seen_list_scroll(at: f32, max: f32) {
    LIST_SCROLL.set((at, max));
}

/// Notes a strip the panel drew, for the flip.
pub fn seen_strip(path: &Path, bounds: Bounds<Pixels>) {
    STRIPS.with(|s| {
        s.borrow_mut().insert(path.to_path_buf(), bounds);
    });
}

/// An invisible layer, absolutely over its parent (which must be
/// `relative`), that notes where `side` drew `path`'s `part`, and its words
/// in `look` when it has words.
pub fn tag(
    side: Side,
    path: &Path,
    part: Part,
    words: Option<(gpui::SharedString, Look)>,
) -> impl IntoElement {
    let path = path.to_path_buf();
    let text = words.map(|(t, l)| (t, l.color, l.size, l.bold));
    canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            // The flip draws some of the views' own elements as they
            // slide: those aren't where the views draw them.
            let id = window.window_handle().window_id();
            if cx
                .try_global::<MorphSlot>()
                .and_then(|m| m.handle)
                .is_some_and(|h| h.window_id() == id)
            {
                return;
            }
            if part == Part::Bar {
                let paint = match side {
                    Side::Panel => &PANEL_PAINT,
                    Side::Column => &COLUMN_PAINT,
                };
                paint.with(|t| t.set(Some(Instant::now())));
            }
            let seen = Seen {
                bounds,
                text: text.clone(),
                height: f32::from(window.viewport_size().height),
                scale: window.scale_factor(),
            };
            let map = match side {
                Side::Panel => &PANEL_PARTS,
                Side::Column => &COLUMN_PARTS,
            };
            map.with(|m| {
                m.borrow_mut().insert((path.clone(), part), seen);
            });
        },
    )
    .absolute()
    .inset_0()
}

/// An invisible layer over a column header noting where its top-left
/// corner is, which its parts are placed beside.
pub fn tag_header(path: &Path) -> impl IntoElement {
    let path = path.to_path_buf();
    canvas(
        |_, _, _| {},
        move |bounds, _, _, _| {
            HEADER_AT.with(|m| {
                m.borrow_mut().insert(path.clone(), bounds.origin);
            });
        },
    )
    .absolute()
    .inset_0()
}

/// Forgets the strips and the panel's parts; the panel is opening afresh.
pub fn clear_strips() {
    STRIPS.with(|s| s.borrow_mut().clear());
    PANEL_PARTS.with(|s| s.borrow_mut().clear());
}

/// Forgets the big window's header parts; it is opening afresh, and a
/// flip back must never land on where an earlier one drew them.
pub fn clear_column_parts() {
    COLUMN_PARTS.with(|s| s.borrow_mut().clear());
    HEADER_AT.with(|s| s.borrow_mut().clear());
}

/// Notes a column body the big window drew, for the flip.
pub fn seen_column(body: Bounds<Pixels>, window: &Window) {
    let top = f32::from(body.top()) * window.scale_factor();
    BODY_TOP.with(|t| t.set(Some(top)));
}

/// A client area minus the bounds asked for, physical px per edge (left,
/// top, right, bottom).
pub type Delta = (i32, i32, i32, i32);

/// Notes where the big window's client area is for the bounds it was
/// opened with (or, maximised, its monitor's work area), both physical px.
/// Only once the window has settled into that state: a maximised window
/// measured before it maximises would send the flip to its normal size.
/// With its title bar's height, physical px, when known.
pub fn seen_client(requested: Rect, client: Rect, maximized: bool, caption: Option<Delta>) {
    let delta = if maximized {
        &MAXIMIZED_DELTA
    } else {
        &CLIENT_DELTA
    };
    if caption.is_some() {
        let cell = if maximized {
            &MAXIMIZED_CAPTION
        } else {
            &CAPTION
        };
        cell.with(|c| c.set(caption));
    }
    let d = (
        client.left - requested.left,
        client.top - requested.top,
        client.right - requested.right,
        client.bottom - requested.bottom,
    );
    // A frame is a few dozen px at most; anything more is a window caught
    // between states.
    let sane = [d.0, d.1, d.2, d.3].iter().all(|v| v.abs() <= 120);
    if sane {
        delta.with(|c| c.set(Some(d)));
    }
}

/// Both frames as measured, (normal, maximised), for window.json.
pub fn deltas() -> (Option<Delta>, Option<Delta>) {
    (
        CLIENT_DELTA.with(Cell::get),
        MAXIMIZED_DELTA.with(Cell::get),
    )
}

/// Both title bar heights as measured, (normal, maximised), for window.json.
pub fn captions() -> (Option<Delta>, Option<Delta>) {
    (CAPTION.with(Cell::get), MAXIMIZED_CAPTION.with(Cell::get))
}

/// Title bar heights kept from an earlier run, where none is measured yet.
pub fn learn_captions(normal: Option<Delta>, maximized: Option<Delta>) {
    for (cell, kept) in [(&CAPTION, normal), (&MAXIMIZED_CAPTION, maximized)] {
        cell.with(|c| {
            if c.get().is_none() {
                c.set(kept);
            }
        });
    }
}

/// Frames kept from an earlier run, where none is measured yet this run.
pub fn learn_deltas(normal: Option<Delta>, maximized: Option<Delta>) {
    for (cell, kept) in [(&CLIENT_DELTA, normal), (&MAXIMIZED_DELTA, maximized)] {
        cell.with(|c| {
            if c.get().is_none() {
                c.set(kept);
            }
        });
    }
}

/// The big window has reached the place and state it was opened with.
pub fn big_settled() {
    SETTLED.with(|s| s.set(Some(Instant::now())));
}

/// Something asked for while a flip runs, done once it has ended: the
/// windows the flip is turning between are its own until then, so nothing
/// opens, closes or moves them under it.
#[derive(Clone)]
enum Pending {
    /// Open the big window (the tray, the badge), through the panel.
    ShowWindow(crate::panel::Anchor),
    /// Open the big window from the panel the flip revealed, at a repo.
    FromPanel(Option<PathBuf>),
    /// Open or close the panel (the tray, the badge).
    TogglePanel(crate::panel::Anchor),
    /// The panel the flip ends in goes again (Esc, a click elsewhere).
    PanelLeaves,
    /// The big window the flip ends in closes again (its close button).
    WindowCloses,
}

#[derive(Default)]
struct MorphSlot {
    handle: Option<WindowHandle<MorphView>>,
    /// Its HWND, which a window opening under it is placed below.
    hwnd: Option<isize>,
    /// When it opened.
    since: Option<Instant>,
    /// Waiting for the trees to glide back to their newest commits (or the
    /// panel to be laid out) before it starts, since then.
    waiting: Option<Instant>,
    /// A flip window opened ahead, hidden, while the views glide home.
    spare: Option<WindowHandle<MorphView>>,
    /// The window the flip ends in, cloaked until the reveal, when it
    /// takes over the shadow from the sheet's in one frame.
    ending: Option<isize>,
    /// Which way the flip runs.
    direction: Option<Direction>,
    /// What was asked for meanwhile, in order.
    pending: Vec<Pending>,
}

/// Opens a hidden flip window ahead of the flip, so starting it needn't
/// wait for a window to open.
fn open_spare(cx: &mut App) {
    if cx.default_global::<MorphSlot>().spare.is_some() {
        return;
    }
    let area = win::primary_monitor().rect;
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(crate::panel::logical(area))),
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
                cx.spawn(async move |_| unsafe { win::style_morph(h) })
                    .detach();
            }
            cx.new(|_| MorphView::new(None, Direction::Open, 1.0, None))
        },
    );
    cx.default_global::<MorphSlot>().spare = handle.ok();
}

/// Closes a spare flip window that no flip took.
fn drop_spare(cx: &mut App) {
    if let Some(spare) = cx.default_global::<MorphSlot>().spare.take() {
        view::remove(spare.into(), cx);
    }
}

impl Global for MorphSlot {}

/// Whether a flip is drawing now, or about to.
pub fn running(cx: &App) -> bool {
    cx.try_global::<MorphSlot>().is_some_and(|s| {
        s.waiting.is_some_and(|t| t.elapsed() < STUCK)
            || s.handle.is_some_and(|h| view::is_live(h.into(), cx))
                && s.since.is_some_and(|t| t.elapsed() < STUCK)
    })
}

/// Keeps `what` for when the flip under way has ended.
fn queue(what: Pending, cx: &mut App) {
    cx.default_global::<MorphSlot>().pending.push(what);
}

/// The flip has ended (or never started): does what was asked meanwhile.
fn run_pending(cx: &mut App) {
    let pending = std::mem::take(&mut cx.default_global::<MorphSlot>().pending);
    for what in pending {
        // One may start another flip: the rest wait for that one.
        if running(cx) {
            queue(what, cx);
            continue;
        }
        match what {
            Pending::ShowWindow(anchor) => show_window(anchor, cx),
            Pending::FromPanel(focus) => {
                if let Some(handle) = cx
                    .try_global::<crate::panel::PanelSlot>()
                    .and_then(|s| s.handle())
                {
                    handle
                        .update(cx, |_, window, cx| open_window(window, focus, cx))
                        .ok();
                }
            }
            Pending::TogglePanel(anchor) => crate::panel::toggle(anchor, cx),
            Pending::PanelLeaves => crate::panel::slide_away(cx),
            Pending::WindowCloses => {
                if let Some(handle) = view::window_handle(cx) {
                    handle
                        .update(cx, |_, window, cx| {
                            if !flip_back(window, cx) {
                                window.remove_window();
                            }
                        })
                        .ok();
                }
            }
        }
    }
}

/// The panel is asked to toggle (the tray, the badge): true when a flip
/// under way takes that for after it ends.
pub fn hold_toggle(anchor: crate::panel::Anchor, cx: &mut App) -> bool {
    if !running(cx) {
        return false;
    }
    queue(Pending::TogglePanel(anchor), cx);
    true
}

/// The panel is to go (Esc, a click elsewhere, focus lost): true when a
/// flip under way owns it, and lets it go once that ends. A panel the flip
/// starts from goes with the flip anyway; one it ends in goes after.
pub fn hold_leave(cx: &mut App) -> bool {
    let Some(slot) = cx.try_global::<MorphSlot>() else {
        return false;
    };
    if !running(cx) {
        return false;
    }
    // Waiting to start from the panel: the flip is called off, and the
    // panel goes as asked.
    if slot.handle.is_none() && slot.direction == Some(Direction::Open) {
        cx.default_global::<MorphSlot>().direction = None;
        return false;
    }
    let ends_in_panel = matches!(slot.direction, Some(Direction::Close | Direction::Minimize));
    if ends_in_panel {
        let slot = cx.default_global::<MorphSlot>();
        if !slot
            .pending
            .iter()
            .any(|p| matches!(p, Pending::PanelLeaves))
        {
            slot.pending.push(Pending::PanelLeaves);
        }
    }
    true
}

/// How long the trees take to scroll back to their newest commits before
/// a flip, at the default speed.
const SCROLL_HOME: Duration = Duration::from_millis(260);

/// `flip_ms` over the default, for the phases that stretch with it.
fn stretch(cx: &App) -> f32 {
    let ms = cx
        .try_global::<Ui>()
        .map_or(560, |ui| ui.state.read(cx).config.flip_ms);
    let fixed = HOLD.as_millis() as f32;
    ((ms as f32 - fixed) / (560.0 - fixed)).clamp(0.2, 12.0)
}

/// Glides every tree back to its newest commits, where the flip starts
/// from: true when they were elsewhere, after which `then` runs once
/// they're there and drawn there.
fn newest_first(direction: Direction, then: impl FnOnce(&mut App) + 'static, cx: &mut App) -> bool {
    let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) else {
        return false;
    };
    let from_panel = direction == Direction::Open;
    // A scroll you can follow, paced with the flip: the trees to their
    // newest commits, and the view the flip starts from to its start, the
    // panel's list to its top or the window's table to its left edge (and
    // its pull requests to their top).
    let wait = SCROLL_HOME.mul_f32(stretch(cx));
    let trees = t.update(cx, |t, cx| t.glide_newest(wait, cx));
    let view = view::glide_to_start(from_panel, wait, cx);
    if !trees && !view {
        return false;
    }
    begin_wait(direction, cx);
    open_spare(cx);
    after_glides(wait, then, cx);
    true
}

/// Marks the flip as about to start, so nothing else moves its windows.
fn begin_wait(direction: Direction, cx: &mut App) {
    let slot = cx.default_global::<MorphSlot>();
    slot.waiting = Some(Instant::now());
    slot.direction = Some(direction);
}

/// Runs `then` once the glides started for `over` have ended and the
/// views have drawn their last step: polled each frame or so, never on a
/// guess of when that is.
fn after_glides(over: Duration, then: impl FnOnce(&mut App) + 'static, cx: &mut App) {
    let start = Instant::now();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(over).await;
        loop {
            let done = cx
                .update(|cx| {
                    let gliding = view::gliding()
                        || cx
                            .try_global::<Ui>()
                            .is_some_and(|ui| ui.timelines.read(cx).gliding());
                    let drawn = view::last_glide_end()
                        .is_none_or(|end| last_paint().is_some_and(|t| t > end));
                    (!gliding && drawn) || start.elapsed() > over + DRAW_WAIT
                })
                .unwrap_or(true);
            if done {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(4))
                .await;
        }
        cx.update(|cx| {
            let called_off = cx.default_global::<MorphSlot>().direction.is_none();
            cx.default_global::<MorphSlot>().waiting = None;
            if called_off {
                drop_spare(cx);
                run_pending(cx);
                return;
            }
            then(cx);
            // `then` may have waited again, for the spare.
            if cx.default_global::<MorphSlot>().waiting.is_none() {
                drop_spare(cx);
            }
            if !running(cx) {
                run_pending(cx);
            }
        })
        .ok();
    })
    .detach();
}

/// The flip's HWND while it draws.
pub fn flip_hwnd(cx: &App) -> Option<isize> {
    if !running(cx) {
        return None;
    }
    cx.try_global::<MorphSlot>()?.hwnd
}

/// Longer than any flip runs: one still around after this is stuck, and
/// no longer holds up the windows.
const STUCK: Duration = Duration::from_secs(12);

fn enabled(cx: &App) -> bool {
    cx.try_global::<Ui>().is_some_and(|ui| {
        let cfg = &ui.state.read(cx).config;
        cfg.flip_animation && cfg.window && cfg.panel
    })
}

/// A filter typed in one view would be cleared by the other as it opens:
/// cleared before the flip instead, so the flip turns what that one shows.
fn clear_filter(cx: &mut App) {
    if let Some(state) = cx.try_global::<Ui>().map(|ui| ui.state.clone())
        && !state.read(cx).filter.is_empty()
    {
        state.update(cx, |s, cx| {
            s.filter.clear();
            cx.notify();
        });
    }
}

/// Opens the big window from the panel, scrolled to `focus` if given, with
/// the flip when it's on. Called from the panel's own handlers; closes the
/// panel either way.
pub fn open_window(panel: &mut Window, focus: Option<PathBuf>, cx: &mut App) {
    if running(cx) {
        // The panel a flip back is revealing: once that flip has ended.
        let back = cx
            .try_global::<MorphSlot>()
            .is_some_and(|s| matches!(s.direction, Some(Direction::Close | Direction::Minimize)));
        if back {
            queue(Pending::FromPanel(focus), cx);
        }
        return;
    }
    // A panel still sliding in finishes first, so the flip starts from it
    // where it rests; one sliding away is going, and takes the click with it.
    match crate::panel::slide_state(cx) {
        crate::panel::SlideState::Leaving => return,
        crate::panel::SlideState::Arriving(left) => {
            let handle = panel.window_handle();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(left + Duration::from_millis(20))
                    .await;
                handle
                    .update(cx, |_, window, cx| open_window(window, focus, cx))
                    .ok();
            })
            .detach();
            return;
        }
        crate::panel::SlideState::Still => {}
    }
    if let Some(path) = &focus
        && let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone())
    {
        t.update(cx, |t, _| t.focus(path));
    }
    let flip = enabled(cx) && !view::window_open(cx);
    if flip {
        clear_filter(cx);
        let handle = panel.window_handle();
        let again = focus.clone();
        if newest_first(
            Direction::Open,
            move |cx| {
                // Dismissed meanwhile: it goes, and nothing opens.
                if !crate::panel::is_open(cx) || crate::panel::is_leaving(cx) {
                    return;
                }
                handle
                    .update(cx, |_, window, cx| open_window(window, again, cx))
                    .ok();
            },
            cx,
        ) {
            panel.refresh();
            return;
        }
    }
    let started = flip
        && win::hwnd(panel)
            .and_then(win::client_rect)
            .and_then(|rect| {
                let plan = plan(rect, None, focus.as_deref(), cx)?;
                Some(launch(plan, Direction::Open, win::hwnd(panel), cx))
            })
            .unwrap_or(false);
    if started {
        // Its hovered row and buttons lose their hover as it starts, in
        // the frame before the sheet comes, not while it fades.
        panel.refresh();
    } else {
        crate::panel::dismiss(panel, cx);
        view::show_window(cx);
    }
}

/// Opens the big window from the tray or the badge: through the panel at
/// `anchor` when the flip is on, else straight away. During a flip, once
/// it has ended.
pub fn show_window(anchor: crate::panel::Anchor, cx: &mut App) {
    if running(cx) {
        queue(Pending::ShowWindow(anchor), cx);
        return;
    }
    if !enabled(cx) || view::window_open(cx) {
        if !view::window_open(cx) {
            crate::panel::close(cx);
        }
        view::show_window(cx);
        return;
    }
    crate::panel::then_window(anchor, cx);
}

/// Closing the big window: starts the flip back into the panel (which then
/// slides away, or stays when the window was minimised) and returns
/// true, leaving the window for the flip to close; false when the flip is
/// off or can't run, so the caller closes it.
pub fn flip_back(big: &mut Window, cx: &mut App) -> bool {
    let minimized = win::take_minimized();
    if running(cx) {
        // Opening into it: closed once it has opened. Closing already:
        // the flip closes it.
        let opening = cx
            .try_global::<MorphSlot>()
            .is_some_and(|s| s.direction == Some(Direction::Open));
        if opening
            && !cx
                .default_global::<MorphSlot>()
                .pending
                .iter()
                .any(|p| matches!(p, Pending::WindowCloses))
        {
            queue(Pending::WindowCloses, cx);
        }
        return true;
    }
    // Opens there again next time.
    view::remember_place(big, cx);
    if !enabled(cx) || crate::panel::guess_place(cx).is_none() {
        return false;
    }
    clear_filter(cx);
    // The columns back at their newest commits first; then the panel laid
    // out, hidden, where the flip lands; then the flip, or, should it not
    // start, the window just closes.
    let handle = big.window_handle();
    let start = move |cx: &mut App| {
        let started = handle
            .update(cx, |_, window, cx| flip_back_now(window, minimized, cx))
            .unwrap_or(false);
        if !started {
            crate::panel::close(cx);
            view::close_window(cx);
        }
    };
    if newest_first(
        if minimized {
            Direction::Minimize
        } else {
            Direction::Close
        },
        move |cx| panel_first(start, cx),
        cx,
    ) {
        return true;
    }
    begin_wait(
        if minimized {
            Direction::Minimize
        } else {
            Direction::Close
        },
        cx,
    );
    panel_first(start, cx);
    true
}

/// Opens the panel the flip back lands in, cloaked, and runs `then` once
/// it is laid out and drawn as it now is, so the flip lands on it exactly.
fn panel_first(then: impl FnOnce(&mut App) + 'static, cx: &mut App) {
    let opened = Instant::now();
    if !crate::panel::reopen_cloaked(cx) {
        cx.default_global::<MorphSlot>().waiting = None;
        then(cx);
        return;
    }
    cx.default_global::<MorphSlot>().waiting = Some(opened);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(4))
                .await;
            let ready = cx
                .update(|cx| {
                    let fitted = crate::panel::fitted_at(cx).unwrap_or(opened);
                    PANEL_PAINT.with(Cell::get).is_some_and(|t| t > fitted)
                        || opened.elapsed() > DRAW_WAIT
                })
                .unwrap_or(true);
            if ready {
                break;
            }
        }
        cx.update(|cx| {
            cx.default_global::<MorphSlot>().waiting = None;
            then(cx);
            if !running(cx) {
                drop_spare(cx);
                run_pending(cx);
            }
        })
        .ok();
    })
    .detach();
}

/// Starts the flip back into the panel, the trees at their newest and the
/// panel laid out, cloaked, where the flip lands.
fn flip_back_now(big: &mut Window, minimized: bool, cx: &mut App) -> bool {
    let Some(panel_rect) = crate::panel::last_rect(cx) else {
        return false;
    };
    let Some(big_rect) = win::hwnd(big).and_then(win::client_rect) else {
        return false;
    };
    let ws = big.scale_factor();
    let scroll = view::table_scroll(cx) * ws;
    let Some(plan) = plan(panel_rect, Some((big_rect, scroll, ws)), None, cx) else {
        return false;
    };
    let direction = if minimized {
        Direction::Minimize
    } else {
        Direction::Close
    };
    launch(plan, direction, win::hwnd(big), cx)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Panel to big window.
    Open,
    /// Big window to panel, which then slides away.
    Close,
    /// Big window to panel, which stays.
    Minimize,
}

/// One repo's tree in the flip.
struct Tree {
    graph: Arc<crate::repo_graph::RepoGraph>,
    dirty: bool,
    /// Physical px, from the flip window's top-left corner.
    ends: Ends,
    /// Where the panel draws its strip, and the big window its column's
    /// body, physical px from the flip window's corner.
    strip: Bounds<f32>,
    column: Bounds<f32>,
    /// How wide the column's tree is, from its left edge, physical px.
    lanes: f32,
    /// Where the strip has what only the panel shows (tags and stashes),
    /// physical px.
    marks: Vec<Bounds<f32>>,
}

/// A part's words: text, colour, size in px, and whether bold.
type Words = (gpui::SharedString, Hsla, f32, bool);

/// Words as drawn this frame: text, colour, size in px and weight.
type Line = (gpui::SharedString, Hsla, f32, f32);

/// A carried part this frame.
struct Drawn {
    bounds: Bounds<Pixels>,
    image: Option<Arc<gpui::RenderImage>>,
    words: Option<Line>,
    kind: Kind,
}

/// What a carried part is.
#[derive(Clone)]
enum Kind {
    Words,
    /// Words centred in their bounds, as a button's glyph.
    Glyph,
    /// The repo's icon, by its tone.
    Icon(Box<crate::git::RepoStatus>),
    /// The health bar: synced, local-work and needs-a-look repos.
    Health([usize; 3]),
}

/// A part carried from a panel row to a column header, physical px from
/// the flip window's top-left corner.
struct Carried {
    from: Bounds<f32>,
    to: Bounds<f32>,
    /// The tree it rides with, by its place in the plan's trees: it turns
    /// about that tree's newest commit as the tree does, rather than
    /// sliding straight across.
    tree: Option<usize>,
    /// The words at each end: text, colour, size and boldness.
    words: Option<(Words, Words)>,
    kind: Kind,
}

/// Everything a flip draws, laid out.
struct Plan {
    /// The flip window's place, physical px.
    union: Rect,
    /// The panel's visible frame, rim and all, physical px from the flip
    /// window's top-left corner: where the sheet starts or ends.
    from_frame: Bounds<f32>,
    /// The big window's client area, physical px from the
    /// flip window's top-left corner.
    to: Bounds<f32>,
    /// The big window's title bar height, physical px: the sheet grows
    /// over it too, so the bar never pops in at the end.
    /// How far its visible frame reaches past its client area: left,
    /// top (the title bar), right, bottom, physical px.
    caption: (f32, f32, f32, f32),
    /// The big window is maximised (or will be): it has no rim.
    maximized: bool,
    /// The work areas the flip may draw in, one per monitor, physical px
    /// from its top-left corner: never over a taskbar, which the windows'
    /// own shadows stay under.
    work: Vec<Bounds<f32>>,
    /// The rim's thickness on the panel and on the big window, physical
    /// px, as Windows draws each on its monitor.
    rims: (f32, f32),
    /// The top bar and the bottom bar in each view, physical px.
    bar: Option<(Bounds<f32>, Bounds<f32>)>,
    footer: Option<(Bounds<f32>, Bounds<f32>)>,
    trees: Vec<Tree>,
    parts: Vec<Carried>,
    sliders: Vec<Slider>,
    turning: Vec<Turning>,
    /// Where the view the flip ends in is to be scrolled, logical px: the
    /// big window's table sideways, or the panel's list down.
    table_to: Option<f32>,
    list_to: Option<f32>,
    /// Where the table then scrolls, after the flip, to bring a repo asked
    /// for into sight, logical px.
    focus_to: Option<f32>,
}

/// A turning heading this frame, in logical px: still, on whole px, at
/// either end, or its four corners part way.
struct Bar {
    shape: Shape,
    wash: Hsla,
    /// The spine's edge line, at the spine's end.
    border: Option<Bounds<Pixels>>,
    label: Option<BarWords>,
    arrow: Option<BarWords>,
}

enum Shape {
    Still(Bounds<Pixels>),
    /// Its corners, and its edge line's with that line's colour.
    Turning(
        [gpui::Point<Pixels>; 4],
        Option<([gpui::Point<Pixels>; 4], Hsla)>,
    ),
}

/// A turning heading's words this frame.
enum BarWords {
    /// As the panel sets them.
    Upright(Bounds<Pixels>, Line),
    /// As the spine draws them: its box, text, size and colour.
    Turned(Bounds<Pixels>, String, f32, Hsla),
    /// Part way: text, size, baseline start, reading direction, the way
    /// up, colour.
    Path(String, f32, (f32, f32), (f32, f32), (f32, f32), Hsla),
}

fn fill_quad(window: &mut Window, corners: &[gpui::Point<Pixels>; 4], color: Hsla) {
    let mut path = gpui::PathBuilder::fill();
    path.move_to(corners[0]);
    for c in &corners[1..] {
        path.line_to(*c);
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn paint_bar(window: &mut Window, cx: &mut App, bar: &Bar) {
    match &bar.shape {
        Shape::Still(b) => window.paint_quad(gpui::fill(*b, bar.wash)),
        Shape::Turning(corners, line) => {
            fill_quad(window, corners, bar.wash);
            if let Some((line, color)) = line {
                fill_quad(window, line, *color);
            }
        }
    }
    if let Some(b) = bar.border {
        window.paint_quad(gpui::fill(b, window_border(window)));
    }
    for words in bar.label.iter().chain(&bar.arrow) {
        match words {
            BarWords::Upright(b, line) => paint_words(window, cx, *b, line, false),
            BarWords::Turned(b, text, size, color) => {
                crate::turned_text::paint(window, *b, text, *size, *color)
            }
            BarWords::Path(text, size, start, dir, up, color) => {
                crate::glyph_path::paint(window, text, *size, *start, *dir, *up, *color)
            }
        }
    }
}

/// The views' edge line colour, for the spine's.
fn window_border(window: &Window) -> Hsla {
    Palette::for_appearance(window.appearance()).border
}

/// A panel section heading that turns with the trees into its spine in
/// the big window, physical px from the flip window's corner. Only its
/// wash turns: each end's words fade with its own view, before or after.
struct Turning {
    section: Section,
    from: Bounds<f32>,
    to: Bounds<f32>,
    folded: bool,
    /// Its words, the same in both, and where each view has them.
    label: Option<TurnWords>,
    /// Its arrow's box in each.
    arrow: Option<(Bounds<f32>, Bounds<f32>)>,
}

/// A turning heading's words: text, colour, size and box at each end.
struct TurnWords {
    text: gpui::SharedString,
    color: Hsla,
    size: (f32, f32),
    at: (Bounds<f32>, Bounds<f32>),
}

/// A bar that slides out of its view before the turn, or into it after:
/// the pull requests into and out of the bottom bar, the panel's section
/// headings out of the top or the bottom of its list, the window's spines
/// out of its sides. The flip draws the view's own element for it.
struct Slider {
    what: Slid,
    /// Where its view draws it, physical px from the flip window's corner.
    at: Bounds<f32>,
    /// How far it moves to be out of sight, physical px.
    away: (f32, f32),
}

#[derive(Clone, Copy, PartialEq)]
enum Slid {
    /// The pull requests' block, in the panel or the big window.
    Prs(view::Mode),
    Heading(Section),
    /// A spine: its section, count and whether folded.
    Spine(Section, usize, bool),
}

/// Lays out a flip between the panel at `panel_rect` (client area,
/// physical px) and the big window: at `big`'s client rect with its table
/// scrolled that far (physical px) and drawn at its scale, or where
/// show_window will open it, scrolled to `focus`. None when nothing would
/// turn.
fn plan(
    panel_rect: Rect,
    big: Option<(Rect, f32, f32)>,
    focus: Option<&Path>,
    cx: &App,
) -> Option<Plan> {
    let ui = cx.try_global::<Ui>()?;
    let state = ui.state.read(cx);
    let timelines = ui.timelines.read(cx);
    let mut strips = STRIPS.with(|s| s.borrow().clone());
    let open = big.is_none();
    // Each view's own scale: the panel's as it last drew (its monitor's),
    // the big window's as it is, or as the monitor it will open on has it.
    let ps = PANEL_PARTS
        .with(|m| {
            m.borrow()
                .get(&(app().to_path_buf(), Part::Bar))
                .map(|s| s.scale)
        })
        .unwrap_or_else(|| {
            let (x, y) = panel_rect.centre();
            view::sane_scale(win::monitor_at(x, y).scale)
        });

    let cells = view::table_cells(state);
    let lefts = view::cell_lefts(&cells);
    let table: f32 = cells.iter().map(view::Cell::width).sum();
    let (target, ws) = match big {
        Some((rect, _, ws)) => (rect, ws),
        None => {
            // Where show_window will put it: GPUI places it at its bounds
            // in the primary monitor's scale, and a monitor of another
            // scale then grows or shrinks it about its top-left corner,
            // to draw at that monitor's scale.
            let (place, primary) = view::window_place(table, cx);
            let bounds = place.get_bounds();
            let at = |v: Pixels| (f32::from(v) * primary).round() as i32;
            let (area, delta) = match place {
                WindowBounds::Maximized(_) => (
                    view::maximized_area(bounds, primary),
                    MAXIMIZED_DELTA.with(Cell::get).unwrap_or_default(),
                ),
                _ => (
                    Rect::new(
                        at(bounds.origin.x),
                        at(bounds.origin.y),
                        at(bounds.origin.x + bounds.size.width),
                        at(bounds.origin.y + bounds.size.height),
                    ),
                    CLIENT_DELTA.with(Cell::get).unwrap_or_default(),
                ),
            };
            let (cx_, cy_) = area.centre();
            let ws = view::sane_scale(win::monitor_at(cx_, cy_).scale);
            let area = match place {
                WindowBounds::Maximized(_) => area,
                _ if (ws - primary).abs() > 0.01 => Rect::new(
                    area.left,
                    area.top,
                    area.left + (f32::from(bounds.size.width) * ws).round() as i32,
                    area.top + (f32::from(bounds.size.height) * ws).round() as i32,
                ),
                _ => area,
            };
            let (dl, dt, dr, db) = delta;
            (
                Rect::new(
                    area.left + dl,
                    area.top + dt,
                    area.right + dr,
                    area.bottom + db,
                ),
                ws,
            )
        }
    };

    // The big window's title bar, above its client area.
    let maximized = matches!(view::window_place(0.0, cx).0, WindowBounds::Maximized(_));
    let caption = if maximized {
        MAXIMIZED_CAPTION.with(Cell::get)
    } else {
        CAPTION.with(Cell::get)
    }
    .unwrap_or((0, (CAPTION_GUESS * ws).round() as i32, 0, 0));
    // Room around both for the sheet's shadow.
    let m = (SHADOW_ROOM * ps.max(ws)).ceil() as i32;
    let union = Rect::new(
        panel_rect.left.min(target.left - caption.0) - m,
        panel_rect.top.min(target.top - caption.1) - m,
        panel_rect.right.max(target.right + caption.2) + m,
        panel_rect.bottom.max(target.bottom + caption.3) + m,
    );
    let local = |r: Rect| Bounds {
        origin: point((r.left - union.left) as f32, (r.top - union.top) as f32),
        size: size(r.width() as f32, r.height() as f32),
    };
    let (from, to) = (local(panel_rect), local(target));
    // The panel's visible frame: its rim, which Windows draws just outside
    // its client area, too, as thick as on the panel's monitor.
    let rims = (view::rim_width(ps).round(), view::rim_width(ws).round());
    let rim = rims.0;
    let from_frame = Bounds {
        origin: point(from.origin.x - rim, from.origin.y - rim),
        size: size(from.size.width + 2.0 * rim, from.size.height + 2.0 * rim),
    };
    // Every monitor's work area the flip reaches into, each on its own:
    // never over a taskbar, wherever the monitors sit.
    let work: Vec<Bounds<f32>> = win::work_areas()
        .into_iter()
        .filter_map(|w| {
            let w = Rect::new(
                w.left.max(union.left),
                w.top.max(union.top),
                w.right.min(union.right),
                w.bottom.min(union.bottom),
            );
            (w.width() > 0 && w.height() > 0).then(|| local(w))
        })
        .collect();
    // In the panel, logical px to the flip window's physical px.
    let in_panel = |b: &Bounds<Pixels>| Bounds {
        origin: point(
            from.origin.x + f32::from(b.left()) * ps,
            from.origin.y + f32::from(b.top()) * ps,
        ),
        size: size(f32::from(b.size.width) * ps, f32::from(b.size.height) * ps),
    };

    // The table's scroll: as it is, or bringing the focused column in.
    let mut scroll = match big {
        Some((_, scroll, _)) => scroll,
        None => focus
            .and_then(|f| view::cell_of(&cells, f))
            .map_or(0.0, |i| {
                ((lefts[i] + COLUMN_WIDTH) * ws - to.size.width).max(0.0)
            }),
    };
    let body_top = BODY_TOP.with(Cell::get).unwrap_or(HEADER_GUESS * ws);

    let mut trees = Vec::new();
    let mut parts = Vec::new();
    let mut spines = Vec::new();
    let mut panel_parts = PANEL_PARTS.with(|m| m.borrow().clone());
    let column_parts = COLUMN_PARTS.with(|m| m.borrow().clone());
    let header_at = HEADER_AT.with(|m| m.borrow().clone());

    // Each repo's strip in the panel: as the panel last drew it, or, for
    // one its list didn't show, rows on from the nearest one it did, so
    // every tree turns, out of sight or not.
    let repo_cells: Vec<&crate::git::RepoStatus> = cells
        .iter()
        .filter_map(|c| match c {
            view::Cell::Repo(s) => Some(*s),
            view::Cell::Spine(..) => None,
        })
        .collect();
    let known: Vec<Option<Bounds<Pixels>>> = repo_cells
        .iter()
        .map(|s| strips.get(&s.path).copied())
        .collect();
    let mut strip_of: HashMap<&Path, Bounds<Pixels>> = repo_cells
        .iter()
        .zip(fill_rows(&known))
        .filter_map(|(s, b)| Some((s.path.as_path(), b?)))
        .collect();

    // One view's scroll lined up with the other's. Opening, the table
    // starts at the panel's topmost repo in view, with its section's spine
    // when that heads it; closing, the panel's list starts at the window's
    // leftmost column in view.
    let (list_scroll, list_max) = LIST_SCROLL.with(Cell::get);
    let list_top = panel_parts
        .get(&(app().to_path_buf(), Part::Bar))
        .map_or(0.0, |b| f32::from(b.bounds.bottom()));
    let cell_of_repo = |path: &Path| {
        cells
            .iter()
            .position(|c| matches!(c, view::Cell::Repo(s) if s.path == path))
    };
    let mut table_to = None;
    let mut list_to = None;
    let mut focus_to = None;
    match big {
        None => {
            let topmost = repo_cells
                .iter()
                .filter_map(|s| Some((*s, strips.get(&s.path)?)))
                .filter(|(_, b)| f32::from(b.origin.y) + f32::from(b.size.height) / 2.0 >= list_top)
                .min_by(|a, b| f32::from(a.1.origin.y).total_cmp(&f32::from(b.1.origin.y)))
                .map(|(s, _)| s);
            let total = lefts
                .last()
                .zip(cells.last())
                .map_or(0.0, |(l, c)| l + c.width());
            let wide = to.size.width / ws;
            // With the spines just before it: its own section's, and any
            // folded ones above it, which the panel shows as headings.
            let x = match topmost.and_then(|s| cell_of_repo(&s.path)) {
                Some(i) => lefts[first_of_run(&cells, i)],
                None => 0.0,
            };
            let x = x.min(total - wide).max(0.0);
            // A repo asked for scrolls into sight once the flip is over, as
            // little moved as it takes: the flip itself lands where the
            // flip back starts from, so the two are each other reversed.
            if let Some(i) = focus.and_then(|f| view::cell_of(&cells, f)) {
                let mut to = x;
                if lefts[i] < to {
                    to = lefts[i];
                } else if lefts[i] + COLUMN_WIDTH > to + wide {
                    to = lefts[i] + COLUMN_WIDTH - wide;
                }
                let to = to.min(total - wide).max(0.0);
                if (to - x).abs() > 0.5 {
                    focus_to = Some(to);
                }
            }
            scroll = x * ws;
            table_to = Some(x);
        }
        Some(_) => {
            let leftmost = cells.iter().zip(&lefts).find_map(|(c, l)| match c {
                view::Cell::Repo(s) if (l + c.width() / 2.0) * ws - scroll >= 0.0 => Some(*s),
                _ => None,
            });
            if let Some(b) = leftmost.and_then(|s| strip_of.get(s.path.as_path())) {
                // The headings just above it too: its section's, and any
                // folded ones before it, which the window shows as spines.
                let i = leftmost.and_then(|s| cell_of_repo(&s.path)).unwrap_or(0);
                let j = first_of_run(&cells, i);
                let heading = match (j < i).then(|| cells[j]) {
                    Some(view::Cell::Spine(section, _)) => panel_parts
                        .get(&(app().to_path_buf(), Part::Heading(section)))
                        .map(|h| f32::from(h.bounds.origin.y)),
                    _ => None,
                };
                let at = heading.unwrap_or(f32::from(b.origin.y)) + list_scroll;
                let y = (at - list_top).clamp(0.0, list_max.max(0.0));
                list_to = Some(y);
                // The list's parts, where the panel will draw them.
                let shift = px(list_scroll - y);
                for b in strips.values_mut().chain(strip_of.values_mut()) {
                    b.origin.y += shift;
                }
                for ((path, part), seen) in panel_parts.iter_mut() {
                    if path != app()
                        || matches!(
                            part,
                            Part::Heading(_) | Part::Label(_) | Part::Arrow(_) | Part::Prs
                        )
                    {
                        seen.bounds.origin.y += shift;
                    }
                }
            }
        }
    }
    // Where each header part sits in a strip and in a column, from a repo
    // both views drew: for a repo one of them didn't.
    let mut strip_template: HashMap<Part, Bounds<Pixels>> = HashMap::new();
    let mut column_template: HashMap<Part, Bounds<Pixels>> = HashMap::new();
    for status in &repo_cells {
        let path = &status.path;
        for part in [Part::Icon, Part::Name, Part::Checkout, Part::Status] {
            let key = (path.clone(), part);
            if let (Some(a), Some(strip)) = (panel_parts.get(&key), strips.get(path)) {
                strip_template.entry(part).or_insert(Bounds {
                    origin: a.bounds.origin - strip.origin,
                    size: a.bounds.size,
                });
            }
            if let (Some(b), Some(hat)) = (column_parts.get(&key), header_at.get(path)) {
                column_template.entry(part).or_insert(Bounds {
                    origin: point(b.bounds.origin.x - hat.x, b.bounds.origin.y),
                    size: b.bounds.size,
                });
            }
        }
    }

    for (cell, left) in cells.iter().zip(&lefts) {
        // Past the big window's edges too: a column out of sight still
        // takes its tree, which turns out of the sheet's side. On whole px,
        // as the window lays its columns out and scrolls them.
        let x = to.origin.x + (left * ws).round() - scroll.round();
        match *cell {
            view::Cell::Repo(status) => {
                let (Some(&strip), Some(graph)) = (
                    strip_of.get(status.path.as_path()),
                    timelines.graph_of(&status.path),
                ) else {
                    continue;
                };
                // Not drawn when out of the flip window at both ends.
                let (uw, uh) = (union.width() as f32, union.height() as f32);
                let out = |b: Bounds<f32>| {
                    b.origin.x + b.size.width < 0.0
                        || b.origin.x > uw
                        || b.origin.y + b.size.height < 0.0
                        || b.origin.y > uh
                };
                let column = Bounds {
                    origin: point(x, to.origin.y),
                    size: size(COLUMN_WIDTH * ws, to.size.height),
                };
                if out(in_panel(&strip)) && out(column) {
                    continue;
                }
                let drawn = strips.contains_key(&status.path);
                // The parts both views show, where each last drew them,
                // or where they'd be.
                for part in [Part::Icon, Part::Name, Part::Checkout, Part::Status] {
                    let key = (status.path.clone(), part);
                    let a = panel_parts.get(&key).filter(|_| drawn);
                    let b =
                        column_parts
                            .get(&key)
                            .zip(header_at.get(&status.path))
                            .map(|(b, hat)| Bounds {
                                origin: point(b.bounds.origin.x - hat.x, b.bounds.origin.y),
                                size: b.bounds.size,
                            });
                    // The words: both views' when both drew them, else
                    // the one that did, for a repo out of the other's
                    // sight (where nobody sees them change).
                    let words = match (a, column_parts.get(&key)) {
                        (Some(a), Some(b)) => carried_words(a, b, open, part),
                        (Some(one), None) | (None, Some(one)) => {
                            one.text.clone().map(|w| (w.clone(), w))
                        }
                        (None, None) => None,
                    };
                    if part != Part::Icon && words.is_none() {
                        continue;
                    }
                    let a = a.map(|a| a.bounds).or_else(|| {
                        strip_template.get(&part).map(|t| Bounds {
                            origin: strip.origin + t.origin,
                            size: t.size,
                        })
                    });
                    let b = b.or_else(|| column_template.get(&part).copied());
                    let (Some(a), Some(b)) = (a, b) else {
                        continue;
                    };
                    let to_b = Bounds {
                        origin: point(
                            x + f32::from(b.origin.x) * ws,
                            to.origin.y + f32::from(b.origin.y) * ws,
                        ),
                        size: size(f32::from(b.size.width) * ws, f32::from(b.size.height) * ws),
                    };
                    parts.push(Carried {
                        from: in_panel(&a),
                        to: to_b,
                        tree: Some(trees.len()),
                        words,
                        kind: if part == Part::Icon {
                            Kind::Icon(Box::new(status.clone()))
                        } else {
                            Kind::Words
                        },
                    });
                }
                let marks = crate::timeline::strip_marks(strip, &graph, status.dirty)
                    .iter()
                    .map(&in_panel)
                    .collect();
                let strip = in_panel(&strip);
                let newest = strip.origin.x + strip.size.width - crate::timeline::STRIP_PAD * ps;
                let column_len = (to.size.height - body_top).max(0.0);
                trees.push(Tree {
                    strip,
                    marks,
                    column: Bounds {
                        origin: point(x, to.origin.y + body_top),
                        size: size(COLUMN_WIDTH * ws, column_len),
                    },
                    lanes: crate::timeline::tree_width(&graph, status.dirty) * ws,
                    graph,
                    dirty: status.dirty,
                    ends: Ends {
                        strip: (newest, strip.origin.y + strip.size.height / 2.0),
                        strip_len: newest - strip.origin.x,
                        column: (x, to.origin.y + body_top),
                        column_len,
                    },
                });
            }
            view::Cell::Spine(section, n) => spines.push((section, n, x)),
        }
    }
    // The top and bottom bars, and what's on the top one, from the
    // panel's to the window's.
    let in_window = |b: &Bounds<Pixels>| Bounds {
        origin: point(
            to.origin.x + f32::from(b.origin.x) * ws,
            to.origin.y + f32::from(b.origin.y) * ws,
        ),
        size: size(f32::from(b.size.width) * ws, f32::from(b.size.height) * ws),
    };
    let both = |part: Part| {
        let key = (app().to_path_buf(), part);
        panel_parts.get(&key).zip(column_parts.get(&key))
    };
    // Kept to the window's bottom: placed by its distance from there then,
    // should the window be taller or shorter now.
    let from_bottom = |seen: &Seen| {
        let mut b = in_window(&seen.bounds);
        b.origin.y =
            to.origin.y + to.size.height - (seen.height - f32::from(seen.bounds.origin.y)) * ws;
        b
    };
    let bar = both(Part::Bar).map(|(a, b)| (in_panel(&a.bounds), in_window(&b.bounds)));
    let footer = both(Part::Footer).map(|(a, b)| (in_panel(&a.bounds), from_bottom(b)));
    // The spines, from under the top bar to the bottom bar: where the
    // window last drew them, or would.
    let spine_at: Vec<(Section, usize, Bounds<f32>)> = spines
        .iter()
        .map(|&(section, n, x)| {
            let key = (app().to_path_buf(), Part::Spine(section));
            let (top, bottom) = match column_parts.get(&key) {
                Some(b) => (
                    to.origin.y + f32::from(b.bounds.origin.y) * ws,
                    to.origin.y + to.size.height - (b.height - f32::from(b.bounds.bottom())) * ws,
                ),
                None => (
                    bar.map_or(to.origin.y + body_top, |b| b.1.origin.y + b.1.size.height),
                    footer.map_or(to.origin.y + to.size.height, |f| f.1.origin.y),
                ),
            };
            let at = Bounds {
                origin: point(x, top),
                size: size(view::SPINE_WIDTH * ws, (bottom - top).max(0.0)),
            };
            (section, n, at)
        })
        .collect();
    let health = view::tone_counts(&state.statuses);
    for part in [
        Part::Headline,
        Part::Subline,
        Part::Settings,
        Part::Refresh,
        Part::Health,
        Part::Count(0),
        Part::Count(1),
        Part::Count(2),
    ] {
        let Some((a, b)) = both(part) else {
            continue;
        };
        let words = carried_words(a, b, open, part);
        let kind = match part {
            Part::Health => Kind::Health(health),
            // A glyph in the middle of its button.
            Part::Settings | Part::Refresh => Kind::Glyph,
            _ => Kind::Words,
        };
        if !matches!(kind, Kind::Health(_)) && words.is_none() {
            continue;
        }
        parts.push(Carried {
            from: in_panel(&a.bounds),
            to: in_window(&b.bounds),
            tree: None,
            words,
            kind,
        });
    }
    // The bars that slide: each view's body runs from under its top bar to
    // its bottom bar.
    let mut sliders = Vec::new();
    let mut turning = Vec::new();
    let bodies = [
        (
            view::Mode::Panel,
            &panel_parts,
            bar.map_or(from.origin.y, |b| b.0.origin.y + b.0.size.height),
            footer.map_or(from.origin.y + from.size.height, |f| f.0.origin.y),
        ),
        (
            view::Mode::Window,
            &column_parts,
            bar.map_or(to.origin.y, |b| b.1.origin.y + b.1.size.height),
            footer.map_or(to.origin.y + to.size.height, |f| f.1.origin.y),
        ),
    ];
    for (mode, seen, top, bottom) in bodies {
        let place = |b: &Seen| match mode {
            view::Mode::Panel => in_panel(&b.bounds),
            view::Mode::Window => from_bottom(b),
        };
        let key = |part| (app().to_path_buf(), part);
        // The pull requests, down into the bottom bar.
        if let Some(b) = seen.get(&key(Part::Prs)) {
            let at = place(b);
            sliders.push(Slider {
                what: Slid::Prs(mode),
                at,
                away: (0.0, bottom - at.origin.y),
            });
        }
        // The panel's headings, out of the nearer end of its list.
        if mode == view::Mode::Panel {
            for section in [Section::NeedsYou, Section::Synced, Section::GithubOnly] {
                let Some(b) = seen.get(&key(Part::Heading(section))) else {
                    continue;
                };
                let at = place(b);
                // Turned with the trees into its spine.
                if let Some(&(_, _, spine)) = spine_at.iter().find(|s| s.0 == section) {
                    // Its words and arrow: in the panel as drawn, on the
                    // spine as drawn there, moved with the spine to where
                    // it is now.
                    let on_spine = |part: Part| {
                        let drawn = column_parts.get(&key(Part::Spine(section)))?;
                        let seen = column_parts.get(&key(part))?;
                        let dx = f32::from(seen.bounds.origin.x - drawn.bounds.origin.x) * ws;
                        let dy = f32::from(seen.bounds.origin.y - drawn.bounds.origin.y) * ws;
                        Some((
                            seen,
                            Bounds {
                                origin: point(spine.origin.x + dx, spine.origin.y + dy),
                                size: size(
                                    f32::from(seen.bounds.size.width) * ws,
                                    f32::from(seen.bounds.size.height) * ws,
                                ),
                            },
                        ))
                    };
                    let label = seen
                        .get(&key(Part::Label(section)))
                        .zip(on_spine(Part::Label(section)))
                        .and_then(|(a, (z, z_at))| {
                            let ((text, color, a_size, _), (_, _, z_size, _)) =
                                carried_words(a, z, open, Part::Label(section))?;
                            Some(TurnWords {
                                text,
                                color,
                                size: (a_size, z_size),
                                at: (place(a), z_at),
                            })
                        });
                    let arrow = seen
                        .get(&key(Part::Arrow(section)))
                        .zip(on_spine(Part::Arrow(section)))
                        .map(|(a, (_, z_at))| (place(a), z_at));
                    turning.push(Turning {
                        section,
                        from: at,
                        to: spine,
                        folded: state.folded(section),
                        label,
                        arrow,
                    });
                    continue;
                }
                let away = if at.origin.y + at.size.height / 2.0 < (top + bottom) / 2.0 {
                    top - (at.origin.y + at.size.height)
                } else {
                    bottom - at.origin.y
                };
                sliders.push(Slider {
                    what: Slid::Heading(section),
                    at,
                    away: (0.0, away),
                });
            }
        }
    }
    // A spine with no heading in sight slides out of the window's nearer
    // side.
    for &(section, n, at) in &spine_at {
        if turning.iter().any(|t| t.section == section) {
            continue;
        }
        let middle = to.origin.x + to.size.width / 2.0;
        let away = if at.origin.x + at.size.width / 2.0 < middle {
            to.origin.x - (at.origin.x + at.size.width)
        } else {
            to.origin.x + to.size.width - at.origin.x
        };
        sliders.push(Slider {
            what: Slid::Spine(section, n, state.folded(section)),
            at,
            away: (away, 0.0),
        });
    }
    (!trees.is_empty()).then_some(Plan {
        union,
        from_frame,
        to,
        caption: (
            caption.0 as f32,
            caption.1 as f32,
            caption.2 as f32,
            caption.3 as f32,
        ),
        maximized,
        work,
        rims,
        bar,
        footer,
        trees,
        parts,
        sliders,
        turning,
        table_to,
        list_to,
        focus_to,
    })
}

/// The first of the cells just before `i` that are spines, or `i`.
fn first_of_run(cells: &[view::Cell], i: usize) -> usize {
    let mut j = i;
    while j > 0 && matches!(cells[j - 1], view::Cell::Spine(..)) {
        j -= 1;
    }
    j
}

/// Strips as the panel drew them, with the ones it didn't filled in: rows
/// on from the nearest drawn one, a row being the least step between two
/// drawn neighbours.
fn fill_rows(known: &[Option<Bounds<Pixels>>]) -> Vec<Option<Bounds<Pixels>>> {
    let step = known
        .windows(2)
        .filter_map(|w| Some(f32::from(w[1]?.origin.y - w[0]?.origin.y)))
        .filter(|d| *d > 0.0)
        .fold(None, |m: Option<f32>, d| Some(m.map_or(d, |m| m.min(d))))
        .or_else(|| {
            known
                .iter()
                .flatten()
                .next()
                .map(|b| f32::from(b.size.height))
        });
    let Some(step) = step else {
        return known.to_vec();
    };
    (0..known.len())
        .map(|i| {
            if known[i].is_some() {
                return known[i];
            }
            // The nearest drawn one, before or after.
            let j = (0..known.len())
                .filter(|&j| known[j].is_some())
                .min_by_key(|&j| (j as isize - i as isize).unsigned_abs())?;
            let mut b = known[j]?;
            b.origin.y += px((i as f32 - j as f32) * step);
            Some(b)
        })
        .collect()
}

/// A part's words at each end, the panel's `a` and the window's `b`. The
/// view the flip ends in last drew them a while ago, while the one it
/// starts from drew them just now, from the same state: its words and
/// their colour are carried, each view's size and weight. (Both views say
/// the same thing in the same colour, but for the checkout, which the
/// panel mutes.)
fn carried_words(a: &Seen, b: &Seen, open: bool, part: Part) -> Option<(Words, Words)> {
    let (wa, wb) = a.text.clone().zip(b.text.clone())?;
    let now = if open { &wa } else { &wb };
    let (text, color) = (now.0.clone(), now.1);
    let keep_color = matches!(part, Part::Checkout | Part::Label(_));
    Some((
        (
            text.clone(),
            if keep_color { wa.1 } else { color },
            wa.2,
            wa.3,
        ),
        (text, if keep_color { wb.1 } else { color }, wb.2, wb.3),
    ))
}

/// Opens the flip's window for `plan`. False if it couldn't.
fn launch(plan: Plan, direction: Direction, from: Option<isize>, cx: &mut App) -> bool {
    let union = plan.union;
    let stretch = stretch(cx);
    debug_assert_eq!((FADE + TURN + HOLD + REVEAL).as_millis(), 450);
    if let Some(x) = plan.table_to {
        view::scroll_table_to(x);
    }
    if let Some(y) = plan.list_to {
        view::scroll_list_to(y);
        // The panel the flip back lands in is laid out already, cloaked:
        // scrolled there now.
        if let Some(handle) = cx
            .try_global::<crate::panel::PanelSlot>()
            .and_then(|s| s.handle())
        {
            handle.update(cx, |_, window, _| window.refresh()).ok();
        }
    }
    cx.default_global::<MorphSlot>().direction = Some(direction);
    crate::frames::start(union, cx);
    crate::frames::frame("before");
    let mut plan = Some(plan);
    // The window opened ahead, while the views glided home, saves opening
    // one now.
    if let Some(spare) = cx.default_global::<MorphSlot>().spare.take() {
        let hwnd = spare
            .update(cx, |view, window, _| {
                *view = MorphView::new(plan.take(), direction, stretch, win::hwnd(window));
                window.refresh();
                view.hwnd
            })
            .ok()
            .flatten();
        if let Some(h) = hwnd {
            // Sized while hidden, and shown once it has drawn a frame of
            // this flip at that size: never a frame of what it was before.
            unsafe { win::size_morph(h, union) };
            spare
                .update(cx, |view, window, _| {
                    view.show_at = Some(union);
                    view.from = from;
                    window.refresh();
                })
                .ok();
            let old = cx.default_global::<MorphSlot>().handle.replace(spare);
            if let Some(old) = old {
                old.update(cx, |_, window, _| window.remove_window()).ok();
            }
            let slot = cx.default_global::<MorphSlot>();
            slot.since = Some(Instant::now());
            slot.hwnd = Some(h);
            return true;
        }
    }
    let mut flip_hwnd = None;
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
            flip_hwnd = win::hwnd(window);
            if let Some(h) = flip_hwnd {
                // Shown a moment after it opens, from a timer: a hidden
                // window may draw no frames, so it can't show itself.
                cx.spawn(async move |cx| {
                    unsafe { win::style_morph(h) };
                    cx.background_executor()
                        .timer(Duration::from_millis(20))
                        .await;
                    unsafe { win::place_morph(h, union) };
                })
                .detach();
            }
            cx.new(|_| {
                let mut view = MorphView::new(plan.take(), direction, stretch, win::hwnd(window));
                view.from = from;
                // Placed again as it first draws: shown on a monitor of
                // another scale, Windows resizes it for that, and it must
                // cover the flip's place exactly.
                view.show_at = Some(union);
                view
            })
        },
    ) else {
        return false;
    };
    // A stuck flip from before goes.
    let old = cx.default_global::<MorphSlot>().handle.replace(handle);
    if let Some(old) = old {
        old.update(cx, |_, window, _| window.remove_window()).ok();
    }
    let slot = cx.default_global::<MorphSlot>();
    slot.since = Some(Instant::now());
    slot.hwnd = flip_hwnd;
    true
}

pub struct MorphView {
    start: Instant,
    /// `flip_ms` over the default.
    stretch: f32,
    /// None while it waits, hidden, for a flip to draw.
    plan: Option<Plan>,
    direction: Direction,
    hwnd: Option<isize>,
    /// The window the flip starts from has closed.
    left: bool,
    /// When the window it ends in was opened.
    arrived: Option<Instant>,
    /// When the reveal began: once that window has drawn.
    reveal_from: Option<Instant>,
    /// Frames drawn; shown from the second.
    frames: u32,
    /// The last frame's turn and sheet fill, for the frame recorder.
    drew: (f32, f32),
    /// The frames between which the sheet draws the windows' rim and
    /// shadow: from the one after the starting window closed until the one
    /// after the ending window showed, so the real and the drawn one hand
    /// over without both showing, or neither.
    framed: (u32, u32),
    /// Hidden, to be removed next frame.
    hiding: bool,
    /// Where to show it once it has drawn its first frame: a window opened
    /// ahead, hidden.
    show_at: Option<Rect>,
    /// The window the flip starts from: hidden when the turn starts, and
    /// closed once the window it ends in has opened.
    from: Option<isize>,
}

impl MorphView {
    fn p(&self) -> &Plan {
        self.plan.as_ref().expect("a flip's plan")
    }

    fn new(plan: Option<Plan>, direction: Direction, stretch: f32, hwnd: Option<isize>) -> Self {
        MorphView {
            start: Instant::now(),
            stretch,
            plan,
            direction,
            hwnd,
            left: false,
            arrived: None,
            reveal_from: None,
            frames: 0,
            drew: (0.0, 0.0),
            framed: (u32::MAX, u32::MAX),
            hiding: false,
            show_at: None,
            from: None,
        }
    }
}

thread_local! {
    /// When the panel and the big window last drew their top bars.
    static PANEL_PAINT: Cell<Option<Instant>> = const { Cell::new(None) };
    static COLUMN_PAINT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// When either view last drew.
fn last_paint() -> Option<Instant> {
    PANEL_PAINT
        .with(Cell::get)
        .max(COLUMN_PAINT.with(Cell::get))
}

/// The longest the flip waits for the window it ends in to draw.
const DRAW_WAIT: Duration = Duration::from_millis(500);

/// `t` eased in and out.
pub(crate) fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// How far `elapsed` is into the phase that starts `at` and lasts `len`.
fn phase(elapsed: Duration, at: Duration, len: Duration) -> f32 {
    if len.is_zero() {
        return if elapsed >= at { 1.0 } else { 0.0 };
    }
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

/// `b` (logical px at `s`) with its edges on whole physical px.
fn snapped(b: Bounds<Pixels>, s: f32) -> Bounds<Pixels> {
    let r = |v: Pixels| (f32::from(v) * s).round() / s;
    let (l, t) = (r(b.left()), r(b.top()));
    let (rt, bt) = (r(b.right()), r(b.bottom()));
    Bounds::new(point(px(l), px(t)), size(px(rt - l), px(bt - t)))
}

/// A tree as a frame draws it: its graph, whether dirty, its ends as it
/// turns, and where the strip and the column body rest, logical px.
type FlipTree = (
    Arc<crate::repo_graph::RepoGraph>,
    bool,
    Ends,
    Bounds<Pixels>,
    Bounds<Pixels>,
);

/// `r` less each of `holes`: the pieces left, which don't overlap.
fn cut_out(r: Bounds<Pixels>, holes: &[Bounds<Pixels>]) -> Vec<Bounds<Pixels>> {
    let mut pieces = vec![r];
    for h in holes {
        let mut next = Vec::with_capacity(pieces.len() + 4);
        for p in pieces {
            let (l, t) = (p.left().max(h.left()), p.top().max(h.top()));
            let (rt, b) = (p.right().min(h.right()), p.bottom().min(h.bottom()));
            if rt <= l || b <= t {
                next.push(p);
                continue;
            }
            let mut push = |x0: Pixels, y0: Pixels, x1: Pixels, y1: Pixels| {
                if x1 > x0 && y1 > y0 {
                    next.push(Bounds::new(point(x0, y0), size(x1 - x0, y1 - y0)));
                }
            };
            // Above and below the hole, full width; beside it, its height.
            push(p.left(), p.top(), p.right(), t);
            push(p.left(), b, p.right(), p.bottom());
            push(p.left(), t, l, b);
            push(rt, t, p.right(), b);
        }
        pieces = next;
    }
    pieces
}

/// The part of `a` inside `b`, if any.
fn overlap(a: Bounds<Pixels>, b: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let (l, t) = (a.left().max(b.left()), a.top().max(b.top()));
    let (r, bt) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (r > l && bt > t).then(|| Bounds::new(point(l, t), size(r - l, bt - t)))
}

/// Physical px from the flip window's corner as logical px at `s`.
fn logical_at(b: Bounds<f32>, s: f32) -> Bounds<Pixels> {
    lerp_bounds(b, b, 0.0, s)
}

impl Render for MorphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.hiding {
            window.remove_window();
            return div();
        }
        if self.plan.is_none() {
            // Waiting to be handed a flip.
            return div();
        }
        if self.frames == 1
            && let (Some(r), Some(h)) = (self.show_at.take(), self.hwnd)
        {
            cx.spawn(async move |_, _| unsafe { win::place_morph(h, r) })
                .detach();
        }
        // Shown once its first frame is drawn; the clock starts then.
        self.frames += 1;
        if self.frames == 2 {
            self.start = Instant::now();
            // Drawn in place of the windows, it takes the clicks meant for
            // them, rather than letting them through to whatever is behind.
            if let Some(h) = self.hwnd {
                cx.spawn(async move |_, _| unsafe { win::click_through(h, false) })
                    .detach();
            }
        }
        let elapsed = if self.frames < 2 {
            Duration::ZERO
        } else {
            self.start.elapsed()
        };
        let k = self.stretch;
        let (fade, turn, reveal_len) = (FADE.mul_f32(k), TURN.mul_f32(k), REVEAL.mul_f32(k));
        let slide = SLIDE.mul_f32(k);
        let open = self.direction == Direction::Open;
        // What the starting view alone shows slides out first, over that
        // view still on screen and with the sheet clear, as long as what
        // the ending view alone shows slides in at the other end; then the
        // sheet fades in, and turns.
        let in_panel =
            |sl: &Slider| matches!(sl.what, Slid::Prs(view::Mode::Panel) | Slid::Heading(_));
        let slide_out = if self.p().sliders.iter().any(|sl| in_panel(sl) == open) {
            slide
        } else {
            Duration::ZERO
        };
        let fade_at = slide_out;
        let turn_at = fade_at + fade;
        let hold_at = turn_at + turn;
        // The reveal waits for the window underneath to have drawn, so it
        // never shows that window empty.
        if self.reveal_from.is_none()
            && let Some(arrived) = self.arrived
        {
            let since = arrived.elapsed();
            // Opening, the big window must also have settled (a maximised
            // one maximises a moment after it shows) and drawn since.
            let ready = if open {
                SETTLED.with(Cell::get).filter(|&t| t >= arrived)
            } else {
                Some(arrived)
            };
            let painted = if open { &COLUMN_PAINT } else { &PANEL_PAINT };
            let drawn = ready
                .zip(painted.with(Cell::get))
                .is_some_and(|(r, t)| t > r.max(arrived + Duration::from_millis(30)));
            // And the slide in has ended.
            if since >= slide.max(HOLD) && (since >= DRAW_WAIT || drawn) {
                self.reveal_from = Some(Instant::now());
                // The ending window and its shadow show as this frame,
                // without the sheet's, does: uncloaked as the next one
                // starts, since a cloak takes effect a frame sooner than
                // a drawn frame reaches the screen.
                self.framed.1 = self.frames;
                if let Some(h) = cx.default_global::<MorphSlot>().ending.take() {
                    window.on_next_frame(move |_, _| {
                        win::uncloak(h);
                        // In front of whatever a click during the flip
                        // may have brought up.
                        win::to_front(h);
                    });
                }
                // The window revealed under it takes clicks from now.
                if let Some(h) = self.hwnd {
                    cx.spawn(async move |_, _| unsafe { win::click_through(h, true) })
                        .detach();
                }
            }
        }
        let revealed = self.reveal_from.map_or(Duration::ZERO, |t| t.elapsed());
        // Named with what the last frame drew: its phase, turn and fill.
        let (last_e, last_fill) = self.drew;
        crate::frames::frame(&format!(
            "{}-e{last_e:.2}-fill{last_fill:.2}",
            if self.reveal_from.is_some() {
                "reveal"
            } else if self.arrived.is_some() {
                "hold"
            } else if elapsed >= turn_at {
                "turn"
            } else if elapsed >= fade_at {
                "fade"
            } else {
                "slide"
            }
        ));

        if elapsed >= turn_at && !self.left {
            self.left = true;
            // The starting window vanishes, its shadow too, as this frame,
            // which draws the sheet's shadow, shows: never both, or
            // neither. A cloak takes effect a frame sooner than a drawn
            // frame reaches the screen, so it's cloaked as the next frame
            // starts. It stays open (and active) until the ending window
            // opens, so that one comes to the front.
            self.framed.0 = self.frames;
            match self.from {
                Some(h) => window.on_next_frame(move |_, _| win::cloak(h)),
                None if open => cx.defer(crate::panel::close),
                None => cx.defer(view::close_window),
            }
        }
        if elapsed >= hold_at && self.arrived.is_none() {
            self.arrived = Some(Instant::now());
            // The sheet keeps drawing the shadow until the reveal, when the
            // ending window, opened cloaked, shows its own instead.
            let cloaked = self.from.is_some();
            if open {
                cx.defer(move |cx| {
                    // Cloaked as it's made, before Windows ever shows it.
                    view::show_window_cloaked(cx);
                    cx.default_global::<MorphSlot>().ending = view::window_hwnd(cx);
                    if cloaked {
                        crate::panel::close(cx);
                    }
                });
            } else {
                let me = self.hwnd;
                cx.defer(move |cx| {
                    // Opened, cloaked and drawn before the flip began; again
                    // here only should that have failed.
                    if !crate::panel::is_open(cx) {
                        crate::panel::reopen_cloaked(cx);
                    }
                    // The big window is off the screen: the panel takes
                    // the keyboard, out of sight.
                    crate::panel::activate(cx);
                    cx.default_global::<MorphSlot>().ending = crate::panel::hwnd(cx);
                    if cloaked {
                        view::close_window(cx);
                    }
                    // The panel is topmost; the flip stays over it until
                    // the reveal.
                    if let Some(h) = me {
                        cx.spawn(async move |_| unsafe { win::reassert_topmost(h) })
                            .detach();
                    }
                });
            }
        }
        if self.reveal_from.is_some() && revealed >= reveal_len {
            // Hidden first and removed a moment later, so its last frame
            // never lingers as a flash. Removed from a timer: a hidden
            // window draws no more frames, so it can't remove itself.
            self.hiding = true;
            let me = window.window_handle();
            let hwnd = self.hwnd;
            cx.spawn(async move |_, cx| {
                if let Some(h) = hwnd {
                    unsafe { win::hide(h) };
                }
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                cx.update_window(me, |_, window, _| window.remove_window())
                    .ok();
            })
            .detach();
            crate::frames::finish(cx);
            // The flip has ended: the windows are the app's again, and
            // what was asked for meanwhile is done now.
            let close = self.direction == Direction::Close;
            let slot = cx.default_global::<MorphSlot>();
            slot.handle = None;
            slot.since = None;
            slot.hwnd = None;
            slot.ending = None;
            slot.direction = None;
            if close {
                // It goes anyway.
                slot.pending.retain(|p| !matches!(p, Pending::PanelLeaves));
            }
            if let Some(x) = self.p().focus_to {
                let over = SCROLL_HOME.mul_f32(self.stretch);
                cx.defer(move |cx| {
                    view::glide_table_to(x, over, cx);
                });
            }
            if close {
                // Back in the panel, which then goes the way it came.
                cx.defer(crate::panel::slide_away_now);
            }
            cx.defer(run_pending);
            window.request_animation_frame();
            return div();
        }

        let sheet_alpha = ease(phase(elapsed, fade_at, fade));
        let turned = ease(phase(elapsed, turn_at, turn));
        // 0 is the panel's layout, 1 the big window's.
        let e = if open { turned } else { 1.0 - turned };
        let reveal = 1.0 - ease(phase(revealed, Duration::ZERO, reveal_len));
        // The sheet's own fills: faded in over the starting window and out
        // over the ending one.
        let fill = sheet_alpha * reveal;
        self.drew = (e, fill);
        // The windows' own shadow, drawn around the sheet while neither
        // window is on screen to show its own.
        let framed = (self.framed.0..self.framed.1).contains(&self.frames);
        // A window is on screen under the sheet: the starting one before
        // the turn, the ending one in the reveal.
        let real = !framed;
        let still = e <= 0.0 || e >= 1.0;
        let at_end = e >= 1.0;
        // Over a window on screen, the sheet fades in or out only what
        // the two views draw differently; where they draw the same (the
        // carried words and icons, the trees, the labelled headings) the
        // window's own shows through, so nothing there is drawn twice.
        let through = still && real;
        let appearance = window.appearance();
        let dark = matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let p = Palette::for_appearance(appearance);
        let s = window.scale_factor();
        let scroll_together = cx
            .try_global::<Ui>()
            .is_some_and(|ui| ui.state.read(cx).config.scroll_together);
        // The sheet grows to the big window's whole visible frame, title
        // bar and border too, so its rim lands where Windows draws that
        // window's own.
        let (fl, cap, fr, fb) = self.p().caption;
        let to_sheet = Bounds {
            origin: point(self.p().to.origin.x - fl, self.p().to.origin.y - cap),
            size: size(
                self.p().to.size.width + fl + fr,
                self.p().to.size.height + cap + fb,
            ),
        };
        // On whole physical px, so its rim stays crisp and even rather
        // than smearing across two px as it moves.
        let sheet = snapped(lerp_bounds(self.p().from_frame, to_sheet, e, s), s);
        // A maximised window is square: the corners straighten as the
        // sheet grows into it.
        let radius = if self.p().maximized {
            px(RADIUS * (1.0 - e))
        } else {
            px(RADIUS)
        };
        // One clip per monitor's work area: never over a taskbar.
        let work: Vec<Bounds<Pixels>> = self.p().work.iter().map(|w| logical_at(*w, s)).collect();

        // The sheet in bands, top to bottom, each its view's colour: the
        // title bar (as tall as the turn has gone), the top bar, the body
        // and the bottom bar, each with its edge line. They never overlap,
        // so as they fade each shows exactly its colour over the same
        // colour beneath, and the bars never flicker.
        let mix = |a: f32, b: f32| a + (b - a) * e;
        let top = f32::from(sheet.top()) * s;
        let bottom = f32::from(sheet.bottom()) * s;
        let title_end = (top + cap * e).round().min(bottom);
        let bar_end = self.p().bar.map_or(title_end, |(a, z)| {
            mix(a.origin.y + a.size.height, z.origin.y + z.size.height)
                .round()
                .clamp(title_end, bottom)
        });
        let footer_at = self.p().footer.map_or(bottom, |(a, z)| {
            mix(a.origin.y, z.origin.y).round().clamp(bar_end, bottom)
        });
        let band = |t: f32, b: f32| {
            Bounds::new(
                point(sheet.left(), px(t / s)),
                size(sheet.size.width, px((b - t).max(0.0) / s)),
            )
        };
        // The bars' edge lines, 1 logical px like the views'.
        let line = s;
        let mut bands = vec![(band(top, title_end), view::title_bar(dark))];
        if self.p().bar.is_some() {
            let edge = (bar_end - line).max(title_end);
            bands.push((band(title_end, edge), p.bg));
            bands.push((band(edge, bar_end), p.border));
        }
        let body = band(bar_end, footer_at);
        bands.push((body, p.surface));
        if self.p().footer.is_some() {
            let edge = (footer_at + line).min(bottom);
            bands.push((band(footer_at, edge), p.border));
            bands.push((band(edge, bottom), p.bg));
        }

        // Where the window on screen shows through, and, inside that,
        // what the sheet still fades: tags and stashes, which only the
        // panel draws, and the big window's row lines across its trees.
        let mut holes = Vec::new();
        let mut cuts = Vec::new();
        if through {
            let pick = |a: Bounds<f32>, z: Bounds<f32>| {
                snapped(logical_at(if at_end { z } else { a }, s), s)
            };
            for c in &self.p().parts {
                holes.push(pick(c.from, c.to));
            }
            for t in &self.p().turning {
                if t.label.is_some() {
                    holes.push(pick(t.from, t.to));
                }
            }
            // In the reveal, the bars that slid in rest in the window too.
            if self.arrived.is_some() {
                for sl in self.p().sliders.iter().filter(|sl| in_panel(sl) != open) {
                    holes.push(snapped(logical_at(sl.at, s), s));
                }
            }
            for t in &self.p().trees {
                if at_end {
                    let mut lanes = t.column;
                    lanes.size.width = t.lanes.min(lanes.size.width);
                    let hole = snapped(logical_at(lanes, s), s);
                    holes.push(hole);
                    if scroll_together {
                        let h = f32::from(hole.size.height);
                        for y in crate::timeline::row_line_tops(h) {
                            let y = f32::from(hole.top()) + y;
                            cuts.push(snapped(
                                Bounds::new(
                                    point(hole.left(), px(y - 1.0)),
                                    size(hole.size.width, px(3.0)),
                                ),
                                s,
                            ));
                        }
                    }
                } else {
                    let strip = logical_at(t.strip, s);
                    let bleed = px(crate::timeline::BLEED);
                    holes.push(snapped(
                        Bounds::new(
                            point(strip.left(), strip.top() - bleed),
                            size(strip.size.width, strip.size.height + bleed * 2.0),
                        ),
                        s,
                    ));
                    cuts.extend(t.marks.iter().map(|m| snapped(logical_at(*m, s), s)));
                }
            }
        }
        // Each band less the holes, with its colour as faded.
        let band_pieces: Vec<(Bounds<Pixels>, Hsla)> = bands
            .iter()
            .flat_map(|(b, color)| {
                let color = color.opacity(color.a * fill);
                cut_out(*b, &holes)
                    .into_iter()
                    .map(move |piece| (piece, color))
            })
            .collect();
        let cuts: Vec<Bounds<Pixels>> = cuts.into_iter().filter_map(|c| overlap(c, body)).collect();

        // The rim, as thick as Windows draws each window's own on its
        // monitor. Over a window on screen it fades with the sheet, over
        // that window's own; a maximised window has none, so there it
        // goes as the sheet grows into that window.
        let rims = self.p().rims;
        let rim_px = (rims.0 + (rims.1 - rims.0) * e).round() / s;
        let rim_alpha =
            if real { fill } else { 1.0 } * if self.p().maximized { 1.0 - e } else { 1.0 };

        let trees: Vec<FlipTree> = self
            .p()
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
                (
                    t.graph.clone(),
                    t.dirty,
                    ends,
                    logical_at(t.strip, s),
                    logical_at(t.column, s),
                )
            })
            .collect();

        // The section headings, turning with the trees into their spines:
        // each a bar along its history from its newest end, the heading's
        // right end and the spine's top, a quarter turn anticlockwise like
        // the trees. Its wash fades with the sheet's fills, over and under
        // its twin in the window beneath, as the bands do.
        let muted = p.muted;
        let turning: Vec<Bar> = self
            .p()
            .turning
            .iter()
            .filter(|t| !(through && t.label.is_some()))
            .map(|t| {
                let tint = view::section_tint(t.section, &p);
                // Carried whole when its words are: opaque, over its twin
                // beneath. Without them, its wash fades with the sheet.
                let wash = if t.label.is_some() {
                    p.surface.blend(tint)
                } else {
                    tint.opacity(tint.a * fill)
                };
                let (a, z) = (t.from, t.to);
                // Each end's arrow: the heading's points down when open,
                // right when folded; the spine's, a quarter turn on.
                let (arrow_a, arrow_z) = if t.folded {
                    ('▸', '▴')
                } else {
                    ('▾', '▸')
                };
                let (arrow_size_a, arrow_size_z) = (14.0, view::SPINE_TEXT);
                if still {
                    let b = if at_end { z } else { a };
                    let still = snapped(logical_at(b, s), s);
                    // The spine's edge line, as the window draws it.
                    let border = at_end.then(|| {
                        Bounds::new(
                            point(still.right() - px(1.0), still.top()),
                            size(px(1.0), still.size.height),
                        )
                    });
                    let label = t.label.as_ref().map(|w| {
                        let at = if at_end { w.at.1 } else { w.at.0 };
                        let at = logical_at(at, s);
                        if at_end {
                            BarWords::Turned(at, w.text.to_string(), w.size.1, w.color)
                        } else {
                            BarWords::Upright(at, (w.text.clone(), w.color, w.size.0, 400.0))
                        }
                    });
                    let arrow = t.arrow.map(|(aa, zz)| {
                        let at = if at_end { zz } else { aa };
                        let (c, size) = if at_end {
                            (arrow_z, arrow_size_z)
                        } else {
                            (arrow_a, arrow_size_a)
                        };
                        BarWords::Upright(
                            logical_at(at, s),
                            (c.to_string().into(), muted, size, 400.0),
                        )
                    });
                    return Bar {
                        shape: Shape::Still(still),
                        wash,
                        border,
                        label,
                        arrow,
                    };
                }
                // Turning, about the heading's right end, which becomes the
                // spine's top: along its length from there, and across.
                let anchor_a = (a.origin.x + a.size.width, a.origin.y + a.size.height / 2.0);
                let anchor_z = (z.origin.x + z.size.width / 2.0, z.origin.y);
                let anchor = (
                    mix(anchor_a.0, anchor_z.0) / s,
                    mix(anchor_a.1, anchor_z.1) / s,
                );
                let len = mix(a.size.width, z.size.height) / s;
                let half = mix(a.size.height, z.size.width) / s / 2.0;
                let (sin, cos) = (-std::f32::consts::FRAC_PI_2 * e).sin_cos();
                let along = (-cos, -sin);
                let across = (-sin, cos);
                let at = |u: f32, v: f32| {
                    (
                        anchor.0 + u * along.0 + v * across.0,
                        anchor.1 + u * along.1 + v * across.1,
                    )
                };
                let pt = |(x, y): (f32, f32)| point(px(x), px(y));
                // A point of each end in the bar's own terms, then where
                // it is now.
                let uv_a = |(x, y): (f32, f32)| ((anchor_a.0 - x) / s, (y - anchor_a.1) / s);
                let uv_z = |(x, y): (f32, f32)| ((y - anchor_z.1) / s, (x - anchor_z.0) / s);
                let carry = |pa: (f32, f32), pz: (f32, f32)| {
                    let (ua, va) = uv_a(pa);
                    let (uz, vz) = uv_z(pz);
                    at(mix(ua, uz), mix(va, vz))
                };
                // Words read back towards the anchor, their tops away from
                // the spine's edge line.
                let dir = (-along.0, -along.1);
                let up = (-across.0, -across.1);
                let label = t.label.as_ref().map(|w| {
                    let (la, lz) = w.at;
                    let size = mix(w.size.0, w.size.1);
                    // Where each starts, on the middle of its line: the
                    // spine's picture sets its pen one device px above
                    // its box's bottom.
                    let start = carry(
                        (la.origin.x, la.origin.y + la.size.height / 2.0),
                        (
                            lz.origin.x + lz.size.width / 2.0,
                            lz.origin.y + lz.size.height - 1.0,
                        ),
                    );
                    let m = crate::glyph_path::middle(size);
                    // On whole device px, as both ends' words are: the
                    // turn starts and ends where they sit, with no half-px
                    // step as the renderer changes over.
                    let snap = |v: f32| (v * s).round() / s;
                    let baseline = (snap(start.0 - m * up.0), snap(start.1 - m * up.1));
                    BarWords::Path(w.text.to_string(), size, baseline, dir, up, w.color)
                });
                let arrow = t.arrow.map(|(aa, zz)| {
                    // The middle of each end's mark, as its view sets it.
                    let mark = |b: Bounds<f32>, c: char, size: f32| {
                        let (gx, gy) = crate::glyph_path::glyph_centre(c, size);
                        let m = crate::glyph_path::middle(size);
                        (
                            b.origin.x + gx * s,
                            b.origin.y + b.size.height / 2.0 + (m - gy) * s,
                        )
                    };
                    let size = mix(arrow_size_a, arrow_size_z);
                    let c = carry(
                        mark(aa, arrow_a, arrow_size_a),
                        mark(zz, arrow_z, arrow_size_z),
                    );
                    let (gx, gy) = crate::glyph_path::glyph_centre(arrow_a, size);
                    let start = (c.0 - gx * dir.0 - gy * up.0, c.1 - gx * dir.1 - gy * up.1);
                    BarWords::Path(arrow_a.to_string(), size, start, dir, up, muted)
                });
                // The spine's edge line, whole as it turns: it rides the
                // bar, which covers its ends' own.
                let line = 1.0;
                let border = [
                    pt(at(0.0, half - line)),
                    pt(at(len, half - line)),
                    pt(at(len, half)),
                    pt(at(0.0, half)),
                ];
                Bar {
                    shape: Shape::Turning(
                        [
                            pt(at(0.0, -half)),
                            pt(at(len, -half)),
                            pt(at(len, half)),
                            pt(at(0.0, half)),
                        ],
                        Some((border, p.border)),
                    ),
                    wash,
                    border: None,
                    label,
                    arrow,
                }
            })
            .collect();

        // The parts both views show, moving and growing from one to the
        // other, over the sheet; over a window on screen, its own show.
        // A check under way dims the health bar, in the views and here.
        let checking = cx
            .try_global::<Ui>()
            .is_some_and(|ui| ui.state.read(cx).sweeping);
        let parts: Vec<Drawn> = if through {
            Vec::new()
        } else {
            self.p()
                .parts
                .iter()
                .map(|c| {
                    let image = match &c.kind {
                        Kind::Icon(st) => {
                            let a = c.from.size.width.round() as u32;
                            let z = c.to.size.width.round() as u32;
                            crate::repo_marks::picture(st, &[a.max(z), a.min(z)], cx)
                        }
                        _ => None,
                    };
                    // The same words at both ends: their size, colour and
                    // weight move from one view's to the other's.
                    let words = c.words.as_ref().map(|(a, z)| {
                        let color = Hsla {
                            h: if e < 0.5 { a.1.h } else { z.1.h },
                            s: mix(a.1.s, z.1.s),
                            l: mix(a.1.l, z.1.l),
                            a: mix(a.1.a, z.1.a),
                        };
                        let weight = |bold: bool| if bold { 600.0 } else { 400.0 };
                        (
                            a.0.clone(),
                            color,
                            mix(a.2, z.2),
                            mix(weight(a.3), weight(z.3)),
                        )
                    });
                    // Riding with its tree: where it sits in the tree's frame
                    // at each end, blended, in the tree's frame now.
                    let bounds = match c.tree.and_then(|i| trees.get(i)) {
                        Some((_, _, ends, _, _)) if !still => {
                            let centre = |b: Bounds<f32>| {
                                (
                                    (b.origin.x + b.size.width / 2.0) / s,
                                    (b.origin.y + b.size.height / 2.0) / s,
                                )
                            };
                            let dot = |a: (f32, f32), b: (f32, f32)| a.0 * b.0 + a.1 * b.1;
                            let local = |p: (f32, f32), at: f32| {
                                let (o, al, ac) = crate::timeline::pose(*ends, at);
                                let d = (p.0 - o.0, p.1 - o.1);
                                (dot(d, al), dot(d, ac))
                            };
                            let (ua, va) = local(centre(c.from), 0.0);
                            let (uz, vz) = local(centre(c.to), 1.0);
                            let (u, v) = (mix(ua, uz), mix(va, vz));
                            let (o, al, ac) = crate::timeline::pose(*ends, e);
                            let (cx_, cy_) = (o.0 + u * al.0 + v * ac.0, o.1 + u * al.1 + v * ac.1);
                            let (w, h) = (
                                mix(c.from.size.width, c.to.size.width) / s,
                                mix(c.from.size.height, c.to.size.height) / s,
                            );
                            Bounds::new(
                                point(px(cx_ - w / 2.0), px(cy_ - h / 2.0)),
                                size(px(w), px(h)),
                            )
                        }
                        // At rest, on the px its view draws it on.
                        _ if still => snapped(lerp_bounds(c.from, c.to, e, s), s),
                        _ => lerp_bounds(c.from, c.to, e, s),
                    };
                    Drawn {
                        bounds,
                        image,
                        words,
                        kind: c.kind.clone(),
                    }
                })
                .collect()
        };

        // The bars that slide, the views' own elements: the starting
        // view's slide out before the fade, the ending view's in before
        // the reveal, both under the sheet's bars. While the starting one
        // is on screen, where they rest is washed as it is once they've
        // gone.
        let out = ease(phase(elapsed, Duration::ZERO, slide_out));
        let into = self
            .arrived
            .map(|a| 1.0 - ease(phase(a.elapsed(), Duration::ZERO, slide)));
        let mut homes = Vec::new();
        let app: &App = cx;
        let mut slid = Vec::new();
        if let Some(ui) = app.try_global::<Ui>() {
            let state = ui.state.read(app);
            let icons_only = state.config.icons_only;
            for sl in &self.p().sliders {
                let panel_side = in_panel(sl);
                let k = if panel_side == open {
                    if !self.left {
                        homes.push(snapped(logical_at(sl.at, s), s));
                    }
                    if out >= 1.0 {
                        continue;
                    }
                    out
                } else {
                    match into {
                        // Revealed, the window shows its own.
                        Some(_) if through => continue,
                        Some(k) => k,
                        None => continue,
                    }
                };
                let element = match sl.what {
                    Slid::Prs(mode) => {
                        view::prs_block(state, mode, icons_only && panel_side, &p, app)
                    }
                    Slid::Heading(section) => {
                        view::panel_heading(section, state, icons_only, &p, app)
                    }
                    Slid::Spine(section, n, folded) => Some(view::spine(section, n, folded, &p)),
                };
                let Some(element) = element else {
                    continue;
                };
                let (dx, dy) = (sl.away.0 * k / s, sl.away.1 * k / s);
                slid.push(
                    div()
                        .absolute()
                        .left(px(sl.at.origin.x / s + dx) - body.left())
                        .top(px(sl.at.origin.y / s + dy) - body.top())
                        .w(px(sl.at.size.width / s))
                        .h(px(sl.at.size.height / s))
                        .flex()
                        .flex_col()
                        .child(element),
                );
            }
        }
        let homes: Vec<Bounds<Pixels>> =
            homes.into_iter().filter_map(|h| overlap(h, body)).collect();

        let painter = canvas(
            |_, _, _| {},
            move |_, _, window, cx| {
                window.request_animation_frame();
                for w in &work {
                    // Never over a taskbar.
                    window.with_content_mask(Some(gpui::ContentMask { bounds: *w }), |window| {
                        if framed {
                            window.paint_shadows(
                                sheet,
                                gpui::Corners::all(radius),
                                &window_shadow(),
                            );
                        }
                        // A piece of a band: the sheet's rounded shape, cut
                        // to the piece.
                        let paint_piece = |window: &mut Window, b: Bounds<Pixels>, color: Hsla| {
                            if color.a <= 0.0 {
                                return;
                            }
                            window.with_content_mask(
                                Some(gpui::ContentMask { bounds: b }),
                                |window| {
                                    window.paint_quad(quad(
                                        sheet,
                                        radius,
                                        color,
                                        px(0.0),
                                        gpui::transparent_black(),
                                        gpui::BorderStyle::Solid,
                                    ));
                                },
                            );
                        };
                        for (b, color) in &band_pieces {
                            paint_piece(window, *b, *color);
                        }
                        // Where the slid bars rest, as their view is once
                        // they've gone.
                        for h in &homes {
                            paint_piece(window, *h, p.surface);
                        }
                        window.with_content_mask(
                            Some(gpui::ContentMask { bounds: body }),
                            |window| {
                                if through {
                                    // Over the window on screen: only where the
                                    // two views' trees differ, faded as the
                                    // sheet, the tree drawn whole over it.
                                    for c in &cuts {
                                        paint_piece(window, *c, p.surface.opacity(fill));
                                    }
                                    if fill > 0.0 {
                                        for c in &cuts {
                                            window.with_content_mask(
                                                Some(gpui::ContentMask { bounds: *c }),
                                                |window| {
                                                    for (graph, dirty, _, strip, column) in &trees {
                                                        if at_end {
                                                            crate::timeline::paint_still_column(
                                                                *column, graph, *dirty, dark, &p,
                                                                window,
                                                            );
                                                        } else {
                                                            crate::timeline::paint_still_strip(
                                                                *strip, graph, *dirty, dark, &p,
                                                                window,
                                                            );
                                                        }
                                                    }
                                                },
                                            );
                                        }
                                    }
                                } else if still {
                                    // At rest under no window: drawn as the
                                    // view at that end draws it.
                                    for (graph, dirty, _, strip, column) in &trees {
                                        if at_end {
                                            crate::timeline::paint_still_column(
                                                *column, graph, *dirty, dark, &p, window,
                                            );
                                        } else {
                                            crate::timeline::paint_still_strip(
                                                *strip, graph, *dirty, dark, &p, window,
                                            );
                                        }
                                    }
                                } else {
                                    for (graph, dirty, ends, _, _) in &trees {
                                        crate::timeline::paint_tree(
                                            graph, *dirty, *ends, e, 1.0, dark, &p, window,
                                        );
                                        crate::timeline::paint_tail_fade(
                                            *ends, e, p.surface, window,
                                        );
                                    }
                                }
                                // The headings over the trees, solid as they turn.
                                for bar in &turning {
                                    paint_bar(window, cx, bar);
                                }
                            },
                        );
                        window.with_content_mask(
                            Some(gpui::ContentMask { bounds: sheet }),
                            |window| {
                                for d in &parts {
                                    let b = d.bounds;
                                    if let Kind::Health(counts) = d.kind {
                                        paint_health(window, b, counts, checking, &p);
                                        continue;
                                    }
                                    if let Some(image) = &d.image {
                                        window
                                            .paint_image(
                                                b,
                                                Default::default(),
                                                image.clone(),
                                                0,
                                                false,
                                            )
                                            .ok();
                                    }
                                    if let Kind::Icon(st) = &d.kind
                                        && st.off_trunk()
                                    {
                                        crate::repo_marks::paint_branch_badge(
                                            b,
                                            f32::from(b.size.width),
                                            window,
                                        );
                                    }
                                    if let Some(words) = &d.words {
                                        let centred = matches!(d.kind, Kind::Glyph);
                                        paint_words(window, cx, b, words, centred);
                                    }
                                }
                            },
                        );
                    });
                }
            },
        )
        .size_full();
        // The rim, over the sliding bars too.
        let rim_work = self
            .p()
            .work
            .iter()
            .map(|w| logical_at(*w, s))
            .collect::<Vec<_>>();
        let rim = canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                if rim_alpha <= 0.0 {
                    return;
                }
                for w in &rim_work {
                    window.with_content_mask(Some(gpui::ContentMask { bounds: *w }), |window| {
                        window.paint_quad(quad(
                            sheet,
                            radius,
                            gpui::transparent_black(),
                            px(rim_px),
                            view::rim(dark).opacity(rim_alpha),
                            gpui::BorderStyle::Solid,
                        ));
                    });
                }
            },
        )
        .absolute()
        .inset_0();

        let p = Palette::for_appearance(appearance);
        div()
            .size_full()
            .relative()
            // As the views: digits of one width, and their text colour.
            .map(|mut d| {
                d.text_style()
                    .get_or_insert_with(Default::default)
                    .font_features =
                    Some(gpui::FontFeatures(Arc::new(vec![("tnum".to_string(), 1)])));
                d
            })
            .text_color(p.text)
            .child(painter)
            .child(
                div()
                    .absolute()
                    .left(body.left())
                    .top(body.top())
                    .w(body.size.width)
                    .h(body.size.height)
                    .overflow_hidden()
                    .children(slid),
            )
            .child(rim)
    }
}

/// One line of words in `b`, cut where the view would cut it.
fn paint_words(
    window: &mut Window,
    cx: &mut App,
    b: Bounds<Pixels>,
    (text, color, size, weight): &Line,
    centred: bool,
) {
    let (color, size) = (*color, *size);
    let mut font = window.text_style().font();
    font.weight = gpui::FontWeight(*weight);
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.clone(), px(size), &[run], None);
    let mut at = b.origin;
    if centred {
        at.x += (b.size.width - line.width) / 2.0;
    }
    window.with_content_mask(Some(gpui::ContentMask { bounds: b }), |window| {
        line.paint(at, b.size.height, window, cx).ok()
    });
}

/// The health bar as the views draw it: a rounded track filled by the
/// synced, local-work and needs-a-look repos' shares, in that order.
fn paint_health(
    window: &mut Window,
    b: Bounds<Pixels>,
    counts: [usize; 3],
    checking: bool,
    p: &Palette,
) {
    // Dimmed while a check runs, as the views dim it.
    let dim = |c: Hsla| if checking { c.opacity(c.a * 0.6) } else { c };
    let r = b.size.height / 2.0;
    // The track shows only with no repos: the segments cover it all, and
    // dimmed over it they would show it through.
    if counts.iter().sum::<usize>() == 0 {
        window.paint_quad(quad(
            b,
            r,
            dim(p.hover),
            px(0.0),
            gpui::transparent_black(),
            gpui::BorderStyle::Solid,
        ));
    }
    let total = counts.iter().sum::<usize>().max(1) as f32;
    let mut x = b.left();
    window.with_content_mask(Some(gpui::ContentMask { bounds: b }), |window| {
        for (n, color) in counts.into_iter().zip([p.green, p.amber, p.red]) {
            if n == 0 {
                continue;
            }
            let w = b.size.width * (n as f32 / total);
            let seg = Bounds::new(point(x, b.top()), size(w, b.size.height));
            let round = |at_end: bool| if at_end { r } else { px(0.0) };
            window.paint_quad(quad(
                seg,
                gpui::Corners {
                    top_left: round(x <= b.left()),
                    bottom_left: round(x <= b.left()),
                    top_right: round(seg.right() >= b.right() - px(0.01)),
                    bottom_right: round(seg.right() >= b.right() - px(0.01)),
                },
                dim(color),
                px(0.0),
                gpui::transparent_black(),
                gpui::BorderStyle::Solid,
            ));
            x = seg.right();
        }
    });
}
