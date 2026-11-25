//! The flyout panel: opened next to the tray icon or the overlay badge,
//! destroyed when it loses focus and recreated on the next open, so there is
//! no hidden-window state to drift.

use crate::config::Corner;
use crate::layout::{self, Rect};
use crate::view::{self, Mode, StatusView, Ui};
use crate::win;
use gpui::{
    App, AppContext, Bounds, Global, Window, WindowBounds, WindowHandle, WindowKind, WindowOptions,
    point, px, size,
};
use std::time::{Duration, Instant};

/// Clicks this soon after an open or close are ignored: the flyout
/// deactivates on the tray's mouse-down, then the click-up arrives, and a
/// double-click sends two clicks.
const DEBOUNCE: Duration = Duration::from_millis(400);
/// Wide enough for a timeline strip next to each repo's name.
const WIDTH: f32 = 480.0;
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
    /// Where the open panel hangs from, and the content height in logical
    /// px it was last fitted to.
    anchor: Option<Anchor>,
    height: f32,
    /// Where the open panel's client area goes, physical px.
    rect: Option<Rect>,
    /// The slide in under way: when it began, how long it lasts, and the
    /// work area edge it comes from.
    slide: Option<Slide>,
    /// It is sliding away to close.
    leaving: bool,
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

/// When the open panel was opened or last refitted.
pub fn fitted_at(cx: &App) -> Option<Instant> {
    cx.try_global::<PanelSlot>()?.fitted
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
    if crate::morph::hold_toggle(anchor, cx) {
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
    let rect = place(anchor, Height::Rows(rows), cx);
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
    if let Some(handle) = slot.handle.take() {
        slot.toggled_at = Some(Instant::now());
        view::remove(handle.into(), cx);
    }
}

/// Makes the open panel, cloaked under a flip back, the active window,
/// once the big window has gone from the screen. Activating places it
/// where it was opened, so it then goes back where it was fitted to since.
pub fn activate(cx: &mut App) {
    let slot = cx.default_global::<PanelSlot>();
    let (Some(handle), Some(h), Some(rect)) = (slot.handle, slot.hwnd, slot.rect) else {
        return;
    };
    handle
        .update(cx, |_, window, _| window.activate_window())
        .ok();
    // After the activation's own placement, which is queued first.
    cx.spawn(async move |_| unsafe { win::place_window(h, rect) })
        .detach();
}

/// Whether the panel draws its own rim: while it slides clipped, with
/// Windows' own off, and only then.
pub fn drawn_rim(cx: &App) -> bool {
    hwnd(cx).is_some_and(win::is_bare)
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

/// Opens the panel at `anchor`, sliding in when `slide`; `cloaked`, it
/// stays off screen until the flip back reveals it.
fn open(anchor: Anchor, slide: bool, cloaked: bool, cx: &mut App) {
    let Some(state) = cx.try_global::<Ui>().map(|ui| ui.state.clone()) else {
        return;
    };
    // Without the tray icon's rect, the cursor stands in for it, kept so
    // a refit doesn't follow the mouse.
    let anchor = match anchor {
        Anchor::Tray(None) => {
            let (x, y) = win::cursor_pos();
            Anchor::Tray(Some(Rect::new(x, y, x + 1, y + 1)))
        }
        a => a,
    };
    crate::morph::clear_strips();
    if !crate::morph::running(cx) {
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
    let rect = place(anchor, Height::Rows(rows), cx);
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
    // Opened where the slide starts, out of sight.
    let first = slide.map_or(rect, |s| layout::slide(rect, s.work, s.edge, 0.0).0);
    let sliding = slide.is_some();
    // Sliding, it stays cloaked until the slide has placed and clipped it,
    // so it never shows whole where it starts.
    let hide = sliding || cloaked;
    let bounds = logical(first);
    let mut hwnd = None;
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                // Hidden, it is placed and shown as it activates, after
                // the cloak below.
                show: !hide,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                ..Default::default()
            },
            |window, cx| {
                hwnd = win::hwnd(window);
                if hide && let Some(h) = hwnd {
                    win::cloak(h);
                }
                if cloaked {
                    // Shown where it goes without taking the foreground:
                    // the big window stays active, title bar and all,
                    // until the flip back has turned. Activated then.
                    if let Some(h) = hwnd {
                        cx.spawn(async move |_| unsafe {
                            win::place_window(h, first);
                            win::set_visible_noactivate(h, true);
                        })
                        .detach();
                    }
                } else {
                    // The tray click gave us the foreground right this needs.
                    window.activate_window();
                }
                if let Some(h) = hwnd {
                    // Native z-order calls must run outside this update.
                    let under = crate::morph::flip_hwnd(cx);
                    let dark = matches!(
                        window.appearance(),
                        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
                    );
                    // Its rim in the colour the flip and a clipped slide draw.
                    cx.spawn(async move |_| unsafe {
                        win::set_frame_colors(h, view::rim_rgb(dark), None);
                        view::seen_rim(h);
                    })
                    .detach();
                    cx.spawn(async move |_| unsafe { win::panel_topmost(h, under, sliding) })
                        .detach();
                }
                cx.new(|cx| StatusView::new(state, Mode::Panel, window, cx))
            },
        )
        .ok();
    let slot = cx.default_global::<PanelSlot>();
    slot.handle = handle;
    slot.leaving = false;
    slot.hwnd = handle.and(hwnd);
    slot.toggled_at = Some(Instant::now());
    slot.anchor = Some(anchor);
    slot.height = 0.0;
    slot.rect = Some(rect);
    slot.fitted = Some(Instant::now());
    slot.slide = slide.filter(|_| handle.is_some());
    if let (Some(slide), Some(handle), Some(h)) = (slot.slide, handle, slot.hwnd) {
        run_slide(slide, handle, h, cx);
    }
}

/// Runs `step` as each of `handle`'s frames starts, until it returns
/// false or the window closes: paced by the screen, not a timer.
fn each_frame(
    handle: WindowHandle<StatusView>,
    step: impl FnMut(&mut App) -> bool + 'static,
    cx: &mut App,
) {
    fn again(window: &mut Window, mut step: Box<dyn FnMut(&mut App) -> bool>) {
        window.on_next_frame(move |window, cx| {
            if step(cx) {
                again(window, step);
            }
        });
    }
    let step: Box<dyn FnMut(&mut App) -> bool> = Box::new(step);
    // Outside any update of that window this may be called from.
    cx.spawn(async move |cx| {
        handle.update(cx, |_, window, _| again(window, step)).ok();
    })
    .detach();
}

/// Moves the panel in, frame by frame, until its slide ends or the panel
/// closes. Reads the panel's place each frame, so a refit while sliding
/// lands where it should. The panel, opened cloaked, shows once the first
/// step has placed and clipped it.
fn run_slide(slide: Slide, handle: WindowHandle<StatusView>, h: isize, cx: &mut App) {
    let mut first = true;
    each_frame(
        handle,
        move |cx| {
            let slot = cx.default_global::<PanelSlot>();
            if slot.handle != Some(handle) || slot.leaving {
                return false;
            }
            let Some(rect) = slot.rect else {
                return false;
            };
            let t = slide.progress();
            if t >= 1.0 {
                slot.slide = None;
            }
            let shows = std::mem::take(&mut first);
            // Native window calls run outside the window's frame.
            cx.spawn(async move |cx| {
                let was = win::is_bare(h);
                if t >= 1.0 {
                    unsafe { win::slide_panel(h, rect, None) };
                } else {
                    let (moved, shown) = layout::slide(rect, slide.work, slide.edge, t);
                    unsafe { win::slide_panel(h, moved, Some(shown)) };
                }
                // Windows' rim off or back: the panel's own follows.
                if was != win::is_bare(h) {
                    handle.update(cx, |_, _, cx| cx.notify()).ok();
                }
                if shows {
                    win::uncloak(h);
                }
            })
            .detach();
            t < 1.0
        },
        cx,
    );
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
            if crate::morph::running(cx) {
                return;
            }
            let slot = cx.default_global::<PanelSlot>();
            if slot.leaving {
                return;
            }
            if let Some(handle) = slot.handle {
                handle
                    .update(cx, |_, window, cx| {
                        crate::morph::open_window(window, None, cx)
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
    if crate::morph::hold_leave(cx) {
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
    let (Some(handle), Some(h), Some(rect), Some(anchor)) =
        (slot.handle, slot.hwnd, slot.rect, slot.anchor)
    else {
        close(cx);
        return;
    };
    if ms == 0 {
        close(cx);
        return;
    }
    let (edge, work) = slide_from(anchor);
    let slide = Slide {
        start: Instant::now(),
        len: Duration::from_millis(u64::from(ms.min(2000))),
        edge,
        work,
    };

    // Keeps a refit from moving it back in.
    slot.slide = Some(slide);
    slot.leaving = true;
    each_frame(
        handle,
        move |cx| {
            if cx.default_global::<PanelSlot>().handle != Some(handle) {
                return false;
            }
            let t = slide.progress();
            if t >= 1.0 {
                cx.defer(close);
                return false;
            }
            let (moved, shown) = layout::slide(rect, work, edge, 1.0 - t);
            // Native window calls run outside the window's frame; its own
            // rim, drawn while it slides clipped, follows.
            cx.spawn(async move |cx| {
                let was = win::is_bare(h);
                unsafe { win::slide_panel(h, moved, Some(shown)) };
                if was != win::is_bare(h) {
                    handle.update(cx, |_, _, cx| cx.notify()).ok();
                }
            })
            .detach();
            true
        },
        cx,
    );
}

/// The work area edge the panel slides in from, and that work area.
fn slide_from(anchor: Anchor) -> (layout::Edge, Rect) {
    let rect = match anchor {
        Anchor::Tray(Some(rect)) | Anchor::Overlay { rect, .. } => rect,
        Anchor::Tray(None) => {
            let (x, y) = win::cursor_pos();
            Rect::new(x, y, x + 1, y + 1)
        }
    };
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

/// Resizes the open panel to `height` logical px of content, keeping it on
/// its anchor, when that differs from its size. Safe to call while drawing.
pub fn fit(height: f32, cx: &mut App) {
    if !is_open(cx) || !height.is_finite() {
        return;
    }
    let slot = cx.default_global::<PanelSlot>();
    let (Some(anchor), Some(h)) = (slot.anchor, slot.hwnd) else {
        return;
    };
    if (slot.height - height).abs() < 1.0 {
        return;
    }
    slot.height = height;
    let r = place(anchor, Height::Fit(height), cx);
    let slot = cx.default_global::<PanelSlot>();
    slot.rect = Some(r);
    slot.fitted = Some(Instant::now());
    // A slide under way moves it there itself.
    if slot.slide.is_some() {
        return;
    }
    // Native window calls must run outside this update.
    cx.spawn(async move |_| unsafe { win::place_window(h, r) })
        .detach();
}

/// How tall the panel is to be.
enum Height {
    /// Estimated from the row count, before it is laid out.
    Rows(usize),
    /// Measured, in logical px.
    Fit(f32),
}

/// The panel's rect in physical px, `height` high, next to `anchor`.
fn place(anchor: Anchor, height: Height, cx: &App) -> Rect {
    // Icons only: the strips' labels shrink to their icons.
    let icons_only = cx
        .try_global::<Ui>()
        .is_some_and(|ui| ui.state.read(cx).config.icons_only);
    let width = if icons_only {
        WIDTH - (crate::timeline::LABEL_WIDTH - crate::timeline::ICON_LABEL_WIDTH)
    } else {
        WIDTH
    };
    let anchor_rect = match anchor {
        Anchor::Tray(Some(rect)) | Anchor::Overlay { rect, .. } => rect,
        Anchor::Tray(None) => {
            let (x, y) = win::cursor_pos();
            Rect::new(x, y, x + 1, y + 1)
        }
    };
    let (ax, ay) = anchor_rect.centre();
    let mon = win::monitor_at(ax, ay);
    let scale = view::sane_scale(mon.scale);
    let work_h = mon.work.height() as f32 / scale;
    let height = match height {
        Height::Rows(rows) => layout::panel_height(rows, work_h),
        Height::Fit(h) => layout::fit_panel_height(h, work_h),
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
