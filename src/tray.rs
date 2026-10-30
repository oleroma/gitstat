//! The notification-area icon and its right-click menu.

use crate::config::{ClickAction, Config, Corner, FolderIconMode};
use crate::git::RepoStatus;
use crate::icon::{self, Badge};
use crate::layout;
use crate::state::Input;
use crate::win;
use futures::channel::mpsc::UnboundedSender;
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Routes tray clicks and menu picks into the UI loop's channel. Call it once,
/// before `Tray::new`: the handlers live in a `OnceCell`, and events sent
/// before it is set go to tray-icon's own channel instead.
///
/// The handlers run inside the tray window's WndProc, where a panic would
/// unwind into Windows, so their bodies are caught.
pub fn install_handlers(tx: UnboundedSender<Input>) {
    let tray_tx = tx.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // Hover events would wake the UI loop for nothing.
        if matches!(
            event,
            TrayIconEvent::Click { .. } | TrayIconEvent::DoubleClick { .. }
        ) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let _ = tray_tx.unbounded_send(Input::Tray(event));
            }));
        }
    }));
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _ = tx.unbounded_send(Input::Menu(event));
        }));
    }));
}

/// Something the user did in the tray.
#[derive(Clone, Debug, PartialEq)]
pub enum TrayAction {
    /// A left click, with the icon's rect in physical px.
    LeftClick(Option<layout::Rect>),
    DoubleClick,
    OpenPanel,
    ShowWindow,
    Refresh,
    OpenRepo(PathBuf),
    EditConfig,
    Toggle(Setting),
    SetCorner(Corner),
    SetFolderIconMode(FolderIconMode),
    SetAutostart(bool),
    Quit,
}

/// The on/off settings the menu can flip.
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
        };
        *field = !*field;
    }
}

const CORNERS: [(Corner, &str); 4] = [
    (Corner::TopLeft, "Top left"),
    (Corner::TopRight, "Top right"),
    (Corner::BottomLeft, "Bottom left"),
    (Corner::BottomRight, "Bottom right"),
];

const FOLDER_ICON_MODES: [(FolderIconMode, &str); 2] = [
    (FolderIconMode::Status, "Status folder"),
    (FolderIconMode::RepoSvg, "Repo icon"),
];

/// The menu is built once and changed in place. Replacing it with
/// `set_menu` while it's open would destroy the menu the user is looking at.
pub struct Tray {
    icon: TrayIcon,
    badge: Badge,
    /// The pixel size `badge` was drawn at: the tray's small-icon size.
    icon_size: u32,
    tooltip: String,
    menu: Menu,
    summary: MenuItem,
    /// One item per repo, inserted after `summary` and its separator.
    repos: Vec<(MenuItem, PathBuf, String)>,
    no_repos: MenuItem,
    open_panel: MenuItem,
    open_window: MenuItem,
    /// Whether `open_panel` and `open_window` are in the menu right now.
    panel_shown: Cell<bool>,
    window_shown: Cell<bool>,
    refresh: MenuItem,
    /// Settings that live in the Display submenu, plus Watch for changes.
    settings: Vec<(CheckMenuItem, Setting)>,
    corner_menu: Submenu,
    corners: Vec<(CheckMenuItem, Corner)>,
    folder_icon_menu: Submenu,
    folder_icon_modes: Vec<(CheckMenuItem, FolderIconMode)>,
    autostart: CheckMenuItem,
    edit_config: MenuItem,
    quit: MenuItem,
}

/// What the tray needs to know to redraw itself.
pub struct TraySnapshot<'a> {
    pub statuses: &'a [RepoStatus],
    pub checking: bool,
    pub has_result: bool,
    pub problem: Option<&'a str>,
    /// A discovery cap cut the repo list short.
    pub incomplete: bool,
}

/// Menu positions: summary, separator, then the repo items.
const FIRST_REPO: usize = 2;

impl Tray {
    /// Starts with the default config's check marks; `sync_settings` puts in
    /// the real ones.
    pub fn new() -> Result<Self, String> {
        let defaults = Config::default();
        let menu = Menu::new();
        let summary = MenuItem::new("gitstat: checking…", false, None);
        let no_repos = MenuItem::new("Checking repos…", false, None);
        let open_panel = MenuItem::new("Open status panel", true, None);
        let open_window = MenuItem::new("Open status window", true, None);
        let refresh = MenuItem::new("Checking…", false, None);

        let check = |label: &str, setting: Setting| {
            let item = CheckMenuItem::new(label, true, setting.is_on(&defaults), None);
            (item, setting)
        };
        let panel = check("Status panel on click", Setting::Panel);
        let window = check("Status window", Setting::Window);
        let icons_only = check("Icons only (panel and badge)", Setting::IconsOnly);
        let overlay = check("Corner badge", Setting::Overlay);
        let hide_synced = check("Hide badge when all synced", Setting::OverlayHideSynced);
        let hide_fullscreen = check("Hide badge in fullscreen", Setting::OverlayHideFullscreen);
        let folder_icons = check("Folder icons in Explorer", Setting::FolderIcons);
        let watch = check("Watch for changes", Setting::Watch);

        let corner_menu = Submenu::new("Badge corner", defaults.overlay.enabled);
        let corners: Vec<(CheckMenuItem, Corner)> = CORNERS
            .iter()
            .map(|&(corner, label)| {
                let item = CheckMenuItem::new(label, true, corner == defaults.overlay.corner, None);
                (item, corner)
            })
            .collect();
        for (item, _) in &corners {
            corner_menu.append(item).map_err(|e| e.to_string())?;
        }
        let folder_icon_menu = Submenu::new("Folder icon style", defaults.folder_icons);
        let folder_icon_modes: Vec<(CheckMenuItem, FolderIconMode)> = FOLDER_ICON_MODES
            .iter()
            .map(|&(mode, label)| {
                let on = mode == defaults.folder_icon_mode;
                (CheckMenuItem::new(label, true, on, None), mode)
            })
            .collect();
        for (item, _) in &folder_icon_modes {
            folder_icon_menu.append(item).map_err(|e| e.to_string())?;
        }

        let display = Submenu::new("Display", true);
        display
            .append_items(&[
                &panel.0,
                &window.0,
                &icons_only.0,
                &PredefinedMenuItem::separator(),
                &overlay.0,
                &corner_menu,
                &hide_synced.0,
                &hide_fullscreen.0,
                &PredefinedMenuItem::separator(),
                &folder_icons.0,
                &folder_icon_menu,
            ])
            .map_err(|e| e.to_string())?;

        let autostart = CheckMenuItem::new(
            "Start with Windows",
            true,
            crate::autostart::is_enabled(),
            None,
        );
        let edit_config = MenuItem::new("Edit config.json", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[
            &summary,
            &PredefinedMenuItem::separator(),
            &no_repos,
            &PredefinedMenuItem::separator(),
            &open_panel,
            &open_window,
            &refresh,
            &display,
            &watch.0,
            &autostart,
            &edit_config,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| e.to_string())?;

        let icon_size = win::tray_icon_size();
        let icon = TrayIconBuilder::new()
            .with_tooltip("gitstat: checking…")
            .with_icon(make_icon(Badge::Checking, icon_size)?)
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        let tray = Self {
            icon,
            badge: Badge::Checking,
            icon_size,
            tooltip: String::new(),
            menu,
            summary,
            repos: Vec::new(),
            no_repos,
            open_panel,
            open_window,
            panel_shown: Cell::new(true),
            window_shown: Cell::new(true),
            refresh,
            settings: vec![
                panel,
                window,
                icons_only,
                overlay,
                hide_synced,
                hide_fullscreen,
                folder_icons,
                watch,
            ],
            corner_menu,
            corners,
            folder_icon_menu,
            folder_icon_modes,
            autostart,
            edit_config,
            quit,
        };
        tray.sync_settings(&defaults);
        Ok(tray)
    }

    pub fn update(&mut self, snap: &TraySnapshot) {
        let unsynced = snap.statuses.iter().filter(|s| !s.is_synced()).count();
        let badge = if !snap.has_result {
            Badge::Checking
        } else if snap.problem.is_some() {
            Badge::Problem
        } else if unsynced == 0 {
            Badge::AllSynced
        } else {
            Badge::Unsynced(unsynced)
        };
        self.set_badge(badge, win::tray_icon_size());

        let summary = summary(snap, unsynced);
        // Windows caps tooltips at 127 characters.
        let tooltip: String = format!("gitstat: {summary}").chars().take(127).collect();
        if tooltip != self.tooltip {
            let _ = self.icon.set_tooltip(Some(&tooltip));
            self.tooltip = tooltip;
        }
        set_text(&self.summary, &summary.replace('&', "&&"));
        set_text(
            &self.refresh,
            if snap.checking {
                "Checking…"
            } else {
                "Refresh now"
            },
        );
        self.refresh.set_enabled(!snap.checking);

        // Repo items: keep the ones whose path is unchanged, so an open menu
        // only sees its labels change.
        let same_repos = self.repos.len() == snap.statuses.len()
            && self
                .repos
                .iter()
                .zip(snap.statuses)
                .all(|((_, path, _), s)| *path == s.path);
        if !same_repos && snap.has_result {
            for (item, _, _) in self.repos.drain(..) {
                let _ = self.menu.remove(&item);
            }
            let _ = self.menu.remove(&self.no_repos);
            for (i, status) in snap.statuses.iter().enumerate() {
                let item = MenuItem::new(repo_text(status), true, None);
                let _ = self.menu.insert(&item, FIRST_REPO + i);
                self.repos
                    .push((item, status.path.clone(), repo_text(status)));
            }
            if snap.statuses.is_empty() {
                set_text(&self.no_repos, "No repos found");
                let _ = self.menu.insert(&self.no_repos, FIRST_REPO);
            }
        } else {
            for ((item, _, text), status) in self.repos.iter_mut().zip(snap.statuses) {
                let new_text = repo_text(status);
                if *text != new_text {
                    item.set_text(&new_text);
                    *text = new_text;
                }
            }
        }
    }

    /// Redraws the icon at `size` px when it or the badge changed. Drawing
    /// at the tray's own size keeps Windows from scaling it, which blurs it
    /// and makes the edges jagged.
    fn set_badge(&mut self, badge: Badge, size: u32) {
        if (badge, size) != (self.badge, self.icon_size)
            && let Ok(icon) = make_icon(badge, size)
        {
            let _ = self.icon.set_icon(Some(icon));
            self.badge = badge;
            self.icon_size = size;
        }
    }

    /// Follows a DPI change of the primary monitor, which changes the
    /// tray's icon size. Two Win32 queries; cheap enough for every input.
    pub fn check_icon_size(&mut self) {
        self.set_badge(self.badge, win::tray_icon_size());
    }

    /// Turns a tray click or menu pick into what the app should do. Anything
    /// else, including clicks that aren't left clicks, gives `None`.
    pub fn map(&self, input: &Input) -> Option<TrayAction> {
        match input {
            Input::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                position,
                ..
            }) => Some(TrayAction::LeftClick(self.anchor(rect, position))),
            Input::Tray(TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }) => Some(TrayAction::DoubleClick),
            Input::Menu(event) => self.map_menu(event),
            _ => None,
        }
    }

    fn map_menu(&self, event: &MenuEvent) -> Option<TrayAction> {
        let id = &event.id;
        if id == self.open_panel.id() {
            Some(TrayAction::OpenPanel)
        } else if id == self.open_window.id() {
            Some(TrayAction::ShowWindow)
        } else if id == self.refresh.id() {
            Some(TrayAction::Refresh)
        } else if id == self.edit_config.id() {
            Some(TrayAction::EditConfig)
        } else if id == self.quit.id() {
            Some(TrayAction::Quit)
        } else if id == self.autostart.id() {
            // Windows has already flipped the check mark.
            Some(TrayAction::SetAutostart(self.autostart.is_checked()))
        } else if let Some((_, setting)) = self.settings.iter().find(|(item, _)| item.id() == id) {
            Some(TrayAction::Toggle(*setting))
        } else if let Some((_, corner)) = self.corners.iter().find(|(item, _)| item.id() == id) {
            // Picking the checked corner again unchecks it; keep exactly one.
            self.check_corner(*corner);
            Some(TrayAction::SetCorner(*corner))
        } else if let Some((_, mode)) = self
            .folder_icon_modes
            .iter()
            .find(|(item, _)| item.id() == id)
        {
            // Like the corners: exactly one stays checked.
            self.check_folder_icon_mode(*mode);
            Some(TrayAction::SetFolderIconMode(*mode))
        } else {
            self.repos
                .iter()
                .find(|(item, _, _)| item.id() == id)
                .map(|(_, path, _)| TrayAction::OpenRepo(path.clone()))
        }
    }

    /// The icon's rect in physical px, for anchoring the panel when it is
    /// opened from the menu.
    pub fn icon_rect(&self) -> Option<layout::Rect> {
        self.icon.rect().as_ref().and_then(to_layout_rect)
    }

    /// The icon's rect in physical px. tray-icon fills it in on every click
    /// it delivers, but fall back to asking again, then to the cursor.
    fn anchor(
        &self,
        rect: &tray_icon::Rect,
        position: &tray_icon::dpi::PhysicalPosition<f64>,
    ) -> Option<layout::Rect> {
        to_layout_rect(rect)
            .or_else(|| self.icon_rect())
            .or_else(|| point_rect(position.x, position.y))
    }

    /// Makes the menu match `cfg`. Cheap enough to call after every change:
    /// it only touches what differs.
    pub fn sync_settings(&self, cfg: &Config) {
        for (item, setting) in &self.settings {
            set_checked(item, setting.is_on(cfg));
        }
        self.check_corner(cfg.overlay.corner);
        self.check_folder_icon_mode(cfg.folder_icon_mode);
        self.folder_icon_menu.set_enabled(cfg.folder_icons);

        let badge = cfg.overlay.enabled;
        self.corner_menu.set_enabled(badge);
        for (item, setting) in &self.settings {
            if matches!(
                setting,
                Setting::OverlayHideSynced | Setting::OverlayHideFullscreen
            ) {
                item.set_enabled(badge);
            }
        }

        // Window first: the panel item sits just above it.
        self.show_item(&self.open_window, &self.window_shown, cfg.window, 0);
        let above = usize::from(self.window_shown.get());
        self.show_item(&self.open_panel, &self.panel_shown, cfg.panel, above);

        self.icon
            .set_show_menu_on_left_click(cfg.left_click() == ClickAction::Menu);
    }

    /// Adds or removes `item` so that it sits `above` items before Refresh.
    fn show_item(&self, item: &MenuItem, shown: &Cell<bool>, want: bool, above: usize) {
        if shown.get() == want {
            return;
        }
        let ok = if want {
            let refresh = self
                .menu
                .items()
                .iter()
                .position(|k| k.id() == self.refresh.id());
            match refresh {
                Some(i) => self.menu.insert(item, i.saturating_sub(above)).is_ok(),
                None => false,
            }
        } else {
            self.menu.remove(item).is_ok()
        };
        if ok {
            shown.set(want);
        }
    }

    fn check_corner(&self, corner: Corner) {
        for (item, c) in &self.corners {
            set_checked(item, *c == corner);
        }
    }

    fn check_folder_icon_mode(&self, mode: FolderIconMode) {
        for (item, m) in &self.folder_icon_modes {
            set_checked(item, *m == mode);
        }
    }

    pub fn set_autostart_checked(&self, checked: bool) {
        self.autostart.set_checked(checked);
    }

    /// Removes the icon from the notification area right away. Otherwise it
    /// lingers until the mouse passes over it.
    pub fn hide(&self) {
        let _ = self.icon.set_visible(false);
    }
}

pub fn summary(snap: &TraySnapshot, unsynced: usize) -> String {
    if let Some(problem) = snap.problem {
        return problem.to_string();
    }
    if !snap.has_result {
        return "checking…".to_string();
    }
    // "500+": the walk stopped early, so there may be more.
    let more = if snap.incomplete { "+" } else { "" };
    match (snap.statuses.len(), unsynced) {
        (0, _) if snap.incomplete => "no repos found (search stopped early)".to_string(),
        (0, _) => "no repos found".to_string(),
        (1, 0) if !snap.incomplete => "the repo is current".to_string(),
        (total, 0) => format!("all {total}{more} repos current"),
        (total, n) => format!("{n} of {total}{more} repos unsynced"),
    }
}

/// `None` for the empty rect tray-icon reports when it has none.
fn to_layout_rect(r: &tray_icon::Rect) -> Option<layout::Rect> {
    let (x, y) = (r.position.x, r.position.y);
    if r.size.width == 0 || r.size.height == 0 || !x.is_finite() || !y.is_finite() {
        return None;
    }
    let (left, top) = (x.round() as i32, y.round() as i32);
    Some(layout::Rect::new(
        left,
        top,
        left.saturating_add_unsigned(r.size.width),
        top.saturating_add_unsigned(r.size.height),
    ))
}

fn point_rect(x: f64, y: f64) -> Option<layout::Rect> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let (x, y) = (x.round() as i32, y.round() as i32);
    Some(layout::Rect::new(
        x,
        y,
        x.saturating_add(1),
        y.saturating_add(1),
    ))
}

fn repo_text(status: &RepoStatus) -> String {
    // A tab right-aligns the status in Windows menus; `&` would mark a mnemonic.
    format!("{}\t{}", status.name, status.label()).replace('&', "&&")
}

fn set_text(item: &MenuItem, text: &str) {
    if item.text() != text {
        item.set_text(text);
    }
}

fn set_checked(item: &CheckMenuItem, checked: bool) {
    if item.is_checked() != checked {
        item.set_checked(checked);
    }
}

/// Straight RGBA, which is what `CreateIcon` (tray-icon's `from_rgba`) takes.
fn make_icon(badge: Badge, size: u32) -> Result<Icon, String> {
    Icon::from_rgba(icon::render(badge, size), size, size).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tray_icon::dpi::{PhysicalPosition, PhysicalSize};

    #[test]
    fn click_rect_converts_and_empty_is_none() {
        let r = tray_icon::Rect {
            size: PhysicalSize::new(24, 40),
            position: PhysicalPosition::new(-100.4, 1040.6),
        };
        assert_eq!(
            to_layout_rect(&r),
            Some(layout::Rect::new(-100, 1041, -76, 1081))
        );
        assert_eq!(to_layout_rect(&tray_icon::Rect::default()), None);
        assert_eq!(point_rect(f64::NAN, 0.0), None);
        assert_eq!(point_rect(5.0, 6.0), Some(layout::Rect::new(5, 6, 6, 7)));
    }

    #[test]
    fn summary_marks_a_capped_list() {
        let repo = |dirty| RepoStatus {
            has_upstream: true,
            dirty,
            ..Default::default()
        };
        let statuses = [repo(true), repo(false)];
        let snap = |statuses, incomplete| TraySnapshot {
            statuses,
            checking: false,
            has_result: true,
            problem: None,
            incomplete,
        };
        assert_eq!(summary(&snap(&statuses, true), 1), "1 of 2+ repos unsynced");
        assert_eq!(summary(&snap(&statuses, false), 1), "1 of 2 repos unsynced");
        assert_eq!(
            summary(&snap(&[], true), 0),
            "no repos found (search stopped early)"
        );
        assert_eq!(summary(&snap(&[], false), 0), "no repos found");
    }

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
}
