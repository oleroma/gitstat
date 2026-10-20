//! The always-on-top corner badge with the unsynced count.
//!
//! The window is opened hidden (`show: false`) and never activated: GPUI's
//! first `activate()` would apply the placement parked at creation. It is
//! placed, shown and hidden natively from spawned tasks, so GPUI's WndProc
//! callbacks never meet a borrowed App.

use crate::config::Corner;
use crate::icon::Badge;
use crate::layout::{self, Rect};
use crate::panel::{self, Anchor};
use crate::state::{AppState, Input};
use crate::view::{self, Ui};
use crate::win;
use gpui::{
    App, AppContext, Bounds, Context, Entity, Global, Hsla, IntoElement, ParentElement, Render,
    Styled, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, div, point, prelude::*, px, rgb, size,
};
use std::time::Duration;

/// How often the badge rechecks the work area, fullscreen apps, its
/// z-order and the keepalive, while it is enabled.
const TICK: Duration = Duration::from_secs(2);

pub struct OverlayView {
    state: Entity<AppState>,
}

impl OverlayView {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

/// The disc's fill and its text for a badge state.
fn face(badge: Badge) -> (Hsla, String) {
    match badge {
        Badge::Checking => (rgb(0x6e7681).into(), "…".to_string()),
        Badge::Problem => (rgb(0xd29922).into(), "!".to_string()),
        Badge::AllSynced => (rgb(0x2ea043).into(), "✓".to_string()),
        Badge::Unsynced(n) if n > 99 => (rgb(0xd73a49).into(), "99+".to_string()),
        Badge::Unsynced(n) => (rgb(0xd73a49).into(), n.to_string()),
    }
}

/// Text height as a share of the diameter, smaller for longer counts.
fn text_share(text: &str) -> f32 {
    match text.chars().count() {
        0 | 1 => 0.45,
        2 => 0.38,
        _ => 0.30,
    }
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (fill, text) = face(self.state.read(cx).badge());
        let ring = Hsla {
            l: fill.l * 0.7,
            ..fill
        };
        let viewport = window.viewport_size();
        let diameter = viewport.width.min(viewport.height);
        let text_size = diameter * text_share(&text);
        // No background on the root: the swap chain is premultiplied and
        // cleared to transparent, so only the disc is drawn.
        div().size_full().child(
            div()
                .id("badge")
                .size_full()
                .rounded_full()
                .bg(fill)
                .border_1()
                .border_color(ring)
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui::white())
                .text_size(text_size)
                .line_height(text_size)
                .font_weight(gpui::FontWeight::BOLD)
                .cursor_pointer()
                .child(text)
                .on_click(|event, _, cx| {
                    // A double-click's second click would close the panel
                    // the first one just opened.
                    if event.click_count() > 1 {
                        return;
                    }
                    // app.rs resolves overlay_click() and anchors the panel.
                    if let Some(ui) = cx.try_global::<Ui>() {
                        ui.input.unbounded_send(Input::Overlay).ok();
                    }
                }),
        )
    }
}

#[derive(Default)]
pub struct OverlaySlot {
    handle: Option<WindowHandle<OverlayView>>,
    hwnd: Option<isize>,
    /// Where the client area was last put, in physical px.
    placed: Option<Rect>,
    visible: bool,
    /// The running timer's id; a timer whose id is no longer here stops.
    timer: Option<u64>,
    timers_started: u64,
}

impl Global for OverlaySlot {}

/// Idempotent: opens, closes, places, shows or hides the badge to match
/// the config and the state. Changing the corner, size or margin re-places
/// the window without recreating it.
pub fn sync(cx: &mut App) {
    let Some(state) = cx.try_global::<Ui>().map(|ui| ui.state.clone()) else {
        return;
    };
    let s = state.read(cx);
    let cfg = &s.config;
    if !cfg.overlay.enabled {
        close(cx);
        return;
    }
    let (corner, diameter, margin) = (cfg.overlay.corner, cfg.overlay_size(), cfg.overlay_margin());
    let hide = (cfg.overlay.hide_when_synced && s.badge() == Badge::AllSynced)
        || (cfg.overlay.hide_in_fullscreen && win::fullscreen_busy());

    ensure_open(state, diameter, cx);
    ensure_timer(cx);

    let mon = win::primary_monitor();
    let scale = view::sane_scale(mon.scale);
    let rect = layout::corner(
        mon.work,
        corner,
        (diameter * scale).round() as i32,
        (margin * scale).round() as i32,
    );
    let panel = panel::hwnd(cx);
    let slot = cx.default_global::<OverlaySlot>();
    let Some(h) = slot.hwnd else { return };
    if hide {
        if slot.visible {
            slot.visible = false;
            cx.spawn(async move |_| unsafe { win::hide(h) }).detach();
        }
    } else if !slot.visible || slot.placed != Some(rect) {
        slot.visible = true;
        slot.placed = Some(rect);
        // Also shows it, topmost and without activation. An open panel goes
        // back above it in the same task, so the badge never covers it.
        cx.spawn(async move |_| unsafe {
            win::place_overlay(h, rect);
            if let Some(p) = panel {
                win::reassert_topmost(p);
            }
        })
        .detach();
    }
}

/// Removes the badge and stops its timer.
pub fn close(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let handle = slot.handle.take();
    slot.hwnd = None;
    slot.placed = None;
    slot.visible = false;
    slot.timer = None;
    if let Some(handle) = handle {
        view::remove(handle.into(), cx);
    }
}

/// The panel's anchor for a badge click: the badge's rect and corner, or
/// `None` while it isn't showing (then anchor at the tray instead).
pub fn anchor(cx: &App) -> Option<Anchor> {
    let slot = cx.try_global::<OverlaySlot>()?;
    let rect = slot.placed.filter(|_| slot.visible)?;
    let corner: Corner = cx.try_global::<Ui>()?.state.read(cx).config.overlay.corner;
    Some(Anchor::Overlay { rect, corner })
}

fn ensure_open(state: Entity<AppState>, diameter: f32, cx: &mut App) {
    if cx
        .try_global::<OverlaySlot>()
        .and_then(|slot| slot.handle)
        .is_some_and(|h| view::is_live(h.into(), cx))
    {
        return;
    }
    if std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_some() {
        eprintln!(
            "gitstat: GPUI_DISABLE_DIRECT_COMPOSITION is set; the badge will draw on a black square"
        );
    }
    let mut hwnd = None;
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(diameter), px(diameter)),
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
                // Only read the HWND here; styling sends messages, so it
                // runs in a task once this update has returned.
                hwnd = win::hwnd(window);
                if let Some(h) = hwnd {
                    cx.spawn(async move |_| unsafe { win::style_overlay(h) })
                        .detach();
                }
                cx.new(|cx| OverlayView::new(state, cx))
            },
        )
        .ok();
    let slot = cx.default_global::<OverlaySlot>();
    *slot = OverlaySlot {
        handle,
        hwnd: handle.and(hwnd),
        timer: slot.timer,
        timers_started: slot.timers_started,
        ..Default::default()
    };
}

fn ensure_timer(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if slot.timer.is_some() {
        return;
    }
    slot.timers_started += 1;
    let id = slot.timers_started;
    slot.timer = Some(id);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(TICK).await;
            if !cx.update(|cx| tick(id, cx)).unwrap_or(false) {
                break;
            }
        }
    })
    .detach();
}

/// One timer pass: Win32 queries only, no git. Returns false once this
/// timer should stop.
fn tick(id: u64, cx: &mut App) -> bool {
    let Some(slot) = cx.try_global::<OverlaySlot>() else {
        return false;
    };
    if slot.timer != Some(id) {
        return false;
    }
    // Something other than us may have hidden or shown it; sync corrects
    // whatever it finds.
    if let Some(h) = slot.hwnd {
        let shown = win::is_visible(h);
        cx.default_global::<OverlaySlot>().visible = shown;
    }
    sync(cx);
    let slot = cx.default_global::<OverlaySlot>();
    if slot.timer != Some(id) {
        return false;
    }
    if let Some(h) = slot.hwnd.filter(|_| slot.visible) {
        let panel = panel::hwnd(cx);
        cx.spawn(async move |_| unsafe {
            win::reassert_topmost(h);
            // Topmost puts the badge above the panel; put the panel back.
            if let Some(p) = panel {
                win::reassert_topmost(p);
            }
        })
        .detach();
    }
    view::hide_keepalive_if_shown(cx);
    true
}
