//! The status list, shared by the flyout panel and the normal window, plus
//! the normal window itself (`show_window`, `close_window`).

use crate::config::Section;
use crate::git::RepoStatus;
use crate::github::{Checks, Mergeable, Pr, PrState, Review};
use crate::monitor::WatchMode;
use crate::panel;
use crate::state::{AppState, Input};
use crate::timeline::Timelines;
use crate::tray::TraySnapshot;
use crate::win;
use futures::channel::mpsc::UnboundedSender;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle, Global, Hsla, IntoElement,
    ParentElement, Render, ScrollHandle, ScrollWheelEvent, SharedString, Styled, Window,
    WindowAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, canvas, div, point,
    prelude::*, px, rgb, size,
};
use std::path::PathBuf;
use std::rc::Rc;

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

/// Brings the normal window to the front, opening it at the bottom-right of
/// the primary work area if it isn't open.
pub fn show_window(cx: &mut App) {
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
    let remembered = last_place(cx).is_some();
    let (repos, known) = {
        let s = state.read(cx);
        (s.statuses.len(), s.has_result)
    };
    let (place, scale) = window_place(repos, cx);
    let bounds = place.get_bounds();
    let maximized = matches!(place, WindowBounds::Maximized(_));
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(place),
                kind: WindowKind::Normal,
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("gitstat".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(300.), px(200.))),
                ..Default::default()
            },
            |window, cx| {
                // Safe: opened with show: true, so there is no parked placement.
                window.activate_window();
                // Where the client area came out, so the flip lands on it.
                if let Some(h) = win::hwnd(window) {
                    let at = |v: gpui::Pixels| (f32::from(v) * scale).round() as i32;
                    let requested = if maximized {
                        maximized_area(bounds, scale)
                    } else {
                        crate::layout::Rect::new(
                            at(bounds.origin.x),
                            at(bounds.origin.y),
                            at(bounds.origin.x + bounds.size.width),
                            at(bounds.origin.y + bounds.size.height),
                        )
                    };
                    cx.spawn(async move |_| {
                        // Minimising goes the way closing does.
                        unsafe { win::minimize_closes(h) };
                        if let Some(client) = win::client_rect(h) {
                            crate::morph::seen_client(requested, client, maximized);
                        }
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

/// How far the open big window's table is scrolled sideways, logical px.
pub fn table_scroll(cx: &App) -> f32 {
    cx.try_global::<WindowSlot>()
        .and_then(|s| s.handle)
        .and_then(|h| h.read(cx).ok())
        .map_or(0.0, |v| (-f32::from(v.table.offset().x)).max(0.0))
}

/// Whether the big window is open.
pub fn window_open(cx: &App) -> bool {
    cx.try_global::<WindowSlot>()
        .and_then(|s| s.handle)
        .is_some_and(|h| is_live(h.into(), cx))
}

/// Where the window goes for `repos` columns, in logical px, at the
/// bottom-right of the primary work area, and that monitor's scale.
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

/// A maximised window's area: the work area of the monitor its restored
/// `bounds` (logical px at `scale`) centre on, physical px.
pub(crate) fn maximized_area(bounds: Bounds<gpui::Pixels>, scale: f32) -> crate::layout::Rect {
    let c = bounds.center();
    let at = |v: gpui::Pixels| (f32::from(v) * scale).round() as i32;
    win::monitor_at(at(c.x), at(c.y)).work
}

/// Where the big window opens: where it was left (maximised again if it
/// was), else sized for `repos` columns at the primary monitor's corner.
/// With the logical px' scale.
pub(crate) fn window_place(repos: usize, cx: &App) -> (WindowBounds, f32) {
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
    let (b, s) = default_place(repos);
    (WindowBounds::Windowed(b), s)
}

fn default_place(repos: usize) -> (Bounds<gpui::Pixels>, f32) {
    let mon = win::primary_monitor();
    let scale = sane_scale(mon.scale);
    let (left, top) = (mon.work.left as f32 / scale, mon.work.top as f32 / scale);
    let (work_w, work_h) = (
        mon.work.width() as f32 / scale,
        mon.work.height() as f32 / scale,
    );
    // Big mode: wide enough for every column, up to most of the screen.
    let width = (repos.max(1) as f32 * crate::timeline::COLUMN_WIDTH + 2.0)
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
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        if mode == Mode::Window {
            // Closing flips it back into the panel when it can.
            window.on_window_should_close(cx, |window, cx| !crate::morph::flip_back(window, cx));
        }
        if mode == Mode::Panel {
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if window.is_window_active() {
                    this.seen_active = true;
                } else if this.seen_active {
                    panel::dismiss(window, cx);
                }
            })
            .detach();
        }
        // The root tracks this handle, so Esc reaches it without a click first.
        let focus = cx.focus_handle();
        focus.focus(window);
        Self {
            state,
            timelines,
            mode,
            table: ScrollHandle::new(),
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
        let (want, scale) = default_place(s.statuses.len());
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

/// Notes where a panel section's heading is drawn, for the flip.
fn seen_header(section: Section) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            if window.content_mask().bounds.intersects(&bounds) {
                crate::morph::seen_header(section, bounds);
            }
        },
    )
    .absolute()
    .inset_0()
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
const SPINE_TEXT: f32 = 12.0;
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

/// Each cell's left edge in the unscrolled table, logical px.
pub(crate) fn cell_lefts(cells: &[Cell]) -> Vec<f32> {
    let mut x = 0.0;
    cells
        .iter()
        .map(|c| {
            let at = x;
            x += c.width();
            at
        })
        .collect()
}

/// The cell holding `path`'s column.
pub(crate) fn cell_of(cells: &[Cell], path: &std::path::Path) -> Option<usize> {
    cells
        .iter()
        .position(|c| matches!(c, Cell::Repo(s) if s.path == path))
}

/// The big-mode cells worth building: the ones in view and one more on
/// each side. The rest are empty boxes of the same width, so the table
/// scrolls as far, but hovering or animating one column doesn't lay out
/// every repo's. Scrolling redraws the view, which builds the new ones.
fn shown_columns(
    table: &ScrollHandle,
    window: &Window,
    lefts: &[f32],
    focused: Option<usize>,
) -> std::ops::Range<usize> {
    let w = crate::timeline::COLUMN_WIDTH;
    // Before the first paint the table has no size yet; the window's
    // width is as wide as it can be.
    let width = match f32::from(table.bounds().size.width) {
        w if w > 0.0 => w,
        _ => f32::from(window.viewport_size().width),
    };
    // A column about to be scrolled to may land at either edge.
    let (from, to) = match focused.and_then(|i| lefts.get(i)) {
        Some(&x) => (x - width, x + width + w),
        None => {
            let left = (-f32::from(table.offset().x)).max(0.0);
            (left - w, left + width + w)
        }
    };
    let first = lefts
        .iter()
        .position(|&x| x + w > from)
        .unwrap_or(lefts.len());
    let last = lefts.iter().rposition(|&x| x < to).map_or(0, |i| i + 1);
    first..last.max(first)
}

/// A section's spine in big mode: its fold arrow, count and name down a
/// narrow column; a click folds or opens it.
/// A section's colour: its heading's words in the panel and its spine's
/// in the big window.
pub(crate) fn section_color(section: Section, p: &Palette) -> Hsla {
    match section {
        Section::NeedsYou => p.red,
        Section::Synced => p.green,
        Section::GithubOnly | Section::PullRequests => p.muted,
    }
}

/// The faint wash of its colour behind a section's heading in the panel
/// and its spine in the big window, alike.
pub(crate) fn section_tint(section: Section, p: &Palette) -> Hsla {
    section_color(section, p).opacity(0.10)
}

fn spine(section: Section, count: usize, folded: bool, p: &Palette) -> AnyElement {
    let (name, color) = match section {
        Section::NeedsYou => ("NEED YOU", p.red),
        Section::Synced => ("SYNCED", p.green),
        Section::GithubOnly => ("GITHUB ONLY", p.muted),
        Section::PullRequests => ("PULL REQUESTS", p.muted),
    };
    let id = match section {
        Section::NeedsYou => "spine-needs-you",
        Section::Synced => "spine-synced",
        Section::GithubOnly => "spine-github-only",
        Section::PullRequests => "spine-prs",
    };
    div()
        .id(id)
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .w(px(SPINE_WIDTH))
        .h_full()
        .pt_2()
        .border_r_1()
        .border_color(p.border)
        .bg(section_tint(section, p))
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .text_xs()
        .text_color(color)
        .child({
            // The section's heading turned a quarter turn anticlockwise,
            // reading upwards: its count nearest the arrow, as in the panel.
            let label = format!("{count}  {name}");
            let len = crate::turned_text::length(&label, SPINE_TEXT);
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    crate::turned_text::paint(window, bounds, &label, SPINE_TEXT, color)
                },
            )
            .w_full()
            .h(px(len))
        })
        // Open, its columns are to the right; folded, the arrow turns with
        // the heading, as the panel's ▸ would.
        .child(
            div()
                .text_color(p.muted)
                .child(if folded { "▴" } else { "▸" }),
        )
        .child(
            // Where the flip lands this section's spine.
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| crate::morph::seen_spine(bounds, window),
            )
            .absolute()
            .inset_0(),
        )
        .relative()
        .on_click(fold(section))
        .into_any_element()
}

/// An unbuilt column: as wide and tall as a real one, with its border.
fn column_placeholder(p: &Palette) -> AnyElement {
    div()
        .flex_none()
        .w(px(crate::timeline::COLUMN_WIDTH))
        .h_full()
        .border_r_1()
        .border_color(p.border)
        .into_any_element()
}

/// Opens a repo the way config's `repo_click` says: its folder, or its
/// GitHub page (the folder when it has none). Only opens; changes nothing.
pub fn open_repo(path: &std::path::Path, cx: &mut App) {
    // A repo with no clone has only its GitHub page.
    if let Some(slug) = crate::github::remote_slug(path) {
        cx.open_url(&format!("https://github.com/{slug}"));
        return;
    }
    let github = cx
        .try_global::<Ui>()
        .is_some_and(|ui| ui.state.read(cx).config.repo_click == crate::config::RepoClick::Github);
    if github {
        let web = cx
            .try_global::<Ui>()
            .and_then(|ui| ui.timelines.read(cx).web(path));
        if let Some(web) = web {
            cx.open_url(&web);
            return;
        }
        // Not drawn yet: ask git off the UI thread, which a slow or busy
        // disk would otherwise freeze.
        let repo = path.to_path_buf();
        let task = cx.background_spawn(async move {
            let web = crate::repo_info::origin_web(&repo);
            (repo, web)
        });
        cx.spawn(async move |cx| {
            let (repo, web) = task.await;
            cx.update(|cx| match web {
                Some(web) => cx.open_url(&web),
                None => cx.open_with_system(&repo),
            })
            .ok();
        })
        .detach();
        return;
    }
    cx.open_with_system(path);
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
                        .child(if icons_only { glyph } else { words }),
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
                .child(format!("#{}", pr.number)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(p.text)
                .when(!icons_only, |d| d.child(pr.title.clone())),
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
            .child(div().flex_1().min_w_0().truncate().child(heading));
        let head = match fold {
            Some((folded, toggle)) => head
                .when(folded, |d| d.pb_3())
                .cursor_pointer()
                .hover(|style| style.bg(p.hover))
                .child(div().flex_none().text_color(p.muted).child(if folded {
                    "▸"
                } else {
                    "▾"
                }))
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
                .child(format!("Pull requests: {problem}"))
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
        .child(format!("GitHub repos: {problem}"))
        .into_any_element()
}

/// Big mode's pull request list at its tallest: the heading and about
/// six rows.
const WINDOW_PRS_HEIGHT: f32 = 230.;

/// A small square glyph button for the panel header.
fn icon_button(
    id: &'static str,
    glyph: &'static str,
    enabled: bool,
    p: &Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size_7()
        .rounded_md()
        .text_color(p.text)
        .when(!enabled, |b| b.opacity(0.4))
        .when(enabled, |b| b.cursor_pointer().hover(|s| s.bg(p.hover)))
        .child(glyph)
}

impl Render for StatusView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_appearance(window.appearance());
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let mode = self.mode;
        // The panel's rows measured as laid out, so the panel can be made
        // exactly as tall as what it shows.
        let content_h = Rc::new(std::cell::Cell::new(0.0f32));
        let timelines = self.timelines.clone();
        // Pictures only: the panel's words move into tooltips.
        let icons_only = mode == Mode::Panel && self.state.read(cx).config.icons_only;
        // A repo row: a timeline strip in the panel, a plain row elsewhere.
        let row = |i: usize, s: &RepoStatus, cx: &App| match (&timelines, mode) {
            (Some(t), Mode::Panel) => Timelines::strip(t, i, s, &p, dark, icons_only, cx),
            _ => repo_row(i, s, &p, mode, cx),
        };
        // A strip's label asked big mode for its repo's column.
        let focus = match (&self.timelines, mode) {
            (Some(t), Mode::Window) => t.update(cx, |t, _| t.take_focus()),
            _ => None,
        };
        let state = self.state.clone();
        let state = state.read(cx);
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
        let open_prs = state.open_prs();

        // The top bar's parts, which the flip carries between the views.
        let side = match mode {
            Mode::Panel => crate::morph::Side::Panel,
            Mode::Window => crate::morph::Side::Column,
        };
        let bar_tag = |part, words: Option<(SharedString, Hsla, f32, bool)>| {
            crate::morph::tag(
                side,
                crate::morph::app(),
                part,
                words.map(|(t, color, size, bold)| (t, crate::morph::Look { color, size, bold })),
            )
        };
        let headline_size = if mode == Mode::Panel { 16.0 } else { 18.0 };
        let titles = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .relative()
                    .when(mode == Mode::Panel, |d| d.text_base())
                    .when(mode == Mode::Window, |d| d.text_lg())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(headline_color)
                    .truncate()
                    .child(headline.clone())
                    .child(bar_tag(
                        crate::morph::Part::Headline,
                        Some((
                            headline.to_string().into(),
                            headline_color,
                            headline_size,
                            true,
                        )),
                    )),
            )
            .child(
                div()
                    .relative()
                    .text_xs()
                    .text_color(p.muted)
                    .truncate()
                    .child(subline.clone())
                    .child(bar_tag(
                        crate::morph::Part::Subline,
                        Some((subline.clone().into(), p.muted, 12.0, false)),
                    )),
            );

        let titles = if icons_only {
            let mut lines = vec![
                (headline.to_string(), headline_color),
                (subline.clone(), p.muted),
                (mode_text(&state.mode), p.muted),
            ];
            if let Some(c) = &state.capped {
                lines.push((format!("Repo list incomplete: {c}"), p.amber));
            }
            div()
                .id("summary")
                .flex()
                .flex_1()
                .items_center()
                .h(px(28.))
                .tooltip(tip(lines, &p))
                .child(summary_bar(&state.statuses, sweeping, &p))
                .into_any_element()
        } else {
            titles.into_any_element()
        };
        let header = div()
            .relative()
            .child(bar_tag(crate::morph::Part::Bar, None))
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(p.border)
            .child(titles);
        let header = match mode {
            Mode::Panel => header
                .child(
                    icon_button("refresh", "↻", !sweeping, &p)
                        .relative()
                        .child(bar_tag(
                            crate::morph::Part::Refresh,
                            Some(("↻".into(), p.text, 16.0, false)),
                        ))
                        .on_click(move |_, _, cx| {
                            if !sweeping {
                                refresh(cx);
                            }
                        }),
                )
                .when(state.config.window, |h| {
                    h.child(
                        icon_button("pop-out", "⧉", true, &p).on_click(|_, window, cx| {
                            crate::morph::open_window(window, None, cx);
                        }),
                    )
                })
                .child(
                    icon_button("settings", "⚙", true, &p)
                        .relative()
                        .child(bar_tag(
                            crate::morph::Part::Settings,
                            Some(("⚙".into(), p.text, 16.0, false)),
                        ))
                        .on_click(|_, window, cx| {
                            panel::dismiss(window, cx);
                            crate::settings::show(cx);
                        }),
                ),
            Mode::Window => header
                .child(
                    icon_button("settings", "⚙", true, &p)
                        .relative()
                        .child(bar_tag(
                            crate::morph::Part::Settings,
                            Some(("⚙".into(), p.text, 16.0, false)),
                        ))
                        .on_click(|_, _, cx| crate::settings::show(cx)),
                )
                .child(
                    div()
                        .id("refresh")
                        .flex_none()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .text_sm()
                        .bg(p.button)
                        .text_color(p.button_text)
                        .when(sweeping, |b| b.opacity(0.6))
                        .when(!sweeping, |b| b.cursor_pointer().hover(|s| s.opacity(0.9)))
                        .relative()
                        .child(if sweeping {
                            "Checking…"
                        } else {
                            "Refresh now"
                        })
                        .child(bar_tag(
                            crate::morph::Part::Refresh,
                            Some((
                                if sweeping {
                                    "Checking…"
                                } else {
                                    "Refresh now"
                                }
                                .into(),
                                p.button_text,
                                14.0,
                                false,
                            )),
                        ))
                        .on_click(move |_, _, cx| {
                            if !sweeping {
                                refresh(cx);
                            }
                        }),
                ),
        };

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
        let mut rows: Vec<AnyElement> = Vec::new();
        match mode {
            // Without timelines (never, once the app is up) a plain list.
            Mode::Window if timelines.is_none() => {
                for (i, s) in state.statuses.iter().enumerate() {
                    rows.push(repo_row(i, s, &p, mode, cx));
                }
            }
            Mode::Window => {}
            Mode::Panel => {
                let sorted = state.sorted_for_panel();
                let (bad, good): (Vec<&RepoStatus>, Vec<&RepoStatus>) =
                    sorted.into_iter().partition(|s| !s.is_synced());
                if !bad.is_empty() {
                    let open = !state.folded(Section::NeedsYou);
                    let label = if icons_only {
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .overflow_hidden()
                            .when(!open, |d| {
                                d.children(bad.iter().map(|s| {
                                    crate::repo_marks::global_mark(s, 16.0, status_color(s, &p), cx)
                                }))
                            })
                    } else {
                        div().flex_1().child(format!("● {} need you", bad.len()))
                    };
                    rows.push(
                        div()
                            .id("needs-you")
                            .bg(section_tint(Section::NeedsYou, &p))
                            .relative()
                            .child(seen_header(Section::NeedsYou))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .px_4()
                            .py(px(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(p.hover))
                            .text_sm()
                            .text_color(p.red)
                            .when(icons_only, |d| {
                                d.tooltip(tip(vec![(format!("{} need you", bad.len()), p.red)], &p))
                            })
                            .child(label)
                            .child(div().flex_none().text_color(p.muted).child(if open {
                                "▾"
                            } else {
                                "▸"
                            }))
                            .on_click(fold(Section::NeedsYou))
                            .into_any_element(),
                    );
                    if open {
                        for (i, s) in bad.iter().enumerate() {
                            rows.push(row(i, s, cx));
                        }
                    }
                }
                if !good.is_empty() {
                    let open = !state.folded(Section::Synced);
                    let label =
                        if icons_only {
                            // The synced repos' icons in a row, while collapsed.
                            div()
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .overflow_hidden()
                                .when(!open, |d| {
                                    d.children(good.iter().map(|s| {
                                        crate::repo_marks::global_mark(s, 16.0, p.green, cx)
                                    }))
                                })
                        } else {
                            div().flex_1().child(format!("✓ {} synced", good.len()))
                        };
                    rows.push(
                        div()
                            .id("synced")
                            .bg(section_tint(Section::Synced, &p))
                            .relative()
                            .child(seen_header(Section::Synced))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .px_4()
                            .py(px(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(p.hover))
                            .text_sm()
                            .text_color(p.green)
                            .when(icons_only, |d| {
                                d.tooltip(tip(
                                    vec![(format!("{} synced", good.len()), p.green)],
                                    &p,
                                ))
                            })
                            .child(label)
                            .child(div().flex_none().text_color(p.muted).child(if open {
                                "▾"
                            } else {
                                "▸"
                            }))
                            .on_click(fold(Section::Synced))
                            .into_any_element(),
                    );
                    if open {
                        let offset = bad.len();
                        for (i, s) in good.iter().enumerate() {
                            rows.push(row(offset + i, s, cx));
                        }
                    }
                }
                // The GitHub repos with no clone, last, folded like the
                // synced ones.
                let remote = state.sorted(&state.remote_statuses);
                if !remote.is_empty() {
                    let open = !state.folded(Section::GithubOnly);
                    let label =
                        if icons_only {
                            div()
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .overflow_hidden()
                                .when(!open, |d| {
                                    d.children(remote.iter().map(|s| {
                                        crate::repo_marks::global_mark(s, 16.0, p.muted, cx)
                                    }))
                                })
                        } else {
                            div()
                                .flex_1()
                                .child(format!("◌ {} on GitHub only", remote.len()))
                        };
                    rows.push(
                        div()
                            .id("remote")
                            .bg(section_tint(Section::GithubOnly, &p))
                            .relative()
                            .child(seen_header(Section::GithubOnly))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .px_4()
                            .py(px(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(p.hover))
                            .text_sm()
                            .text_color(p.muted)
                            .tooltip(tip(
                                vec![
                                    (
                                        format!("{} GitHub repos not cloned here", remote.len()),
                                        p.text,
                                    ),
                                    ("Their trees come from GitHub".to_string(), p.muted),
                                ],
                                &p,
                            ))
                            .child(label)
                            .child(div().flex_none().child(if open { "▾" } else { "▸" }))
                            .on_click(fold(Section::GithubOnly))
                            .into_any_element(),
                    );
                    if open {
                        let offset = bad.len() + good.len();
                        for (i, s) in remote.iter().enumerate() {
                            rows.push(row(offset + i, s, cx));
                        }
                    }
                } else if let Some(problem) = remote_problem {
                    rows.push(remote_problem_row(problem, &p));
                }
                rows.extend(pr_rows(
                    &open_prs,
                    pr_problem,
                    &p,
                    mode,
                    icons_only,
                    Some((
                        state.folded(Section::PullRequests),
                        fold(Section::PullRequests),
                    )),
                    cx,
                ));
            }
        }

        let empty = state.has_result && state.statuses.is_empty() && problem.is_none();
        let empty_text = match state.config.depth() {
            1 => "No git repos directly inside this folder. Set \"path\" or \"depth\" in \
                  config.json."
                .to_string(),
            depth => format!(
                "No git repos in this folder, down to {depth} levels below it. Set \"path\" or \
                 \"depth\" in config.json."
            ),
        };
        let table = match (&timelines, mode) {
            (Some(t), Mode::Window) => {
                // Each section's spine, then its repos' columns unless
                // folded; the GitHub repos with no clone come last.
                let cells = table_cells(state);
                let lefts = cell_lefts(&cells);
                let focused = focus.as_deref().and_then(|f| cell_of(&cells, f));
                if let Some(i) = focused {
                    self.table.scroll_to_item(i);
                }
                let shown = shown_columns(&self.table, window, &lefts, focused);
                let columns: Vec<AnyElement> = cells
                    .iter()
                    .enumerate()
                    .map(|(i, cell)| match *cell {
                        Cell::Spine(section, n) => spine(section, n, state.folded(section), &p),
                        Cell::Repo(s) if shown.contains(&i) => {
                            Timelines::column(t, i, s, &p, dark, window, cx)
                        }
                        Cell::Repo(_) => column_placeholder(&p),
                    })
                    .collect();
                Some(
                    div()
                        .id("table")
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_h_0()
                        .overflow_x_scroll()
                        .track_scroll(&self.table)
                        .bg(p.surface)
                        .children(columns),
                )
            }
            _ => None,
        };
        let list_h = content_h.clone();
        let list = div()
            .on_children_prepainted(move |rows, _, _| {
                let h = match (rows.first(), rows.last()) {
                    (Some(first), Some(last)) => f32::from(last.bottom()) - f32::from(first.top()),
                    _ => 0.0,
                };
                list_h.set(h);
            })
            .id("repos")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .bg(p.surface)
            .children(rows)
            .when(empty, |list| {
                list.child(div().p_4().text_sm().text_color(p.muted).child(empty_text))
            });

        // Big mode lists the pull requests under the columns, a few rows
        // high, scrolling when there are more.
        let window_prs = (mode == Mode::Window)
            .then(|| {
                let mut rows = pr_rows(
                    &open_prs,
                    pr_problem,
                    &p,
                    mode,
                    false,
                    Some((
                        state.folded(Section::PullRequests),
                        fold(Section::PullRequests),
                    )),
                    cx,
                );
                if state.remote_statuses.is_empty()
                    && let Some(problem) = remote_problem
                {
                    rows.push(remote_problem_row(problem, &p));
                }
                rows
            })
            .filter(|rows| !rows.is_empty())
            .map(|rows| {
                div()
                    .id("prs")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .max_h(px(WINDOW_PRS_HEIGHT))
                    .overflow_y_scroll()
                    .bg(p.surface)
                    .children(rows)
            });

        let config_file: PathBuf = state.config_file.clone();
        let edit_config = div()
            .id("config")
            .flex_none()
            .cursor_pointer()
            .hover(|s| s.text_color(p.text))
            .child("Edit config.json")
            .on_click(move |_, window, cx| {
                cx.open_with_system(&config_file);
                if mode == Mode::Panel {
                    panel::dismiss(window, cx);
                }
            });
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(p.border)
            .text_xs()
            .text_color(p.muted);
        // The cap message, e.g. "showing the first 500 repos found".
        let capped = state
            .capped
            .as_ref()
            .map(|c| format!("Repo list incomplete: {c}"));
        let footer = match mode {
            Mode::Panel => {
                let skips = state.folder_skips.len();
                footer
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(div().truncate().child(mode_text(&state.mode)))
                            .when_some(capped, |d, capped| {
                                d.child(div().truncate().text_color(p.amber).child(capped))
                            })
                            .when(skips > 0, |d| {
                                d.child(
                                    div()
                                        .truncate()
                                        .child(format!("Folder icons: {skips} skipped")),
                                )
                            }),
                    )
                    .child(edit_config)
            }
            Mode::Window => footer
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .when_some(capped.clone(), |d, _| d.text_color(p.amber))
                        .child(capped.unwrap_or_else(|| {
                            match state.config.repo_click {
                                crate::config::RepoClick::Folder => {
                                    "Click a repo's name to open its folder. Wheel over the \
                                     commits scrolls them, anywhere else the table."
                                }
                                crate::config::RepoClick::Github => {
                                    "Click a repo's name to open it on GitHub. Wheel over the \
                                     commits scrolls them, anywhere else the table."
                                }
                            }
                            .into()
                        })),
                )
                .child(edit_config),
        };

        div()
            .when(mode == Mode::Panel, |d| {
                // Header, rows, footer: grow or shrink the panel to them
                // whenever they change (a section folded, repos or pull
                // requests come or go), with no empty space below.
                d.on_children_prepainted(move |parts, _, cx| {
                    let header = parts.first().map_or(0.0, |b| f32::from(b.size.height));
                    let footer = if icons_only {
                        0.0
                    } else {
                        parts.get(2).map_or(0.0, |b| f32::from(b.size.height))
                    };
                    panel::fit(header + content_h.get() + footer, cx);
                })
            })
            .id("root")
            .track_focus(&self.focus)
            .key_context("StatusView")
            .on_key_down(move |event, window, cx| {
                if event.keystroke.key == "escape" {
                    match mode {
                        Mode::Panel => panel::dismiss(window, cx),
                        Mode::Window => {
                            if !crate::morph::flip_back(window, cx) {
                                window.remove_window();
                            }
                        }
                    }
                }
            })
            .flex()
            .flex_col()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .child(sideways(header, mode, cx))
            .map(|d| match table {
                Some(table) if !empty => d.child(table),
                _ => d.child(list),
            })
            .children(window_prs)
            .when(!icons_only, |d| d.child(sideways(footer, mode, cx)))
            .children(
                self.timelines
                    .as_ref()
                    .and_then(|t| t.read(cx).hover_card(&p, dark, cx)),
            )
    }
}

/// In the window, a wheel over `part` (the header or the footer) scrolls the
/// table sideways, as it does over the columns' headers; only over a
/// column's commits does it scroll up and down.
fn sideways(part: impl IntoElement, mode: Mode, cx: &mut Context<StatusView>) -> AnyElement {
    if mode != Mode::Window {
        return part.into_any_element();
    }
    div()
        .flex_none()
        .child(part)
        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, window, cx| {
            let d = e.delta.pixel_delta(window.line_height());
            let dx = if d.x.abs() > d.y.abs() { d.x } else { d.y };
            let table = &this.table;
            let o = table.offset();
            let max = table.max_offset().width;
            let x = (o.x + dx).clamp(-max, px(0.));
            if x != o.x {
                table.set_offset(point(x, o.y));
                cx.notify();
            }
        }))
        .into_any_element()
}

/// A bar of the repos' tones: green synced, amber local work, red needs
/// action, each as wide as its share; dimmed while a check runs.
fn summary_bar(statuses: &[RepoStatus], checking: bool, p: &Palette) -> gpui::Div {
    let mut counts = [0usize; 3];
    for s in statuses {
        let i = match crate::icon::FolderState::of(s).tone() {
            crate::icon::Tone::Synced => 0,
            crate::icon::Tone::Local => 1,
            crate::icon::Tone::Attention => 2,
        };
        counts[i] += 1;
    }
    let total = statuses.len().max(1) as f32;
    let bar = div()
        .flex()
        .flex_1()
        .h(px(6.))
        .rounded_full()
        .overflow_hidden()
        .bg(p.hover)
        .when(checking, |b| b.opacity(0.6));
    [
        (counts[0], p.green),
        (counts[1], p.amber),
        (counts[2], p.red),
    ]
    .into_iter()
    .filter(|&(n, _)| n > 0)
    .fold(bar, |bar, (n, color)| {
        bar.child(div().h_full().w(gpui::relative(n as f32 / total)).bg(color))
    })
}

/// A tooltip of coloured lines.
/// The tooltip's widest, and the characters of `text_xs` that fit in it.
const TIP_WIDTH: f32 = 320.;
const TIP_LINE_CHARS: usize = 44;

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

/// A tooltip builder showing `lines`, for icons-only mode.
pub(crate) fn tip(
    lines: Vec<(String, Hsla)>,
    p: &Palette,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let lines: Vec<(SharedString, Hsla)> = lines.into_iter().map(|(t, c)| (t.into(), c)).collect();
    let (bg, border) = (p.bg, p.border);
    move |_, cx| {
        let lines = lines.clone();
        cx.new(|_| Tip { lines, bg, border }).into()
    }
}

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
