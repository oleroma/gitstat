//! The app's shared state and the one input type the UI loop consumes.

use crate::config::{BadgeNumber, Config};
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
    /// Each watched folder's repos and cap message, by its slot in
    /// `config.roots()`; `statuses` holds the repos of all of them.
    slots: Vec<(Vec<PathBuf>, Option<String>)>,
    /// Folders whose fetch sweep is running.
    sweeping_slots: HashSet<usize>,
    /// Each folder's watch mode; `mode` shows the worst.
    modes: Vec<WatchMode>,
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
    /// Each GitHub repo's pull requests from the last poll that could read
    /// it: open ones first, then recently merged and closed.
    pub prs: HashMap<PathBuf, Vec<crate::github::Pr>>,
    /// Why the last GitHub poll read nothing.
    pub github_problem: Option<String>,
    /// The GitHub account's repos with no clone here, most recently pushed
    /// first.
    pub remote_repos: Vec<crate::github::RemoteRepo>,
    /// The same repos as statuses, by name, so the timeline can list them
    /// after the cloned ones. Never counted as unsynced.
    pub remote_statuses: Vec<RepoStatus>,
    /// The list of those is folded: in the panel, in big mode. Folded at
    /// first, since an account can have many.
    remote_folded: [bool; 2],
    /// The pull request list is folded to its heading: in the panel, in
    /// big mode. Kept while gitstat runs, so the panel reopens the same.
    prs_folded: [bool; 2],
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
            slots: Vec::new(),
            sweeping_slots: HashSet::new(),
            modes: Vec::new(),
            has_result: false,
            sweeping: false,
            in_flight: HashSet::new(),
            checks: HashMap::new(),
            last_checked: None,
            mode,
            folder_skips: Vec::new(),
            prs: HashMap::new(),
            github_problem: None,
            remote_repos: Vec::new(),
            remote_statuses: Vec::new(),
            remote_folded: [true; 2],
            prs_folded: [false; 2],
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
        if !self.has_result {
            return None;
        }
        self.config
            .roots()
            .into_iter()
            .find(|dir| !dir.is_dir())
            .map(|dir| format!("folder not found: {}", dir.display()))
    }

    /// Red with the configured count while any repo is unsynced; green
    /// with it, or a tick when it is 0, once all are synced.
    pub fn badge(&self) -> Badge {
        if !self.has_result {
            Badge::Checking
        } else if self.problem().is_some() {
            Badge::Problem
        } else {
            match (self.unsynced(), self.badge_count()) {
                (0, 0) => Badge::AllSynced,
                (0, n) => Badge::Count(n),
                (_, n) => Badge::Unsynced(n),
            }
        }
    }

    /// What the badge's number counts, per config's `badge_number`.
    pub fn badge_count(&self) -> usize {
        let repos = |f: fn(&RepoStatus) -> bool| self.statuses.iter().filter(|s| f(s)).count();
        let prs = |f: fn(&crate::github::Pr) -> bool| {
            self.open_prs().into_iter().filter(|(_, p)| f(p)).count()
        };
        match self.config.badge_number {
            BadgeNumber::Unsynced => self.unsynced(),
            BadgeNumber::Dirty => repos(|s| s.dirty),
            BadgeNumber::Ahead => repos(|s| s.ahead > 0),
            BadgeNumber::Behind => repos(|s| s.behind > 0),
            BadgeNumber::PullRequests => {
                let remote: u64 = self.remote_repos.iter().map(|r| r.open_prs).sum();
                prs(|_| true) + remote as usize
            }
            BadgeNumber::PullRequestsForYou => prs(crate::github::Pr::needs_attention),
            BadgeNumber::ReviewRequests => prs(|p| p.review_requested),
        }
    }

    /// The badge's number in words, when it counts something other than
    /// unsynced repos (which the summary already says).
    pub fn badge_note(&self) -> Option<String> {
        let n = self.badge_count();
        let plural = |one: &str, many: &str| {
            if n == 1 {
                one.to_string()
            } else {
                many.to_string()
            }
        };
        let what = match self.config.badge_number {
            BadgeNumber::Unsynced => return None,
            BadgeNumber::Dirty => plural(
                "repo with uncommitted changes",
                "repos with uncommitted changes",
            ),
            BadgeNumber::Ahead => plural("repo to push", "repos to push"),
            BadgeNumber::Behind => plural("repo to pull", "repos to pull"),
            BadgeNumber::PullRequests => plural("open pull request", "open pull requests"),
            BadgeNumber::PullRequestsForYou => {
                plural("pull request needs you", "pull requests need you")
            }
            BadgeNumber::ReviewRequests => plural("review asked of you", "reviews asked of you"),
        };
        Some(format!("{n} {what}"))
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

    /// Takes in one folder's repo list. Returns (anything changed, a repo
    /// was dropped).
    pub fn set_slot_repos(
        &mut self,
        slot: usize,
        repos: Vec<PathBuf>,
        capped: Option<String>,
    ) -> (bool, bool) {
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, Default::default);
        }
        self.slots[slot] = (repos, capped);
        self.refresh_slots()
    }

    /// Forgets the folders from `count` on. Returns whether repos went.
    pub fn keep_slots(&mut self, count: usize) -> bool {
        self.sweeping_slots.retain(|&s| s < count);
        self.sweeping = !self.sweeping_slots.is_empty();
        self.modes.truncate(count);
        self.mode = worst_mode(&self.modes).unwrap_or_else(|| self.mode.clone());
        if self.slots.len() <= count {
            return false;
        }
        self.slots.truncate(count);
        self.refresh_slots().1
    }

    /// The repos of every watched folder.
    pub fn all_repos(&self) -> Vec<PathBuf> {
        self.slots
            .iter()
            .flat_map(|(r, _)| r.iter().cloned())
            .collect()
    }

    fn refresh_slots(&mut self) -> (bool, bool) {
        let all: HashSet<PathBuf> = self.all_repos().into_iter().collect();
        let before = self.statuses.len();
        self.retain_repos(|p| all.contains(p));
        let removed = self.statuses.len() != before;
        let capped = self.slots.iter().find_map(|(_, c)| c.clone());
        let capped_changed = self.capped != capped;
        self.capped = capped;
        (removed || capped_changed, removed)
    }

    /// A folder's sweep started or ended; `sweeping` is any folder's.
    pub fn set_sweeping(&mut self, slot: usize, on: bool) {
        if on {
            self.sweeping_slots.insert(slot);
        } else {
            self.sweeping_slots.remove(&slot);
        }
        self.sweeping = !self.sweeping_slots.is_empty();
    }

    /// A folder's watch mode; `mode` becomes the worst of them.
    pub fn set_mode(&mut self, slot: usize, mode: WatchMode) {
        if self.modes.len() <= slot {
            self.modes.resize(slot + 1, mode.clone());
        }
        self.modes[slot] = mode;
        self.mode = worst_mode(&self.modes).unwrap_or(WatchMode::Off);
    }

    /// Forgets repos no longer in the list.
    pub fn retain_repos(&mut self, keep: impl Fn(&Path) -> bool) {
        self.statuses.retain(|s| keep(&s.path));
        self.in_flight.retain(|p| keep(p));
        self.checks.retain(|p, _| keep(p));
        self.prs.retain(|p, _| keep(p));
    }

    /// Folds in a GitHub poll. A failed poll keeps what the last good one
    /// read, so a network blip doesn't empty the list.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn github_report(&mut self, report: crate::github::Report) {
        if report.problem.is_none() {
            self.prs = report.prs;
        } else {
            self.prs.extend(report.prs);
        }
        if let Some(remote) = report.remote_only {
            self.set_remote_repos(remote);
        }
        self.github_problem = report.problem;
    }

    /// Takes in the uncloned repos. One pushed to since the last list
    /// counts as checked, so its tree is read again.
    pub fn set_remote_repos(&mut self, remote: Vec<crate::github::RemoteRepo>) {
        for r in &remote {
            let pushed_before = self
                .remote_repos
                .iter()
                .find(|o| o.slug == r.slug)
                .map(|o| o.pushed_at.as_str());
            if pushed_before.is_some_and(|p| p != r.pushed_at) {
                self.checked(&crate::github::remote_path(&r.slug));
            }
        }
        let mut statuses: Vec<RepoStatus> = remote
            .iter()
            .map(|r| RepoStatus {
                name: r.slug.clone(),
                path: crate::github::remote_path(&r.slug),
                has_upstream: true,
                remote_only: true,
                last_commit: crate::github::unix_time(&r.pushed_at).unwrap_or(0),
                ..Default::default()
            })
            .collect();
        statuses.sort_by_cached_key(|s| s.name.to_lowercase());
        let listed: HashSet<PathBuf> = statuses.iter().map(|s| s.path.clone()).collect();
        self.checks
            .retain(|p, _| crate::github::remote_slug(p).is_none() || listed.contains(p));
        self.remote_statuses = statuses;
        self.remote_repos = remote;
    }

    /// Whether the not-cloned repo list is folded, in big mode or the panel.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn remote_folded(&self, big: bool) -> bool {
        self.remote_folded[usize::from(big)]
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn fold_remote(&mut self, big: bool) {
        let f = &mut self.remote_folded[usize::from(big)];
        *f = !*f;
    }

    /// Whether the pull request list is folded, in big mode or the panel.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn prs_folded(&self, big: bool) -> bool {
        self.prs_folded[usize::from(big)]
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn fold_prs(&mut self, big: bool) {
        let f = &mut self.prs_folded[usize::from(big)];
        *f = !*f;
    }

    /// The listed repos' open pull requests, most pressing first.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn open_prs(&self) -> Vec<(&RepoStatus, &crate::github::Pr)> {
        let order = crate::github::panel_order(
            self.prs
                .iter()
                .flat_map(|(path, prs)| prs.iter().map(move |p| (path.as_path(), p))),
        );
        order
            .into_iter()
            .filter_map(|(path, pr)| {
                let status = self.statuses.iter().find(|s| s.path == path)?;
                Some((status, pr))
            })
            .collect()
    }

    /// The cloned repos in the configured order; the panel splits them
    /// into unsynced and synced keeping it.
    pub fn sorted_for_panel(&self) -> Vec<&RepoStatus> {
        self.sorted(&self.statuses)
    }

    /// `list` in the configured order, by name among equals.
    pub fn sorted<'a>(&self, list: &'a [RepoStatus]) -> Vec<&'a RepoStatus> {
        use crate::config::RepoSort;
        use std::cmp::Reverse;
        let mut sorted: Vec<&RepoStatus> = list.iter().collect();
        let sort = self.config.sort;
        // Larger is earlier, then the name.
        sorted.sort_by_cached_key(|s| {
            let score: (i64, i64) = match sort {
                RepoSort::Name => (0, 0),
                RepoSort::Status => (-i64::from(severity(s)), 0),
                RepoSort::LastCommit => (s.last_commit, 0),
                RepoSort::LastWorked => (s.last_worked, 0),
                RepoSort::LastPullRequest => (self.last_pr_time(s), 0),
                RepoSort::OpenPullRequests => self.pr_counts(s),
                RepoSort::HangingBranches => (self.hanging(s) as i64, 0),
            };
            (Reverse(score), s.name.to_lowercase())
        });
        sorted
    }

    /// When `s`'s newest pull request was last updated, Unix seconds.
    fn last_pr_time(&self, s: &RepoStatus) -> i64 {
        self.prs
            .get(&s.path)
            .into_iter()
            .flatten()
            .filter_map(|p| crate::github::unix_time(&p.updated_at))
            .max()
            .unwrap_or(0)
    }

    /// (needing you, open) pull requests of `s`.
    fn pr_counts(&self, s: &RepoStatus) -> (i64, i64) {
        if s.remote_only {
            let open = self
                .remote_repos
                .iter()
                .find(|r| crate::github::remote_path(&r.slug) == s.path)
                .map_or(0, |r| r.open_prs as i64);
            return (0, open);
        }
        let prs = self.prs.get(&s.path).map(Vec::as_slice).unwrap_or_default();
        let count = |f: fn(&crate::github::Pr) -> bool| prs.iter().filter(|p| f(p)).count() as i64;
        (
            count(crate::github::Pr::needs_attention),
            count(crate::github::Pr::is_open),
        )
    }

    /// `s`'s branches that aren't merged and have no open pull request; a
    /// merged pull request counts as merged (squash merges).
    pub fn hanging(&self, s: &RepoStatus) -> usize {
        let prs = self.prs.get(&s.path).map(Vec::as_slice).unwrap_or_default();
        s.unmerged
            .iter()
            .filter(|b| {
                !prs.iter().any(|p| {
                    &p.head_branch == *b
                        && (p.is_open() || p.state == crate::github::PrState::Merged)
                })
            })
            .count()
    }
}

/// Polling (watching failed somewhere) over interval only over events.
fn worst_mode(modes: &[WatchMode]) -> Option<WatchMode> {
    let rank = |m: &WatchMode| match m {
        WatchMode::Polling { .. } => 0,
        WatchMode::Off => 1,
        WatchMode::Events => 2,
    };
    modes.iter().min_by_key(|m| rank(m)).cloned()
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
    /// A monitor's update, with the slot of the folder it watches.
    Monitor(usize, crate::monitor::Update),
    /// A click on the badge.
    Overlay,
    /// The badge was dragged and dropped with its disc here (physical px).
    OverlayMoved(crate::layout::Rect),
    /// The folder-icon worker's report: skipped folders and why.
    FolderSkips(Vec<(PathBuf, String)>),
    /// A GitHub pull request poll.
    Github(crate::github::Report),
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
    fn sorts_by_the_configured_key_then_name() {
        use crate::config::RepoSort;
        let mk = |name: &str, f: &dyn Fn(&mut RepoStatus)| {
            let mut r = repo(name);
            f(&mut r);
            r
        };
        let list = vec![
            mk("b", &|r| {
                r.last_commit = 30;
                r.last_worked = 1;
                r.unmerged = vec!["x".into(), "y".into()];
            }),
            mk("a", &|r| {
                r.last_commit = 10;
                r.last_worked = 3;
                r.dirty = true;
            }),
            mk("c", &|r| {
                r.last_commit = 20;
                r.last_worked = 2;
                r.unmerged = vec!["z".into()];
            }),
        ];
        let mut s = state(Vec::new());
        let order = |s: &AppState| -> Vec<String> {
            s.sorted(&list).iter().map(|r| r.name.clone()).collect()
        };
        assert_eq!(order(&s), ["a", "b", "c"]);
        s.config.sort = RepoSort::Status;
        assert_eq!(order(&s), ["a", "b", "c"]);
        s.config.sort = RepoSort::LastCommit;
        assert_eq!(order(&s), ["b", "c", "a"]);
        s.config.sort = RepoSort::LastWorked;
        assert_eq!(order(&s), ["a", "c", "b"]);
        s.config.sort = RepoSort::HangingBranches;
        assert_eq!(order(&s), ["b", "c", "a"]);
        // An open PR on y leaves b one hanging branch, tied with c: by name.
        let mut pr = crate::github::tests_pr();
        pr.head_branch = "y".into();
        s.prs.insert(PathBuf::from("b"), vec![pr]);
        assert_eq!(s.hanging(&list[0]), 1);
        assert_eq!(order(&s), ["b", "c", "a"]);
        s.config.sort = RepoSort::OpenPullRequests;
        assert_eq!(order(&s), ["b", "a", "c"]);
        s.config.sort = RepoSort::LastPullRequest;
        assert_eq!(order(&s), ["b", "a", "c"]);
    }

    #[test]
    fn folders_combine_their_repos_sweeps_and_modes() {
        let mut s = state(vec![repo("a"), repo("b"), repo("c")]);
        let (a, b, c) = (PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c"));
        assert_eq!(
            s.set_slot_repos(0, vec![a.clone(), b.clone()], None),
            (true, true)
        );
        assert_eq!(s.statuses.len(), 2, "c belongs to no folder yet");
        s.merge_status(repo("c"));
        assert_eq!(
            s.set_slot_repos(1, vec![c.clone()], Some("cap".into())),
            (true, false)
        );
        assert_eq!(s.capped.as_deref(), Some("cap"));
        assert_eq!(s.all_repos(), [a, b, c]);
        // Folder 0 losing b drops it; folder 1's c stays.
        assert_eq!(
            s.set_slot_repos(0, vec![PathBuf::from("a")], None),
            (true, true)
        );
        assert_eq!(s.statuses.len(), 2);

        s.set_sweeping(0, true);
        s.set_sweeping(1, true);
        s.set_sweeping(0, false);
        assert!(s.sweeping);
        s.set_mode(0, WatchMode::Events);
        s.set_mode(1, WatchMode::Polling { reason: "x".into() });
        assert!(matches!(s.mode, WatchMode::Polling { .. }));

        // Folder 1 removed: its repo, sweep and mode go with it.
        assert!(s.keep_slots(1));
        assert_eq!(s.statuses.len(), 1);
        assert!(!s.sweeping && s.capped.is_none());
        assert_eq!(s.mode, WatchMode::Events);
        assert!(!s.keep_slots(1));
    }

    #[test]
    fn badge_counts_what_config_says_and_colours_by_sync() {
        use crate::config::BadgeNumber;
        let mut dirty = repo("dirty");
        dirty.dirty = true;
        let mut ahead = repo("ahead");
        ahead.ahead = 2;
        let mut both = repo("both");
        both.ahead = 1;
        both.behind = 1;
        let mut s = state(vec![dirty, ahead, both, repo("synced")]);
        s.has_result = true;
        let count = |s: &mut AppState, n: BadgeNumber| {
            s.config.badge_number = n;
            (s.badge_count(), s.badge())
        };
        assert_eq!(
            count(&mut s, BadgeNumber::Unsynced),
            (3, Badge::Unsynced(3))
        );
        assert_eq!(count(&mut s, BadgeNumber::Dirty), (1, Badge::Unsynced(1)));
        assert_eq!(count(&mut s, BadgeNumber::Ahead), (2, Badge::Unsynced(2)));
        assert_eq!(count(&mut s, BadgeNumber::Behind), (1, Badge::Unsynced(1)));
        // No PRs read: still red, since repos are unsynced.
        assert_eq!(
            count(&mut s, BadgeNumber::PullRequests),
            (0, Badge::Unsynced(0))
        );
        assert_eq!(s.badge_note().as_deref(), Some("0 open pull requests"));
        s.config.badge_number = BadgeNumber::Unsynced;
        assert_eq!(s.badge_note(), None);

        // All synced: green with a count, or the tick at 0.
        let mut s = state(vec![repo("a")]);
        s.has_result = true;
        s.remote_repos.push(crate::github::RemoteRepo {
            slug: "o/r".into(),
            url: String::new(),
            private: false,
            fork: false,
            pushed_at: String::new(),
            default_branch: String::new(),
            checks: crate::github::Checks::None,
            open_prs: 3,
        });
        assert_eq!(
            count(&mut s, BadgeNumber::PullRequests),
            (3, Badge::Count(3))
        );
        assert_eq!(
            count(&mut s, BadgeNumber::ReviewRequests),
            (0, Badge::AllSynced)
        );
        assert_eq!(count(&mut s, BadgeNumber::Unsynced), (0, Badge::AllSynced));
    }

    #[test]
    fn github_report_keeps_remote_repos_when_unread() {
        let mut s = state(Vec::new());
        assert!(s.remote_folded(false) && s.remote_folded(true));
        let repo = crate::github::RemoteRepo {
            slug: "o/a".into(),
            url: "https://github.com/o/a".into(),
            private: false,
            fork: false,
            pushed_at: String::new(),
            default_branch: "main".into(),
            checks: crate::github::Checks::None,
            open_prs: 0,
        };
        s.github_report(crate::github::Report {
            remote_only: Some(vec![repo.clone()]),
            ..Default::default()
        });
        assert_eq!(s.remote_repos, std::slice::from_ref(&repo));
        // A failed poll reads no list: the last one stays.
        s.github_report(crate::github::Report {
            problem: Some("offline".into()),
            ..Default::default()
        });
        assert_eq!(s.remote_repos, [repo]);
        assert_eq!(s.github_problem.as_deref(), Some("offline"));
        s.github_report(crate::github::Report {
            remote_only: Some(Vec::new()),
            ..Default::default()
        });
        assert!(s.remote_repos.is_empty() && s.github_problem.is_none());
        s.fold_remote(false);
        assert!(!s.remote_folded(false) && s.remote_folded(true));
    }

    #[test]
    fn status_order_is_worst_first_then_name() {
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
        let mut s = state(vec![synced, dirty, ahead, error, diverged, dirty2]);
        s.config.sort = crate::config::RepoSort::Status;
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
