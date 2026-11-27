//! The floating view's tooltips and commit cards, drawn as the panel and
//! the big window draw theirs, in a window of their own.
//!
//! The view's window is no bigger than the view, so nothing it draws can
//! reach past it. This window covers the work area of the view's screen:
//! transparent, topmost, never activated and click-through, it draws one
//! tooltip or one commit card anywhere on it. It never moves while it
//! shows, so what it shows changes in place from one frame to the next;
//! when it comes it is drawn twice, hidden, before it shows.

use crate::layout::Rect;
use crate::timeline::Timelines;
use crate::view::{self, Palette, Ui};
use crate::win;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Corner, Entity, Global, Hsla, IntoElement,
    ParentElement, Pixels, Point, Render, SharedString, Styled, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, anchored, canvas, div, point, px, size,
};
use std::time::Duration;

/// How long the mouse rests on something before its tooltip shows: as
/// GPUI's own tooltips wait.
pub const DELAY: Duration = Duration::from_millis(500);

/// A tooltip's lines, and where the mouse was (screen px).
type Asked<T> = Option<(Vec<(T, Hsla)>, (i32, i32))>;

/// What the window shows.
#[derive(Clone, Debug, PartialEq)]
enum Shows {
    /// A tooltip's lines, at the mouse (screen px).
    Lines(Vec<(SharedString, Hsla)>, (i32, i32)),
    /// The card of the dot under the mouse in the view's window, at that
    /// dot (screen px).
    Card((i32, i32)),
}

#[derive(Default)]
struct TipSlot {
    handle: Option<WindowHandle<TipView>>,
    hwnd: Option<isize>,
    /// The tooltip the view asks for, and where the mouse was.
    lines: Asked<SharedString>,
    /// What shows, and the screen's work area the window covers.
    shows: Option<Shows>,
    area: Option<Rect>,
    /// Frames drawn where it now is, while it waits to show; and whether
    /// it is on the screen.
    drawn: u32,
    visible: bool,
    /// Where what it shows is on the screen, as last drawn.
    content: Option<Rect>,
    /// A task follows the mouse while it is on what this shows.
    watching: bool,
}

impl Global for TipSlot {}

pub struct TipView {
    timelines: Option<Entity<Timelines>>,
}

impl Render for TipView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_appearance(window.appearance());
        let dark = matches!(
            window.appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        );
        let s = window.scale_factor();
        let slot = cx.default_global::<TipSlot>();
        let (shows, area) = (slot.shows.clone(), slot.area);
        // A screen px as a px of this window.
        let local = |(x, y): (i32, i32)| -> Point<Pixels> {
            let a = area.unwrap_or_default();
            point(px((x - a.left) as f32 / s), px((y - a.top) as f32 / s))
        };
        let half = px(area.map_or(0, |a| a.height()) as f32 / s / 2.0);
        let origin = area.map_or((0, 0), |a| (a.left, a.top));
        // What it shows, with where that lands on the screen noted: the
        // mouse on it is on it, not on what it covers.
        let measured = move |el: AnyElement| {
            div().relative().child(el).child(
                canvas(
                    move |b, _, cx| {
                        let e = |v: Pixels| (f32::from(v) * s).round() as i32;
                        cx.default_global::<TipSlot>().content = Some(Rect::new(
                            origin.0 + e(b.left()),
                            origin.1 + e(b.top()),
                            origin.0 + e(b.right()),
                            origin.1 + e(b.bottom()),
                        ));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
        };
        let body = match &shows {
            // Where GPUI puts a tooltip: a px right of and below the
            // mouse, the other way where the screen ends.
            Some(Shows::Lines(lines, at)) => Some(
                anchored()
                    .anchor(Corner::TopLeft)
                    .position(local(*at) + point(px(1.), px(1.)))
                    .child(measured(
                        view::tip_body(lines, p.bg, p.border).into_any_element(),
                    ))
                    .into_any_element(),
            ),
            // Where the panel puts a card: from a little left of its dot,
            // below it, or over it in the screen's lower half.
            Some(Shows::Card(at)) => {
                let float = crate::overlay::hwnd(cx);
                self.timelines.as_ref().and_then(|t| {
                    let (card, _) = t.read(cx).card_for(&p, dark, float, cx)?;
                    let dot = local(*at);
                    let (ox, oy) = Timelines::CARD_OFFSET;
                    let x = dot.x - px(ox);
                    let (corner, y) = if dot.y > half {
                        (Corner::BottomLeft, dot.y - px(oy))
                    } else {
                        (Corner::TopLeft, dot.y + px(oy))
                    };
                    Some(
                        anchored()
                            .anchor(corner)
                            .position(point(x, y))
                            .snap_to_window_with_margin(px(8.))
                            .child(measured(card))
                            .into_any_element(),
                    )
                })
            }
            None => None,
        };
        let hwnd = win::hwnd(window);
        div()
            .size_full()
            .when_some(body, |d, body| d.child(body))
            .child(
                canvas(|_, _, _| (), move |_, _, window, cx| drew(hwnd, window, cx))
                    .absolute()
                    .size_full(),
            )
    }
}

/// The window drew a frame: once it has drawn twice where it now is, it
/// shows.
fn drew(hwnd: Option<isize>, window: &mut Window, cx: &mut App) {
    let slot = cx.default_global::<TipSlot>();
    if slot.visible || slot.shows.is_none() || slot.hwnd != hwnd {
        return;
    }
    let Some(h) = hwnd else { return };
    if slot.area.is_some() && win::client_rect(h) == slot.area {
        slot.drawn += 1;
    }
    if slot.drawn >= 2 {
        slot.visible = true;
        cx.defer(move |_| {
            win::dwm_flush();
            win::uncloak(h);
        });
    } else {
        window.request_animation_frame();
    }
}

/// The view's tooltip: these lines at the mouse (screen px), or none.
pub fn set_lines(lines: Asked<String>, cx: &mut App) {
    let lines = lines.map(|(lines, at)| {
        let lines = lines
            .into_iter()
            .map(|(t, c)| (SharedString::from(t), c))
            .collect();
        (lines, at)
    });
    let slot = cx.default_global::<TipSlot>();
    if slot.lines == lines {
        return;
    }
    slot.lines = lines;
    sync(cx);
}

/// Shows what there is to show (a commit card over a tooltip), or
/// nothing.
pub fn sync(cx: &mut App) {
    let cards = cx
        .try_global::<Ui>()
        .is_some_and(|ui| ui.state.read(cx).config.float.cards);
    // Only on the view at rest: not while it grows, shrinks or is the
    // badge.
    let card = (cards && crate::overlay::resting(cx) == Some(true))
        .then(|| {
            let float = crate::overlay::hwnd(cx)?;
            let (left, top) = crate::overlay::origin(cx)?;
            let t = cx.try_global::<Ui>()?.timelines.clone();
            let at = t.read(cx).card_at(Some(float))?;
            let s = win::window_scale(float).unwrap_or(1.0);
            Some((
                left + (f32::from(at.x) * s).round() as i32,
                top + (f32::from(at.y) * s).round() as i32,
            ))
        })
        .flatten();
    let slot = cx.default_global::<TipSlot>();
    let want = match (card, &slot.lines) {
        (Some(dot), _) => Shows::Card(dot),
        (None, Some((lines, at))) => Shows::Lines(lines.clone(), *at),
        (None, None) => {
            hide(cx);
            return;
        }
    };
    let at = match &want {
        Shows::Card(at) | Shows::Lines(_, at) => *at,
    };
    let area = win::monitor_at(at.0, at.1).work;
    if slot.shows.as_ref() == Some(&want) && slot.area == Some(area) {
        return;
    }
    ensure_open(cx);
    let slot = cx.default_global::<TipSlot>();
    let Some(handle) = slot.handle else { return };
    slot.shows = Some(want);
    if slot.area == Some(area) && slot.visible {
        // In place already: what it shows changes in the next frame, over
        // the view, which may have changed windows meanwhile.
        if let Some(h) = slot.hwnd {
            cx.spawn(async move |_| unsafe { win::reassert_topmost(h) })
                .detach();
        }
        cx.defer(move |cx| {
            handle.update(cx, |_, window, _| window.refresh()).ok();
        });
        return;
    }
    // Hidden while it is put in place; shown once drawn there.
    slot.content = None;
    slot.area = Some(area);
    slot.drawn = 0;
    slot.visible = false;
    let Some(h) = slot.hwnd else { return };
    win::cloak(h);
    cx.spawn(async move |cx| {
        unsafe { win::place_overlay(h, area, None) };
        cx.update(|cx| {
            handle.update(cx, |_, window, _| window.refresh()).ok();
        })
        .ok();
    })
    .detach();
}

/// Hides whatever shows.
#[track_caller]
pub fn hide(cx: &mut App) {
    let Some(slot) = cx.try_global::<TipSlot>() else {
        return;
    };
    if slot.shows.is_none() && slot.lines.is_none() {
        return;
    }
    trace(format!(
        "hide from {}: {}",
        std::panic::Location::caller(),
        trace_where(cx)
    ));
    let slot = cx.default_global::<TipSlot>();
    slot.shows = None;
    slot.lines = None;
    slot.visible = false;
    slot.content = None;
    if let Some(h) = slot.hwnd {
        win::cloak(h);
    }
}

/// Removes the window, as the view's go.
pub fn close(cx: &mut App) {
    let Some(slot) = cx.try_global::<TipSlot>() else {
        return;
    };
    let handle = slot.handle;
    cx.set_global(TipSlot::default());
    if let Some(handle) = handle {
        view::remove(handle.into(), cx);
    }
}

fn ensure_open(cx: &mut App) {
    if cx.default_global::<TipSlot>().handle.is_some() {
        return;
    }
    let timelines = cx.try_global::<Ui>().map(|ui| ui.timelines.clone());
    let mut hwnd = None;
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(64.), px(64.)),
                ))),
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
                hwnd = win::hwnd(window);
                if let Some(h) = hwnd {
                    win::cloak(h);
                }
                if let Some(t) = &timelines {
                    // A card comes and goes with the dot under the mouse.
                    cx.observe(t, |_, cx| cx.defer(sync)).detach();
                }
                cx.new(|_| TipView { timelines })
            },
        )
        .ok();
    if let Some(h) = hwnd {
        cx.spawn(async move |_| unsafe { win::style_see_through(h) })
            .detach();
    }
    let slot = cx.default_global::<TipSlot>();
    slot.handle = handle;
    slot.hwnd = hwnd;
    slot.area = None;
    slot.visible = false;
}

/// The mouse at `pos` in `window` is on what this shows.
pub fn holds_event(pos: Point<Pixels>, window: &Window, cx: &App) -> bool {
    let Some(c) = win::hwnd(window).and_then(win::client_rect) else {
        return false;
    };
    let s = window.scale_factor();
    let x = c.left + (f32::from(pos.x) * s).floor() as i32;
    let y = c.top + (f32::from(pos.y) * s).floor() as i32;
    holds_at(x, y, cx) || holds_mouse(cx)
}

/// Its window, while it shows: kept over the view.
pub fn shown_hwnd(cx: &App) -> Option<isize> {
    let slot = cx.try_global::<TipSlot>()?;
    slot.hwnd.filter(|_| slot.visible && slot.shows.is_some())
}

/// A trace of what made the floating view's tooltip or card go, in
/// %TEMP%\\gitstat-tip.log, kept short: to find what closes them.
pub fn trace(what: impl AsRef<str>) {
    use std::io::Write;
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let ms = EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis();
    let path = std::env::temp_dir().join("gitstat-tip.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 512 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{ms:8} {}", what.as_ref());
    }
}

/// Where the mouse is, and where what shows is, for the trace.
pub fn trace_where(cx: &App) -> String {
    let (x, y) = win::cursor_pos();
    let slot = cx.try_global::<TipSlot>();
    let content = slot
        .and_then(|s| s.content)
        .map(|r| (r.left, r.top, r.right, r.bottom));
    let visible = slot.is_some_and(|s| s.visible);
    let kind = slot.and_then(|s| s.shows.as_ref()).map(|s| match s {
        Shows::Lines(..) => "lines",
        Shows::Card(_) => "card",
    });
    format!("mouse ({x},{y}) shows {kind:?} visible {visible} content {content:?}")
}

/// The mouse is on what it shows: that stays while it is.
pub fn holds_mouse(cx: &App) -> bool {
    let (x, y) = win::cursor_pos();
    holds_at(x, y, cx)
}

/// What it shows is at (`x`, `y`) on the screen, physical px.
pub fn holds_at(x: i32, y: i32, cx: &App) -> bool {
    let Some(slot) = cx.try_global::<TipSlot>() else {
        return false;
    };
    slot.visible
        && slot.shows.is_some()
        && slot
            .content
            .is_some_and(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
}

/// How long what this shows waits for the mouse, after it left the view
/// or what this shows, before it goes.
pub const GRACE: Duration = Duration::from_millis(350);

/// The mouse left the view while this shows: it stays a moment for the
/// mouse to get onto it, and as long as the mouse is on it. Once the mouse
/// has been off it, and off the view, a moment, it all goes.
pub fn watch_mouse(cx: &mut App) {
    let slot = cx.default_global::<TipSlot>();
    if slot.watching {
        return;
    }
    slot.watching = true;
    cx.spawn(async move |cx| {
        let tick = Duration::from_millis(40);
        let mut off = Duration::ZERO;
        loop {
            cx.background_executor().timer(tick).await;
            let done = cx
                .update(|cx| {
                    let (x, y) = win::cursor_pos();
                    let on_view = crate::overlay::placed(cx)
                        .is_some_and(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom);
                    // Back on the view: the view's own hover takes over.
                    if on_view || shown_hwnd(cx).is_none() {
                        cx.default_global::<TipSlot>().watching = false;
                        return true;
                    }
                    if holds_at(x, y, cx) {
                        off = Duration::ZERO;
                        return false;
                    }
                    off += tick;
                    if off < GRACE {
                        return false;
                    }
                    cx.default_global::<TipSlot>().watching = false;
                    trace(format!("watch lets go: {}", trace_where(cx)));
                    crate::overlay::let_go(cx);
                    true
                })
                .unwrap_or(true);
            if done {
                break;
            }
        }
    })
    .detach();
}
