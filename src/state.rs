//! The app's shared state and the one input type the UI loop consumes.

use crate::config::Config;
use crate::git::RepoStatus;
use crate::icon::Badge;
use crate::monitor::WatchMode;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// Only the Windows UI reads most fields; Linux builds this for its tests.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct AppState {
    pub config_file: PathBuf,
    pub config: Config,
    pub config_error: Option<String>,
    /// Sorted by name (the path below the root), as `find_repos` returns
    /// them.
    pub statuses: Vec<RepoStatus>,
    /// Why the repo list may be incomplete: a discovery cap was hit.
    pub capped: Option<String>,
    pub has_result: bool,
    /// A fetch sweep is running.
    pub sweeping: bool,
    /// Repos with a check running.
    pub in_flight: HashSet<PathBuf>,
    /// Finished checks by repo, counted even when the status came back the
    /// same: a new tag or stash changes the timeline but not the status.
    checks: HashMap<PathBuf, u64>,
    pub last_checked: Option<String>,
    pub mode: WatchMode,
    /// Folders the folder-icon worker skipped, with the reason.
    pub folder_skips: Vec<(PathBuf, String)>,
}

impl AppState {
    pub fn new(config_file: PathBuf, config: Config, config_error: Option<String>) -> Self {
        let mode = if config.watch {
            WatchMode::Events
        } else {
            WatchMode::Off
        };
        Self {
            config_file,
            config,
            config_error,
            statuses: Vec::new(),
            capped: None,
            has_result: false,
            sweeping: false,
            in_flight: HashSet::new(),
            checks: HashMap::new(),
            last_checked: None,
            mode,
            folder_skips: Vec::new(),
        }
    }

    pub fn unsynced(&self) -> usize {
        self.statuses.iter().filter(|s| !s.is_synced()).count()
    }

    /// A problem worth showing instead of the repo summary.
    pub fn problem(&self) -> Option<String> {
        if let Some(e) = &self.config_error {
            return Some(e.clone());
        }
        let dir = self.config.repos_dir();
        if self.has_result && !dir.is_dir() {
            return Some(format!("folder not found: {}", dir.display()));
        }
        None
    }

    pub fn badge(&self) -> Badge {
        if !self.has_result {
            Badge::Checking
        } else if self.problem().is_some() {
            Badge::Problem
        } else {
            match self.unsynced() {
                0 => Badge::AllSynced,
                n => Badge::Unsynced(n),
            }
        }
    }

    /// Replaces the status with the same path, or inserts it in name order
    /// (the order `find_repos` uses). A repo whose name changed (a new root
    /// above it) moves to its new place. Returns whether anything changed.
    pub fn merge_status(&mut self, status: RepoStatus) -> bool {
        if let Some(i) = self.statuses.iter().position(|s| s.path == status.path) {
            let existing = &mut self.statuses[i];
            if *existing == status {
                return false;
            }
            if existing.name == status.name {
                *existing = status;
                return true;
            }
            self.statuses.remove(i);
        }
        let key = status.name.to_lowercase();
        let at = self
            .statuses
            .partition_point(|s| s.name.to_lowercase() < key);
        self.statuses.insert(at, status);
        true
    }

    /// Counts a finished check of `path`.
    pub fn checked(&mut self, path: &Path) {
        *self.checks.entry(path.to_path_buf()).or_default() += 1;
    }

    /// How many checks of `path` have finished; the timeline reloads a
    /// strip when this moves past what it last loaded.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn checks(&self, path: &Path) -> u64 {
        self.checks.get(path).copied().unwrap_or(0)
    }

    /// Forgets repos no longer in the list.
    pub fn retain_repos(&mut self, keep: impl Fn(&Path) -> bool) {
        self.statuses.retain(|s| keep(&s.path));
        self.in_flight.retain(|p| keep(p));
        self.checks.retain(|p, _| keep(p));
    }

    /// Worst first, then by name.
    pub fn sorted_for_panel(&self) -> Vec<&RepoStatus> {
        let mut sorted: Vec<&RepoStatus> = self.statuses.iter().collect();
        sorted.sort_by_cached_key(|s| (severity(s), s.name.to_lowercase()));
        sorted
    }
}

/// 0 is worst: error, diverged, behind, ahead, dirty, no upstream, synced.
fn severity(s: &RepoStatus) -> u8 {
    if s.error.is_some() {
        0
    } else if s.is_diverged() {
        1
    } else if s.behind > 0 {
        2
    } else if s.ahead > 0 {
        3
    } else if s.dirty {
        4
    } else if !s.has_upstream {
        5
    } else {
        6
    }
}

/// Everything the UI loop reacts to, funnelled through one channel.
#[cfg(windows)]
pub enum Input {
    Tray(tray_icon::TrayIconEvent),
    Menu(tray_icon::menu::MenuEvent),
    Monitor(crate::monitor::Update),
    /// A click on the badge.
    Overlay,
    /// The badge was dragged and dropped with its disc here (physical px).
    OverlayMoved(crate::layout::Rect),
    /// The folder-icon worker's report: skipped folders and why.
    FolderSkips(Vec<(PathBuf, String)>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> RepoStatus {
        RepoStatus {
            name: name.into(),
            path: PathBuf::from(name),
            has_upstream: true,
            ..Default::default()
        }
    }

    fn state(statuses: Vec<RepoStatus>) -> AppState {
        let config = Config {
            path: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let mut state = AppState::new(PathBuf::from("config.json"), config, None);
        state.statuses = statuses;
        state.has_result = true;
        state
    }

    #[test]
    fn panel_order_is_worst_first_then_name() {
        let synced = repo("a-synced");
        let dirty = RepoStatus {
            dirty: true,
            ..repo("B-dirty")
        };
        let ahead = RepoStatus {
            ahead: 1,
            ..repo("c-ahead")
        };
        let error = RepoStatus {
            error: Some("x".into()),
            ..repo("d-error")
        };
        let diverged = RepoStatus {
            ahead: 1,
            behind: 1,
            ..repo("e-diverged")
        };
        let dirty2 = RepoStatus {
            dirty: true,
            ..repo("a-dirty")
        };
        let s = state(vec![synced, dirty, ahead, error, diverged, dirty2]);
        let names: Vec<&str> = s
            .sorted_for_panel()
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "d-error",
                "e-diverged",
                "c-ahead",
                "a-dirty",
                "B-dirty",
                "a-synced"
            ]
        );
    }

    #[test]
    fn merge_replaces_by_path_and_inserts_in_name_order() {
        let mut s = state(vec![repo("a"), repo("C")]);
        assert!(s.merge_status(repo("b")));
        assert!(!s.merge_status(repo("b")));
        assert!(s.merge_status(RepoStatus {
            dirty: true,
            ..repo("C")
        }));
        assert!(s.merge_status(repo("d")));
        let names: Vec<&str> = s.statuses.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "C", "d"]);
        assert!(s.statuses[2].dirty);
        // A new root renames a repo: it moves rather than staying put.
        assert!(s.merge_status(RepoStatus {
            name: "z".into(),
            ..repo("a")
        }));
        let names: Vec<&str> = s.statuses.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["b", "C", "d", "z"]);
        assert_eq!(s.statuses.len(), 4);
    }

    #[test]
    fn every_check_counts_even_when_the_status_is_unchanged() {
        let mut s = state(vec![repo("a")]);
        let a = Path::new("a");
        assert_eq!(s.checks(a), 0);
        assert!(!s.merge_status(repo("a")));
        s.checked(a);
        s.checked(a);
        assert_eq!(s.checks(a), 2);
        assert_eq!(s.checks(Path::new("b")), 0);
        s.retain_repos(|p| p != a);
        assert_eq!(s.checks(a), 0);
        assert!(s.statuses.is_empty());
    }

    #[test]
    fn badge_follows_results_and_problems() {
        let mut s = state(vec![repo("a")]);
        assert_eq!(s.badge(), Badge::AllSynced);
        s.statuses.push(RepoStatus {
            behind: 2,
            ..repo("b")
        });
        assert_eq!(s.badge(), Badge::Unsynced(1));
        s.config_error = Some("bad".into());
        assert_eq!(s.badge(), Badge::Problem);
        s.has_result = false;
        assert_eq!(s.badge(), Badge::Checking);
    }
}
