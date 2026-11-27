//! Native Win32 helpers for the panel, the overlay and the keepalive window.
//!
//! HWNDs cross async boundaries as `isize`. Every helper checks `IsWindow`
//! first, ignores Win32 failures and never panics. The `unsafe fn`s send
//! messages to a GPUI window: call them only from a `cx.spawn` task outside
//! `update`, or GPUI's WndProc callbacks (which `try_borrow_mut` the App)
//! lose the resulting `WM_SIZE`/`WM_MOVE`.

use crate::layout::Rect;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_COLOR_NONE,
    DWMWA_NCRENDERING_POLICY, DWMWA_VISIBLE_FRAME_BORDER_THICKNESS, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DwmFlush, DwmGetWindowAttribute, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, CreateEllipticRgn, DeleteObject, GetMonitorInfoW, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    ScreenToClient, SetWindowRgn,
};
use windows_sys::Win32::UI::Controls::{
    ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TOOLTIPS_CLASSW, TTDT_AUTOPOP,
    TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETDELAYTIME, TTM_SETMAXTIPWIDTH,
    TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForMonitor, GetDpiForWindow, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    SHQueryUserNotificationState, SetWindowSubclass,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, FindWindowExW, FindWindowW, HTMINBUTTON, IsIconic, IsZoomed,
    PostMessageW, SC_MINIMIZE, SIZE_MINIMIZED, SendMessageW, WM_CLOSE, WM_MOUSEMOVE, WM_NCHITTEST,
    WM_NCLBUTTONUP, WM_SIZE, WM_SYSCOMMAND, WS_EX_TOPMOST, WS_POPUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetClientRect, GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    HWND_TOPMOST, IDYES, IsWindow, IsWindowVisible, LWA_ALPHA, MB_ICONQUESTION, MB_SETFOREGROUND,
    MB_YESNO, MessageBoxW, SM_CXSCREEN, SM_CXSMICON, SM_CYSCREEN, SM_SWAPBUTTON, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow, WINDOW_EX_STYLE,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT, WindowFromPoint,
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

/// Moves and sizes a normal window so its client area is `r`, without
/// activating it or changing its z-order.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn place_window(h: isize, r: Rect) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        let (dx, dy, bw, bh) = frame(hwnd);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            r.left - dx,
            r.top - dy,
            r.width() + bw,
            r.height() + bh,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// A window's client area on screen, in physical px.
pub fn client_rect(h: isize) -> Option<Rect> {
    let hwnd = live(h)?;
    if unsafe { IsIconic(hwnd) } != 0 {
        return None;
    }
    let mut cr = RECT::default();
    let mut origin = POINT { x: 0, y: 0 };
    unsafe {
        (GetClientRect(hwnd, &mut cr) != 0 && ClientToScreen(hwnd, &mut origin) != 0).then(|| {
            Rect::new(
                origin.x,
                origin.y,
                origin.x + cr.right,
                origin.y + cr.bottom,
            )
        })
    }
}

/// Styles the flip's window once: never activated, click-through, and no
/// DWM border, rounded corners or shadow.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn style_morph(h: isize) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        add_ex_style(hwnd, WS_EX_NOACTIVATE | WS_EX_TRANSPARENT);
        dwm_set(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_DONOTROUND as u32,
        );
        dwm_set(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
        dwm_set(hwnd, DWMWA_NCRENDERING_POLICY, DWMNCRP_DISABLED as u32);
    }
}

/// Hides `h` from the screen, shadow and all, at the next composition,
/// without hiding it from Windows: it stays the active window, so the
/// window that follows it can still come to the front.
pub fn cloak(h: isize) {
    let Some(hwnd) = live(h) else { return };
    // DWMWA_CLOAK
    unsafe { dwm_set(hwnd, 13, 1) };
}

/// Lets clicks through the flip's window to what is beneath it, or
/// takes them: while it draws in place of the windows, a click lands on
/// it rather than on whatever lies behind them.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn click_through(h: isize, through: bool) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let ex = if through {
            ex | WS_EX_TRANSPARENT as isize
        } else {
            ex & !(WS_EX_TRANSPARENT as isize)
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
    }
}

/// The scale `h` draws at, from its monitor's DPI.
pub fn window_scale(h: isize) -> Option<f32> {
    let hwnd = live(h)?;
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    (dpi > 0).then(|| dpi as f32 / 96.0)
}

/// Makes `h` the foreground window unless it is already. During a flip a
/// click lands on the flip's window (gitstat's own), so Windows lets
/// gitstat bring the window it ends on in front of the app that was.
pub fn to_front(h: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    let Some(hwnd) = live(h) else { return };
    unsafe {
        if GetForegroundWindow() != hwnd {
            SetForegroundWindow(hwnd);
        }
    }
}

/// Waits for Windows to put the next frame on the screen. Changes made to
/// windows right after it returns have a whole refresh to land together.
pub fn dwm_flush() {
    unsafe { DwmFlush() };
}

/// Shows a window [`cloak`] hid, at the next composition.
pub fn uncloak(h: isize) {
    let Some(hwnd) = live(h) else { return };
    unsafe { dwm_set(hwnd, 13, 0) };
}

/// Puts the flip's client area at `r`, topmost and shown without activation.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn place_morph(h: isize, r: Rect) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
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
    }
}

/// Whether a click at (`x`, `y`) on screen, inside `h`, would pass through
/// `h` to what is under it once [`click_through`] is on: tried and put
/// back at once, for the self-test.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn passes_clicks(h: isize, x: i32, y: i32) -> Option<bool> {
    let hwnd = live(h)?;
    unsafe {
        let was = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let at = POINT { x, y };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, was & !(WS_EX_TRANSPARENT as isize));
        let solid = WindowFromPoint(at) == hwnd;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, was | WS_EX_TRANSPARENT as isize);
        let through = WindowFromPoint(at) != hwnd;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, was);
        // Only if it takes the click when it should, too.
        solid.then_some(through)
    }
}

/// `h`'s extended window styles, for the self-test's report.
pub fn ex_style(h: isize) -> u32 {
    live(h).map_or(0, |hwnd| unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }
        as u32)
}

/// Tells `h` the mouse is at (`x`, `y`) on screen, as a mouse move would:
/// for a window clicks pass through, which hears of no move itself.
pub fn tell_mouse(h: isize, x: i32, y: i32) {
    let Some(hwnd) = live(h) else { return };
    let mut at = POINT { x, y };
    unsafe {
        if ScreenToClient(hwnd, &mut at) != 0 {
            let at = ((at.y as u16 as isize) << 16) | (at.x as u16 as isize);
            PostMessageW(hwnd, WM_MOUSEMOVE, 0, at);
        }
    }
}

/// `h`'s whole window, its frame too, screen px.
pub fn window_rect(h: isize) -> Option<Rect> {
    let hwnd = live(h)?;
    let mut r = RECT::default();
    (unsafe { GetWindowRect(hwnd, &mut r) } != 0).then(|| rect(r))
}

/// What `h` says is at (`x`, `y`) on screen, Windows' hit-test code (2 its
/// title bar, 8, 9 and 20 its three buttons, 12 its top edge), with the
/// mouse told to be there first: for the self-test.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn hit_test(h: isize, x: i32, y: i32) -> Option<isize> {
    let hwnd = live(h)?;
    let mut at = POINT { x, y };
    unsafe {
        if ScreenToClient(hwnd, &mut at) == 0 {
            return None;
        }
        let client = ((at.y as u16 as isize) << 16) | (at.x as u16 as isize);
        SendMessageW(hwnd, WM_MOUSEMOVE, 0, client);
        let screen = ((y as u16 as isize) << 16) | (x as u16 as isize);
        Some(SendMessageW(hwnd, WM_NCHITTEST, 0, screen))
    }
}

/// Takes Windows' own frame off the panel's window for good: no border,
/// rounded corners or shadow. The panel draws its own, inside a window
/// far bigger than itself.
pub fn bare(h: isize) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        dwm_set(hwnd, DWMWA_NCRENDERING_POLICY, DWMNCRP_DISABLED as u32);
        dwm_set(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_DONOTROUND as u32,
        );
        dwm_set(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
    }
}

/// Puts the panel's window, its client area, at `area` for as long as it
/// is open, on top, or just under `under` (a flip drawing over it). Made
/// a layered window, which is what lets [`click_through`] pass clicks on
/// to other programs' windows.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn place_panel(h: isize, area: Rect, under: Option<isize>) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        add_ex_style(hwnd, WS_EX_LAYERED);
        SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
        let (dx, dy, bw, bh) = frame(hwnd);
        SetWindowPos(
            hwnd,
            // Placed after a topmost window, it becomes topmost too.
            under.and_then(live).unwrap_or(HWND_TOPMOST),
            area.left - dx,
            area.top - dy,
            area.width() + bw,
            area.height() + bh,
            SWP_NOACTIVATE,
        );
    }
}

/// The panel's rounded corners, logical px (Windows 11's own).
pub const PANEL_RADIUS: f32 = 8.0;

/// How far past a window its shadow reaches, physical px.
const SHADOW_REACH: i32 = 24;

/// Puts the panel back on top of the topmost band, or, beside a taskbar,
/// just below that taskbar, where it slid in from.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn panel_restack(h: isize) {
    let Some(hwnd) = live(h) else { return };
    let after = client_rect(h)
        .and_then(|r| taskbar_over(r.outset(SHADOW_REACH)))
        .unwrap_or(HWND_TOPMOST);
    unsafe {
        SetWindowPos(
            hwnd,
            after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };
}

/// The thickness Windows draws a window's rim at, physical px, as it
/// draws it on `h` (Windows 11; it scales with the display).
pub fn rim_thickness(h: isize) -> Option<u32> {
    let hwnd = live(h)?;
    let mut v: u32 = 0;
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_VISIBLE_FRAME_BORDER_THICKNESS as u32,
            (&raw mut v).cast(),
            size_of::<u32>() as u32,
        )
    } == 0;
    (ok && (1..=8).contains(&v)).then_some(v)
}

/// `s` as a NUL-ended UTF-16 string.
fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The notification area's rect on the main taskbar, screen px, where the
/// tray icon sits: or, without it, the main taskbar's.
pub fn tray_area() -> Option<Rect> {
    let main = w("Shell_TrayWnd");
    let notify = w("TrayNotifyWnd");
    unsafe {
        let tray = FindWindowW(main.as_ptr(), std::ptr::null());
        if tray.is_null() {
            return None;
        }
        let area = FindWindowExW(
            tray,
            std::ptr::null_mut(),
            notify.as_ptr(),
            std::ptr::null(),
        );
        let mut r = RECT::default();
        let got = (!area.is_null() && GetWindowRect(area, &mut r) != 0)
            || GetWindowRect(tray, &mut r) != 0;
        got.then(|| rect(r))
    }
}

/// The taskbar (any monitor's) overlapping `r`, screen px.
fn taskbar_over(r: Rect) -> Option<HWND> {
    let main = w("Shell_TrayWnd");
    let other = w("Shell_SecondaryTrayWnd");
    let overlaps = |hwnd: HWND| {
        let mut wr = RECT::default();
        !hwnd.is_null()
            && unsafe { IsWindowVisible(hwnd) } != 0
            && unsafe { GetWindowRect(hwnd, &mut wr) } != 0
            && wr.left < r.right
            && r.left < wr.right
            && wr.top < r.bottom
            && r.top < wr.bottom
    };
    let tray = unsafe { FindWindowW(main.as_ptr(), std::ptr::null()) };
    if overlaps(tray) {
        return Some(tray);
    }
    let mut after: HWND = std::ptr::null_mut();
    loop {
        after = unsafe {
            FindWindowExW(
                std::ptr::null_mut(),
                after,
                other.as_ptr(),
                std::ptr::null(),
            )
        };
        if after.is_null() {
            return None;
        }
        if overlaps(after) {
            return Some(after);
        }
    }
}

/// 0xRRGGBB as a COLORREF (0x00BBGGRR).
fn colorref(rgb: u32) -> u32 {
    ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff)
}

/// Sets the big window's rim and title bar to exact colours (0xRRGGBB),
/// which gitstat then draws itself in the flip, so the drawn and the real
/// ones hand over unseen.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn set_frame_colors(h: isize, rim: u32, caption: u32) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        dwm_set(hwnd, DWMWA_BORDER_COLOR, colorref(rim));
        dwm_set(hwnd, DWMWA_CAPTION_COLOR, colorref(caption));
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

/// Off-screen, where a forced show of the keepalive draws nothing visible.
const PARKED: i32 = -32000;

/// Makes minimising the window close it instead (the minimise button, the
/// taskbar button, Win+Down), so it goes the same way as closing: through
/// GPUI's should-close callback.
///
/// # Safety
/// Call only from a spawned task outside `update`, on the window's thread.
pub unsafe fn minimize_closes(h: isize) {
    let Some(hwnd) = live(h) else { return };
    unsafe {
        SetWindowSubclass(hwnd, Some(minimize_closes_proc), 1, 0);
    }
}

/// The close under way came from minimising.
static MINIMIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The next close counts as a minimise: the self-test's, which can't press
/// the button.
pub fn mark_minimized() {
    MINIMIZED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the close under way came from minimising; clears it.
pub fn take_minimized() -> bool {
    MINIMIZED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

unsafe extern "system" fn minimize_closes_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    // The taskbar button and Win+Down ask with SC_MINIMIZE; GPUI's own
    // minimise button minimises on the button's release, never asking.
    let asked = msg == WM_SYSCOMMAND && (wparam & 0xFFF0) as u32 == SC_MINIMIZE;
    let button = msg == WM_NCLBUTTONUP && wparam as u32 == HTMINBUTTON;
    if asked || button {
        MINIMIZED.store(true, std::sync::atomic::Ordering::Relaxed);
        unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
        return 0;
    }
    // Minimised some other way: close all the same (without the flip, as
    // there's nothing on screen to flip from).
    if msg == WM_SIZE && wparam as u32 == SIZE_MINIMIZED {
        unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

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

/// Whether the window is maximised.
pub fn is_zoomed(h: isize) -> bool {
    live(h).is_some_and(|hwnd| unsafe { IsZoomed(hwnd) } != 0)
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

/// Creates a native tooltip over the whole of `owner`, empty until
/// [`set_tooltip`]; returns the tooltip's HWND. It subclasses `owner` to
/// see the mouse, so it works for a window that never takes focus.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn create_tooltip(owner: isize) -> Option<isize> {
    let hwnd = live(owner)?;
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        };
        InitCommonControlsEx(&icc);
        let tip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            std::ptr::null(),
            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            hwnd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if tip.is_null() {
            return None;
        }
        let mut empty = [0u16];
        let info = tool_info(hwnd, empty.as_mut_ptr());
        SendMessageW(tip, TTM_ADDTOOLW, 0, (&raw const info) as isize);
        // Lines break at `\n` rather than running off the screen.
        SendMessageW(tip, TTM_SETMAXTIPWIDTH, 0, 480);
        // Stays up while the mouse rests on the badge.
        SendMessageW(tip, TTM_SETDELAYTIME, TTDT_AUTOPOP as usize, 30_000);
        Some(tip as isize)
    }
}

/// Sets the text of `owner`'s tooltip; an empty text shows none.
///
/// # Safety
/// Call only from a spawned task outside `update`.
pub unsafe fn set_tooltip(tip: isize, owner: isize, text: &str) {
    let (Some(tip), Some(owner)) = (live(tip), live(owner)) else {
        return;
    };
    let mut text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let info = tool_info(owner, text.as_mut_ptr());
    unsafe {
        SendMessageW(tip, TTM_UPDATETIPTEXTW, 0, (&raw const info) as isize);
    }
}

/// The tool that is all of `owner`. Sized without `lpReserved`, so it is
/// accepted by comctl32 5 as well as 6: the app has no manifest asking for 6.
fn tool_info(owner: HWND, text: *mut u16) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32,
        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd: owner,
        uId: owner as usize,
        lpszText: text,
        ..Default::default()
    }
}

/// The screen's pixels in `r` (physical px) as they are shown now, layered
/// windows included: straight RGBA rows, top down. For the flip's frame
/// recorder.
pub fn capture(r: Rect) -> Option<(u32, u32, Vec<u8>)> {
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
        CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, GetDC, GetDIBits, ReleaseDC, SRCCOPY,
        SelectObject,
    };
    let (w, h) = (r.width(), r.height());
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let dc = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(dc, bitmap);
        let copied = BitBlt(dc, 0, 0, w, h, screen, r.left, r.top, SRCCOPY | CAPTUREBLT) != 0;
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        // Negative: rows top down.
        info.bmiHeader.biHeight = -h;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut pixels = vec![0u8; w as usize * h as usize * 4];
        let rows = if copied {
            GetDIBits(
                dc,
                bitmap,
                0,
                h as u32,
                pixels.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
        ReleaseDC(std::ptr::null_mut(), screen);
        if rows == 0 {
            return None;
        }
        for px in pixels.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
            px[3] = 255;
        }
        Some((w as u32, h as u32, pixels))
    }
}
