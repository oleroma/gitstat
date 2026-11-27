//! The status list, shared by the flyout panel and the normal window, plus
//! the normal window itself (`show_window`, `close_window`).

use crate::config::Section;
use crate::git::RepoStatus;
use crate::github::{Checks, Mergeable, Pr, PrState, Review};
use crate::monitor::WatchMode;
use crate::panel;
use crate::scene::text::text;
use crate::state::{AppState, Input};
use crate::timeline::Timelines;
use crate::tray::TraySnapshot;
use crate::win;
use futures::channel::mpsc::UnboundedSender;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle, Global, Hsla, IntoElement,
    ParentElement, Render, ScrollHandle, SharedString, Styled, Window, WindowAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, canvas, div, point, prelude::*, px, rgb,
    size,
};
use std::path::PathBuf;
use std::rc::Rc;

mod panel_scene;
mod stage;
mod window_scene;

/// The window's smallest opening size; big mode grows it to fit columns.
const WINDOW_SIZE: (f32, f32) = (420.0, 520.0);
/// Logical px between the window and the work area's bottom-right corner.
const WINDOW_MARGIN: f32 = 16.0;

/// What the panel, the window and the overlay need from app.rs, which sets
/// it once with `cx.set_global` before opening any of them.
pub struct Ui {
    pub state: Entity<AppState>,
    /// Every repo's loaded history, for the timeline strips.
    pub timelines: Entity<Timelines>,
    /// Every repo's own icon, drawn for the windows.
    pub marks: Entity<crate::repo_marks::RepoMarks>,
    /// The app's one input channel; the overlay posts its clicks here.
    pub input: UnboundedSender<Input>,
    /// Starts a fetch sweep now (the refresh buttons).
    pub refresh: Rc<dyn Fn(&mut App)>,
    /// The never-shown keepalive window, hidden again by
    /// `hide_keepalive_if_shown` if GPUI's display-change handling shows it.
    pub keepalive: Option<isize>,
}

impl Global for Ui {}

/// GPUI shows every window whose display went away, the never-shown
/// keepalive included. It is parked off-screen, so this only tidies up.
/// Win32 queries only; cheap enough for every input batch.
pub fn hide_keepalive_if_shown(cx: &mut App) {
    if let Some(k) = cx.try_global::<Ui>().and_then(|ui| ui.keepalive)
        && win::is_visible(k)
    {
        cx.spawn(async move |_| unsafe { win::hide(k) }).detach();
    }
}

#[derive(Default)]
struct WindowSlot {
    handle: Option<WindowHandle<StatusView>>,
}

impl Global for WindowSlot {}

/// The big window's HWND while it is open.
pub fn window_hwnd(cx: &mut App) -> Option<isize> {
    let handle = cx.try_global::<WindowSlot>().and_then(|s| s.handle)?;
    handle
        .update(cx, |_, window, _| crate::win::hwnd(window))
        .ok()
        .flatten()
}

/// The big window, while it is open.
pub fn window_handle(cx: &App) -> Option<WindowHandle<StatusView>> {
    let handle = cx.try_global::<WindowSlot>().and_then(|s| s.handle)?;
    is_live(handle.into(), cx).then_some(handle)
}

/// Brings the normal window to the front, opening it at the bottom-right of
/// the primary work area if it isn't open.
pub fn show_window(cx: &mut App) {
    show_window_as(false, cx);
}

/// Opens the big window cloaked, for the flip to reveal: cloaked as it is
/// made, before Windows first shows it, so neither it nor its shadow ever
/// shows under the flip.
pub fn show_window_cloaked(cx: &mut App) {
    show_window_as(true, cx);
}

fn show_window_as(cloaked: bool, cx: &mut App) {
    if let Some(handle) = cx.try_global::<WindowSlot>().and_then(|s| s.handle)
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let Some(state) = cx.try_global::<Ui>().map(|ui| ui.state.clone()) else {
        return;
    };
    if let Some(t) = cx.try_global::<Ui>().map(|ui| ui.timelines.clone()) {
        t.update(cx, |t, cx| t.show_newest(cx));
    }
    let remembered = last_place(cx).is_some();
    let (table, known) = {
        let s = state.read(cx);
        (table_width(s), s.has_result)
    };
    let (place, _) = window_place(table, cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(place),
                kind: WindowKind::Normal,
                // gitstat draws the title bar itself (`window_scene`).
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("gitstat".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_min_size: Some(size(px(300.), px(200.))),
                // Cloaked, it is placed and shown as it activates, after
                // the cloak below.
                show: !cloaked,
                ..Default::default()
            },
            |window, cx| {
                if cloaked && let Some(h) = win::hwnd(window) {
                    win::cloak(h);
                }
                // Shown, there is no parked placement; cloaked, activating
                // places and shows it, cloaked.
                window.activate_window();
                if let Some(h) = win::hwnd(window) {
                    let dark = matches!(
                        window.appearance(),
                        WindowAppearance::Dark | WindowAppearance::VibrantDark
                    );
                    cx.spawn(async move |_| {
                        // Minimising goes the way closing does.
                        unsafe { win::minimize_closes(h) };
                        // Its rim and title bar in colours the flip draws,
                        // as thick as Windows draws it.
                        seen_rim(h);
                        unsafe { win::set_frame_colors(h, rim_rgb(dark), title_bar_rgb(dark)) };
                    })
                    .detach();
                }
                cx.new(|cx| {
                    let mut view = StatusView::new(state, Mode::Window, window, cx);
                    // Opened before the first check found the repos: fit
                    // the columns once it has, unless it opens where it
                    // was left.
                    if !known && !remembered {
                        view.fit = Some(window.bounds());
                    }
                    view
                })
            },
        )
        .ok();
    cx.default_global::<WindowSlot>().handle = handle;
}

/// Scrolls the panel's list to its top, or the big window's table to its
/// left edge (and its pull requests to their top), over `over`, eased:
/// true when it wasn't there already.
pub fn glide_to_start(panel: bool, over: std::time::Duration, cx: &mut App) -> bool {
    let also = (!panel).then(|| PRS_SCROLL.with(Clone::clone));
    glide_scroll(panel, point(px(0.), px(0.)), also, over, cx)
}

thread_local! {
    /// The big window's pull requests' own scroll, up and down.
    static PRS_SCROLL: ScrollHandle = ScrollHandle::new();
    /// Each view's latest glide (the list's, the table's), by number: a
    /// new one takes over from one under way.
    static GLIDES: std::cell::Cell<[u64; 2]> = const { std::cell::Cell::new([0; 2]) };
    /// Until when each view's glide runs, while it does.
    static GLIDING: std::cell::Cell<[Option<std::time::Instant>; 2]> =
        const { std::cell::Cell::new([None; 2]) };
    /// When a glide last took its last step.
    static GLIDE_END: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Whether a list or table glide is under way.
pub fn gliding() -> bool {
    let now = std::time::Instant::now();
    GLIDING.get().iter().flatten().any(|&until| now < until)
}

/// When a glide last took its last step.
pub fn last_glide_end() -> Option<std::time::Instant> {
    GLIDE_END.get()
}

/// One glide: the handle it moves, from where to where, and over how long.
#[derive(Clone)]
struct Glide {
    slot: usize,
    id: u64,
    scroll: ScrollHandle,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
    /// Another handle glided with it to the top: the pull requests'.
    also: Option<(ScrollHandle, gpui::Point<gpui::Pixels>)>,
    start: std::time::Instant,
    over: std::time::Duration,
}

/// Takes the glide's next step as each frame starts, until it ends or a
/// newer one takes over.
fn glide_step(window: &mut Window, g: Glide) {
    window.on_next_frame(move |window, _| {
        if GLIDES.get()[g.slot] != g.id {
            return;
        }
        let t = (g.start.elapsed().as_secs_f32() / g.over.as_secs_f32().max(0.001)).min(1.0);
        let k = crate::flip::ease(t);
        let at = |from: gpui::Point<gpui::Pixels>, to: gpui::Point<gpui::Pixels>| {
            point(from.x + (to.x - from.x) * k, from.y + (to.y - from.y) * k)
        };
        g.scroll.set_offset(at(g.from, g.to));
        if let Some((also, from)) = &g.also {
            also.set_offset(at(*from, point(px(0.), px(0.))));
        }
        window.refresh();
        if t >= 1.0 {
            let mut gliding = GLIDING.get();
            gliding[g.slot] = None;
            GLIDING.set(gliding);
            GLIDE_END.set(Some(std::time::Instant::now()));
        } else {
            glide_step(window, g);
        }
    });
}

/// Moves the panel's list or the window's table to offset `to`, eased,
/// a step each frame, taking over from a glide of it under way: true
/// when it wasn't there already.
fn glide_scroll(
    panel: bool,
    to: gpui::Point<gpui::Pixels>,
    also: Option<ScrollHandle>,
    over: std::time::Duration,
    cx: &mut App,
) -> bool {
    // Through the scroll handles themselves: this runs from inside the
    // view's own window handlers, where that window can't be read.
    let handle = if panel {
        cx.try_global::<panel::PanelSlot>().and_then(|s| s.handle())
    } else {
        cx.try_global::<WindowSlot>().and_then(|s| s.handle)
    };
    let (Some(handle), Some(scroll)) = (handle, SCROLLS.with(|s| s.borrow().get(panel))) else {
        return false;
    };
    let from = scroll.offset();
    let also = also
        .map(|h| (h.clone(), h.offset()))
        .filter(|(_, o)| f32::from(o.y).abs() >= 0.5);
    let near = |a: gpui::Pixels, b: gpui::Pixels| f32::from(a - b).abs() < 0.5;
    if near(from.x, to.x) && near(from.y, to.y) && also.is_none() {
        return false;
    }
    let slot = usize::from(!panel);
    let mut ids = GLIDES.get();
    ids[slot] += 1;
    GLIDES.set(ids);
    let start = std::time::Instant::now();
    let mut gliding = GLIDING.get();
    // Never longer than this, should its window close under it.
    gliding[slot] = Some(start + over + std::time::Duration::from_millis(100));
    GLIDING.set(gliding);
    let g = Glide {
        slot,
        id: ids[slot],
        scroll,
        from,
        to,
        also,
        start,
        over,
    };
    // Outside the update this may be called from.
    cx.spawn(async move |cx| {
        let started = handle
            .update(cx, |_, window, _| glide_step(window, g))
            .is_ok();
        if !started {
            let mut gliding = GLIDING.get();
            gliding[slot] = None;
            GLIDING.set(gliding);
        }
    })
    .detach();
    true
}

/// The open panel's list and big window's table scroll handles, kept apart
/// from their views so they can be read and moved from anywhere.
#[derive(Default)]
struct Scrolls {
    list: Option<ScrollHandle>,
    table: Option<ScrollHandle>,
}

impl Scrolls {
    fn get(&self, panel: bool) -> Option<ScrollHandle> {
        if panel {
            self.list.clone()
        } else {
            self.table.clone()
        }
    }
}

thread_local! {
    static SCROLLS: std::cell::RefCell<Scrolls> = std::cell::RefCell::new(Scrolls::default());
}

/// Whether the big window is open.
pub fn window_open(cx: &App) -> bool {
    cx.try_global::<WindowSlot>()
        .and_then(|s| s.handle)
        .is_some_and(|h| is_live(h.into(), cx))
}

/// Where the big window was left: its restored bounds in logical px, and
/// whether it was maximised.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Place {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    maximized: bool,
}

/// The last place, kept for this run and in window.json beside config.json.
#[derive(Default)]
struct LastPlace(Option<Option<Place>>);

impl Global for LastPlace {}

fn place_file(cx: &App) -> Option<PathBuf> {
    let ui = cx.try_global::<Ui>()?;
    Some(ui.state.read(cx).config_file.parent()?.join("window.json"))
}

fn last_place(cx: &App) -> Option<Place> {
    if let Some(known) = cx.try_global::<LastPlace>().and_then(|l| l.0) {
        return known;
    }
    // Not closed yet this run: as the last run left it.
    place_file(cx)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|text| serde_json::from_str::<Place>(&text).ok())
        .filter(|p| p.w >= 100.0 && p.h >= 100.0 && p.x.is_finite() && p.y.is_finite())
}

/// Notes where the big window is, as it closes, so it opens there again.
pub(crate) fn remember_place(window: &Window, cx: &mut App) {
    let (b, maximized) = match window.window_bounds() {
        WindowBounds::Maximized(b) => (b, true),
        WindowBounds::Windowed(b) | WindowBounds::Fullscreen(b) => (b, false),
    };
    let place = Place {
        x: f32::from(b.origin.x),
        y: f32::from(b.origin.y),
        w: f32::from(b.size.width),
        h: f32::from(b.size.height),
        maximized,
    };
    cx.default_global::<LastPlace>().0 = Some(Some(place));
    if let (Some(file), Ok(text)) = (place_file(cx), serde_json::to_string(&place)) {
        std::fs::write(file, text).ok();
    }
}

/// Whether the big window opens maximised: as it was left.
pub(crate) fn opens_maximized(cx: &App) -> bool {
    let table = cx
        .try_global::<Ui>()
        .map_or(0.0, |ui| table_width(ui.state.read(cx)));
    matches!(window_place(table, cx).0, WindowBounds::Maximized(_))
}

/// Where the big window opens: where it was left (maximised again if it
/// was), else sized for `repos` columns at the primary monitor's corner.
/// With the logical px' scale.
/// Where the big window opens: where it last was, else wide enough for a
/// `table` this wide (logical px).
pub(crate) fn window_place(table: f32, cx: &App) -> (WindowBounds, f32) {
    let mon = win::primary_monitor();
    let scale = sane_scale(mon.scale);
    if let Some(p) = last_place(cx) {
        let b = Bounds::new(point(px(p.x), px(p.y)), size(px(p.w), px(p.h)));
        // Still on a monitor: its centre is inside one's area.
        let at = |v: f32| (v * scale).round() as i32;
        let (cx_, cy_) = (at(p.x + p.w / 2.0), at(p.y + p.h / 2.0));
        let on = win::monitor_at(cx_, cy_).rect;
        if (on.left..on.right).contains(&cx_) && (on.top..on.bottom).contains(&cy_) {
            return (
                if p.maximized {
                    WindowBounds::Maximized(b)
                } else {
                    WindowBounds::Windowed(b)
                },
                scale,
            );
        }
    }
    // GPUI places a window in workspace coordinates, which start at the
    // primary work area's corner, not the screen's.
    let (mut b, s) = default_place(table);
    b.origin.x -= px((mon.work.left - mon.rect.left) as f32 / s);
    b.origin.y -= px((mon.work.top - mon.rect.top) as f32 / s);
    (WindowBounds::Windowed(b), s)
}

/// The table's whole width, logical px: its columns and spines, as the
/// window lays them out.
pub(crate) fn table_width(state: &AppState) -> f32 {
    table_cells(state).iter().map(Cell::width).sum()
}

/// Where the window goes for a table `table` logical px wide, in logical
/// px, at the bottom-right of the primary work area, and that monitor's
/// scale.
fn default_place(table: f32) -> (Bounds<gpui::Pixels>, f32) {
    let mon = win::primary_monitor();
    let scale = sane_scale(mon.scale);
    let (left, top) = (mon.work.left as f32 / scale, mon.work.top as f32 / scale);
    let (work_w, work_h) = (
        mon.work.width() as f32 / scale,
        mon.work.height() as f32 / scale,
    );
    // Big mode: wide enough for every column, up to most of the screen.
    let width = (table.max(crate::timeline::COLUMN_WIDTH) + 2.0)
        .min(work_w * 0.9)
        .max(WINDOW_SIZE.0);
    let height = (work_h * 0.8).max(WINDOW_SIZE.1).min(work_h);
    let x = (mon.work.right as f32 / scale - width - WINDOW_MARGIN).max(left);
    let y = (mon.work.bottom as f32 / scale - height - WINDOW_MARGIN).max(top);
    (
        Bounds::new(point(px(x), px(y)), size(px(width), px(height))),
        scale,
    )
}

/// Closes the normal window, if it is open (`window` turned off).
pub fn close_window(cx: &mut App) {
    if let Some(handle) = cx.default_global::<WindowSlot>().handle.take() {
        remove(handle.into(), cx);
    }
}

/// Removes a window now, or once the current update has returned it to the
/// app when this is called from inside that window's own handler.
pub(crate) fn remove(handle: gpui::AnyWindowHandle, cx: &mut App) {
    if handle
        .update(cx, |_, window, _| window.remove_window())
        .is_err()
    {
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        });
    }
}

/// Whether `handle` still names an open window. A window closed by the
/// system (Alt+F4) leaves its handle behind in a slot.
pub(crate) fn is_live(handle: gpui::AnyWindowHandle, cx: &App) -> bool {
    cx.windows().contains(&handle)
}

/// GPUI's logical px are physical px over this; never divide by zero.
pub(crate) fn sane_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Panel,
    Window,
}

pub struct StatusView {
    state: Entity<AppState>,
    timelines: Option<Entity<Timelines>>,
    mode: Mode,
    /// Big mode's table of columns, scrolled sideways.
    table: ScrollHandle,
    /// The panel's list, scrolled up and down.
    list: ScrollHandle,
    focus: FocusHandle,
    /// The panel has been active once, so a deactivation now means the user
    /// clicked elsewhere rather than the foreground lock refusing us.
    seen_active: bool,
    /// The window opened before the first check, at these bounds: once the
    /// check is done it grows to fit the columns, unless the user has moved
    /// or resized it meanwhile.
    fit: Option<Bounds<gpui::Pixels>>,
}

impl StatusView {
    pub fn new(
        state: Entity<AppState>,
        mode: Mode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        // A new panel or window starts unfiltered.
        state.update(cx, |s, _| s.filter.clear());
        if mode == Mode::Window {
            cx.observe_in(&state, window, |this: &mut Self, state, window, cx| {
                this.fit_to_repos(&state, window, cx)
            })
            .detach();
        }
        let timelines = cx.try_global::<Ui>().map(|ui| ui.timelines.clone());
        if let Some(t) = &timelines {
            cx.observe(t, |_, _, cx| cx.notify()).detach();
        }
        // Repo icons land as they are drawn.
        if let Some(marks) = cx.try_global::<Ui>().map(|ui| ui.marks.clone()) {
            cx.observe(&marks, |_, _, cx| cx.notify()).detach();
        }
        cx.observe_window_appearance(window, move |_, window, cx| {
            // The rim and title bar follow the theme, in the colours the
            // flip draws.
            if let Some(h) = win::hwnd(window) {
                let dark = matches!(
                    window.appearance(),
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                );
                // The panel draws its own rim.
                if mode == Mode::Window {
                    cx.spawn(async move |_, _| unsafe {
                        win::set_frame_colors(h, rim_rgb(dark), title_bar_rgb(dark))
                    })
                    .detach();
                }
            }
            cx.notify()
        })
        .detach();
        if mode == Mode::Window {
            // Closing flips it back into the panel when it can.
            window.on_window_should_close(cx, |window, cx| !crate::flip::flip_back(window, cx));
        }
        if mode == Mode::Panel {
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if window.is_window_active() {
                    this.seen_active = true;
                } else if this.seen_active && !crate::repo_menu::is_open() {
                    panel::dismiss(window, cx);
                }
            })
            .detach();
        }
        // The root tracks this handle, so Esc reaches it without a click first.
        let focus = cx.focus_handle();
        focus.focus(window);
        let (table, list) = (ScrollHandle::new(), ScrollHandle::new());
        SCROLLS.with(|s| {
            let mut s = s.borrow_mut();
            match mode {
                Mode::Panel => s.list = Some(list.clone()),
                Mode::Window => s.table = Some(table.clone()),
            }
        });
        Self {
            state,
            timelines,
            mode,
            table,
            list,
            focus,
            seen_active: false,
            fit: None,
        }
    }

    /// Fits the window to the repos' columns once the first check has
    /// found them, if it opened before that.
    fn fit_to_repos(
        &mut self,
        state: &Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(opened) = self.fit else {
            return;
        };
        let s = state.read(cx);
        if !s.has_result {
            return;
        }
        self.fit = None;
        let near = |a: gpui::Pixels, b: gpui::Pixels| (f32::from(a) - f32::from(b)).abs() < 2.0;
        let now = window.bounds();
        let untouched = near(now.origin.x, opened.origin.x)
            && near(now.origin.y, opened.origin.y)
            && near(now.size.width, opened.size.width)
            && near(now.size.height, opened.size.height);
        let (want, scale) = default_place(table_width(s));
        if !untouched || near(want.size.width, opened.size.width) {
            return;
        }
        let Some(h) = win::hwnd(window) else {
            return;
        };
        let at = |v: gpui::Pixels| (f32::from(v) * scale).round() as i32;
        let r = crate::layout::Rect::new(
            at(want.origin.x),
            at(want.origin.y),
            at(want.origin.x + want.size.width),
            at(want.origin.y + want.size.height),
        );
        cx.spawn(async move |_, _| unsafe { win::place_window(h, r) })
            .detach();
    }
}

/// How many rows the panel opens with, for its first size before it is
/// measured: the unsynced repos, the collapsed "N synced" row, or the one
/// empty-state line; then the pull requests unless folded.
pub fn panel_rows(state: &AppState) -> usize {
    use crate::config::Section;
    // A section's heading, and its rows unless folded.
    let section = |n: usize, s: Section| match n {
        0 => 0,
        _ if state.folded(s) => 1,
        n => n + 1,
    };
    let unsynced = state.unsynced();
    let synced = state.statuses.len() - unsynced;
    let repos = (section(unsynced, Section::NeedsYou) + section(synced, Section::Synced)).max(1);
    // The pull requests' heading and rows, or the one GitHub problem line.
    let prs = match state.open_prs().len() {
        0 => usize::from(state.github_problem.is_some()),
        n => section(n, Section::PullRequests),
    };
    repos + prs + section(state.remote_statuses.len(), Section::GithubOnly)
}

/// Folds or opens `section` everywhere, saved in config.json.
fn fold(section: crate::config::Section) -> OnClick {
    Box::new(move |_, _, cx| {
        if let Some(ui) = cx.try_global::<Ui>() {
            let _ = ui
                .input
                .unbounded_send(Input::Action(crate::tray::TrayAction::Fold(section)));
        }
    })
}

/// The spine's heading text height, logical px.
pub(crate) const SPINE_TEXT: f32 = 12.0;
/// A folded or open section's narrow column in big mode, its name
/// running up it.
pub(crate) const SPINE_WIDTH: f32 = 30.0;

/// One cell of big mode's table, left to right.
#[derive(Clone, Copy)]
pub(crate) enum Cell<'a> {
    /// A section's spine, with how many repos it holds.
    Spine(Section, usize),
    Repo(&'a RepoStatus),
}

impl Cell<'_> {
    pub(crate) fn width(&self) -> f32 {
        match self {
            Cell::Spine(..) => SPINE_WIDTH,
            Cell::Repo(_) => crate::timeline::COLUMN_WIDTH,
        }
    }
}

/// Big mode's table: each section with repos as its spine, then its
/// repos' columns unless it is folded. Needs you, synced, then the GitHub
/// repos with no clone, each in the configured order.
pub(crate) fn table_cells(state: &AppState) -> Vec<Cell<'_>> {
    let sorted = state.sorted(&state.statuses);
    let (bad, good): (Vec<&RepoStatus>, Vec<&RepoStatus>) =
        sorted.into_iter().partition(|s| !s.is_synced());
    let remote = state.sorted(&state.remote_statuses);
    let mut cells = Vec::new();
    for (section, repos) in [
        (Section::NeedsYou, bad),
        (Section::Synced, good),
        (Section::GithubOnly, remote),
    ] {
        if repos.is_empty() {
            continue;
        }
        cells.push(Cell::Spine(section, repos.len()));
        if !state.folded(section) {
            cells.extend(repos.into_iter().map(Cell::Repo));
        }
    }
    cells
}

/// The cell holding `path`'s column.
pub(crate) fn cell_of(cells: &[Cell], path: &std::path::Path) -> Option<usize> {
    cells
        .iter()
        .position(|c| matches!(c, Cell::Repo(s) if s.path == path))
}

/// A section's colour: its heading's words in the panel and its spine's
/// in the big window.
pub(crate) fn section_color(section: Section, p: &Palette) -> Hsla {
    match section {
        Section::NeedsYou => p.red,
        Section::Synced => p.green,
        Section::GithubOnly | Section::PullRequests => p.muted,
    }
}

/// A panel section heading's words.
/// A section's heading words, alike in the panel and on its spine.
pub(crate) fn section_text(section: Section, n: usize) -> String {
    match section {
        Section::NeedsYou => format!("● {n} need you"),
        Section::Synced => format!("✓ {n} synced"),
        Section::PullRequests => format!("{n} pull requests"),
        Section::GithubOnly => format!("◌ {n} on GitHub only"),
    }
}

/// The faint wash of its colour behind a section's heading in the panel
/// and its spine in the big window, alike.
pub(crate) fn section_tint(section: Section, p: &Palette) -> Hsla {
    section_color(section, p).opacity(0.10)
}

/// A section's spine in big mode: its fold arrow, count and name down a
/// narrow column; a click folds or opens it. Its wash and the line at its
/// side are the scene's parts, drawn under this.
pub(crate) fn spine(section: Section, count: usize, folded: bool, p: &Palette) -> AnyElement {
    let color = section_color(section, p);
    let id = match section {
        Section::NeedsYou => "spine-needs-you",
        Section::Synced => "spine-synced",
        Section::GithubOnly => "spine-github-only",
        Section::PullRequests => "spine-prs",
    };
    let label = section_text(section, count);
    let len = crate::turned_text::length(&label, SPINE_TEXT);
    div()
        .id(id)
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .size_full()
        .pt_2()
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .text_xs()
        .text_color(color)
        // The panel's heading turned a quarter turn anticlockwise: its
        // arrow at the top (open, its columns are to the right; folded,
        // it points up), its words below reading upwards.
        .child(
            div()
                .text_color(p.muted)
                .child(if folded { "▴" } else { "▸" }),
        )
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    crate::turned_text::paint(window, bounds, &label, SPINE_TEXT, color)
                },
            )
            .w_full()
            .h(px(len)),
        )
        .on_click(fold(section))
        .into_any_element()
}

/// Opens a repo the way config's `repo_click` says: its folder, or its
/// GitHub page (the folder when it has none). Only opens; changes nothing.
pub fn open_repo(path: &std::path::Path, cx: &mut App) {
    let github = cx
        .try_global::<Ui>()
        .is_some_and(|ui| ui.state.read(cx).config.repo_click == crate::config::RepoClick::Github);
    // A repo with no clone has only its GitHub page.
    if github || crate::github::remote_slug(path).is_some() {
        crate::repo_menu::open_github(path, cx);
    } else {
        cx.open_with_system(path);
    }
}

fn refresh(cx: &mut App) {
    if let Some(refresh) = cx.try_global::<Ui>().map(|ui| ui.refresh.clone()) {
        refresh(cx);
    }
}

fn mode_text(mode: &WatchMode) -> String {
    match mode {
        WatchMode::Events => "Watching for changes".to_string(),
        WatchMode::Polling { reason } => format!("Polling ({reason})"),
        WatchMode::Off => "Interval only".to_string(),
    }
}

pub(crate) fn status_color(s: &RepoStatus, p: &Palette) -> Hsla {
    if s.remote_only {
        p.muted
    } else if s.error.is_some() || s.is_diverged() || s.behind > 0 {
        p.red
    } else if !s.is_synced() {
        p.amber
    } else {
        p.green
    }
}

/// One repo; a click opens its folder (and closes the panel).
fn repo_row(index: usize, s: &RepoStatus, p: &Palette, mode: Mode, cx: &App) -> AnyElement {
    let color = status_color(s, p);
    let path = s.path.clone();
    let (py, gap) = match mode {
        Mode::Panel => (px(6.), px(10.)),
        Mode::Window => (px(8.), px(12.)),
    };
    div()
        .id(("repo", index))
        .flex()
        .items_center()
        .gap(gap)
        .px_4()
        .py(py)
        .cursor_pointer()
        .hover(|style| style.bg(p.hover))
        .on_click(move |_, window, cx| {
            open_repo(&path, cx);
            if mode == Mode::Panel {
                panel::dismiss(window, cx);
            }
        })
        .on_mouse_down(gpui::MouseButton::Right, {
            let s = s.clone();
            move |_, window, cx| {
                cx.stop_propagation();
                crate::repo_menu::show(&s, window, cx);
            }
        })
        .child(crate::repo_marks::global_mark(s, 16.0, color, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(p.text)
                .child(s.name.clone()),
        )
        .child(
            // Long fetch errors would otherwise push the name out.
            div()
                .flex_none()
                .max_w(gpui::relative(0.6))
                .truncate()
                .text_sm()
                .text_color(color)
                .child(s.label()),
        )
        .into_any_element()
}

/// A draft PR's mark in icons-only mode, drawn rather than typed.
const DRAFT_GLYPH: &str = "◌";

/// What a pull request's row says on its right, most pressing first:
/// (glyph for icons-only mode, words, colour).
fn pr_marks(pr: &Pr, p: &Palette) -> Vec<(&'static str, &'static str, Hsla)> {
    let mut marks = Vec::new();
    if pr.review_requested {
        marks.push(("◉", "review requested", p.amber));
    }
    match pr.checks {
        Checks::Failing => marks.push(("✗", "checks failing", p.red)),
        Checks::Pending => marks.push(("●", "checks running", p.amber)),
        Checks::Passing => marks.push(("✓", "checks pass", p.green)),
        Checks::None => {}
    }
    if pr.mergeable == Mergeable::Conflicts {
        marks.push(("⚠", "conflicts", p.red));
    }
    match pr.review {
        Review::ChangesRequested => marks.push(("✎", "changes requested", p.red)),
        Review::Approved => marks.push(("★", "approved", p.green)),
        Review::Required if !pr.review_requested => marks.push(("…", "awaiting review", p.muted)),
        _ => {}
    }
    if pr.state == PrState::Draft {
        marks.push((DRAFT_GLYPH, "draft", p.muted));
    }
    marks
}

/// One open pull request; a click opens it on GitHub (and closes the
/// panel, not the window). Only a browser link: nothing is changed on GitHub.
fn pr_row(
    index: usize,
    s: &RepoStatus,
    pr: &Pr,
    p: &Palette,
    mode: Mode,
    icons_only: bool,
    cx: &App,
) -> AnyElement {
    let marks = pr_marks(pr, p);
    let url = pr.url.clone();
    let tone = if pr.needs_attention() {
        p.amber
    } else if pr.state == PrState::Draft {
        p.muted
    } else {
        p.green
    };
    let mut lines = vec![
        (format!("{} #{}", s.name, pr.number), p.text),
        (pr.title.clone(), p.text),
        (format!("{} by {}", pr.head_branch, pr.author), p.muted),
    ];
    lines.extend(marks.iter().map(|&(_, words, c)| (words.to_string(), c)));
    let right = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .text_sm()
        .children(
            marks
                .iter()
                .take(if icons_only { 5 } else { 2 })
                .map(|&(glyph, words, c)| match (icons_only, glyph) {
                    // The font's dotted circle comes out jagged at this
                    // size; a dashed ring is drawn smooth.
                    (true, DRAFT_GLYPH) => div()
                        .flex_none()
                        .size(px(11.))
                        .rounded_full()
                        .border_1()
                        .border_dashed()
                        .border_color(c),
                    _ => div()
                        .text_color(c)
                        .child(text(if icons_only { glyph } else { words })),
                }),
        );
    div()
        .id(("pr", index))
        .flex()
        .items_center()
        .gap(px(10.))
        .px_4()
        .py(px(6.))
        .cursor_pointer()
        .hover(|style| style.bg(p.hover))
        .tooltip(tip(lines, p))
        .on_click(move |_, window, cx| {
            cx.open_url(&url);
            if mode == Mode::Panel {
                panel::dismiss(window, cx);
            }
        })
        .child(crate::repo_marks::global_mark(s, 16.0, tone, cx))
        .child(
            div()
                .flex_none()
                .text_sm()
                .text_color(p.muted)
                .child(text(format!("#{}", pr.number))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(p.text)
                .when(!icons_only, |d| d.child(text(pr.title.clone()))),
        )
        .child(right)
        .into_any_element()
}

/// A click handler, boxed.
type OnClick = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>;

/// The pull requests' heading and rows, or the one GitHub problem line.
fn pr_rows(
    open_prs: &[(&RepoStatus, &Pr)],
    problem: Option<&str>,
    p: &Palette,
    mode: Mode,
    icons_only: bool,
    // Big mode: whether the list is folded, and what folds or unfolds it.
    fold: Option<(bool, OnClick)>,
    cx: &App,
) -> Vec<AnyElement> {
    let mut out = Vec::new();
    let folded = fold.as_ref().is_some_and(|(f, _)| *f);
    if !open_prs.is_empty() {
        let asked = open_prs
            .iter()
            .filter(|(_, pr)| pr.review_requested)
            .count();
        let mut heading = format!("Pull requests · {} open", open_prs.len());
        if asked > 0 {
            heading.push_str(&format!(" · {asked} for your review"));
        }
        let head = div()
            .id("prs-heading")
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .pt_3()
            .pb_1()
            .border_t_1()
            .border_color(p.border)
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(if asked > 0 { p.amber } else { p.muted })
            .child(div().flex_1().min_w_0().truncate().child(text(heading)));
        let head = match fold {
            Some((folded, toggle)) => head
                .when(folded, |d| d.pb_3())
                .cursor_pointer()
                .hover(|style| style.bg(p.hover))
                .child(div().flex_none().text_color(p.muted).child(text(if folded {
                    "▸"
                } else {
                    "▾"
                })))
                .on_click(toggle),
            None => head,
        };
        out.push(head.into_any_element());
        for (i, (s, pr)) in open_prs.iter().enumerate().filter(|_| !folded) {
            out.push(pr_row(i, s, pr, p, mode, icons_only, cx));
        }
    } else if let Some(problem) = problem {
        out.push(
            div()
                .px_4()
                .py(px(6.))
                .border_t_1()
                .border_color(p.border)
                .text_xs()
                .text_color(p.muted)
                .truncate()
                .child(text(format!("Pull requests: {problem}")))
                .into_any_element(),
        );
    }
    out
}

/// Why the GitHub repo list couldn't be read, when the pull requests
/// aren't already saying it.
fn remote_problem_row(problem: &str, p: &Palette) -> AnyElement {
    div()
        .px_4()
        .py(px(6.))
        .border_t_1()
        .border_color(p.border)
        .text_xs()
        .text_color(p.muted)
        .truncate()
        .child(text(format!("GitHub repos: {problem}")))
        .into_any_element()
}

/// The pull requests' block: in the panel the last rows of its list, in
/// the big window the list under the columns, a few rows high and
/// scrolling when there are more. Tagged for the flip, which slides it
/// out of and into the bottom bar. None when there's nothing to list.
pub(crate) fn prs_block(
    state: &AppState,
    mode: Mode,
    icons_only: bool,
    p: &Palette,
    cx: &App,
) -> Option<AnyElement> {
    let open_prs = state.open_prs();
    let pr_problem = state
        .github_problem
        .as_deref()
        .filter(|_| state.config.github_prs);
    // The pull requests show a GitHub problem; without them, this does.
    let remote_problem = if state.config.github_prs || !state.config.github_repos {
        None
    } else {
        state.github_problem.as_deref()
    };
    let mut rows = pr_rows(
        &open_prs,
        pr_problem,
        p,
        mode,
        icons_only,
        Some((
            state.folded(Section::PullRequests),
            fold(Section::PullRequests),
        )),
        cx,
    );
    if mode == Mode::Window
        && state.remote_statuses.is_empty()
        && let Some(problem) = remote_problem
    {
        rows.push(remote_problem_row(problem, p));
    }
    if rows.is_empty() {
        return None;
    }
    let block = div()
        .id("prs")
        .flex()
        .flex_col()
        .flex_none()
        .bg(p.surface)
        .children(rows);
    Some(match mode {
        Mode::Panel => block.into_any_element(),
        Mode::Window => block
            .max_h(px(WINDOW_PRS_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&PRS_SCROLL.with(Clone::clone))
            .into_any_element(),
    })
}

/// Big mode's pull request list at its tallest: the heading and about
/// six rows.
const WINDOW_PRS_HEIGHT: f32 = 230.;

/// The top bar's words: the headline in its colour, and the line under it.
fn titles(state: &AppState, p: &Palette) -> (SharedString, Hsla, String) {
    let unsynced = state.unsynced();
    let problem = state.problem();
    let sweeping = state.sweeping;
    let headline: SharedString = crate::tray::summary(
        &TraySnapshot {
            statuses: &state.statuses,
            checking: sweeping,
            has_result: state.has_result,
            problem: problem.as_deref(),
            incomplete: state.capped.is_some(),
        },
        unsynced,
    )
    .into();
    let headline_color = if problem.is_some() || unsynced > 0 {
        p.red
    } else if state.has_result {
        p.green
    } else {
        p.muted
    };
    let roots = state.config.roots();
    let mut subline = roots
        .first()
        .map(|r| r.display().to_string())
        .unwrap_or_default();
    if roots.len() > 1 {
        subline.push_str(&format!(" + {} more", roots.len() - 1));
    }
    if let Some(time) = &state.last_checked {
        subline.push_str(&format!(" · checked {time}"));
    }
    if sweeping {
        subline.push_str(" · checking…");
    }
    if !state.filter.is_empty() {
        let found =
            state.sorted(&state.statuses).len() + state.sorted(&state.remote_statuses).len();
        subline = format!(
            "Filter “{}”: {found} {} · Esc clears",
            state.filter,
            if found == 1 { "repo" } else { "repos" }
        );
    }
    (headline, headline_color, subline)
}

impl Render for StatusView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.mode == Mode::Panel {
            return self.render_panel(window, cx);
        }
        self.render_window(window, cx)
    }
}

/// Typing filters the repos: letters add to the filter, Backspace takes
/// one off, Esc clears it, Enter opens the first repo listed. True when
/// the key was the filter's.
fn filter_key(
    state: &Entity<AppState>,
    event: &gpui::KeyDownEvent,
    mode: Mode,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let k = &event.keystroke;
    let m = &k.modifiers;
    if m.control || m.alt || m.platform || m.function {
        return false;
    }
    let filter = state.read(cx).filter.clone();
    let typed = k
        .key_char
        .as_deref()
        .filter(|c| c.chars().all(|ch| !ch.is_control()) && !c.is_empty());
    let new = match k.key.as_str() {
        "escape" if !filter.is_empty() => String::new(),
        "backspace" if !filter.is_empty() => {
            let mut f = filter.clone();
            f.pop();
            f
        }
        "enter" if !filter.is_empty() => {
            let first = {
                let s = state.read(cx);
                s.sorted(&s.statuses)
                    .first()
                    .or(s.sorted(&s.remote_statuses).first())
                    .map(|r| r.path.clone())
            };
            if let Some(path) = first {
                open_repo(&path, cx);
                if mode == Mode::Panel {
                    panel::dismiss(window, cx);
                }
            }
            return true;
        }
        _ => match typed {
            // A space can't start a filter.
            Some(c) if !(filter.is_empty() && c.trim().is_empty()) => format!("{filter}{c}"),
            _ => return false,
        },
    };
    state.update(cx, |s, cx| {
        s.filter = new;
        cx.notify();
    });
    true
}

/// The light rim Windows 11 draws around a window, for where gitstat
/// draws it itself (the flip, a clipped slide).
/// gitstat sets it on its windows itself (`win::set_frame_colors`), so the
/// one it draws is exactly the same and they hand over unseen.
pub(crate) fn rim(dark: bool) -> Hsla {
    gpui::rgb(rim_rgb(dark)).into()
}

thread_local! {
    /// How thick Windows draws the rim, physical px, by the scale (in
    /// hundredths) it was read at: each monitor's scale has its own.
    static RIM_PX: std::cell::RefCell<std::collections::HashMap<u32, u32>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn scale_key(s: f32) -> u32 {
    (s * 100.0).round() as u32
}

/// Notes how thick Windows draws `h`'s rim at the scale it draws at, for
/// gitstat to draw its own exactly as thick.
pub(crate) fn seen_rim(h: isize) {
    if let (Some(v), Some(s)) = (win::rim_thickness(h), win::window_scale(h)) {
        RIM_PX.with(|c| {
            c.borrow_mut().insert(scale_key(s), v);
        });
    }
}

/// The rim's thickness, physical px, at scale `s`: as Windows draws it
/// there, else one logical px.
pub(crate) fn rim_width(s: f32) -> f32 {
    RIM_PX
        .with(|c| c.borrow().get(&scale_key(s)).copied())
        .map_or(s.round().max(1.0), |v| v as f32)
}

/// The rim's colour, 0xRRGGBB.
pub(crate) fn rim_rgb(dark: bool) -> u32 {
    if dark { 0x444444 } else { 0xd6d6d6 }
}

/// The big window's title bar colour, set on it and drawn by the flip.
pub(crate) fn title_bar(dark: bool) -> Hsla {
    gpui::rgb(title_bar_rgb(dark)).into()
}

/// The title bar's colour, 0xRRGGBB.
pub(crate) fn title_bar_rgb(dark: bool) -> u32 {
    if dark { 0x202020 } else { 0xf3f3f3 }
}

/// Repos synced, with local work, and needing a look.
pub(crate) fn tone_counts(statuses: &[RepoStatus]) -> [usize; 3] {
    let mut counts = [0usize; 3];
    for s in statuses {
        let i = match crate::icon::FolderState::of(s).tone() {
            crate::icon::Tone::Synced => 0,
            crate::icon::Tone::Local => 1,
            crate::icon::Tone::Attention => 2,
        };
        counts[i] += 1;
    }
    counts
}

/// The tooltip's widest, and the characters of `text_xs` that fit in it.
const TIP_WIDTH: f32 = 320.;
const TIP_LINE_CHARS: usize = 44;

/// A tooltip of coloured lines.
struct Tip {
    lines: Vec<(SharedString, Hsla)>,
    bg: Hsla,
    border: Hsla,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A line that wraps needs a definite width: under only a max width
        // its height is measured as one line and the next line overlaps it.
        let wraps = self
            .lines
            .iter()
            .any(|(text, _)| text.chars().count() > TIP_LINE_CHARS);
        div()
            .flex()
            .flex_col()
            .max_w(px(TIP_WIDTH))
            .when(wraps, |d| d.w(px(TIP_WIDTH)))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(self.border)
            .bg(self.bg)
            .shadow_md()
            .text_xs()
            .children(
                self.lines
                    .iter()
                    .filter(|(text, _)| !text.is_empty())
                    .map(|(text, color)| div().text_color(*color).child(text.clone())),
            )
    }
}

/// What a repo's tooltip adds below its name and status: the rest of a
/// fetch error, the unpushed branches by name, the stash, and where the
/// actions are.
pub(crate) fn status_details(s: &RepoStatus, p: &Palette) -> Vec<(String, Hsla)> {
    let mut lines = Vec::new();
    if let Some(error) = &s.error {
        for line in error.lines().skip(1).filter(|l| !l.trim().is_empty()) {
            lines.push((line.trim().to_string(), p.muted));
        }
    }
    for (branch, n) in &s.unpushed_branches {
        let commits = if *n == 1 { "commit" } else { "commits" };
        lines.push((format!("{branch}: {n} {commits} on no remote"), p.amber));
    }
    if s.stashes > 0 {
        lines.push((format!("{} stashed", s.stashes), p.muted));
    }
    let hint = if s.error.is_some() && !s.remote_only {
        "Right-click to fetch again, or for more"
    } else {
        "Right-click for more"
    };
    lines.push((hint.to_string(), p.muted));
    lines
}

/// A tooltip builder showing `lines`, for icons-only mode.
pub(crate) fn tip(
    lines: Vec<(String, Hsla)>,
    p: &Palette,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let lines: Vec<(SharedString, Hsla)> = lines.into_iter().map(|(t, c)| (t.into(), c)).collect();
    let (bg, border) = (p.bg, p.border);
    move |_, cx| {
        // Nothing is pointed at while the views turn into one another.
        if crate::flip::running(cx) {
            return cx.new(|_| gpui::EmptyView).into();
        }
        let lines = lines.clone();
        cx.new(|_| Tip { lines, bg, border }).into()
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Hsla,
    pub surface: Hsla,
    pub hover: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub green: Hsla,
    pub red: Hsla,
    pub amber: Hsla,
    pub button: Hsla,
    pub button_text: Hsla,
}

impl Palette {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        let dark = matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        if dark {
            Self {
                bg: rgb(0x1e1f22).into(),
                surface: rgb(0x2b2d31).into(),
                hover: rgb(0x35373c).into(),
                border: rgb(0x3a3c42).into(),
                text: rgb(0xe8e8ea).into(),
                muted: rgb(0x9a9ca3).into(),
                green: rgb(0x3fb950).into(),
                red: rgb(0xf26b6b).into(),
                amber: rgb(0xd6a33b).into(),
                button: rgb(0x3d6fe0).into(),
                button_text: rgb(0xffffff).into(),
            }
        } else {
            Self {
                bg: rgb(0xf6f7f9).into(),
                surface: rgb(0xffffff).into(),
                hover: rgb(0xeef0f4).into(),
                border: rgb(0xdcdfe5).into(),
                text: rgb(0x1f2328).into(),
                muted: rgb(0x656d76).into(),
                green: rgb(0x1a7f37).into(),
                red: rgb(0xcf222e).into(),
                amber: rgb(0x9a6700).into(),
                button: rgb(0x2f5fd0).into(),
                button_text: rgb(0xffffff).into(),
            }
        }
    }
}
