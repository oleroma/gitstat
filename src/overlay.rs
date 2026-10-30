//! The always-on-top corner badge with the unsynced count.
//!
//! The window is opened hidden (`show: false`) and never activated: GPUI's
//! first `activate()` would apply the placement parked at creation. It is
//! placed, shown, hidden and moved natively from spawned tasks, so GPUI's
//! WndProc callbacks never meet a borrowed App.
//!
//! Clicks outside the disc go to the window underneath through a window
//! region. A region is the only per-pixel hit test that works across
//! processes here: `WS_EX_TRANSPARENT` passes every click, `HTTRANSPARENT`
//! from `WM_NCHITTEST` only reaches windows of the same thread, and a
//! layered window's alpha hit-testing needs `UpdateLayeredWindow`, which
//! GPUI's DirectComposition swap chain doesn't use. The region is 1-bit and
//! so jagged; it is a few px larger than the disc, which GPUI draws
//! anti-aliased with a soft shadow entirely inside it.
//!
//! Dragging with the left button moves the badge; a press that moves less
//! than 4 px is a click. GPUI captures the mouse on button-down
//! (`SetCapture` in its `WM_LBUTTONDOWN` handler, released on button-up), so
//! moves and the release arrive while the cursor is outside the small
//! window. Positions come from the screen cursor, not GPUI's window-relative
//! ones, which shift as the window moves. The window is moved with
//! `SetWindowPos` from spawned tasks, never activated. On release the
//! position is saved as the nearest corner plus offsets.

use crate::config::Corner;
use crate::icon::{self, Badge};
use crate::layout::{self, Drag, Rect};
use crate::panel::{self, Anchor};
use crate::state::{AppState, Input};
use crate::view::{self, Ui};
use crate::win;
use gpui::{
    App, AppContext, Bounds, BoxShadow, Context, Entity, Global, Hsla, IntoElement, MouseButton,
    MouseMoveEvent, MouseUpEvent, ParentElement, Render, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, canvas, div,
    point, prelude::*, px, rgb, size,
};
use std::cell::Cell;
use std::rc::Rc;
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
        Badge::Unsynced(n) => (rgb(0xd73a49).into(), icon::count_text(n)),
    }
}

/// Repos synced, with local work, and needing action.
fn tone_counts(statuses: &[crate::git::RepoStatus]) -> [usize; 3] {
    let mut counts = [0; 3];
    for s in statuses {
        counts[match icon::FolderState::of(s).tone() {
            icon::Tone::Synced => 0,
            icon::Tone::Local => 1,
            icon::Tone::Attention => 2,
        }] += 1;
    }
    counts
}

/// The icons-only face: a ring cut into the repos' tones, each arc as long
/// as its share (red, amber, then green, clockwise from the top), around a
/// dot in the most urgent one. Grey while the first check runs.
fn paint_rings(bounds: Bounds<gpui::Pixels>, badge: Badge, tones: [usize; 3], window: &mut Window) {
    use crate::timeline::ink;
    let d = f32::from(bounds.size.width.min(bounds.size.height));
    let c = bounds.center();
    let center = (f32::from(c.x), f32::from(c.y));
    let width = (d * 0.13).max(2.0);
    let r = d / 2.0 - width / 2.0 - d * 0.08;
    let colors: [Hsla; 3] = [
        rgb(0x2ea043).into(),
        rgb(0xd29922).into(),
        rgb(0xd73a49).into(),
    ];
    let grey: Hsla = rgb(0x6e7681).into();
    let tau = std::f32::consts::TAU;
    let total: usize = tones.iter().sum();
    let worst = match badge {
        Badge::Checking => None,
        Badge::Problem => Some(colors[1]),
        _ => (0..3).rev().find(|&i| tones[i] > 0).map(|i| colors[i]),
    };
    if matches!(badge, Badge::Checking) || total == 0 {
        ink::arc(window, center, r, 0.0, tau, width, grey);
    } else {
        let parts: Vec<(usize, Hsla)> = [
            (tones[2], colors[2]),
            (tones[1], colors[1]),
            (tones[0], colors[0]),
        ]
        .into_iter()
        .filter(|&(n, _)| n > 0)
        .collect();
        // A gap between arcs, as wide as the ring, unless it is one arc.
        let gap = if parts.len() > 1 {
            width * 1.2 / r
        } else {
            0.0
        };
        let mut at = 0.0;
        for (n, color) in parts {
            let span = tau * n as f32 / total as f32;
            let (from, to) = (at + gap / 2.0, at + span - gap / 2.0);
            if to > from {
                ink::arc(window, center, r, from, to, width, color);
            }
            at += span;
        }
    }
    if let Some(color) = worst {
        let dot = d * 0.14;
        window.paint_quad(
            gpui::fill(
                Bounds::new(
                    point(c.x - px(dot), c.y - px(dot)),
                    size(px(2.0 * dot), px(2.0 * dot)),
                ),
                color,
            )
            .corner_radii(px(dot)),
        );
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
        let state = self.state.read(cx);
        let badge = state.badge();
        let icons_only = state.config.icons_only;
        let tones = tone_counts(&state.statuses);
        let (fill, text) = face(badge);
        // Pictures only: a dark disc whose ring shows the repos' tones.
        let fill = if icons_only {
            rgb(0x24292f).into()
        } else {
            fill
        };
        let ring = Hsla {
            l: fill.l * 0.75,
            ..fill
        };
        // The window is the disc plus `pad` physical px of transparent room
        // on every side, for the anti-aliased edge and the shadow.
        let pad = cx.try_global::<OverlaySlot>().map_or(0, |slot| slot.pad);
        let inset = px(pad as f32 / window.scale_factor());
        let viewport = window.viewport_size();
        let diameter = (viewport.width.min(viewport.height) - inset * 2.).max(px(1.));
        // A whole number of device px: DirectWrite hints whole sizes
        // crisply, and a fractional one blurs every stem.
        let scale = window.scale_factor();
        let text_size = px((f32::from(diameter * text_share(&text)) * scale).round() / scale);
        // No background on the root: the swap chain is premultiplied and
        // cleared to transparent, so only the disc and its shadow are drawn.
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            // The whole region takes the press, so the transparent ring
            // around the disc isn't a dead spot.
            .on_mouse_down(MouseButton::Left, |_, _, cx| press(cx))
            .child(
                div()
                    .size(diameter)
                    .flex_none()
                    .rounded_full()
                    .bg(fill)
                    .border_1()
                    .border_color(ring)
                    .shadow(vec![BoxShadow {
                        color: gpui::hsla(0., 0., 0., 0.35),
                        offset: point(px(0.), px(0.5)),
                        blur_radius: px(0.75),
                        spread_radius: px(0.),
                    }])
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(gpui::white())
                    .text_size(text_size)
                    .line_height(text_size)
                    .font_weight(gpui::FontWeight::BOLD)
                    .cursor_pointer()
                    .when(!icons_only, |d| d.child(text))
                    .when(icons_only, |d| {
                        d.child(
                            canvas(
                                |_, _, _| {},
                                move |bounds, _, window, _| {
                                    paint_rings(bounds, badge, tones, window)
                                },
                            )
                            .size_full(),
                        )
                    }),
            )
            .child(
                // Window-wide listeners, hit or not: with the mouse
                // captured, moves and the release outside the badge land
                // here too.
                canvas(
                    |_, _, _| {},
                    |_, _, window, _| {
                        window.on_mouse_event(|e: &MouseMoveEvent, phase, _, cx| {
                            if phase == gpui::DispatchPhase::Bubble {
                                drag_to(e.pressed_button == Some(MouseButton::Left), cx);
                            }
                        });
                        window.on_mouse_event(|e: &MouseUpEvent, phase, _, cx| {
                            if phase == gpui::DispatchPhase::Bubble && e.button == MouseButton::Left
                            {
                                release(Some(e.click_count), cx);
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

/// Left button down on the badge: maybe a click, maybe a drag.
fn press(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    if let Some(rect) = slot.placed.filter(|_| slot.visible) {
        slot.drag = Some(Drag::new(win::cursor_pos(), rect));
    }
}

/// The cursor moved while a press is on: once it is a drag, the badge
/// follows, kept on the primary work area. A move without the button means
/// the release went elsewhere (capture lost): end the press there.
fn drag_to(pressed: bool, cx: &mut App) {
    if !pressed {
        release(None, cx);
        return;
    }
    let work = win::primary_monitor().work;
    let slot = cx.default_global::<OverlaySlot>();
    let Some(drag) = slot.drag.as_mut() else {
        return;
    };
    let Some(rect) = drag.track(win::cursor_pos(), work) else {
        return;
    };
    slot.held = Some(rect);
    slot.placed = Some(rect);
    move_window(rect, cx);
}

/// Moves the window to show the disc at `rect`. Moves are coalesced: one
/// task at a time, which takes the latest target when it runs.
fn move_window(rect: Rect, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(h) = slot.hwnd else { return };
    let r = rect.outset(slot.pad);
    if slot.move_to.replace(Some(r)).is_none() {
        let target = slot.move_to.clone();
        cx.spawn(async move |_| {
            if let Some(r) = target.take() {
                unsafe { win::move_overlay(h, r.left, r.top) };
            }
        })
        .detach();
    }
}

/// The press ended: a click opens the panel, a drag saves where the badge
/// was dropped. `click_count` is `None` when the release wasn't seen.
fn release(click_count: Option<usize>, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(mut drag) = slot.drag.take() else {
        return;
    };
    let work = win::primary_monitor().work;
    if let Some(rect) = drag.track(win::cursor_pos(), work) {
        slot.held = Some(rect);
        slot.placed = Some(rect);
        move_window(rect, cx);
    }
    let input = if drag.is_drag() {
        cx.default_global::<OverlaySlot>()
            .held
            .map(Input::OverlayMoved)
    } else {
        // A double-click's second click would close the panel the first
        // one just opened. app.rs resolves overlay_click() and anchors the
        // panel.
        click_count.filter(|&n| n <= 1).map(|_| Input::Overlay)
    };
    if let (Some(input), Some(ui)) = (input, cx.try_global::<Ui>()) {
        ui.input.unbounded_send(input).ok();
    }
}

/// Where a dropped badge is kept: its corner and offsets in logical px.
/// app.rs saves them, then calls [`drop_saved`].
pub fn drop_position(rect: Rect) -> (Corner, f32, f32) {
    let mon = win::primary_monitor();
    layout::drop_position(rect, mon.work, view::sane_scale(mon.scale))
}

/// The config now holds the dropped position, so the badge goes back to
/// being placed from it (at the same spot).
pub fn drop_saved(cx: &mut App) {
    cx.default_global::<OverlaySlot>().held = None;
    sync(cx);
}

#[derive(Default)]
pub struct OverlaySlot {
    handle: Option<WindowHandle<OverlayView>>,
    hwnd: Option<isize>,
    /// Where the disc was last put, in physical px. The window is this
    /// grown by `pad`.
    placed: Option<Rect>,
    /// Transparent physical px around the disc, inside the window region.
    pad: i32,
    visible: bool,
    /// A left-button press on the badge, until its release.
    drag: Option<Drag>,
    /// Where the user put the badge, while dragging and until the config
    /// holds it: placement uses this over the config's corner and offset.
    held: Option<Rect>,
    /// The next window position for the move task; `Some` while one is
    /// queued.
    move_to: Rc<Cell<Option<Rect>>>,
    /// The running timer's id; a timer whose id is no longer here stops.
    timer: Option<u64>,
    timers_started: u64,
    /// The badge's native tooltip, in icons-only mode: its HWND once made,
    /// and the text it was last given.
    tip: Option<isize>,
    tip_pending: bool,
    tip_text: String,
}

impl Global for OverlaySlot {}

/// Idempotent: opens, closes, places, shows or hides the badge to match
/// the config and the state. Changing the corner, offset, size or margin
/// re-places the window without recreating it.
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
    let (corner, diameter, offset) = (cfg.overlay.corner, cfg.overlay_size(), cfg.overlay_offset());
    let hide = (cfg.overlay.hide_when_synced && s.badge() == Badge::AllSynced)
        || (cfg.overlay.hide_in_fullscreen && win::fullscreen_busy());

    let tip_text = if cfg.icons_only {
        tooltip_text(s)
    } else {
        String::new()
    };
    ensure_open(state, diameter, cx);
    ensure_timer(cx);
    sync_tooltip(tip_text, cx);

    let mon = win::primary_monitor();
    let scale = view::sane_scale(mon.scale);
    let physical = |v: f32| (v * scale).round() as i32;
    let pad = layout::badge_pad(scale);
    let panel = panel::hwnd(cx);
    let slot = cx.default_global::<OverlaySlot>();
    let Some(h) = slot.hwnd else { return };
    if slot.drag.is_some_and(|d| d.is_drag()) {
        // The drag moves it.
        return;
    }
    let rect = slot.held.unwrap_or_else(|| {
        layout::corner(
            mon.work,
            corner,
            physical(diameter),
            (physical(offset.0), physical(offset.1)),
        )
    });
    if hide {
        if slot.visible {
            slot.visible = false;
            slot.drag = None;
            cx.spawn(async move |_| unsafe { win::hide(h) }).detach();
        }
    } else if !slot.visible || slot.placed != Some(rect) || slot.pad != pad {
        slot.visible = true;
        slot.placed = Some(rect);
        slot.pad = pad;
        let window = rect.outset(pad);
        // Also shows it, topmost and without activation. An open panel goes
        // back above it in the same task, so the badge never covers it.
        cx.spawn(async move |_| unsafe {
            win::place_overlay(h, window);
            if let Some(p) = panel {
                win::reassert_topmost(p);
            }
        })
        .detach();
    }
}

/// What the badge says on hover in icons-only mode: the tray's summary,
/// then a line per repo that isn't synced.
fn tooltip_text(s: &AppState) -> String {
    let problem = s.problem();
    let summary = crate::tray::summary(
        &crate::tray::TraySnapshot {
            statuses: &s.statuses,
            checking: s.sweeping,
            has_result: s.has_result,
            problem: problem.as_deref(),
            incomplete: s.capped.is_some(),
        },
        s.unsynced(),
    );
    let mut text = format!("gitstat: {summary}");
    const SHOWN: usize = 12;
    let unsynced: Vec<_> = s
        .sorted_for_panel()
        .into_iter()
        .filter(|r| !r.is_synced())
        .collect();
    for r in unsynced.iter().take(SHOWN) {
        text.push_str(&format!("\n{}: {}", r.name, r.label()));
    }
    if unsynced.len() > SHOWN {
        text.push_str(&format!("\n…and {} more", unsynced.len() - SHOWN));
    }
    text
}

/// Gives the badge its tooltip text, making the tooltip first if needed.
/// An empty text shows none.
fn sync_tooltip(text: String, cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let Some(h) = slot.hwnd else { return };
    if slot.tip_text == text && (slot.tip.is_some() || text.is_empty()) {
        return;
    }
    match slot.tip {
        Some(tip) => {
            slot.tip_text = text.clone();
            cx.spawn(async move |_| unsafe { win::set_tooltip(tip, h, &text) })
                .detach();
        }
        None if text.is_empty() || slot.tip_pending => {}
        None => {
            slot.tip_pending = true;
            cx.spawn(async move |cx| {
                let tip = unsafe { win::create_tooltip(h) };
                cx.update(|cx| {
                    let slot = cx.default_global::<OverlaySlot>();
                    slot.tip_pending = false;
                    // The badge was closed or made again meanwhile; the
                    // tooltip went with its owner.
                    if slot.hwnd != Some(h) {
                        return;
                    }
                    slot.tip = tip;
                    slot.tip_text = String::new();
                    sync(cx);
                })
                .ok();
            })
            .detach();
        }
    }
}

/// Removes the badge and stops its timer.
pub fn close(cx: &mut App) {
    let slot = cx.default_global::<OverlaySlot>();
    let handle = slot.handle.take();
    slot.hwnd = None;
    slot.placed = None;
    slot.visible = false;
    slot.drag = None;
    slot.held = None;
    slot.timer = None;
    slot.tip = None;
    slot.tip_text.clear();
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
    // A press whose release never arrived (capture taken away mid-drag)
    // ends here rather than leaving the badge stuck to a drag.
    if slot.drag.is_some() && !win::left_button_down() {
        let slot = cx.default_global::<OverlaySlot>();
        if slot.drag.as_mut().is_some_and(|d| d.stale()) {
            release(None, cx);
        }
    }
    let slot = cx.default_global::<OverlaySlot>();
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
