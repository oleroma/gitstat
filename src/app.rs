//! The GPUI side: startup, the one input loop, and the settings the
//! settings window changes. The views live in view.rs, panel.rs, overlay.rs
//! and settings.rs.

use crate::config::{self, ClickAction, Config, FolderIconMode};
use crate::folder_icons;
use crate::monitor::{Command, Monitor, Update};
use crate::panel::{self, Anchor};
use crate::settings::{self, Setting};
use crate::state::{AppState, Input};
use crate::tray::{Tray, TrayAction, TraySnapshot};
use crate::view::{self, Ui};
use crate::{overlay, win};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::{
    App, AppContext, Application, BorrowAppContext, Bounds, Context, Global, IntoElement, Render,
    Window, WindowBounds, WindowKind, WindowOptions, div, point, px, size,
};
use std::path::PathBuf;
use std::rc::Rc;

/// Inputs handled per UI update; the rest wait for the next pass.
const MAX_BATCH: usize = 512;

const FOLDER_ICONS_CONSENT: &str = "\
Show each repo's status as its folder icon in Explorer?

gitstat will write a hidden desktop.ini into each repo folder on local fixed \
drives, mark the folder read-only (which is how Windows enables custom folder \
icons), and add /desktop.ini to the repo's .git/info/exclude so git ignores it.

Folders with their own desktop.ini, OneDrive folders and network drives are \
skipped. Turning the option off, or running gitstat --remove-folder-icons, \
restores the folders.";

/// What only the input loop touches.
struct Shared {
    tray: Tray,
    /// One per folder in the config's `roots()`, in that order.
    monitors: Vec<Monitor>,
    /// Where new monitors report.
    monitor_input: futures::channel::mpsc::UnboundedSender<Input>,
    config_file: PathBuf,
    icons: folder_icons::Worker,
    /// Reads pull requests from GitHub; never writes anything.
    github: crate::github::Poller,
    /// The "couldn't save config.json" notice was shown this session.
    save_notice_shown: bool,
    /// The folder-icon consent box is open.
    consent_pending: bool,
    /// A self-update failed and said so; later tries stay quiet.
    update_error_shown: bool,
}

impl Global for Shared {}

impl Shared {
    fn send_monitors(&self, command: Command) {
        for m in &self.monitors {
            m.send(command.clone());
        }
    }

    /// A monitor for `slot`, starting from the app's `config`: the file may
    /// be broken, or hold less than memory when a save was refused.
    fn start_monitor(&self, slot: usize, config: Config) -> Monitor {
        let tx = self.monitor_input.clone();
        Monitor::start_slot(self.config_file.clone(), slot, Some(config), move |u| {
            let _ = tx.unbounded_send(Input::Monitor(slot, u));
        })
    }
}

/// Starts or stops monitors so there is one per folder in `config.roots()`,
/// and forgets the repos of folders no longer watched.
fn sync_monitors(cx: &mut App) {
    let state = cx.global::<Ui>().state.clone();
    let config = state.read(cx).config.clone();
    let want = config.roots().len().max(1);
    cx.update_global::<Shared, _>(|shared, _| {
        while shared.monitors.len() < want {
            let m = shared.start_monitor(shared.monitors.len(), config.clone());
            shared.monitors.push(m);
        }
        // Dropping a monitor shuts it down.
        shared.monitors.truncate(want);
    });
    let removed = state.update(cx, |s, _| s.keep_slots(want));
    if removed {
        let repos = state.read(cx).all_repos();
        cx.global::<Shared>().github.set_repos(repos);
    }
}

pub fn run(config_file: PathBuf) {
    Application::new().run(move |cx: &mut App| {
        let (tx, rx) = unbounded::<Input>();
        // Before Tray::new: the handlers can be set only once.
        crate::tray::install_handlers(tx.clone());

        // GPUI quits when its last window closes. A window that is never
        // shown keeps the app alive while the panel and window are closed.
        let mut keepalive = None;
        cx.open_window(
            WindowOptions {
                show: false,
                focus: false,
                titlebar: None,
                kind: WindowKind::PopUp,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1.), px(1.)),
                ))),
                ..Default::default()
            },
            |window, cx| {
                keepalive = win::hwnd(window);
                if let Some(h) = keepalive {
                    // GPUI can show it on a display change; make that harmless.
                    cx.spawn(async move |_| unsafe { win::harden_keepalive(h) })
                        .detach();
                }
                cx.new(|_| Keepalive)
            },
        )
        .ok();

        // A file that won't parse runs on the last config that did; with
        // none, on the defaults, but folder icons are left alone until a
        // good config says what they should be.
        let (cfg, config_error, held) = match Config::load_or_create(&config_file) {
            Ok(cfg) => {
                cfg.remember_good(&config_file);
                (cfg, None, false)
            }
            Err(e) => match Config::last_good(&config_file) {
                Some(good) => (good, Some(config_problem(&e)), false),
                None => (Config::default(), Some(config_problem(&e)), true),
            },
        };
        let state = cx.new(|_| {
            let mut s = AppState::new(config_file.clone(), cfg.clone(), config_error);
            s.config_held = held;
            s
        });

        let tray = match Tray::new() {
            Ok(tray) => tray,
            Err(e) => {
                let _ = crate::error_box(format!("Couldn't create the tray icon: {e}")).join();
                cx.quit();
                return;
            }
        };

        let monitor_tx = tx.clone();
        let monitor = Monitor::start_slot(config_file.clone(), 0, Some(cfg.clone()), move |u| {
            let _ = monitor_tx.unbounded_send(Input::Monitor(0, u));
        });
        let icons_tx = tx.clone();
        let icons = folder_icons::Worker::start(config::data_dir(), move |skips| {
            let _ = icons_tx.unbounded_send(Input::FolderSkips(skips));
        });

        let github_tx = tx.clone();
        let github = crate::github::Poller::start(move |report| {
            let _ = github_tx.unbounded_send(Input::Github(report));
        });

        let timelines = cx.new(|cx| crate::timeline::Timelines::new(&state, cx));
        let marks = cx.new(|cx| crate::repo_marks::RepoMarks::new(&state, cx));
        cx.set_global(Ui {
            state: state.clone(),
            timelines,
            marks,
            input: tx.clone(),
            refresh: Rc::new(|cx: &mut App| {
                if let Some(shared) = cx.try_global::<Shared>() {
                    shared.send_monitors(Command::RefreshAll);
                    shared.github.refresh();
                }
            }),
            keepalive,
        });
        cx.set_global(Shared {
            tray,
            monitors: vec![monitor],
            monitor_input: tx.clone(),
            config_file: config_file.clone(),
            icons,
            github,
            save_notice_shown: false,
            consent_pending: false,
            update_error_shown: false,
        });
        apply_config(&Config::default(), cfg, cx);
        update_outputs(false, cx);

        if let Ok(exe) = std::env::current_exe() {
            let update_tx = tx.clone();
            crate::self_update::watch(exe, move || {
                let _ = update_tx.unbounded_send(Input::NewBuild);
            });
        }

        cx.spawn(async move |cx| input_loop(rx, cx).await).detach();
    });
}

/// The one foreground task: waits for input, then handles everything
/// already queued in one update so a burst of statuses redraws once.
async fn input_loop(mut rx: UnboundedReceiver<Input>, cx: &mut gpui::AsyncApp) {
    while let Some(first) = rx.next().await {
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match rx.try_recv() {
                Ok(input) => batch.push(input),
                Err(_) => break,
            }
        }
        if !cx.update(|cx| handle_batch(batch, cx)).unwrap_or(false) {
            break;
        }
    }
}

/// Returns false once the app is quitting.
fn handle_batch(batch: Vec<Input>, cx: &mut App) -> bool {
    let state = cx.global::<Ui>().state.clone();
    let mut changed = false;
    let mut checked = false;
    let mut icons_stale = false;
    // A repo listed after the poll that read its PRs needs them too.
    let listed = state.read(cx).statuses.len();
    let mut prs_stale = false;
    for input in batch {
        match input {
            Input::Monitor(slot, update) => {
                // A monitor being shut down for a folder no longer watched.
                if slot >= cx.global::<Shared>().monitors.len() {
                    continue;
                }
                // A check that changed no status may still have moved refs
                // the timeline shows; it observes the state for this.
                checked |= matches!(update, Update::Status(_));
                let (c, s) = on_update(slot, update, cx);
                changed |= c;
                icons_stale |= s;
            }
            Input::FolderSkips(skips) => {
                state.update(cx, |s, _| s.folder_skips = skips);
                changed = true;
            }
            Input::Github(report) => {
                let c = &state.read(cx).config;
                if c.github_prs || c.github_repos {
                    let lines = {
                        let s = state.read(cx);
                        let names = s
                            .statuses
                            .iter()
                            .map(|r| (r.path.clone(), r.name.clone()))
                            .collect();
                        crate::notify::pr_changes(&s.config.notify, &names, &s.prs, &report.prs)
                    };
                    notify(&lines, cx);
                    if !report.pushed.is_empty() {
                        cx.global::<Shared>()
                            .send_monitors(Command::Pushed(report.pushed.clone()));
                    }
                    state.update(cx, |s, _| {
                        let (prs, repos) = (s.config.github_prs, s.config.github_repos);
                        s.github_report(report);
                        // A poll that was under way when one option went off.
                        if !prs {
                            s.prs.clear();
                        }
                        if !repos {
                            s.set_remote_repos(Vec::new());
                        }
                    });
                    prs_stale = true;
                    changed = true;
                }
            }
            Input::Overlay => {
                let click = state.read(cx).config.overlay_click();
                match click {
                    Some(ClickAction::Panel) => {
                        let anchor = overlay::anchor(cx).unwrap_or(Anchor::Tray(None));
                        panel::toggle(anchor, cx);
                    }
                    Some(ClickAction::Window) => {
                        let anchor = overlay::anchor(cx).unwrap_or(Anchor::Tray(None));
                        crate::morph::show_window(anchor, cx);
                    }
                    Some(ClickAction::Menu) | None => {}
                }
            }
            Input::OverlayMoved(rect) => {
                let (corner, x, y) = overlay::drop_position(rect);
                change_setting(
                    |c| {
                        c.overlay.corner = corner;
                        c.overlay.offset_x = Some(x);
                        c.overlay.offset_y = Some(y);
                    },
                    cx,
                );
                overlay::drop_saved(cx);
                changed = true;
            }
            Input::NewBuild => {
                if update_and_restart(cx) {
                    return false;
                }
            }
            Input::Action(action) => {
                if !on_action(action, cx) {
                    return false;
                }
                changed = true;
            }
            input @ (Input::Tray(_) | Input::Menu(_)) => {
                let Some(action) = cx.global::<Shared>().tray.map(&input) else {
                    continue;
                };
                if !on_action(action, cx) {
                    return false;
                }
                changed = true;
            }
        }
    }
    if prs_stale || state.read(cx).statuses.len() != listed {
        sync_pr_colors(cx);
    }
    if changed || checked {
        state.update(cx, |_, cx| cx.notify());
    }
    // An icon.svg change alone changes nothing on screen.
    if changed || icons_stale {
        update_outputs(icons_stale, cx);
    } else {
        cx.global_mut::<Shared>().tray.check_icon_size();
    }
    // Monitor updates arrive at least every interval, badge or not.
    view::hide_keepalive_if_shown(cx);
    true
}

/// Folds one monitor update into the state. Returns (anything changed,
/// the folder icons need a sync: statuses changed, or a repo's own icon
/// may have).
fn on_update(slot: usize, update: Update, cx: &mut App) -> (bool, bool) {
    let state = cx.global::<Ui>().state.clone();
    let repo_svgs = {
        let config = &state.read(cx).config;
        config.folder_icons && config.folder_icon_mode == FolderIconMode::RepoSvg
    };
    match update {
        Update::Config(Ok(new)) => {
            let file = state.update(cx, |s, _| {
                s.config_error = None;
                s.config_held = false;
                s.config_file.clone()
            });
            new.remember_good(&file);
            let old = state.read(cx).config.clone();
            // Our own writes come back equal, and are not re-applied.
            if new != old {
                apply_config(&old, new, cx);
            }
            (true, false)
        }
        Update::Config(Err(e)) => {
            state.update(cx, |s, _| s.config_error = Some(config_problem(&e)));
            (true, false)
        }
        Update::Repos { repos, capped } => {
            let result = state.update(cx, |s, _| s.set_slot_repos(slot, repos, capped));
            let all = state.read(cx).all_repos();
            cx.global::<Shared>().github.set_repos(all);
            result
        }
        Update::Started { repo, .. } => {
            state.update(cx, |s, _| s.in_flight.insert(repo));
            (false, false)
        }
        Update::Status(status) => {
            let (line, changes) = {
                let s = state.read(cx);
                let old = s.statuses.iter().find(|o| o.path == status.path);
                (
                    crate::notify::behind(&s.config.notify, old, &status),
                    crate::notify::remote_changes(&s.config.notify, old, &status),
                )
            };
            if let Some(line) = line {
                notify(&[line], cx);
            }
            if !changes.is_empty() {
                notify_remote(&status, changes, cx);
            }
            state.update(cx, |s, _| {
                s.in_flight.remove(&status.path);
                s.checked(&status.path);
                let changed = s.merge_status(status);
                (changed, changed)
            })
        }
        Update::SweepStarted => {
            state.update(cx, |s, _| s.set_sweeping(slot, true));
            (true, false)
        }
        Update::SweepDone => state.update(cx, |s, _| {
            s.set_sweeping(slot, false);
            s.last_checked = Some(local_time());
            // The first result: the folder-icon worker can start now.
            // Every sweep has another look at the repos' own icons, which
            // covers a missed event and `watch: false`.
            let first = s.swept(slot);
            (true, first || repo_svgs)
        }),
        Update::RepoIcons => (false, repo_svgs),
        Update::Mode(mode) => {
            state.update(cx, |s, _| s.set_mode(slot, mode));
            (true, false)
        }
    }
}

/// Brings the tray, the folder icons and the badge up to date with the
/// state; the folder icons only with `sync_icons`.
fn update_outputs(sync_icons: bool, cx: &mut App) {
    let state = cx.global::<Ui>().state.clone();
    cx.update_global::<Shared, _>(|shared, cx| {
        let s = state.read(cx);
        let problem = s.problem();
        let note = s.badge_note();
        shared.tray.update(
            &TraySnapshot {
                statuses: &s.statuses,
                checking: s.sweeping,
                has_result: s.has_result,
                problem: problem.as_deref(),
                incomplete: s.capped.is_some(),
            },
            s.badge(),
            note.as_deref(),
        );
        if sync_icons && s.has_result && !s.config_held {
            let config = &s.config;
            shared.icons.sync(
                s.statuses.clone(),
                config.folder_icons,
                config.folder_icon_mode,
            );
        }
    });
    overlay::sync(cx);
}

/// Runs a tray action. Returns false once the app is quitting.
fn on_action(action: TrayAction, cx: &mut App) -> bool {
    let cfg = cx.global::<Ui>().state.read(cx).config.clone();
    match action {
        TrayAction::LeftClick(rect) => match cfg.left_click() {
            ClickAction::Panel => panel::toggle(Anchor::Tray(rect), cx),
            ClickAction::Window => crate::morph::show_window(Anchor::Tray(rect), cx),
            // tray-icon shows the menu itself (`set_show_menu_on_left_click`).
            ClickAction::Menu => {}
        },
        TrayAction::DoubleClick => {
            if cfg.window {
                let rect = cx.global::<Shared>().tray.icon_rect();
                crate::morph::show_window(Anchor::Tray(rect), cx);
            }
        }
        TrayAction::OpenPanel => {
            if !panel::is_open(cx) {
                let rect = cx.global::<Shared>().tray.icon_rect();
                panel::toggle(Anchor::Tray(rect), cx);
            }
        }
        TrayAction::ShowWindow => {
            let rect = cx.global::<Shared>().tray.icon_rect();
            crate::morph::show_window(Anchor::Tray(rect), cx);
        }
        TrayAction::Refresh => {
            // The same as the panel's ↻: GitHub is asked again too.
            let shared = cx.global::<Shared>();
            shared.send_monitors(Command::RefreshAll);
            shared.github.refresh();
        }
        TrayAction::OpenRepo(path) => view::open_repo(&path, cx),
        TrayAction::RepoMenu(action, target) => {
            use crate::repo_menu::RepoAction;
            if action.leaves() {
                panel::close(cx);
            }
            match action {
                RepoAction::Fetch => {
                    cx.global::<Shared>()
                        .send_monitors(Command::Fetch(target.path.clone()));
                }
                RepoAction::Hide => {
                    let name = if target.remote_only {
                        crate::github::remote_slug(&target.path)
                            .unwrap_or(&target.name)
                            .to_string()
                    } else {
                        target.name.clone()
                    };
                    change_setting(
                        move |c| {
                            if !c.is_ignored(&name) {
                                c.ignore.push(name);
                            }
                        },
                        cx,
                    );
                }
                _ => crate::repo_menu::run(action, &target, cx),
            }
        }
        TrayAction::EditConfig => {
            let file = cx.global::<Ui>().state.read(cx).config_file.clone();
            cx.open_with_system(&file);
        }
        TrayAction::Toggle(Setting::FolderIcons) if !cfg.folder_icons => ask_folder_icons(cx),
        TrayAction::Toggle(setting) => change_setting(|c| setting.toggle(c), cx),
        // A corner from the menu drops any dragged offset: margin from both edges.
        TrayAction::SetCorner(corner) => change_setting(
            |c| {
                c.overlay.corner = corner;
                c.overlay.offset_x = None;
                c.overlay.offset_y = None;
            },
            cx,
        ),
        TrayAction::SetFolderIconMode(mode) => change_setting(|c| c.folder_icon_mode = mode, cx),
        TrayAction::SetRepoClick(click) => change_setting(|c| c.repo_click = click, cx),
        TrayAction::SetBadgeNumber(n) => change_setting(|c| c.badge_number = n, cx),
        TrayAction::SetSort(s) => change_setting(|c| c.sort = s, cx),
        TrayAction::SetPanelSlide(ms) => change_setting(|c| c.panel_slide_ms = ms, cx),
        TrayAction::SetFlipSpeed(ms) => change_setting(|c| c.flip_ms = ms, cx),
        TrayAction::Fold(section) => change_setting(|c| c.folds.toggle(section), cx),
        TrayAction::SetInterval(minutes) => change_setting(|c| c.interval_minutes = minutes, cx),
        TrayAction::SetDepth(depth) => change_setting(|c| c.depth = depth, cx),
        TrayAction::SetFolders(path, more) => change_setting(
            |c| {
                c.path = path;
                c.more_paths = more;
            },
            cx,
        ),
        TrayAction::SetAutostart(enabled) => {
            if let Err(e) = crate::autostart::set_enabled(enabled) {
                crate::error_box(format!("Couldn't change the startup setting: {e}"));
            }
            settings::autostart_changed(cx);
        }
        TrayAction::OpenSettings => settings::show(cx),
        TrayAction::Quit => {
            quit(cx);
            return false;
        }
    }
    true
}

fn quit(cx: &mut App) {
    let shared = cx.global::<Shared>();
    shared.send_monitors(Command::Shutdown);
    shared.tray.hide();
    cx.quit();
}

/// Swaps in the build an install left next to the exe, starts it and quits;
/// or, run from outside the install folder, starts the installed build an
/// install asked it to hand over to, and quits. Returns true when quitting.
/// A failed swap leaves this gitstat running and is tried again on the next
/// poll; a failed handover is not.
fn update_and_restart(cx: &mut App) -> bool {
    use crate::self_update;
    let mut handover = None;
    let result = std::env::current_exe().and_then(|exe| {
        if let Some(installed) = self_update::handover(&exe) {
            handover = Some(installed.clone());
            // Taken first, so a failed start isn't tried on every poll.
            std::fs::remove_file(self_update::handover_file(&exe))?;
            self_update::restart(&installed)?;
            return Ok(true);
        }
        if !self_update::swap(&exe)? {
            return Ok(false);
        }
        if let Err(e) = self_update::restart(&exe) {
            // Back to the build that is running, so the next start isn't a surprise.
            let _ = std::fs::rename(&exe, self_update::new_build(&exe));
            let _ = std::fs::rename(self_update::old_build(&exe), &exe);
            return Err(e);
        }
        Ok(true)
    });
    match result {
        Ok(true) => {
            quit(cx);
            true
        }
        Ok(false) => false,
        Err(e) => {
            let shared = cx.global_mut::<Shared>();
            if !shared.update_error_shown {
                shared.update_error_shown = true;
                crate::error_box(match handover {
                    Some(installed) => format!(
                        "gitstat was installed at {}, but this gitstat couldn't hand over \
                         to it: {e}\n\nQuit this gitstat and start the installed one.",
                        installed.display()
                    ),
                    None => format!(
                        "A new gitstat build is waiting, but gitstat couldn't switch to it: \
                         {e}\n\nIt will keep trying. Quitting and starting gitstat also picks \
                         it up."
                    ),
                });
            }
            false
        }
    }
}

/// Folder icons change the user's repo folders, so turning them on from the
/// settings window asks first. A hand edit of config.json is consent enough.
fn ask_folder_icons(cx: &mut App) {
    let shared = cx.global_mut::<Shared>();
    if shared.consent_pending {
        return;
    }
    shared.consent_pending = true;
    let answer = win::confirm_box(FOLDER_ICONS_CONSENT.to_string());
    // Waited for on a thread of its own: the box can stay open for minutes,
    // and a background executor thread waiting that long is one fewer for
    // timelines and icons.
    let (tx, wait) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(answer.join().unwrap_or(false));
    });
    cx.spawn(async move |cx| {
        let yes = wait.await.unwrap_or(false);
        cx.update(|cx| {
            cx.global_mut::<Shared>().consent_pending = false;
            if yes {
                change_setting(|c| c.folder_icons = true, cx);
            }
        })
        .ok();
    })
    .detach();
}

/// A settings change: change one field, save, apply. A save that is refused
/// (config.json doesn't parse) keeps the change in memory, with one notice
/// per session. The monitor gets the change directly either way: it only
/// rereads a file that saved.
fn change_setting(f: impl FnOnce(&mut Config), cx: &mut App) {
    let state = cx.global::<Ui>().state.clone();
    let (old, file) = {
        let s = state.read(cx);
        (s.config.clone(), s.config_file.clone())
    };
    let mut new = old.clone();
    f(&mut new);
    if new == old {
        return;
    }
    if let Err(e) = new.save_over(&file, &old) {
        let shared = cx.global_mut::<Shared>();
        if !shared.save_notice_shown {
            shared.save_notice_shown = true;
            crate::error_box(format!(
                "Couldn't save config.json, so this change lasts only until config.json \
                 is reloaded, or gitstat quits.\n\n{e}"
            ));
        }
    }
    cx.global::<Shared>()
        .send_monitors(Command::Apply(new.clone()));
    apply_config(&old, new, cx);
}

/// Makes the app match `new`. Idempotent, so startup applies the file
/// config over the defaults. The monitor reads `path`, `watch` and the
/// interval from the file itself, and from `change_setting`.
fn apply_config(old: &Config, new: Config, cx: &mut App) {
    let state = cx.global::<Ui>().state.clone();
    state.update(cx, |s, cx| {
        s.config = new.clone();
        if new.ignore != old.ignore {
            let remote = s.remote_repos.clone();
            s.set_remote_repos(remote);
        }
        cx.notify();
    });
    if !new.panel {
        panel::close(cx);
    }
    if !new.window {
        view::close_window(cx);
    }
    if new.overlay != old.overlay {
        overlay::sync(cx);
    }
    let s = state.read(cx);
    // Turning icons off reverts even before the first result. A new mode
    // matters only while they are on.
    let icons_changed = new.folder_icons != old.folder_icons
        || (new.folder_icons && new.folder_icon_mode != old.folder_icon_mode);
    let sync_icons = icons_changed && (s.has_result || !new.folder_icons) && !s.config_held;
    let statuses = s.statuses.clone();
    let shared = cx.global::<Shared>();
    if sync_icons {
        shared
            .icons
            .sync(statuses, new.folder_icons, new.folder_icon_mode);
    }
    let wants = crate::github::Wants {
        prs: new.github_prs,
        remote_repos: new.github_repos,
    };
    shared.github.settings(wants, new.interval());
    shared.tray.sync_settings(&new);
    if !new.github_repos {
        state.update(cx, |s, _| s.set_remote_repos(Vec::new()));
    }
    if !new.github_prs {
        state.update(cx, |s, _| s.prs.clear());
        sync_pr_colors(cx);
    }
    if !new.github_prs && !new.github_repos {
        state.update(cx, |s, _| s.github_problem = None);
    }
    // A folder added to or removed from `more_paths`.
    sync_monitors(cx);
}

/// Hands each listed repo's PRs, merged and closed ones too, to its graph
/// so its branches take their PR's colour.
fn sync_pr_colors(cx: &mut App) {
    let Some(ui) = cx.try_global::<Ui>() else {
        return;
    };
    let (state, timelines) = (ui.state.clone(), ui.timelines.clone());
    let colors: Vec<_> = {
        let s = state.read(cx);
        s.statuses
            .iter()
            .map(|r| {
                let prs = s.prs.get(&r.path).map(Vec::as_slice).unwrap_or_default();
                let colors = crate::graph::PrColors::from_prs(
                    prs.iter()
                        .filter(|p| p.is_for(&p.head_branch, &r.tips))
                        .map(|p| (p.head_branch.as_str(), p.state)),
                );
                (r.path.clone(), colors)
            })
            .collect()
    };
    timelines.update(cx, |t, cx| {
        for (path, colors) in colors {
            t.set_prs(&path, colors, cx);
        }
    });
}

fn config_problem(e: &str) -> String {
    format!("config.json: {e}")
}

struct Keepalive;

impl Render for Keepalive {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The current local time as HH:MM.
/// Notifies what a fetch brought in, counting the new commits off the UI
/// thread first.
fn notify_remote(
    status: &crate::git::RepoStatus,
    changes: Vec<crate::notify::RemoteChange>,
    cx: &mut App,
) {
    use crate::notify::RemoteChange;
    let (repo, name) = (status.path.clone(), status.name.clone());
    let task = cx.background_spawn(async move {
        changes
            .into_iter()
            .map(|c| match c {
                RemoteChange::New { branch } => format!("{name}: new branch {branch}"),
                RemoteChange::Moved { branch, from, to } => {
                    let range = format!("{from}..{to}");
                    let count = crate::git::git(
                        &repo,
                        &["rev-list", "--count", &range, "--"],
                        crate::git::LOCAL_TIMEOUT,
                    )
                    .ok()
                    .filter(|o| o.status.success())
                    .and_then(|o| {
                        String::from_utf8_lossy(&o.stdout)
                            .trim()
                            .parse::<u32>()
                            .ok()
                    });
                    match count {
                        Some(1) => format!("{name}: 1 new commit on {branch}"),
                        Some(n) if n > 0 => format!("{name}: {n} new commits on {branch}"),
                        // Rewritten (a force push), or git couldn't say.
                        _ => format!("{name}: {branch} changed"),
                    }
                }
            })
            .collect::<Vec<String>>()
    });
    cx.spawn(async move |cx| {
        let lines = task.await;
        cx.update(|cx| notify(&lines, cx)).ok();
    })
    .detach();
}

/// Shows `lines` as one Windows notification from the tray icon.
fn notify(lines: &[String], cx: &mut App) {
    if let Some(note) = crate::notify::note(lines) {
        crate::notify::show(cx.global::<Shared>().tray.hwnd(), &note);
    }
}

fn local_time() -> String {
    let mut t = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}
