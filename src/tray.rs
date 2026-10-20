//! The notification-area icon and its right-click menu.

use crate::git::RepoStatus;
use crate::icon::{self, Badge};
use std::path::PathBuf;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Something the user did in the tray.
pub enum TrayAction {
    ShowWindow,
    Refresh,
    OpenRepo(PathBuf),
    SetAutostart(bool),
    Quit,
}

/// The menu is built once and changed in place. Replacing it with
/// `set_menu` while it's open would destroy the menu the user is looking at.
pub struct Tray {
    icon: TrayIcon,
    badge: Badge,
    tooltip: String,
    menu: Menu,
    summary: MenuItem,
    /// One item per repo, inserted after `summary` and its separator.
    repos: Vec<(MenuItem, PathBuf, String)>,
    no_repos: MenuItem,
    show: MenuItem,
    refresh: MenuItem,
    autostart: CheckMenuItem,
    quit: MenuItem,
}

/// What the tray needs to know to redraw itself.
pub struct TraySnapshot<'a> {
    pub statuses: &'a [RepoStatus],
    pub checking: bool,
    pub has_result: bool,
    pub problem: Option<&'a str>,
}

/// Menu positions: summary, separator, then the repo items.
const FIRST_REPO: usize = 2;

impl Tray {
    pub fn new() -> Result<Self, String> {
        let menu = Menu::new();
        let summary = MenuItem::new("gitstat: checking…", false, None);
        let no_repos = MenuItem::new("Checking repos…", false, None);
        let show = MenuItem::new("Show status window", true, None);
        let refresh = MenuItem::new("Checking…", false, None);
        let autostart = CheckMenuItem::new(
            "Start with Windows",
            true,
            crate::autostart::is_enabled(),
            None,
        );
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[
            &summary,
            &PredefinedMenuItem::separator(),
            &no_repos,
            &PredefinedMenuItem::separator(),
            &show,
            &refresh,
            &autostart,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| e.to_string())?;

        let icon = TrayIconBuilder::new()
            .with_tooltip("gitstat: checking…")
            .with_icon(make_icon(Badge::Checking))
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            icon,
            badge: Badge::Checking,
            tooltip: String::new(),
            menu,
            summary,
            repos: Vec::new(),
            no_repos,
            show,
            refresh,
            autostart,
            quit,
        })
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
        if badge != self.badge {
            let _ = self.icon.set_icon(Some(make_icon(badge)));
            self.badge = badge;
        }

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

    /// Drains pending tray clicks and menu picks.
    pub fn poll(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
                | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => actions.push(TrayAction::ShowWindow),
                _ => {}
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id;
            if id == *self.show.id() {
                actions.push(TrayAction::ShowWindow);
            } else if id == *self.refresh.id() {
                actions.push(TrayAction::Refresh);
            } else if id == *self.quit.id() {
                actions.push(TrayAction::Quit);
            } else if id == *self.autostart.id() {
                actions.push(TrayAction::SetAutostart(self.autostart.is_checked()));
            } else if let Some((_, path, _)) =
                self.repos.iter().find(|(item, _, _)| *item.id() == id)
            {
                actions.push(TrayAction::OpenRepo(path.clone()));
            }
        }
        actions
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
    match (snap.statuses.len(), unsynced) {
        (0, _) => "no repos found".to_string(),
        (1, 0) => "the repo is current".to_string(),
        (total, 0) => format!("all {total} repos current"),
        (total, n) => format!("{n} of {total} repos unsynced"),
    }
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

fn make_icon(badge: Badge) -> Icon {
    Icon::from_rgba(icon::render(badge), icon::SIZE, icon::SIZE)
        .expect("icon buffer has the right size")
}
