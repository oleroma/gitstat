//! The flyout panel: opened next to the tray icon or the overlay badge,
//! destroyed when it loses focus and recreated on the next open, so there is
//! no hidden-window state to drift.
//!
//! Its window never moves or changes size while it is open: a see-through
//! window over its monitor's whole work area, in which the panel draws
//! itself, rim and shadow too, wherever it is to be. Sliding in or out and
//! growing or shrinking are then only what it draws, each frame whole, and
//! the work area's edge cuts it as a taskbar would.

use crate::config::Corner;
use crate::layout::{self, Rect};
use crate::view::{self, Mode, StatusView, Ui};
use crate::win;
use gpui::{
    App, AppContext, Bounds, Global, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, point, px, size,
};
use std::time::{Duration, Instant};

/// Clicks this soon after an open or close are ignored: the flyout
/// deactivates on the tray's mouse-down, then the click-up arrives, and a
/// double-click sends two clicks.
const DEBOUNCE: Duration = Duration::from_millis(400);
const WIDTH: f32 = crate::scene::panel::WIDTH;
/// Logical px between the panel and the taskbar or the badge.
const GAP: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// The tray icon's rect, when tray-icon reported one.
    Tray(Option<Rect>),
    Overlay {
        rect: Rect,
        corner: Corner,
    },
}

#[derive(Default)]
pub struct PanelSlot {
    handle: Option<WindowHandle<StatusView>>,
    /// The open panel's HWND, so the badge can stay below it.
    hwnd: Option<isize>,
    toggled_at: Option<Instant>,
    /// Where the open panel hangs from, and its height in physical px as
    /// last laid out.
    anchor: Option<Anchor>,
    height: i32,
    /// Where the open panel is on screen once slid in (its sheet inside
    /// the rim), physical px.
    rect: Option<Rect>,
    /// Where its window's client area was put: its monitor's work area.
    area: Option<Rect>,
    /// Frames it has drawn, and whether it shows yet: it opens cloaked
    /// and shows once it has drawn itself.
    drawn: u32,
    shown: bool,
    /// Opened for a flip back to land on: cloaked until that reveals it.
    held: bool,
    /// Clicks go through its window to what is under it (the mouse is
    /// off the panel itself).
    through: bool,
    /// The slide in under way: when it began, how long it lasts, and the
    /// work area edge it comes from.
    slide: Option<Slide>,
    /// How long after it was opened it first drew in place, and showed.
    timing: [Option<Duration>; 2],
    /// It is sliding away to close, and has slid all the way.
    leaving: bool,
    closing: bool,
    /// When it was last opened or refitted: the flip back waits for it to
    /// have drawn since.
    fitted: Option<Instant>,
}

/// Where the open panel is in its slide.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SlideState {
    Still,
    /// Sliding in, with this long left.
    Arriving(Duration),
    /// Sliding away to close.
    Leaving,
}

/// Where the open panel is in its slide; Still when none is open.
pub fn slide_state(cx: &App) -> SlideState {
    let Some(slot) = cx.try_global::<PanelSlot>() else {
        return SlideState::Still;
    };
    if !is_open(cx) {
        return SlideState::Still;
    }
    if slot.leaving {
        return SlideState::Leaving;
    }
    match slot.slide {
        Some(s) if s.progress() < 1.0 => {
            SlideState::Arriving(s.len.saturating_sub(s.start.elapsed()))
        }
        _ => SlideState::Still,
    }
}

/// Whether the open panel is sliding away to close.
pub fn is_leaving(cx: &App) -> bool {
    slide_state(cx) == SlideState::Leaving
}

/// The open panel's window on the screen, physical px: its monitor's
/// work area.
pub fn area(cx: &App) -> Option<Rect> {
    if !is_open(cx) {
        return None;
    }
    cx.try_global::<PanelSlot>()?.area
}

/// Where the open panel's window's inside starts on the screen, physical
/// px: what it draws is placed from there.
pub fn origin(cx: &App) -> Option<(i32, i32)> {
    let slot = cx.try_global::<PanelSlot>()?;
    let client = slot.hwnd.and_then(win::client_rect).or(slot.area)?;
    Some((client.left, client.top))
}

/// Whether the open panel's window is where it belongs yet.
pub fn placed(cx: &App) -> bool {
    cx.try_global::<PanelSlot>().is_some_and(|slot| {
        slot.area.is_some() && slot.hwnd.and_then(win::client_rect) == slot.area
    })
}

#[derive(Clone, Copy)]
struct Slide {
    start: Instant,
    len: Duration,
    edge: layout::Edge,
    work: Rect,
}

impl Slide {
    fn progress(&self) -> f32 {
        if self.len.is_zero() {
            return 1.0;
        }
        (self.start.elapsed().as_secs_f32() / self.len.as_secs_f32()).min(1.0)
    }
}

impl Global for PanelSlot {}

impl PanelSlot {
    /// The panel's window, open or being drawn.
    pub fn handle(&self) -> Option<WindowHandle<StatusView>> {
        self.handle
    }
}

/// Closes the panel if it is open, else opens it at `anchor` unless the
/// click is the tail of the one that just closed it.
pub fn toggle(anchor: Anchor, cx: &mut App) {
    // A flip under way owns the panel: toggled once it has ended.
    if crate::flip::hold_toggle(anchor, cx) {
        return;
    }
    if is_open(cx) {
        slide_away(cx);
        return;
    }
    if cx
        .default_global::<PanelSlot>()
        .toggled_at
        .is_some_and(|t| t.elapsed() < DEBOUNCE)
    {
        return;
    }
    open(anchor, true, false, cx);
}

/// Whether the open panel is on screen yet: drawn, and no longer cloaked.
pub fn shows(cx: &App) -> bool {
    is_open(cx) && cx.try_global::<PanelSlot>().is_some_and(|s| s.shown)
}

/// The open panel's window, and whether clicks pass it where it should
/// let them (`win::passes_clicks`), tried beside the panel: for the
/// self-test's report.
pub fn click_test(cx: &App) -> Option<(isize, i32, i32)> {
    let slot = cx.try_global::<PanelSlot>()?;
    let (h, rect, area) = (slot.hwnd?, slot.rect?, slot.area?);
    // A point of its window well away from the panel itself.
    let x = if rect.left - area.left > area.right - rect.right {
        (area.left + rect.left) / 2
    } else {
        (rect.right + area.right) / 2
    };
    Some((h, x, (area.top + area.bottom) / 2))
}

/// The open panel's state in words, for the self-test's report.
pub fn facts(cx: &App) -> String {
    let Some(slot) = cx.try_global::<PanelSlot>() else {
        return "no panel yet".into();
    };
    format!(
        "  rect {:?}\n  window area {:?}\n  client now {:?}\n  ex-style {:#x}\n  frames in place {} \
         shown {} click-through {} leaving {} height {}\n  first drawn in place after {:?}, \
         shown after {:?}",
        slot.rect,
        slot.area,
        slot.hwnd.and_then(win::client_rect),
        slot.hwnd.map_or(0, win::ex_style),
        slot.drawn,
        slot.shown,
        slot.through,
        slot.leaving,
        slot.height,
        slot.timing[0],
        slot.timing[1],
    )
}

/// The open panel's client area, or the last one's, physical px.
pub fn last_rect(cx: &App) -> Option<Rect> {
    cx.try_global::<PanelSlot>()?.rect
}

/// Where the panel would open, when it hasn't been open since gitstat
/// started: by the floating button if it shows, else by the tray. So the
/// big window, opened some other way, still flips back into it.
pub fn guess_place(cx: &mut App) -> Option<Rect> {
    if let Some(rect) = last_rect(cx) {
        return Some(rect);
    }
    let anchor =
        crate::overlay::anchor(cx).or_else(|| win::tray_area().map(|r| Anchor::Tray(Some(r))))?;
    let state = cx.try_global::<Ui>()?.state.clone();
    let rows = view::panel_rows(state.read(cx));
    let rect = place(anchor, Height::Rows(rows));
    let slot = cx.default_global::<PanelSlot>();
    slot.anchor = Some(anchor);
    slot.rect = Some(rect);
    Some(rect)
}

/// Opens the panel where it last was, at once and cloaked, for the flip
/// back to land on and reveal. One sliding away is closed first: it is
/// going, and a fresh one comes. False when it has never been open.
pub fn reopen_cloaked(cx: &mut App) -> bool {
    let Some(anchor) = cx.try_global::<PanelSlot>().and_then(|s| s.anchor) else {
        return false;
    };
    if is_leaving(cx) {
        close(cx);
    }
    if !is_open(cx) {
        open(anchor, false, true, cx);
    }
    is_open(cx)
}

pub fn close(cx: &mut App) {
    let slot = cx.default_global::<PanelSlot>();
    slot.hwnd = None;
    slot.leaving = false;
    slot.closing = false;
    slot.slide = None;
    slot.shown = false;
    slot.held = false;
    if let Some(handle) = slot.handle.take() {
        slot.toggled_at = Some(Instant::now());
        view::remove(handle.into(), cx);
    }
}

/// Makes the open panel, cloaked under a flip back, the active window,
/// once the big window has gone from the screen: the flip then shows it.
/// By Windows' own call: GPUI's first activation of a window also puts
/// it where it was opened, a little off where it belongs, and one frame
/// of the flip would show there. Only when Windows won't have it (the
/// foreground is another app's) does GPUI's way stand in, put right at
/// once after.
pub fn activate(cx: &mut App) {
    let slot = cx.default_global::<PanelSlot>();
    let (Some(handle), Some(h), Some(area)) = (slot.handle, slot.hwnd, slot.area) else {
        return;
    };
    slot.shown = true;
    slot.held = false;
    win::to_front(h);
    if win::is_front(h) {
        return;
    }
    handle
        .update(cx, |_, window, _| window.activate_window())
        .ok();
    // After the activation's own placement, which is queued first.
    cx.spawn(async move |_| unsafe { win::place_window(h, area) })
        .detach();
}

pub fn is_open(cx: &App) -> bool {
    cx.try_global::<PanelSlot>()
        .and_then(|slot| slot.handle)
        .is_some_and(|h| view::is_live(h.into(), cx))
}

/// The panel's HWND while it is open.
pub fn hwnd(cx: &App) -> Option<isize> {
    if !is_open(cx) {
        return None;
    }
    cx.try_global::<PanelSlot>()?.hwnd
}

/// Closes the panel from inside one of its own handlers, where its window
/// is borrowed and `close` could only defer.
pub(crate) fn dismiss(window: &mut Window, cx: &mut App) {
    let ours = cx
        .default_global::<PanelSlot>()
        .handle
        .is_some_and(|h| gpui::AnyWindowHandle::from(h) == window.window_handle());
    if ours {
        slide_away(cx);
    } else {
        window.remove_window();
    }
}

/// The window the panel at `anchor` draws in: its monitor's work area.
/// Never a whole monitor, which Windows would take for a full-screen app
/// and drop the taskbar behind: with no taskbar taking room, a px less.
fn window_area(anchor: Anchor) -> Rect {
    let (x, y) = anchor_rect(anchor).centre();
    let mon = win::monitor_at(x, y);
    if mon.work == mon.rect {
        Rect::new(
            mon.work.left,
            mon.work.top + 1,
            mon.work.right,
            mon.work.bottom,
        )
    } else {
        mon.work
    }
}

fn anchor_rect(anchor: Anchor) -> Rect {
    match anchor {
        Anchor::Tray(Some(rect)) | Anchor::Overlay { rect, .. } => rect,
        Anchor::Tray(None) => {
            let (x, y) = win::cursor_pos();
            Rect::new(x, y, x + 1, y + 1)
        }
    }
}

/// Opens the panel at `anchor`, sliding in when `slide`; `cloaked`, it
/// stays off screen until the flip back reveals it.
fn open(anchor: Anchor, slide: bool, cloaked: bool, cx: &mut App) {
    let Some(state) = cx.try_global::<Ui>().map(|ui| ui.state.clone()) else {
        return;
    };
    // Without the tray icon's rect, the cursor stands in for it, kept so
    // a refit doesn't follow the mouse.
    let anchor = match anchor {
        Anchor::Tray(None) => Anchor::Tray(Some(anchor_rect(anchor))),
        a => a,
    };
    if !crate::flip::running(cx) {
        crate::frames::watch(win::primary_monitor().rect, "panel", cx);
    }
    if let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) {
        // The big window, open, shares the trees: its columns glide back
        // rather than jump.
        if view::window_open(cx) {
            t.update(cx, |t, cx| t.glide_newest(Duration::from_millis(200), cx));
        } else {
            t.update(cx, |t, cx| t.show_newest(cx));
        }
    }
    let rows = view::panel_rows(state.read(cx));
    let rect = place(anchor, Height::Rows(rows));
    // Slides in from the taskbar's side, or the badge's top or bottom.
    let ms = state.read(cx).config.panel_slide_ms;
    let slide = (slide && ms > 0).then(|| {
        let (edge, work) = slide_from(anchor);
        Slide {
            start: Instant::now(),
            len: Duration::from_millis(u64::from(ms.min(2000))),
            edge,
            work,
        }
    });
    let area = window_area(anchor);
    let mut hwnd = None;
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(logical(area))),
                titlebar: None,
                // Placed and shown as it activates, after the cloak below.
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
                    // Out of sight until it has drawn itself, and with none
                    // of Windows' frame: it draws its own.
                    win::cloak(h);
                    win::bare(h);
                }
                if !cloaked {
                    // The tray click gave us the foreground right this needs.
                    window.activate_window();
                }
                if let Some(h) = hwnd {
                    // Native window calls must run outside this update.
                    cx.spawn(async move |_| unsafe {
                        win::place_panel(h, area, None);
                        win::click_through(h, true);
                        if cloaked {
                            // Shown without taking the foreground: the big
                            // window stays active, title bar and all, until
                            // the flip back has turned. Activated then.
                            win::set_visible_noactivate(h, true);
                        }
                    })
                    .detach();
                }
                cx.new(|cx| StatusView::new(state, Mode::Panel, window, cx))
            },
        )
        .ok();
    let slot = cx.default_global::<PanelSlot>();
    slot.handle = handle;
    slot.leaving = false;
    slot.closing = false;
    slot.hwnd = handle.and(hwnd);
    slot.toggled_at = Some(Instant::now());
    slot.anchor = Some(anchor);
    slot.height = 0;
    slot.rect = Some(rect);
    slot.area = Some(area);
    slot.drawn = 0;
    slot.timing = [None; 2];
    slot.shown = false;
    slot.held = cloaked;
    slot.through = true;
    slot.fitted = Some(Instant::now());
    slot.slide = slide.filter(|_| handle.is_some());
    if let Some(handle) = handle {
        track_mouse(handle, cx);
    }
}

/// How often the mouse is looked for while the panel is open.
const MOUSE_EVERY: Duration = Duration::from_millis(16);

/// While the panel is open, lets clicks through its window wherever the
/// panel itself isn't: the window covers the whole work area, and what is
/// under the rest of it must stay as usable as ever. A click elsewhere
/// then takes the focus, which closes the panel.
fn track_mouse(handle: WindowHandle<StatusView>, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(MOUSE_EVERY).await;
            let open = cx.update(|cx| {
                // A flip is drawn all over its window, and takes what
                // clicks land on it.
                let flipping = crate::flip::running(cx);
                let slot = cx.default_global::<PanelSlot>();
                if slot.handle != Some(handle) {
                    return false;
                }
                let (Some(h), Some(rect)) = (slot.hwnd, slot.rect) else {
                    return true;
                };
                let (x, y) = win::cursor_pos();
                let on_panel = flipping
                    || slot.shown
                        && !slot.leaving
                        && (rect.left..rect.right).contains(&x)
                        && (rect.top..rect.bottom).contains(&y);
                if slot.through == on_panel {
                    slot.through = !on_panel;
                    cx.spawn(async move |_| unsafe { win::click_through(h, !on_panel) })
                        .detach();
                    if !on_panel {
                        // No mouse move reaches it from out there: told
                        // where the mouse went, so nothing stays hovered.
                        win::tell_mouse(h, x, y);
                    }
                }
                true
            });
            if !open.unwrap_or(false) {
                break;
            }
        }
    })
    .detach();
}

/// How long a just opened panel waits for its window to be where it
/// belongs before it shows anyway.
const PLACED_BY: Duration = Duration::from_millis(300);

/// Where the open panel draws itself this frame.
pub struct Sheet {
    /// Its top-left corner in its window now, physical px: where it rests,
    /// or part way there while it slides.
    pub at: (i32, i32),
    /// It is sliding, or yet to show: the next frame differs.
    pub moving: bool,
}

/// The panel's width and the most it may be tall, physical px: as wide as
/// its rows need, and as tall as its monitor's work area less the gaps.
pub fn room(cx: &App) -> Option<(i32, i32)> {
    let anchor = cx.try_global::<PanelSlot>()?.anchor?;
    let r = place(anchor, Height::Px(i32::MAX / 4));
    Some((r.width(), r.height()))
}

/// The open panel is `height` physical px tall as laid out now: where it
/// goes, keeping it on its anchor, and where it draws itself this frame.
/// Called as `window`, the panel's, lays itself out. None for any other.
pub fn sheet(height: i32, window: &Window, cx: &mut App) -> Option<Sheet> {
    let me = window.window_handle();
    let slot = cx.default_global::<PanelSlot>();
    if slot.handle.map(gpui::AnyWindowHandle::from) != Some(me) {
        return None;
    }
    let (anchor, h) = (slot.anchor?, slot.hwnd?);
    if slot.height != height {
        slot.height = height;
        slot.fitted = Some(Instant::now());
    }
    let rect = place(anchor, Height::Px(height));
    let slot = cx.default_global::<PanelSlot>();
    slot.rect = Some(rect);
    let client = win::client_rect(h).or(slot.area)?;
    // Until its window is where it belongs (give or take, should Windows
    // have its own idea of that), no frame counts as drawn.
    let late = slot.toggled_at.is_some_and(|t| t.elapsed() > PLACED_BY);
    if !slot.shown && Some(client) != slot.area && !late {
        slot.drawn = 0;
    }
    let mut at = rect;
    let mut moving = !slot.shown && !slot.held;
    if let Some(slide) = slot.slide {
        // Its slide starts once it shows.
        let t = if slot.shown { slide.progress() } else { 0.0 };
        let inside = if slot.leaving { 1.0 - t } else { t };
        at = layout::slide(rect, slide.work, slide.edge, inside).0;
        if t < 1.0 {
            moving = true;
        } else if slot.leaving {
            // Slid away: closed, once, outside this draw of its window.
            if !slot.closing {
                slot.closing = true;
                cx.spawn(async move |cx| cx.update(close).ok()).detach();
            }
        } else {
            slot.slide = None;
        }
    }
    Some(Sheet {
        at: (at.left - client.left, at.top - client.top),
        moving,
    })
}

/// The panel has drawn a frame: once it has drawn itself where it goes,
/// it shows, and its slide in starts from there.
pub fn drew(window: &mut Window, cx: &mut App) {
    let me = window.window_handle();
    let slot = cx.default_global::<PanelSlot>();
    if slot.handle.map(gpui::AnyWindowHandle::from) != Some(me) {
        return;
    }
    slot.drawn += 1;
    let since = slot.toggled_at.map(|t| t.elapsed());
    if slot.drawn == 1 && slot.timing[0].is_none() {
        slot.timing[0] = since;
    }
    // The second frame: the first may have been laid out before the
    // window was where it belongs.
    if slot.drawn == 2 && !slot.shown && !slot.held {
        slot.shown = true;
        slot.timing[1] = since;
        if let Some(slide) = &mut slot.slide {
            slide.start = Instant::now();
        }
        if let Some(h) = slot.hwnd {
            // As the next frame starts: this one, drawn where the slide
            // starts, is on its way to the screen by then.
            window.on_next_frame(move |_, _| win::uncloak(h));
        }
    }
}

/// Opens the big window through the panel: slides the panel in at
/// `anchor` (or lets the open one finish sliding), then flips it into the
/// window.
pub fn then_window(anchor: Anchor, cx: &mut App) {
    let open_now = is_open(cx);
    let slot = cx.default_global::<PanelSlot>();
    let settled = open_now && !slot.leaving;
    let remaining = |s: &Slide| s.len.saturating_sub(s.start.elapsed());
    let mut wait = slot.slide.as_ref().map_or(Duration::ZERO, remaining);
    if !settled {
        // Sliding away (a double click's second click toggled it): back
        // at once, without sliding in again.
        let leaving = open_now;
        if leaving {
            close(cx);
        }
        open(anchor, !leaving, false, cx);
        wait = cx
            .try_global::<PanelSlot>()
            .and_then(|s| s.slide.as_ref().map(remaining))
            .unwrap_or_default();
    }
    // A moment more for the panel to draw its strips, which the flip
    // starts from.
    let wait = wait + Duration::from_millis(60);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(wait).await;
        cx.update(|cx| {
            if crate::flip::running(cx) {
                return;
            }
            let slot = cx.default_global::<PanelSlot>();
            if slot.leaving {
                return;
            }
            if let Some(handle) = slot.handle {
                handle
                    .update(cx, |_, window, cx| {
                        crate::flip::open_window(window, None, cx)
                    })
                    .ok();
            }
        })
        .ok();
    })
    .detach();
}

/// Slides the open panel back out the way it came in, then closes it; closes
/// it at once when sliding is off. A flip under way that owns the panel
/// lets it go once that ends.
pub fn slide_away(cx: &mut App) {
    if crate::flip::hold_leave(cx) {
        return;
    }
    slide_away_now(cx);
}

/// `slide_away`, whatever a flip is doing: for the flip itself, as it ends.
pub fn slide_away_now(cx: &mut App) {
    let ms = cx
        .try_global::<Ui>()
        .map_or(0, |ui| ui.state.read(cx).config.panel_slide_ms);
    let slot = cx.default_global::<PanelSlot>();
    if slot.leaving && slot.handle.is_some() {
        return;
    }
    let (Some(handle), Some(anchor)) = (slot.handle, slot.anchor) else {
        close(cx);
        return;
    };
    if ms == 0 || !slot.shown {
        close(cx);
        return;
    }
    let len = Duration::from_millis(u64::from(ms.min(2000)));
    // A panel still sliding in leaves from where it is, back the way it
    // came, rather than jumping fully in first.
    let (edge, work, start) = match slot.slide {
        Some(inward) => {
            let shown = inward.progress();
            let back = len.mul_f32(1.0 - shown);
            let now = Instant::now();
            (
                inward.edge,
                inward.work,
                now.checked_sub(back).unwrap_or(now),
            )
        }
        None => {
            let (edge, work) = slide_from(anchor);
            (edge, work, Instant::now())
        }
    };
    slot.slide = Some(Slide {
        start,
        len,
        edge,
        work,
    });
    slot.leaving = true;
    crate::frames::watch(win::primary_monitor().rect, "panel-out", cx);
    // A commit's card stays where the mouse is: gone before the panel
    // moves from under it.
    if let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) {
        t.update(cx, |t, cx| t.drop_hover(cx));
    }
    // It draws itself sliding out, a frame at a time, and closes as it
    // gets there (`sheet`); outside any update of its window this may be
    // called from.
    cx.spawn(async move |cx| {
        handle.update(cx, |_, window, _| window.refresh()).ok();
        // Should it stop drawing (its screen gone, say), it closes anyway.
        cx.background_executor()
            .timer(len + Duration::from_millis(150))
            .await;
        cx.update(|cx| {
            let slot = cx.default_global::<PanelSlot>();
            if slot.handle == Some(handle) && slot.leaving {
                close(cx);
            }
        })
        .ok();
    })
    .detach();
}

/// The work area edge the panel slides in from, and that work area.
fn slide_from(anchor: Anchor) -> (layout::Edge, Rect) {
    let rect = anchor_rect(anchor);
    let (cx, cy) = rect.centre();
    let mon = win::monitor_at(cx, cy);
    let edge = match anchor {
        Anchor::Overlay { corner, .. } => match corner {
            Corner::TopLeft | Corner::TopRight => layout::Edge::Top,
            Corner::BottomLeft | Corner::BottomRight => layout::Edge::Bottom,
        },
        Anchor::Tray(_) => layout::taskbar_edge(mon.rect, mon.work, rect),
    };
    (edge, mon.work)
}

/// How tall the panel is to be.
enum Height {
    /// Estimated from the row count, before it is laid out.
    Rows(usize),
    /// Laid out, in physical px.
    Px(i32),
}

/// The panel's rect in physical px, `height` high, next to `anchor`.
fn place(anchor: Anchor, height: Height) -> Rect {
    let width = WIDTH;
    let anchor_rect = anchor_rect(anchor);
    let (ax, ay) = anchor_rect.centre();
    let mon = win::monitor_at(ax, ay);
    let scale = view::sane_scale(mon.scale);
    let work_h = mon.work.height() as f32 / scale;
    let height = match height {
        Height::Rows(rows) => layout::panel_height(rows, work_h),
        Height::Px(h) => layout::fit_panel_height(h as f32 / scale, work_h),
    };
    let (w, h, gap) = (
        (width * scale).round() as i32,
        (height * scale).round() as i32,
        (GAP * scale).round() as i32,
    );
    match anchor {
        Anchor::Overlay { rect, corner } => {
            layout::flyout_from_corner(rect, corner, mon.work, w, h, gap)
        }
        Anchor::Tray(_) => {
            let edge = layout::taskbar_edge(mon.rect, mon.work, anchor_rect);
            layout::flyout(anchor_rect, mon.work, edge, w, h, gap)
        }
    }
}

/// A physical rect as GPUI's window bounds. GPUI converts window_bounds
/// with the primary monitor's scale (the window is created at
/// CW_USEDEFAULT), and the tray and the badge live on the primary monitor.
pub(crate) fn logical(r: Rect) -> Bounds<gpui::Pixels> {
    let primary = view::sane_scale(win::primary_monitor().scale);
    let at = |v: i32| px(v as f32 / primary);
    Bounds::new(
        point(at(r.left), at(r.top)),
        size(at(r.width()), at(r.height())),
    )
}
