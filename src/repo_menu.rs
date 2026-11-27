//! The menu a right-click on a repo opens, in the panel and the big
//! window: open it somewhere, copy its branch or path, fetch it now, or hide
//! it. Nothing here changes the repo.
//!
//! The menu runs on a thread of its own, like `error_box`: its modal loop
//! would otherwise run inside GPUI's event handling, whose window callbacks
//! can't reach the App while a handler holds it. The thread owns a hidden
//! window for the menu, and sends the pick back as a `TrayAction`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, AppContext, ClipboardItem, Window};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DestroyMenu, DestroyWindow, GetCursorPos,
    IsWindow, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_NULL, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::git::RepoStatus;
use crate::state::Input;
use crate::tray::TrayAction;
use crate::view::Ui;

/// A menu is showing. The panel stays open meanwhile, though the menu's
/// window takes the focus from it.
static OPEN: AtomicBool = AtomicBool::new(false);

pub fn is_open() -> bool {
    OPEN.load(Ordering::SeqCst)
}

/// The repo a menu was opened for.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub path: PathBuf,
    pub name: String,
    pub branch: Option<String>,
    /// Only on GitHub: just its page, and hiding it.
    pub remote_only: bool,
    /// Pinned to the floating view, and whether there is one to pin to
    /// (the badge is on).
    pub pinned: bool,
    pub float: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoAction {
    OpenFolder,
    Terminal,
    Editor,
    GitHub,
    CopyBranch,
    CopyPath,
    Fetch,
    Hide,
    /// Pin it to the floating view, or take it off.
    Pin,
    Unpin,
}

impl RepoAction {
    /// The picks that open something elsewhere, so the panel closes.
    pub fn leaves(self) -> bool {
        matches!(
            self,
            Self::OpenFolder | Self::Terminal | Self::Editor | Self::GitHub
        )
    }
}

/// The menu's items for `target`: an action and its text, `None` for a
/// separator.
fn items(target: &Target) -> Vec<Option<(RepoAction, String)>> {
    use RepoAction::*;
    let item = |a, t: &str| Some((a, t.to_string()));
    let pin = || {
        if target.pinned {
            item(Unpin, "Unpin from the floating view")
        } else {
            item(Pin, "Pin to the floating view")
        }
    };
    if target.remote_only {
        let mut items = vec![item(GitHub, "Open on GitHub"), None];
        if target.float {
            items.push(pin());
        }
        items.push(item(Hide, "Hide this repo"));
        return items;
    }
    let mut items = vec![
        item(OpenFolder, "Open folder"),
        item(Terminal, "Open terminal here"),
    ];
    if editor().is_some() {
        items.push(item(Editor, "Open in VS Code"));
    }
    items.push(item(GitHub, "Open on GitHub"));
    items.push(None);
    if let Some(branch) = &target.branch {
        items.push(Some((CopyBranch, format!("Copy branch name ({branch})"))));
    }
    items.push(item(CopyPath, "Copy path"));
    items.push(None);
    items.push(item(Fetch, "Fetch now"));
    if target.float {
        items.push(pin());
    }
    items.push(item(Hide, "Hide this repo"));
    items
}

/// Shows the menu for `status` at the mouse. `window` gets the focus back
/// when the menu closes without leaving.
pub fn show(status: &RepoStatus, window: &Window, cx: &mut App) {
    show_from(status, crate::win::hwnd(window), cx);
}

/// Shows the menu for `status` at the mouse; `return_to`, if any, is the
/// window that gets the focus back.
pub fn show_from(status: &RepoStatus, return_to: Option<isize>, cx: &mut App) {
    let Some(ui) = cx.try_global::<Ui>() else {
        return;
    };
    let input = ui.input.clone();
    let state = ui.state.read(cx);
    let target = Target {
        path: status.path.clone(),
        name: status.name.clone(),
        branch: status.branch.clone(),
        remote_only: status.remote_only,
        pinned: state.pinned(status),
        float: state.config.overlay.enabled,
    };
    if OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("gitstat-repo-menu".into())
        .spawn(move || {
            let picked = unsafe { track(&items(&target)) };
            OPEN.store(false, Ordering::SeqCst);
            let leaves = picked.is_some_and(RepoAction::leaves);
            if !leaves && let Some(hwnd) = return_to {
                let hwnd = hwnd as _;
                unsafe {
                    if IsWindow(hwnd) != 0 {
                        SetForegroundWindow(hwnd);
                    }
                }
            }
            if let Some(action) = picked {
                let _ = input.unbounded_send(Input::Action(TrayAction::RepoMenu(action, target)));
            }
        });
    if spawned.is_err() {
        OPEN.store(false, Ordering::SeqCst);
    }
}

/// Runs the menu on this thread and returns the pick.
unsafe fn track(items: &[Option<(RepoAction, String)>]) -> Option<RepoAction> {
    unsafe {
        let mut at = POINT { x: 0, y: 0 };
        GetCursorPos(&mut at);
        let class: Vec<u16> = "STATIC".encode_utf16().chain(Some(0)).collect();
        let owner = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            at.x,
            at.y,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if owner.is_null() {
            return None;
        }
        let menu = CreatePopupMenu();
        if menu.is_null() {
            DestroyWindow(owner);
            return None;
        }
        for (i, item) in items.iter().enumerate() {
            match item {
                Some((_, text)) => {
                    // `&` marks an access key; the names are shown as they are.
                    let text: Vec<u16> = text
                        .replace('&', "&&")
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                    AppendMenuW(menu, MF_STRING, i + 1, text.as_ptr());
                }
                None => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                }
            }
        }
        // Without the foreground, a click elsewhere wouldn't close the menu.
        SetForegroundWindow(owner);
        let picked = TrackPopupMenuEx(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            owner,
            std::ptr::null(),
        );
        PostMessageW(owner, WM_NULL, 0, 0);
        DestroyMenu(menu);
        DestroyWindow(owner);
        let index = usize::try_from(picked).ok()?.checked_sub(1)?;
        items.get(index)?.as_ref().map(|(action, _)| *action)
    }
}

/// `code` on the PATH: VS Code's command-line launcher.
fn editor() -> Option<PathBuf> {
    static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| on_path(&["code.cmd", "code.exe"]))
        .clone()
}

fn on_path(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Runs the picks that need only the repo; the app runs Fetch and Hide.
pub fn run(action: RepoAction, target: &Target, cx: &mut App) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let path = target.path.as_path();
    match action {
        RepoAction::OpenFolder => cx.open_with_system(path),
        RepoAction::Terminal => {
            // Windows Terminal when it's there, else a PowerShell console.
            let spawned = match on_path(&["wt.exe"]) {
                Some(wt) => Command::new(wt).arg("-d").arg(path).spawn(),
                None => Command::new("powershell.exe")
                    .arg("-NoExit")
                    .current_dir(path)
                    .creation_flags(CREATE_NEW_CONSOLE)
                    .spawn(),
            };
            if let Err(e) = spawned {
                crate::error_box(format!(
                    "Couldn't open a terminal in {}.\n\n{e}",
                    path.display()
                ));
            }
        }
        RepoAction::Editor => {
            if let Some(code) = editor() {
                // Run directly, not through `cmd /c`: Rust quotes the
                // arguments of a .cmd for cmd itself (so a `&` or a space
                // in the folder name stays part of the path), but only when
                // the program it runs is the .cmd.
                let spawned = Command::new(code)
                    .arg(path)
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn();
                if let Err(e) = spawned {
                    crate::error_box(format!("Couldn't start VS Code.\n\n{e}"));
                }
            }
        }
        RepoAction::GitHub => open_github(path, cx),
        RepoAction::CopyBranch => {
            if let Some(branch) = &target.branch {
                cx.write_to_clipboard(ClipboardItem::new_string(branch.clone()));
            }
        }
        RepoAction::CopyPath => {
            cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        }
        RepoAction::Fetch | RepoAction::Hide | RepoAction::Pin | RepoAction::Unpin => {}
    }
}

/// The repo's GitHub page: its slug for a repo with no clone, the one its
/// drawn tree read, else read from its `origin` off the UI thread, which a
/// slow or busy disk would otherwise freeze. Its folder when it has none.
pub(crate) fn open_github(path: &Path, cx: &mut App) {
    if let Some(slug) = crate::github::remote_slug(path) {
        cx.open_url(&format!("https://github.com/{slug}"));
        return;
    }
    let known = cx
        .try_global::<Ui>()
        .and_then(|ui| ui.timelines.read(cx).web(path));
    if let Some(web) = known {
        cx.open_url(&web);
        return;
    }
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
}
