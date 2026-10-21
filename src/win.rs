//! Native Win32 helpers for the panel, the overlay and the keepalive window.
//!
//! HWNDs cross async boundaries as `isize`. Every helper checks `IsWindow`
//! first, ignores Win32 failures and never panics. The `unsafe fn`s send
//! messages to a GPUI window: call them only from a `cx.spawn` task outside
//! `update`, or GPUI's WndProc callbacks (which `try_borrow_mut` the App)
//! lose the resulting `WM_SIZE`/`WM_MOVE`.

use crate::layout::Rect;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, CreateEllipticRgn, DeleteObject, GetMonitorInfoW, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    SetWindowRgn,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows_sys::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetClientRect, GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    HWND_TOPMOST, IDYES, IsWindow, IsWindowVisible, MB_ICONQUESTION, MB_SETFOREGROUND, MB_YESNO,
    MessageBoxW, SM_CXSCREEN, SM_CXSMICON, SM_CYSCREEN, SM_SWAPBUTTON, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
};

/// The HWND of a GPUI window.
pub fn hwnd(window: &gpui::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // Spelled out: Window::window_handle() is an inherent method returning
    // AnyWindowHandle.
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// `h` as an HWND, if it still names a window.
fn live(h: isize) -> Option<HWND> {
    let hwnd = h as HWND;
    (h != 0 && unsafe { IsWindow(hwnd) } != 0).then_some(hwnd)
}

fn rect(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorInfo {
    pub rect: Rect,
    /// `rcWork`: the monitor without the taskbar.
    pub work: Rect,
    /// Effective DPI / 96.
    pub scale: f32,
}

/// The monitor holding (0, 0), which is where the taskbar and tray live.
pub fn primary_monitor() -> MonitorInfo {
    monitor_info(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) })
}

/// The monitor under or nearest to a physical point.
pub fn monitor_at(x: i32, y: i32) -> MonitorInfo {
    monitor_info(unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) })
}

fn monitor_info(mon: HMONITOR) -> MonitorInfo {
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        rcMonitor: RECT::default(),
        rcWork: RECT::default(),
        dwFlags: 0,
    };
    if mon.is_null() || unsafe { GetMonitorInfoW(mon, &mut mi) } == 0 {
        // Practically unreachable; the primary screen size beats a 0×0 rect.
        let screen = unsafe {
            Rect::new(
                0,
                0,
                GetSystemMetrics(SM_CXSCREEN),
                GetSystemMetrics(SM_CYSCREEN),
            )
        };
        return MonitorInfo {
            rect: screen,
            work: screen,
            scale: 1.0,
        };
    }
    let (mut dx, mut dy) = (0u32, 0u32);
    let hr = unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
    let scale = if hr >= 0 && dx > 0 {
        dx as f32 / 96.0
    } else {
        1.0
    };
    MonitorInfo {
        rect: rect(mi.rcMonitor),
        work: rect(mi.rcWork),
        scale,
    }
}

/// The notification area's icon size in px: the small-icon size at the
/// primary monitor's DPI (16 at 100%, 20 at 125%, 24 at 150%, 32 at 200%).
pub fn tray_icon_size() -> u32 {
    let dpi = (primary_monitor().scale * 96.0).round() as u32;
    match unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi.max(96)) } {
        px @ 1..=256 => px as u32,
        _ => crate::icon::TRAY_SIZE,
    }
}

/// The cursor in physical px, or (0, 0) if it can't be read (secure desktop).
pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } == 0 {
        return (0, 0);
    }
    (p.x, p.y)
}

/// ORs `extra` into the window's extended style.
unsafe fn add_ex_style(hwnd: HWND, extra: WINDOW_EX_STYLE) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | extra as isize);
    }
}

/// Sets a 4-byte DWM attribute. Failure is harmless (older Windows) and ignored.
unsafe fn dwm_set(hwnd: HWND, attribute: i32, value: u32) {
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute as u32,
            (&raw const value).cast(),
            size_of::<u32>() as u32,
        );
    }
}

/// Styles the overlay once: no focus on click, and no DWM border, rounded
/// corners or shadow around the circle.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn style_overlay(h: isize) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        add_ex_style(hwnd, WS_EX_NOACTIVATE);
        dwm_set(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_DONOTROUND as u32,
        );
        dwm_set(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
        dwm_set(hwnd, DWMWA_NCRENDERING_POLICY, DWMNCRP_DISABLED as u32);
    }
}

/// GPUI's PopUp keeps a frame offset between the window and its client
/// area: (client left, client top) relative to the window, and how much
/// wider and taller the window is than its client area.
unsafe fn frame(hwnd: HWND) -> (i32, i32, i32, i32) {
    let (mut wr, mut cr) = (RECT::default(), RECT::default());
    let mut origin = POINT { x: 0, y: 0 };
    unsafe {
        if GetWindowRect(hwnd, &mut wr) != 0
            && GetClientRect(hwnd, &mut cr) != 0
            && ClientToScreen(hwnd, &mut origin) != 0
        {
            (
                origin.x - wr.left,
                origin.y - wr.top,
                (wr.right - wr.left) - cr.right,
                (wr.bottom - wr.top) - cr.bottom,
            )
        } else {
            (0, 0, 0, 0)
        }
    }
}

/// Puts the overlay's client area at `r`, topmost and shown without
/// activation, and clips it (drawing and clicks) to the ellipse inscribed
/// in `r`. The region is 1-bit, so its edge is jagged: `r` includes
/// transparent padding around the drawn disc, which keeps the disc's own
/// anti-aliased edge away from it.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn place_overlay(h: isize, r: Rect) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        // Size the window so the client area is exactly `r`.
        let (dx, dy, bw, bh) = frame(hwnd);
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            r.left - dx,
            r.top - dy,
            r.width() + bw,
            r.height() + bh,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        // Region coordinates are relative to the window, not the client area.
        let rgn = CreateEllipticRgn(dx, dy, dx + r.width() + 1, dy + r.height() + 1);
        // On success the system owns the region; on failure it is ours to free.
        if !rgn.is_null() && SetWindowRgn(hwnd, rgn, 1) == 0 {
            DeleteObject(rgn);
        }
    }
}

/// Moves the overlay's client area to (left, top) without resizing it,
/// activating it or changing its z-order. Its region is relative to the
/// window, so it moves along. For dragging.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn move_overlay(h: isize, left: i32, top: i32) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        let (dx, dy, _, _) = frame(hwnd);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            left - dx,
            top - dy,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Whether the primary (left, unless swapped) mouse button is down right now.
pub fn left_button_down() -> bool {
    let vk = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 {
        VK_RBUTTON
    } else {
        VK_LBUTTON
    };
    (unsafe { GetAsyncKeyState(vk as i32) } as u16) & 0x8000 != 0
}

/// Shows without taking focus, or hides.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn set_visible_noactivate(h: isize, visible: bool) {
    let Some(hwnd) = live(h) else { return };
    let cmd = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
    unsafe { ShowWindow(hwnd, cmd) };
}

/// Moves the window back to the top of the topmost band, where another
/// topmost window may have pushed past it. Leaves visibility alone.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn reassert_topmost(h: isize) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };
}

/// Makes the panel topmost so the badge never covers it, with the Win11
/// rounded corners of a system flyout (ignored on Win10).
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn panel_topmost(h: isize) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        dwm_set(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32);
        reassert_topmost(h);
    }
}

/// Off-screen, where a forced show of the keepalive draws nothing visible.
const PARKED: i32 = -32000;

/// Makes the never-shown keepalive harmless if GPUI ever shows it (its
/// `WM_DISPLAYCHANGE` handling does when a monitor disconnects): 1×1,
/// off-screen, click-through and never activated. `SW_SHOWNORMAL` restores
/// the normal position, which is this one: it is never minimised or maximised.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn harden_keepalive(h: isize) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        add_ex_style(hwnd, WS_EX_NOACTIVATE | WS_EX_TRANSPARENT);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            PARKED,
            PARKED,
            1,
            1,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub fn is_visible(h: isize) -> bool {
    live(h).is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd) } != 0)
}

/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn hide(h: isize) {
    unsafe { set_visible_noactivate(h, false) };
}

/// Whether a fullscreen app, a presentation or "busy" (also fullscreen) mode
/// is on, so the overlay should keep out of the way.
pub fn fullscreen_busy() -> bool {
    let mut state = 0;
    if unsafe { SHQueryUserNotificationState(&mut state) } < 0 {
        return false;
    }
    matches!(
        state,
        QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
    )
}

/// A Yes/No question on its own thread, like `error_box`: a modal loop on
/// the UI thread would pump GPUI tasks while App is borrowed. True for Yes.
pub fn confirm_box(text: String) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "gitstat".encode_utf16().chain(Some(0)).collect();
        let answer = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND,
            )
        };
        answer == IDYES
    })
}
