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
const WIDTH: f32 = 360.0;
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
    toggled_at: Option<Instant>,
}

impl Global for PanelSlot {}

/// Closes the panel if it is open, else opens it at `anchor` unless the
/// click is the tail of the one that just closed it.
pub fn toggle(anchor: Anchor, cx: &mut App) {
    if is_open(cx) {
        close(cx);
        return;
    }
    if cx
        .default_global::<PanelSlot>()
        .toggled_at
        .is_some_and(|t| t.elapsed() < DEBOUNCE)
    {
        return;
    }
    open(anchor, cx);
}

pub fn close(cx: &mut App) {
    let slot = cx.default_global::<PanelSlot>();
    if let Some(handle) = slot.handle.take() {
        slot.toggled_at = Some(Instant::now());
        view::remove(handle.into(), cx);
    }
}

pub fn is_open(cx: &App) -> bool {
    cx.try_global::<PanelSlot>()
        .and_then(|slot| slot.handle)
        .is_some_and(|h| view::is_live(h.into(), cx))
}

/// Closes the panel from inside one of its own handlers, where its window
/// is borrowed and `close` could only defer.
pub(crate) fn dismiss(window: &mut Window, cx: &mut App) {
    let slot = cx.default_global::<PanelSlot>();
    if slot
        .handle
        .is_some_and(|h| gpui::AnyWindowHandle::from(h) == window.window_handle())
    {
        slot.handle = None;
        slot.toggled_at = Some(Instant::now());
    }
    window.remove_window();
}

fn open(anchor: Anchor, cx: &mut App) {
    let Some(state) = cx.try_global::<Ui>().map(|ui| ui.state.clone()) else {
        return;
    };
    let rows = view::panel_rows(state.read(cx));
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
    let height = layout::panel_height(rows, mon.work.height() as f32 / scale);
    let (w, h, gap) = (
        (WIDTH * scale).round() as i32,
        (height * scale).round() as i32,
        (GAP * scale).round() as i32,
    );
    let r = match anchor {
        Anchor::Overlay { rect, corner } => {
            layout::flyout_from_corner(rect, corner, mon.work, w, h, gap)
        }
        Anchor::Tray(_) => {
            let edge = layout::taskbar_edge(mon.rect, mon.work, anchor_rect);
            layout::flyout(anchor_rect, mon.work, edge, w, h, gap)
        }
    };
    // GPUI converts window_bounds with the primary monitor's scale (the
    // window is created at CW_USEDEFAULT), and the tray and the badge live
    // on the primary monitor.
    let primary = view::sane_scale(win::primary_monitor().scale);
    let logical = |v: i32| px(v as f32 / primary);
    let bounds = Bounds::new(
        point(logical(r.left), logical(r.top)),
        size(logical(r.width()), logical(r.height())),
    );
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                ..Default::default()
            },
            |window, cx| {
                // Safe: opened with show: true. The tray click gave us the
                // foreground right this needs.
                window.activate_window();
                if let Some(h) = win::hwnd(window) {
                    // Native z-order calls must run outside this update.
                    cx.spawn(async move |_| unsafe { win::panel_topmost(h) })
                        .detach();
                }
                cx.new(|cx| StatusView::new(state, Mode::Panel, window, cx))
            },
        )
        .ok();
    let slot = cx.default_global::<PanelSlot>();
    slot.handle = handle;
    slot.toggled_at = Some(Instant::now());
}
