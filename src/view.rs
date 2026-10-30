//! The status list, shared by the flyout panel and the normal window, plus
//! the normal window itself (`show_window`, `close_window`).

use crate::git::RepoStatus;
use crate::monitor::WatchMode;
use crate::panel;
use crate::state::{AppState, Input};
use crate::timeline::Timelines;
use crate::tray::TraySnapshot;
use crate::win;
use futures::channel::mpsc::UnboundedSender;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle, Global, Hsla, IntoElement,
    ParentElement, Render, ScrollHandle, SharedString, Styled, Window, WindowAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point, prelude::*, px, rgb, size,
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
    let mon = win::primary_monitor();
    let scale = sane_scale(mon.scale);
    let (left, top) = (mon.work.left as f32 / scale, mon.work.top as f32 / scale);
    let (work_w, work_h) = (
        mon.work.width() as f32 / scale,
        mon.work.height() as f32 / scale,
    );
    // Big mode: wide enough for every column, up to most of the screen.
    let repos = state.read(cx).statuses.len().max(1) as f32;
    let width = (repos * crate::timeline::COLUMN_WIDTH + 2.0)
        .min(work_w * 0.9)
        .max(WINDOW_SIZE.0);
    let height = (work_h * 0.8).max(WINDOW_SIZE.1).min(work_h);
    let x = (mon.work.right as f32 / scale - width - WINDOW_MARGIN).max(left);
    let y = (mon.work.bottom as f32 / scale - height - WINDOW_MARGIN).max(top);
    let bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(height)));
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
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
                cx.new(|cx| StatusView::new(state, Mode::Window, window, cx))
            },
        )
        .ok();
    cx.default_global::<WindowSlot>().handle = handle;
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
    show_synced: bool,
    /// Big mode's table of columns, scrolled sideways.
    table: ScrollHandle,
    focus: FocusHandle,
    /// The panel has been active once, so a deactivation now means the user
    /// clicked elsewhere rather than the foreground lock refusing us.
    seen_active: bool,
}

impl StatusView {
    pub fn new(
        state: Entity<AppState>,
        mode: Mode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
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
            show_synced: false,
            table: ScrollHandle::new(),
            focus,
            seen_active: false,
        }
    }
}

/// How many rows the panel shows before "N synced" is expanded: the
/// unsynced repos, the collapsed synced row, or the one empty-state line.
pub fn panel_rows(state: &AppState) -> usize {
    let unsynced = state.unsynced();
    let synced = state.statuses.len() - unsynced;
    (unsynced + usize::from(synced > 0)).max(1)
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
    if s.error.is_some() || s.is_diverged() || s.behind > 0 {
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
            cx.open_with_system(&path);
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
        let mut subline = state.config.repos_dir().display().to_string();
        if let Some(time) = &state.last_checked {
            subline.push_str(&format!(" · checked {time}"));
        }
        if sweeping {
            subline.push_str(" · checking…");
        }

        let titles = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .when(mode == Mode::Panel, |d| d.text_base())
                    .when(mode == Mode::Window, |d| d.text_lg())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(headline_color)
                    .truncate()
                    .child(headline.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(p.muted)
                    .truncate()
                    .child(subline.clone()),
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
                    icon_button("refresh", "↻", !sweeping, &p).on_click(move |_, _, cx| {
                        if !sweeping {
                            refresh(cx);
                        }
                    }),
                )
                .when(state.config.window, |h| {
                    h.child(
                        icon_button("pop-out", "⧉", true, &p).on_click(|_, window, cx| {
                            panel::dismiss(window, cx);
                            show_window(cx);
                        }),
                    )
                }),
            Mode::Window => header.child(
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
                    .child(if sweeping {
                        "Checking…"
                    } else {
                        "Refresh now"
                    })
                    .on_click(move |_, _, cx| {
                        if !sweeping {
                            refresh(cx);
                        }
                    }),
            ),
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
                for (i, s) in bad.iter().enumerate() {
                    rows.push(row(i, s, cx));
                }
                if !good.is_empty() {
                    let open = self.show_synced;
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
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_synced = !this.show_synced;
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                    if open {
                        let offset = bad.len();
                        for (i, s) in good.iter().enumerate() {
                            rows.push(row(offset + i, s, cx));
                        }
                    }
                }
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
                if let Some(i) = focus
                    .as_deref()
                    .and_then(|f| crate::timeline::column_index(&state.statuses, f))
                {
                    self.table.scroll_to_item(i);
                }
                let columns: Vec<AnyElement> = state
                    .statuses
                    .iter()
                    .enumerate()
                    .map(|(i, s)| Timelines::column(t, i, s, &p, dark, window, cx))
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
        let list = div()
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
                            "Click a repo's name to open its folder. Wheel scrolls a column, \
                             Shift+wheel the table."
                                .into()
                        })),
                )
                .child(edit_config),
        };

        div()
            .id("root")
            .track_focus(&self.focus)
            .key_context("StatusView")
            .on_key_down(move |event, window, cx| {
                if event.keystroke.key == "escape" {
                    match mode {
                        Mode::Panel => panel::dismiss(window, cx),
                        Mode::Window => window.remove_window(),
                    }
                }
            })
            .flex()
            .flex_col()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .child(header)
            .map(|d| match table {
                Some(table) if !empty => d.child(table),
                _ => d.child(list),
            })
            .when(!icons_only, |d| d.child(footer))
            .children(
                self.timelines
                    .as_ref()
                    .and_then(|t| t.read(cx).hover_card(&p, dark, cx)),
            )
    }
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
struct Tip {
    lines: Vec<(SharedString, Hsla)>,
    bg: Hsla,
    border: Hsla,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .max_w(px(320.))
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
