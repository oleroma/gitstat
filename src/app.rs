//! The GPUI side: startup, the one input loop, and the settings the tray
//! menu changes. The views live in view.rs, panel.rs and overlay.rs.

use crate::config::{self, ClickAction, Config};
use crate::folder_icons;
use crate::monitor::{Command, Monitor, Update};
use crate::panel::{self, Anchor};
use crate::state::{AppState, Input};
use crate::tray::{Setting, Tray, TrayAction, TraySnapshot};
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
    monitor: Monitor,
    icons: folder_icons::Worker,
    /// The "couldn't save config.json" notice was shown this session.
    save_notice_shown: bool,
    /// The folder-icon consent box is open.
    consent_pending: bool,
}

impl Global for Shared {}

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

        let (cfg, config_error) = match Config::load_or_create(&config_file) {
            Ok(cfg) => (cfg, None),
            Err(e) => (Config::default(), Some(config_problem(&e))),
        };
        let state = cx.new(|_| AppState::new(config_file.clone(), cfg.clone(), config_error));

        let tray = match Tray::new() {
            Ok(tray) => tray,
            Err(e) => {
                let _ = crate::error_box(format!("Couldn't create the tray icon: {e}")).join();
                cx.quit();
                return;
            }
        };

        let monitor_tx = tx.clone();
        let monitor = Monitor::start(config_file, move |u| {
            let _ = monitor_tx.unbounded_send(Input::Monitor(u));
        });
        let icons_tx = tx.clone();
        let icons = folder_icons::Worker::start(config::data_dir(), move |skips| {
            let _ = icons_tx.unbounded_send(Input::FolderSkips(skips));
        });

        cx.set_global(Ui {
            state: state.clone(),
            input: tx,
            refresh: Rc::new(|cx: &mut App| {
                if let Some(shared) = cx.try_global::<Shared>() {
                    shared.monitor.send(Command::RefreshAll);
                }
            }),
            keepalive,
        });
        cx.set_global(Shared {
            tray,
            monitor,
            icons,
            save_notice_shown: false,
            consent_pending: false,
        });
        apply_config(&Config::default(), cfg, cx);
        update_outputs(false, cx);

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
    let mut statuses_changed = false;
    for input in batch {
        match input {
            Input::Monitor(update) => {
                let (c, s) = on_update(update, cx);
                changed |= c;
                statuses_changed |= s;
            }
            Input::FolderSkips(skips) => {
                state.update(cx, |s, _| s.folder_skips = skips);
                changed = true;
            }
            Input::Overlay => {
                let click = state.read(cx).config.overlay_click();
                match click {
                    Some(ClickAction::Panel) => {
                        let anchor = overlay::anchor(cx).unwrap_or(Anchor::Tray(None));
                        panel::toggle(anchor, cx);
                    }
                    Some(ClickAction::Window) => view::show_window(cx),
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
    if changed {
        state.update(cx, |_, cx| cx.notify());
        update_outputs(statuses_changed, cx);
    } else {
        cx.global_mut::<Shared>().tray.check_icon_size();
    }
    // Monitor updates arrive at least every interval, badge or not.
    view::hide_keepalive_if_shown(cx);
    true
}

/// Folds one monitor update into the state. Returns (anything changed,
/// statuses changed).
fn on_update(update: Update, cx: &mut App) -> (bool, bool) {
    let state = cx.global::<Ui>().state.clone();
    match update {
        Update::Config(Ok(new)) => {
            state.update(cx, |s, _| s.config_error = None);
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
        Update::Repos { repos, capped } => state.update(cx, |s, _| {
            let before = s.statuses.len();
            s.statuses.retain(|st| repos.contains(&st.path));
            s.in_flight.retain(|p| repos.contains(p));
            let removed = s.statuses.len() != before;
            let capped_changed = s.capped != capped;
            s.capped = capped;
            (removed || capped_changed, removed)
        }),
        Update::Started { repo, .. } => {
            state.update(cx, |s, _| s.in_flight.insert(repo));
            (false, false)
        }
        Update::Status(status) => state.update(cx, |s, _| {
            s.in_flight.remove(&status.path);
            let changed = s.merge_status(status);
            (changed, changed)
        }),
        Update::SweepStarted => {
            state.update(cx, |s, _| s.sweeping = true);
            (true, false)
        }
        Update::SweepDone => state.update(cx, |s, _| {
            s.sweeping = false;
            s.last_checked = Some(local_time());
            // The first result: the folder-icon worker can start now.
            let first = !s.has_result;
            s.has_result = true;
            (true, first)
        }),
        Update::Mode(mode) => {
            state.update(cx, |s, _| s.mode = mode);
            (true, false)
        }
    }
}

/// Brings the tray, the folder icons and the badge up to date with the state.
fn update_outputs(statuses_changed: bool, cx: &mut App) {
    let state = cx.global::<Ui>().state.clone();
    cx.update_global::<Shared, _>(|shared, cx| {
        let s = state.read(cx);
        let problem = s.problem();
        shared.tray.update(&TraySnapshot {
            statuses: &s.statuses,
            checking: s.sweeping,
            has_result: s.has_result,
            problem: problem.as_deref(),
            incomplete: s.capped.is_some(),
        });
        if statuses_changed && s.has_result {
            shared.icons.sync(s.statuses.clone(), s.config.folder_icons);
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
            ClickAction::Window => view::show_window(cx),
            // tray-icon shows the menu itself (`set_show_menu_on_left_click`).
            ClickAction::Menu => {}
        },
        TrayAction::DoubleClick => {
            if cfg.window {
                panel::close(cx);
                view::show_window(cx);
            }
        }
        TrayAction::OpenPanel => {
            if !panel::is_open(cx) {
                let rect = cx.global::<Shared>().tray.icon_rect();
                panel::toggle(Anchor::Tray(rect), cx);
            }
        }
        TrayAction::ShowWindow => view::show_window(cx),
        TrayAction::Refresh => cx.global::<Shared>().monitor.send(Command::RefreshAll),
        TrayAction::OpenRepo(path) => cx.open_with_system(&path),
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
        TrayAction::SetAutostart(enabled) => {
            if let Err(e) = crate::autostart::set_enabled(enabled) {
                crate::error_box(format!("Couldn't change the startup setting: {e}"));
            }
            cx.global::<Shared>()
                .tray
                .set_autostart_checked(crate::autostart::is_enabled());
        }
        TrayAction::Quit => {
            let shared = cx.global::<Shared>();
            shared.monitor.send(Command::Shutdown);
            shared.tray.hide();
            cx.quit();
            return false;
        }
    }
    true
}

/// Folder icons change the user's repo folders, so turning them on from the
/// menu asks first. A hand edit of config.json is consent enough.
fn ask_folder_icons(cx: &mut App) {
    let cfg = cx.global::<Ui>().state.read(cx).config.clone();
    let shared = cx.global_mut::<Shared>();
    // Windows has already ticked the item; untick it until the answer.
    shared.tray.sync_settings(&cfg);
    if shared.consent_pending {
        return;
    }
    shared.consent_pending = true;
    let answer = win::confirm_box(FOLDER_ICONS_CONSENT.to_string());
    let wait = cx
        .background_executor()
        .spawn(async move { answer.join().unwrap_or(false) });
    cx.spawn(async move |cx| {
        let yes = wait.await;
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

/// A menu toggle: change one field, save, apply. A save that is refused
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
        // The menu may have flipped a check mark on its own.
        cx.global::<Shared>().tray.sync_settings(&old);
        return;
    }
    if let Err(e) = new.save(&file) {
        let shared = cx.global_mut::<Shared>();
        if !shared.save_notice_shown {
            shared.save_notice_shown = true;
            crate::error_box(format!(
                "Couldn't save config.json, so this change lasts only until config.json \
                 is fixed and reloaded, or gitstat quits.\n\n{e}"
            ));
        }
    }
    cx.global::<Shared>()
        .monitor
        .send(Command::Apply(new.clone()));
    apply_config(&old, new, cx);
}

/// Makes the app match `new`. Idempotent, so startup applies the file
/// config over the defaults. The monitor reads `path`, `watch` and the
/// interval from the file itself, and from `change_setting`.
fn apply_config(old: &Config, new: Config, cx: &mut App) {
    let state = cx.global::<Ui>().state.clone();
    state.update(cx, |s, cx| {
        s.config = new.clone();
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
    // Turning icons off reverts even before the first result.
    let sync_icons = new.folder_icons != old.folder_icons && (s.has_result || !new.folder_icons);
    let statuses = s.statuses.clone();
    let shared = cx.global::<Shared>();
    if sync_icons {
        shared.icons.sync(statuses, new.folder_icons);
    }
    shared.tray.sync_settings(&new);
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
fn local_time() -> String {
    let mut t = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}
