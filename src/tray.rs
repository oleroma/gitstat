//! The notification-area icon and its right-click menu.

use crate::git::RepoStatus;
use crate::icon::{self, Badge};
use std::collections::HashMap;
use std::path::PathBuf;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Something the user did in the tray.
pub enum TrayAction {
    ShowWindow,
    Refresh,
    OpenRepo(PathBuf),
    SetAutostart(bool),
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    badge: Badge,
    show_id: MenuId,
    refresh_id: MenuId,
    autostart: CheckMenuItem,
    quit_id: MenuId,
    repo_ids: HashMap<MenuId, PathBuf>,
}

/// What the tray needs to know to redraw itself.
pub struct TraySnapshot<'a> {
    pub statuses: &'a [RepoStatus],
    pub checking: bool,
    pub has_result: bool,
    pub problem: Option<&'a str>,
}

impl Tray {
    pub fn new() -> Result<Self, String> {
        let icon = TrayIconBuilder::new()
            .with_tooltip("gitstat: checking…")
            .with_icon(make_icon(Badge::Checking))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        let mut tray = Self {
            icon,
            badge: Badge::Checking,
            show_id: MenuId::new("show"),
            refresh_id: MenuId::new("refresh"),
            autostart: CheckMenuItem::with_id(
                "autostart",
                "Start with Windows",
                true,
                crate::autostart::is_enabled(),
                None,
            ),
            quit_id: MenuId::new("quit"),
            repo_ids: HashMap::new(),
        };
        tray.update(&TraySnapshot {
            statuses: &[],
            checking: true,
            has_result: false,
            problem: None,
        });
        Ok(tray)
    }

    pub fn update(&mut self, snap: &TraySnapshot) {
        let unsynced = snap.statuses.iter().filter(|s| !s.is_synced()).count();
        let badge = if !snap.has_result {
            Badge::Checking
        } else if unsynced == 0 && snap.problem.is_none() {
            Badge::AllSynced
        } else {
            Badge::Unsynced(unsynced)
        };
        if badge != self.badge {
            let _ = self.icon.set_icon(Some(make_icon(badge)));
            self.badge = badge;
        }

        let summary = summary(snap, unsynced);
        // The tooltip is capped at 127 characters by Windows.
        let tooltip: String = format!("gitstat: {summary}").chars().take(127).collect();
        let _ = self.icon.set_tooltip(Some(tooltip));

        let menu = Menu::new();
        let _ = menu.append(&MenuItem::new(summary, false, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        self.repo_ids.clear();
        for (i, status) in snap.statuses.iter().enumerate() {
            let id = MenuId::new(format!("repo:{i}"));
            let text = format!("{}\t{}", status.name, status.label()).replace('&', "&&");
            let _ = menu.append(&MenuItem::with_id(id.clone(), text, true, None));
            self.repo_ids.insert(id, status.path.clone());
        }
        if !snap.statuses.is_empty() {
            let _ = menu.append(&PredefinedMenuItem::separator());
        }
        let _ = menu.append(&MenuItem::with_id(
            self.show_id.clone(),
            "Show status window",
            true,
            None,
        ));
        let refresh_text = if snap.checking {
            "Checking…"
        } else {
            "Refresh now"
        };
        let _ = menu.append(&MenuItem::with_id(
            self.refresh_id.clone(),
            refresh_text,
            !snap.checking,
            None,
        ));
        let _ = menu.append(&self.autostart);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id(self.quit_id.clone(), "Quit", true, None));
        self.icon.set_menu(Some(Box::new(menu)));
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
            if id == self.show_id {
                actions.push(TrayAction::ShowWindow);
            } else if id == self.refresh_id {
                actions.push(TrayAction::Refresh);
            } else if id == self.quit_id {
                actions.push(TrayAction::Quit);
            } else if id == *self.autostart.id() {
                actions.push(TrayAction::SetAutostart(self.autostart.is_checked()));
            } else if let Some(path) = self.repo_ids.get(&id) {
                actions.push(TrayAction::OpenRepo(path.clone()));
            }
        }
        actions
    }

    pub fn set_autostart_checked(&self, checked: bool) {
        self.autostart.set_checked(checked);
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

fn make_icon(badge: Badge) -> Icon {
    Icon::from_rgba(icon::render(badge), icon::SIZE, icon::SIZE)
        .expect("icon buffer has the right size")
}
