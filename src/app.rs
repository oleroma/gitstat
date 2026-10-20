//! The GPUI side: shared state, the refresh loop, and the status window.

use crate::config::Config;
use crate::git::{self, RepoStatus};
use crate::tray::{Tray, TrayAction, TraySnapshot};
use gpui::{
    App, AppContext, Application, Bounds, Context, Entity, Global, Hsla, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, WindowAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, point, prelude::*, px, rgb, size,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const POLL_EVERY: Duration = Duration::from_millis(100);
const WINDOW_SIZE: (f32, f32) = (420.0, 520.0);

pub struct AppState {
    config_file: PathBuf,
    config: Config,
    config_error: Option<String>,
    statuses: Vec<RepoStatus>,
    has_result: bool,
    checking: bool,
    last_checked: Option<String>,
    next_check: Instant,
}

impl AppState {
    fn new(config_file: PathBuf) -> Self {
        let mut state = Self {
            config_file,
            config: Config::default(),
            config_error: None,
            statuses: Vec::new(),
            has_result: false,
            checking: false,
            last_checked: None,
            next_check: Instant::now(),
        };
        state.reload_config();
        state
    }

    /// Re-reads config.json so edits apply on the next check without a restart.
    fn reload_config(&mut self) {
        match Config::load_or_create(&self.config_file) {
            Ok(config) => {
                self.config = config;
                self.config_error = None;
            }
            Err(e) => self.config_error = Some(format!("config.json: {e}")),
        }
    }

    fn unsynced(&self) -> usize {
        self.statuses.iter().filter(|s| !s.is_synced()).count()
    }

    /// A problem worth showing instead of the repo summary.
    fn problem(&self) -> Option<String> {
        if let Some(e) = &self.config_error {
            return Some(e.clone());
        }
        let dir = self.config.repos_dir();
        if self.has_result && !dir.is_dir() {
            return Some(format!("folder not found: {}", dir.display()));
        }
        None
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.reload_config();
        self.checking = true;
        cx.notify();
        let dir = self.config.repos_dir();
        let work = cx
            .background_executor()
            .spawn(async move { git::check_all(&dir, true) });
        cx.spawn(async move |this, cx| {
            let statuses = work.await;
            this.update(cx, |state, cx| {
                state.statuses = statuses;
                state.has_result = true;
                state.checking = false;
                state.last_checked = Some(local_time());
                state.next_check = Instant::now() + state.config.interval();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

struct Shared {
    state: Entity<AppState>,
    tray: Tray,
    window: Option<WindowHandle<StatusView>>,
}

impl Global for Shared {}

pub fn run(config_file: PathBuf) {
    Application::new().run(move |cx: &mut App| {
        let tray = match Tray::new() {
            Ok(tray) => tray,
            Err(e) => {
                crate::error_box(&format!("Couldn't create the tray icon: {e}"));
                cx.quit();
                return;
            }
        };

        // GPUI quits when its last window closes. A window that is never shown
        // keeps the app alive while the status window is closed.
        cx.open_window(
            WindowOptions {
                show: false,
                focus: false,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1.), px(1.)),
                ))),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Keepalive),
        )
        .ok();

        let state = cx.new(|_| AppState::new(config_file));
        cx.observe(&state, |state, cx| {
            let s = state.read(cx);
            let (statuses, checking, has_result, problem) =
                (s.statuses.clone(), s.checking, s.has_result, s.problem());
            cx.update_global::<Shared, _>(|shared, _| {
                shared.tray.update(&TraySnapshot {
                    statuses: &statuses,
                    checking,
                    has_result,
                    problem: problem.as_deref(),
                })
            });
        })
        .detach();
        cx.set_global(Shared {
            state: state.clone(),
            tray,
            window: None,
        });

        // Tray events arrive on channels; poll them, and kick off checks when due.
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL_EVERY).await;
                let keep_going = cx.update(tick).unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    });
}

/// One pass of the main loop. Returns false once the app is quitting.
fn tick(cx: &mut App) -> bool {
    let shared = cx.global::<Shared>();
    let state = shared.state.clone();
    let actions = shared.tray.poll();
    for action in actions {
        match action {
            TrayAction::ShowWindow => show_window(cx),
            TrayAction::Refresh => state.update(cx, |s, cx| s.refresh(cx)),
            TrayAction::OpenRepo(path) => cx.open_with_system(&path),
            TrayAction::SetAutostart(enabled) => {
                if let Err(e) = crate::autostart::set_enabled(enabled) {
                    crate::error_box(&format!("Couldn't change the startup setting: {e}"));
                }
                cx.global::<Shared>()
                    .tray
                    .set_autostart_checked(crate::autostart::is_enabled());
            }
            TrayAction::Quit => {
                cx.quit();
                return false;
            }
        }
    }
    if Instant::now() >= state.read(cx).next_check {
        state.update(cx, |s, cx| s.refresh(cx));
    }
    true
}

fn show_window(cx: &mut App) {
    if let Some(handle) = cx.global::<Shared>().window
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let (width, height) = (px(WINDOW_SIZE.0), px(WINDOW_SIZE.1));
    // Bottom-right of the primary display, above where the taskbar usually sits.
    let bounds = match cx.primary_display() {
        Some(display) => {
            let d = display.bounds();
            let origin = point(
                d.origin.x + d.size.width - width - px(16.),
                d.origin.y + d.size.height - height - px(64.),
            );
            Bounds::new(origin, size(width, height))
        }
        None => Bounds::centered(None, size(width, height), cx),
    };
    let state = cx.global::<Shared>().state.clone();
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
                window.activate_window();
                cx.new(|cx| StatusView::new(state, cx))
            },
        )
        .ok();
    cx.global_mut::<Shared>().window = handle;
}

struct Keepalive;

impl Render for Keepalive {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct StatusView {
    state: Entity<AppState>,
}

impl StatusView {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

struct Palette {
    bg: Hsla,
    surface: Hsla,
    hover: Hsla,
    border: Hsla,
    text: Hsla,
    muted: Hsla,
    green: Hsla,
    red: Hsla,
    amber: Hsla,
    button: Hsla,
    button_text: Hsla,
}

impl Palette {
    fn for_appearance(appearance: WindowAppearance) -> Self {
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

impl Render for StatusView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_appearance(window.appearance());
        let state = self.state.read(cx);
        let unsynced = state.unsynced();
        let problem = state.problem();
        let headline: SharedString = crate::tray::summary(
            &TraySnapshot {
                statuses: &state.statuses,
                checking: state.checking,
                has_result: state.has_result,
                problem: problem.as_deref(),
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
        if state.checking {
            subline.push_str(" · checking…");
        } else if let Some(time) = &state.last_checked {
            subline.push_str(&format!(" · checked at {time}"));
        }

        let entity = self.state.clone();
        let refresh = div()
            .id("refresh")
            .px_3()
            .py_1()
            .rounded_md()
            .text_sm()
            .bg(p.button)
            .text_color(p.button_text)
            .when(state.checking, |b| b.opacity(0.6))
            .when(!state.checking, |b| {
                b.cursor_pointer().hover(|s| s.opacity(0.9))
            })
            .child(if state.checking {
                "Checking…"
            } else {
                "Refresh now"
            })
            .on_click(move |_, _, cx| entity.update(cx, |s, cx| s.refresh(cx)));

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(headline_color)
                            .child(headline),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(p.muted)
                            .truncate()
                            .child(subline),
                    ),
            )
            .child(refresh);

        let rows = state.statuses.iter().enumerate().map(|(i, s)| {
            let color = if s.error.is_some() || s.is_diverged() || s.behind > 0 {
                p.red
            } else if !s.is_synced() {
                p.amber
            } else {
                p.green
            };
            let path = s.path.clone();
            div()
                .id(("repo", i))
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .cursor_pointer()
                .hover(|style| style.bg(p.hover))
                .on_click(move |_, _, cx| cx.open_with_system(&path))
                .child(div().size_2().rounded_full().bg(color).flex_none())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(p.text)
                        .child(s.name.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_sm()
                        .text_color(color)
                        .child(s.label()),
                )
        });

        let empty = state.has_result && state.statuses.is_empty() && problem.is_none();
        let list = div()
            .id("repos")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            .bg(p.surface)
            .children(rows)
            .when(empty, |list| {
                list.child(div().p_4().text_sm().text_color(p.muted).child(
                    "No git repos directly inside this folder. Set \"path\" in config.json.",
                ))
            });

        let config_file = self.state.read(cx).config_file.clone();
        let footer = div()
            .flex()
            .justify_between()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(p.border)
            .text_xs()
            .text_color(p.muted)
            .child("Click a repo to open its folder")
            .child(
                div()
                    .id("config")
                    .cursor_pointer()
                    .hover(|s| s.text_color(p.text))
                    .child("Edit config.json")
                    .on_click(move |_, _, cx| cx.open_with_system(&config_file)),
            );

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .child(header)
            .child(list)
            .child(footer)
    }
}

/// The current local time as HH:MM.
fn local_time() -> String {
    let mut t = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}
