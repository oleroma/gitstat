//! The settings window: every option, grouped into a few pages, each change
//! saved to config.json at once. The tray menu keeps only what's used day to
//! day. Changes go through the app's input channel as `TrayAction`s, the same
//! path the tray takes, and the window redraws from the state, so a hand edit
//! of config.json shows here too.

use crate::config::{BadgeNumber, Config, Corner, FolderIconMode, RepoClick, RepoSort};
use crate::state::{AppState, Input};
use crate::tray::TrayAction;
use crate::view::{self, Palette, Ui};
use crate::win;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Div, Entity, FontWeight, Global, IntoElement,
    ParentElement, PathPromptOptions, Render, SharedString, Styled, Subscription, Window,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point, prelude::*, px, size,
};

/// The window's opening size in logical px.
const WINDOW_SIZE: (f32, f32) = (780.0, 600.0);

/// The on/off settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Watch,
    Panel,
    Window,
    IconsOnly,
    Overlay,
    OverlayHideSynced,
    OverlayHideFullscreen,
    FolderIcons,
    GithubPrs,
    GithubRepos,
    ScrollTogether,
    FlipAnimation,
}

impl Setting {
    pub fn is_on(self, cfg: &Config) -> bool {
        match self {
            Setting::Watch => cfg.watch,
            Setting::Panel => cfg.panel,
            Setting::Window => cfg.window,
            Setting::IconsOnly => cfg.icons_only,
            Setting::Overlay => cfg.overlay.enabled,
            Setting::OverlayHideSynced => cfg.overlay.hide_when_synced,
            Setting::OverlayHideFullscreen => cfg.overlay.hide_in_fullscreen,
            Setting::FolderIcons => cfg.folder_icons,
            Setting::GithubPrs => cfg.github_prs,
            Setting::GithubRepos => cfg.github_repos,
            Setting::ScrollTogether => cfg.scroll_together,
            Setting::FlipAnimation => cfg.flip_animation,
        }
    }

    pub fn toggle(self, cfg: &mut Config) {
        let field = match self {
            Setting::Watch => &mut cfg.watch,
            Setting::Panel => &mut cfg.panel,
            Setting::Window => &mut cfg.window,
            Setting::IconsOnly => &mut cfg.icons_only,
            Setting::Overlay => &mut cfg.overlay.enabled,
            Setting::OverlayHideSynced => &mut cfg.overlay.hide_when_synced,
            Setting::OverlayHideFullscreen => &mut cfg.overlay.hide_in_fullscreen,
            Setting::FolderIcons => &mut cfg.folder_icons,
            Setting::GithubPrs => &mut cfg.github_prs,
            Setting::GithubRepos => &mut cfg.github_repos,
            Setting::ScrollTogether => &mut cfg.scroll_together,
            Setting::FlipAnimation => &mut cfg.flip_animation,
        };
        *field = !*field;
    }
}

/// The pages, in the sidebar's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    General,
    Views,
    Badges,
    FolderIcons,
    Github,
}

const PAGES: [(Page, &str, &str); 5] = [
    (Page::General, "⚙", "General"),
    (Page::Views, "▤", "Status views"),
    (Page::Badges, "◉", "Icon and badge"),
    (Page::FolderIcons, "▣", "Folder icons"),
    (Page::Github, "⑂", "GitHub"),
];

const SORTS: [(RepoSort, &str, &str); 7] = [
    (RepoSort::Name, "Name", ""),
    (
        RepoSort::Status,
        "Status",
        "Worst first: errors, diverged, behind, ahead, dirty",
    ),
    (
        RepoSort::LastCommit,
        "Last commit",
        "Newest commit on any branch first",
    ),
    (
        RepoSort::LastWorked,
        "Last worked on",
        "Where HEAD moved last first",
    ),
    (RepoSort::LastPullRequest, "Last pull request activity", ""),
    (
        RepoSort::OpenPullRequests,
        "Most open pull requests",
        "Those needing you count first",
    ),
    (RepoSort::HangingBranches, "Most hanging branches", ""),
];

const BADGE_NUMBERS: [(BadgeNumber, &str, &str); 7] = [
    (
        BadgeNumber::Unsynced,
        "Unsynced repos",
        "Dirty, ahead, behind, no upstream or erroring",
    ),
    (BadgeNumber::Dirty, "Repos with uncommitted changes", ""),
    (BadgeNumber::Ahead, "Repos to push", ""),
    (BadgeNumber::Behind, "Repos to pull", ""),
    (BadgeNumber::PullRequests, "Open pull requests", ""),
    (
        BadgeNumber::PullRequestsForYou,
        "Pull requests needing you",
        "A review asked of you, failing checks, conflicts or requested changes",
    ),
    (BadgeNumber::ReviewRequests, "Reviews asked of you", ""),
];

const CORNERS: [(Corner, &str); 4] = [
    (Corner::TopLeft, "Top left"),
    (Corner::TopRight, "Top right"),
    (Corner::BottomLeft, "Bottom left"),
    (Corner::BottomRight, "Bottom right"),
];

/// The check intervals the − and + buttons step through, in minutes.
const INTERVALS: [f64; 14] = [
    1.0, 2.0, 3.0, 5.0, 10.0, 15.0, 20.0, 30.0, 60.0, 120.0, 240.0, 480.0, 720.0, 1440.0,
];

#[derive(Default)]
struct SettingsSlot {
    handle: Option<WindowHandle<SettingsView>>,
}

impl Global for SettingsSlot {}

/// Brings the settings window to the front, opening it in the middle of the
/// primary work area if it isn't open.
pub fn show(cx: &mut App) {
    if let Some(handle) = cx.try_global::<SettingsSlot>().and_then(|s| s.handle)
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
    let scale = view::sane_scale(mon.scale);
    let (work_w, work_h) = (
        mon.work.width() as f32 / scale,
        mon.work.height() as f32 / scale,
    );
    let (w, h) = (WINDOW_SIZE.0.min(work_w), WINDOW_SIZE.1.min(work_h));
    let x = mon.work.left as f32 / scale + (work_w - w) / 2.0;
    let y = mon.work.top as f32 / scale + (work_h - h) / 2.0;
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(x), px(y)),
                    size(px(w), px(h)),
                ))),
                kind: WindowKind::Normal,
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("gitstat settings".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(560.), px(400.))),
                ..Default::default()
            },
            |window, cx| {
                window.activate_window();
                cx.new(|cx| SettingsView::new(state, cx))
            },
        )
        .ok();
    cx.default_global::<SettingsSlot>().handle = handle;
}

/// Rereads "Start with Windows", which lives in the registry, not the config.
pub fn autostart_changed(cx: &mut App) {
    if let Some(handle) = cx.try_global::<SettingsSlot>().and_then(|s| s.handle) {
        handle
            .update(cx, |view, _, cx| {
                view.autostart = crate::autostart::is_enabled();
                cx.notify();
            })
            .ok();
    }
}

fn send(action: TrayAction, cx: &mut App) {
    if let Some(ui) = cx.try_global::<Ui>() {
        let _ = ui.input.unbounded_send(Input::Action(action));
    }
}

pub struct SettingsView {
    state: Entity<AppState>,
    page: Page,
    autostart: bool,
    _observe: Subscription,
}

impl SettingsView {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            page: Page::General,
            autostart: crate::autostart::is_enabled(),
            _observe: observe,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_appearance(window.appearance());
        let (cfg, config_error, github_problem) = {
            let s = self.state.read(cx);
            (
                s.config.clone(),
                s.config_error.clone(),
                s.github_problem.clone(),
            )
        };

        let nav = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(220.))
            .h_full()
            .px_3()
            .pt_5()
            .pb_4()
            .gap_1()
            .child(
                div()
                    .px_3()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("gitstat"),
                    )
                    .child(div().text_xs().text_color(p.muted).child("Settings")),
            )
            .children(PAGES.iter().map(|&(page, glyph, label)| {
                let on = page == self.page;
                div()
                    .id(label)
                    .flex()
                    .items_center()
                    .gap_3()
                    .h(px(36.))
                    .pr_3()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.hover))
                    .when(!on, |d| d.hover(|s| s.bg(p.hover.opacity(0.6))))
                    .child(
                        // The selected page's accent bar.
                        div()
                            .w(px(3.))
                            .h(px(16.))
                            .rounded_full()
                            .when(on, |d| d.bg(p.button)),
                    )
                    .child(
                        div()
                            .w(px(18.))
                            .flex()
                            .justify_center()
                            .text_color(if on { p.button } else { p.muted })
                            .child(glyph),
                    )
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }))
                    .into_any_element()
            }))
            .child(div().flex_1())
            .child(
                div()
                    .id("edit-config")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .text_xs()
                    .text_color(p.muted)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover).text_color(p.text))
                    .child("Edit config.json…")
                    .on_click(|_, _, cx| send(TrayAction::EditConfig, cx)),
            )
            .child(
                div()
                    .px_3()
                    .text_xs()
                    .text_color(p.muted.opacity(0.7))
                    .child(concat!("gitstat ", env!("CARGO_PKG_VERSION"))),
            );

        let (title, body) = match self.page {
            Page::General => ("General", self.general(&cfg, &p)),
            Page::Views => ("Status views", views(&cfg, &p)),
            Page::Badges => ("Icon and badge", badges(&cfg, &p)),
            Page::FolderIcons => ("Folder icons", folder_icons(&cfg, &p)),
            Page::Github => ("GitHub", github(&cfg, github_problem.as_deref(), &p)),
        };
        let content = div()
            .id("content")
            .flex_1()
            .h_full()
            .min_w_0()
            .overflow_y_scroll()
            .border_l_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .px_8()
                    .pt_6()
                    .pb_8()
                    .max_w(px(720.))
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when_some(config_error, |d, e| d.child(config_banner(&e, &p)))
                    .children(body),
            );

        div()
            .flex()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .child(nav)
            .child(content)
    }
}

impl SettingsView {
    fn general(&self, cfg: &Config, p: &Palette) -> Vec<AnyElement> {
        let mut folders: Vec<AnyElement> = std::iter::once(&cfg.path)
            .chain(&cfg.more_paths)
            .enumerate()
            .map(|(i, path)| folder_row(i, path, p))
            .collect();
        folders.push(
            row_base()
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child("Repos are looked for in each folder and the folders below it."),
                )
                .child(
                    text_button("add-folder", "Add folder…", p).on_click(|_, _, cx| {
                        pick_folder(None, cx);
                    }),
                )
                .into_any_element(),
        );

        let minutes = cfg.interval().as_secs_f64() / 60.0;
        let depth = cfg.depth() as i64;
        let max_depth = crate::git::MAX_DEPTH as i64;
        vec![
            group(
                "Startup",
                vec![toggle_row(
                    "autostart",
                    "Start with Windows",
                    "Run gitstat in the tray when you sign in.",
                    self.autostart,
                    true,
                    TrayAction::SetAutostart(!self.autostart),
                    p,
                )],
                p,
            ),
            group("Repo folders", folders, p),
            group(
                "Checking",
                vec![
                    setting_row(
                        "Folder depth",
                        "How many levels below each folder to look for repos.",
                        stepper(
                            "depth",
                            if depth == 1 {
                                "1 level".into()
                            } else {
                                format!("{depth} levels")
                            },
                            (depth > 1).then(|| TrayAction::SetDepth(depth - 1)),
                            (depth < max_depth).then(|| TrayAction::SetDepth(depth + 1)),
                            p,
                        ),
                        p,
                    ),
                    flag(
                        "watch",
                        "Watch for changes",
                        "Update a repo as soon as its files change. Off: only on the interval.",
                        Setting::Watch,
                        true,
                        cfg,
                        p,
                    ),
                    setting_row(
                        "Check every",
                        "How often to fetch from the remotes and check every repo.",
                        stepper(
                            "interval",
                            interval_text(minutes),
                            INTERVALS
                                .iter()
                                .rev()
                                .find(|&&m| m < minutes - 0.01)
                                .map(|&m| TrayAction::SetInterval(m)),
                            INTERVALS
                                .iter()
                                .find(|&&m| m > minutes + 0.01)
                                .map(|&m| TrayAction::SetInterval(m)),
                            p,
                        ),
                        p,
                    ),
                ],
                p,
            ),
        ]
    }
}

fn views(cfg: &Config, p: &Palette) -> Vec<AnyElement> {
    let sorts = SORTS
        .iter()
        .enumerate()
        .map(|(i, &(s, label, desc))| {
            radio_row(
                ("sort", i),
                label,
                desc,
                s == cfg.sort,
                TrayAction::SetSort(s),
                p,
            )
        })
        .collect();
    vec![
        group(
            "Views",
            vec![
                flag(
                    "panel",
                    "Status panel",
                    "Clicking the tray icon opens a panel above it.",
                    Setting::Panel,
                    true,
                    cfg,
                    p,
                ),
                flag(
                    "window",
                    "Status window",
                    "A normal window with every repo's history side by side.",
                    Setting::Window,
                    true,
                    cfg,
                    p,
                ),
                flag(
                    "icons-only",
                    "Icons only",
                    "The panel and the badge show pictures only; the words move to tooltips.",
                    Setting::IconsOnly,
                    true,
                    cfg,
                    p,
                ),
                flag(
                    "scroll-together",
                    "Scroll columns together",
                    "The window's columns scroll row for row, with lines between the rows.",
                    Setting::ScrollTogether,
                    cfg.window,
                    cfg,
                    p,
                ),
                setting_row(
                    "Panel slides in",
                    "From the taskbar's side, or the badge's, when the panel opens.",
                    segmented(
                        "panel-slide",
                        [(0, "Off"), (100, "Fast"), (150, "Normal"), (300, "Slow")]
                            .into_iter()
                            .map(|(ms, label)| {
                                (
                                    label,
                                    cfg.panel_slide_ms == ms,
                                    TrayAction::SetPanelSlide(ms),
                                )
                            })
                            .collect(),
                        cfg.panel,
                        p,
                    ),
                    p,
                ),
                flag(
                    "flip-animation",
                    "Flip animation",
                    "Opening the window from the panel turns each repo's strip into its column.",
                    Setting::FlipAnimation,
                    cfg.window && cfg.panel,
                    cfg,
                    p,
                ),
                setting_row(
                    "Flip speed",
                    "How long the panel takes to turn into the window, and back.",
                    segmented(
                        "flip-speed",
                        [(350, "Fast"), (560, "Normal"), (900, "Slow")]
                            .into_iter()
                            .map(|(ms, label)| {
                                (label, cfg.flip_ms == ms, TrayAction::SetFlipSpeed(ms))
                            })
                            .collect(),
                        cfg.window && cfg.panel && cfg.flip_animation,
                        p,
                    ),
                    p,
                ),
                setting_row(
                    "Clicking a repo opens",
                    "A repo not on GitHub always opens its folder.",
                    segmented(
                        "repo-click",
                        vec![
                            (
                                "Folder",
                                cfg.repo_click == RepoClick::Folder,
                                TrayAction::SetRepoClick(RepoClick::Folder),
                            ),
                            (
                                "GitHub page",
                                cfg.repo_click == RepoClick::Github,
                                TrayAction::SetRepoClick(RepoClick::Github),
                            ),
                        ],
                        true,
                        p,
                    ),
                    p,
                ),
            ],
            p,
        ),
        group("Sort repos by", sorts, p),
    ]
}

fn badges(cfg: &Config, p: &Palette) -> Vec<AnyElement> {
    let numbers = BADGE_NUMBERS
        .iter()
        .enumerate()
        .map(|(i, &(n, label, desc))| {
            radio_row(
                ("badge-number", i),
                label,
                desc,
                n == cfg.badge_number,
                TrayAction::SetBadgeNumber(n),
                p,
            )
        })
        .collect();
    let on = cfg.overlay.enabled;
    let corners = CORNERS
        .iter()
        .map(|&(c, label)| (label, c == cfg.overlay.corner, TrayAction::SetCorner(c)))
        .collect();
    vec![
        group(
            "Corner badge",
            vec![
                toggle_row(
                    "overlay",
                    "Show the corner badge",
                    "A round badge in a corner of the screen. Drag it to move it.",
                    on,
                    true,
                    TrayAction::Toggle(Setting::Overlay),
                    p,
                ),
                setting_row(
                    "Corner",
                    "Picking a corner puts a dragged badge back in it.",
                    segmented("corner", corners, on, p),
                    p,
                )
                .into_any_element(),
                flag(
                    "hide-synced",
                    "Hide when all synced",
                    "",
                    Setting::OverlayHideSynced,
                    on,
                    cfg,
                    p,
                ),
                flag(
                    "hide-fullscreen",
                    "Hide in fullscreen",
                    "While a fullscreen app or game is in front.",
                    Setting::OverlayHideFullscreen,
                    on,
                    cfg,
                    p,
                ),
            ],
            p,
        ),
        group_with_note(
            "Number on the icon and badge",
            "Their colour always follows sync: red while any repo is unsynced, else green.",
            numbers,
            p,
        ),
    ]
}

fn folder_icons(cfg: &Config, p: &Palette) -> Vec<AnyElement> {
    let on = cfg.folder_icons;
    vec![group(
        "Explorer",
        vec![
            toggle_row(
                "folder-icons",
                "Folder icons in Explorer",
                "Show each repo's status as its folder icon. Asks first: it writes a hidden \
                 desktop.ini into each repo folder.",
                on,
                true,
                TrayAction::Toggle(Setting::FolderIcons),
                p,
            ),
            setting_row(
                "Style",
                "Repo icon uses the repo's own .gitstat/icon.svg where it has one.",
                segmented(
                    "folder-icon-mode",
                    vec![
                        (
                            "Status folder",
                            cfg.folder_icon_mode == FolderIconMode::Status,
                            TrayAction::SetFolderIconMode(FolderIconMode::Status),
                        ),
                        (
                            "Repo icon",
                            cfg.folder_icon_mode == FolderIconMode::RepoSvg,
                            TrayAction::SetFolderIconMode(FolderIconMode::RepoSvg),
                        ),
                    ],
                    on,
                    p,
                ),
                p,
            ),
        ],
        p,
    )]
}

fn github(cfg: &Config, problem: Option<&str>, p: &Palette) -> Vec<AnyElement> {
    let mut out = vec![group_with_note(
        "Through the gh CLI",
        "Read only: gitstat never writes to GitHub. Signed in with gh auth login.",
        vec![
            flag(
                "github-prs",
                "Pull requests",
                "Show open pull requests and colour each branch by its pull request.",
                Setting::GithubPrs,
                true,
                cfg,
                p,
            ),
            flag(
                "github-repos",
                "Repos not cloned here",
                "List your GitHub repos, and your organisations', that aren't in your folders.",
                Setting::GithubRepos,
                true,
                cfg,
                p,
            ),
        ],
        p,
    )];
    if let Some(problem) = problem.filter(|_| cfg.github_prs || cfg.github_repos) {
        out.push(
            div()
                .px_4()
                .py_3()
                .rounded_lg()
                .border_1()
                .border_color(p.amber.opacity(0.5))
                .bg(p.amber.opacity(0.08))
                .text_xs()
                .text_color(p.amber)
                .child(problem.to_string())
                .into_any_element(),
        );
    }
    out
}

/// Opens a folder picker; the folder replaces entry `replace` of the
/// folder list (0 is `path`), or is added to it.
fn pick_folder(replace: Option<usize>, cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Choose folder".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = picked.await else {
            return;
        };
        let Some(dir) = paths.into_iter().next() else {
            return;
        };
        // Stored like a new config stores its folder.
        let dir = dir.to_string_lossy().replace('\\', "/");
        cx.update(|cx| {
            let cfg = cx.global::<Ui>().state.read(cx).config.clone();
            if let Some(folders) = with_folder(&cfg, replace, dir) {
                send(TrayAction::SetFolders(folders.0, folders.1), cx);
            }
        })
        .ok();
    })
    .detach();
}

/// The folder list with `dir` put in at `replace`, or added; `None` when it
/// is already listed.
fn with_folder(cfg: &Config, replace: Option<usize>, dir: String) -> Option<(String, Vec<String>)> {
    let (mut path, mut more) = (cfg.path.clone(), cfg.more_paths.clone());
    if path == dir || more.contains(&dir) {
        return None;
    }
    match replace {
        Some(0) => path = dir,
        Some(i) => *more.get_mut(i - 1)? = dir,
        None => more.push(dir),
    }
    Some((path, more))
}

fn interval_text(minutes: f64) -> String {
    let whole = (minutes - minutes.round()).abs() < 0.01;
    if whole && minutes >= 60.0 && (minutes as i64) % 60 == 0 {
        match minutes as i64 / 60 {
            1 => "1 hour".into(),
            h => format!("{h} hours"),
        }
    } else if whole {
        format!("{} min", minutes.round() as i64)
    } else {
        format!("{minutes:.1} min")
    }
}

fn config_banner(e: &str, p: &Palette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_4()
        .py_3()
        .rounded_lg()
        .border_1()
        .border_color(p.red.opacity(0.5))
        .bg(p.red.opacity(0.08))
        .child(
            div().text_sm().text_color(p.red).child(
                "config.json has an error, so changes made here aren't saved until it's fixed.",
            ),
        )
        .child(div().text_xs().text_color(p.muted).child(e.to_string()))
        .into_any_element()
}

/// A heading over a card of rows.
fn group(title: &'static str, rows: Vec<AnyElement>, p: &Palette) -> AnyElement {
    group_with_note(title, "", rows, p)
}

fn group_with_note(
    title: &'static str,
    note: &'static str,
    rows: Vec<AnyElement>,
    p: &Palette,
) -> AnyElement {
    let count = rows.len();
    let mut card = div()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(p.border)
        .bg(p.surface)
        .overflow_hidden();
    for (i, row) in rows.into_iter().enumerate() {
        card = card.child(row);
        if i + 1 < count {
            card = card.child(div().mx_4().h(px(1.)).bg(p.border.opacity(0.7)));
        }
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .px_1()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .when(!note.is_empty(), |d| {
            d.child(div().px_1().text_xs().text_color(p.muted).child(note))
        })
        .child(card)
        .into_any_element()
}

fn row_base() -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3()
        .min_h(px(56.))
}

/// The title and description on a row's left.
fn row_text(title: impl Into<SharedString>, desc: impl Into<SharedString>, p: &Palette) -> Div {
    let desc: SharedString = desc.into();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(2.))
        .child(div().text_sm().child(title.into()))
        .when(!desc.is_empty(), |d| {
            d.child(div().text_xs().text_color(p.muted).child(desc))
        })
}

/// A row with a control on its right.
fn setting_row(
    title: &'static str,
    desc: &'static str,
    control: impl IntoElement,
    p: &Palette,
) -> AnyElement {
    row_base()
        .child(row_text(title, desc, p))
        .child(control)
        .into_any_element()
}

/// A switch for one of the on/off settings.
fn flag(
    id: &'static str,
    title: &'static str,
    desc: &'static str,
    setting: Setting,
    enabled: bool,
    cfg: &Config,
    p: &Palette,
) -> AnyElement {
    let on = setting.is_on(cfg);
    toggle_row(id, title, desc, on, enabled, TrayAction::Toggle(setting), p)
}

/// A row that flips a switch; the whole row is the hit target.
fn toggle_row(
    id: &'static str,
    title: &'static str,
    desc: &'static str,
    on: bool,
    enabled: bool,
    action: TrayAction,
    p: &Palette,
) -> AnyElement {
    let hover = p.hover.opacity(0.5);
    row_base()
        .id(id)
        .child(row_text(title, desc, p))
        .child(switch(on, p))
        .when(!enabled, |r| r.opacity(0.45))
        .when(enabled, |r| {
            r.cursor_pointer()
                .hover(|s| s.bg(hover))
                .on_click(move |_, _, cx| send(action.clone(), cx))
        })
        .into_any_element()
}

fn switch(on: bool, p: &Palette) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .w(px(40.))
        .h(px(20.))
        .px(px(3.))
        .rounded_full()
        .border_1()
        .when(on, |d| d.justify_end().bg(p.button).border_color(p.button))
        .when(!on, |d| d.justify_start().border_color(p.muted))
        .child(
            div()
                .size(px(12.))
                .rounded_full()
                .bg(if on { p.button_text } else { p.muted }),
        )
}

fn radio_row(
    id: (&'static str, usize),
    title: &'static str,
    desc: &'static str,
    on: bool,
    action: TrayAction,
    p: &Palette,
) -> AnyElement {
    let hover = p.hover.opacity(0.5);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py(px(10.))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(18.))
                .rounded_full()
                .border_2()
                .border_color(if on { p.button } else { p.muted })
                .when(on, |d| {
                    d.child(div().size(px(8.)).rounded_full().bg(p.button))
                }),
        )
        .child(row_text(title, desc, p))
        .when(!on, |r| {
            r.cursor_pointer()
                .hover(|s| s.bg(hover))
                .on_click(move |_, _, cx| send(action.clone(), cx))
        })
        .into_any_element()
}

/// A row of buttons, one of them picked.
fn segmented(
    id: &'static str,
    options: Vec<(&'static str, bool, TrayAction)>,
    enabled: bool,
    p: &Palette,
) -> AnyElement {
    let (hover, text) = (p.hover, p.text);
    div()
        .flex()
        .flex_none()
        .p(px(2.))
        .gap(px(2.))
        .rounded_md()
        .border_1()
        .border_color(p.border)
        .bg(p.bg)
        .when(!enabled, |d| d.opacity(0.45))
        .children(
            options
                .into_iter()
                .enumerate()
                .map(|(i, (label, on, action))| {
                    div()
                        .id((id, i))
                        .px_3()
                        .py_1()
                        .rounded(px(4.))
                        .text_xs()
                        .when(on, |b| b.bg(p.button).text_color(p.button_text))
                        .when(!on, |b| b.text_color(p.muted))
                        .when(enabled && !on, |b| {
                            b.cursor_pointer()
                                .hover(|s| s.bg(hover).text_color(text))
                                .on_click(move |_, _, cx| send(action.clone(), cx))
                        })
                        .child(label)
                }),
        )
        .into_any_element()
}

/// − value +, each button off at its end of the range.
fn stepper(
    id: &'static str,
    value: String,
    down: Option<TrayAction>,
    up: Option<TrayAction>,
    p: &Palette,
) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .child(step_button((id, 0), "−", down, p))
        .child(div().w(px(84.)).text_center().text_sm().child(value))
        .child(step_button((id, 1), "+", up, p))
        .into_any_element()
}

fn step_button(
    id: (&'static str, usize),
    glyph: &'static str,
    action: Option<TrayAction>,
    p: &Palette,
) -> AnyElement {
    let hover = p.hover;
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size_7()
        .rounded_md()
        .border_1()
        .border_color(p.border)
        .bg(p.bg)
        .child(glyph)
        .map(|b| match action {
            Some(action) => b
                .cursor_pointer()
                .hover(|s| s.bg(hover))
                .on_click(move |_, _, cx| send(action.clone(), cx)),
            None => b.opacity(0.35),
        })
        .into_any_element()
}

fn text_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    p: &Palette,
) -> gpui::Stateful<Div> {
    let hover = p.hover;
    div()
        .id(id)
        .flex_none()
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(p.border)
        .bg(p.bg)
        .text_xs()
        .cursor_pointer()
        .hover(|s| s.bg(hover))
        .child(label)
}

/// Folder `i` of the list (0 is `path`), with its buttons.
fn folder_row(i: usize, path: &str, p: &Palette) -> AnyElement {
    let desc = if i == 0 { "Main folder" } else { "" };
    let shown = if path.trim().is_empty() {
        "(none)".to_string()
    } else {
        path.to_string()
    };
    row_base()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .flex_1()
                .min_w_0()
                .child(div().flex_none().text_color(p.muted).child("▭"))
                .child(row_text(shown, desc, p)),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .gap_2()
                .child(
                    text_button(("change-folder", i), "Change…", p)
                        .on_click(move |_, _, cx| pick_folder(Some(i), cx)),
                )
                .when(i > 0, |d| {
                    d.child(text_button(("remove-folder", i), "Remove", p).on_click(
                        move |_, _, cx| {
                            let cfg = cx.global::<Ui>().state.read(cx).config.clone();
                            let mut more = cfg.more_paths;
                            if i - 1 < more.len() {
                                more.remove(i - 1);
                                send(TrayAction::SetFolders(cfg.path, more), cx);
                            }
                        },
                    ))
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_flips_exactly_its_setting() {
        let all = [
            Setting::Watch,
            Setting::Panel,
            Setting::Window,
            Setting::IconsOnly,
            Setting::Overlay,
            Setting::OverlayHideSynced,
            Setting::OverlayHideFullscreen,
            Setting::FolderIcons,
            Setting::GithubPrs,
            Setting::GithubRepos,
            Setting::ScrollTogether,
            Setting::FlipAnimation,
        ];
        for setting in all {
            let before = Config::default();
            let mut after = before.clone();
            setting.toggle(&mut after);
            for other in all {
                let flipped = other.is_on(&before) != other.is_on(&after);
                assert_eq!(flipped, other == setting, "{setting:?} vs {other:?}");
            }
        }
    }

    #[test]
    fn folders_are_replaced_added_and_never_doubled() {
        let cfg = Config {
            path: "D:/dev".into(),
            more_paths: vec!["E:/work".into()],
            ..Config::default()
        };
        assert_eq!(
            with_folder(&cfg, None, "F:/x".into()),
            Some(("D:/dev".into(), vec!["E:/work".into(), "F:/x".into()]))
        );
        assert_eq!(
            with_folder(&cfg, Some(0), "C:/src".into()),
            Some(("C:/src".into(), vec!["E:/work".into()]))
        );
        assert_eq!(
            with_folder(&cfg, Some(1), "G:/y".into()),
            Some(("D:/dev".into(), vec!["G:/y".into()]))
        );
        assert_eq!(with_folder(&cfg, None, "E:/work".into()), None);
        assert_eq!(with_folder(&cfg, Some(5), "H:/z".into()), None);
    }

    #[test]
    fn intervals_read_naturally() {
        assert_eq!(interval_text(1.0), "1 min");
        assert_eq!(interval_text(15.0), "15 min");
        assert_eq!(interval_text(60.0), "1 hour");
        assert_eq!(interval_text(240.0), "4 hours");
        assert_eq!(interval_text(90.0), "90 min");
        assert_eq!(interval_text(2.5), "2.5 min");
    }
}
