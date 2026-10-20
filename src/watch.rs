//! Turns raw filesystem events into "this repo needs a check" decisions:
//! the `Router` classifies paths, the `Debouncer` decides when to check.

use notify::EventKind;
use notify::event::ModifyKind;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Trailing quiet window after a git-dir event (HEAD, index, refs).
pub const QUIET_META: Duration = Duration::from_millis(250);
/// Trailing quiet window after a worktree event: editors and builds write in bursts.
pub const QUIET_WORKTREE: Duration = Duration::from_secs(1);
/// Continuous churn still gets a check this often.
pub const MAX_WAIT: Duration = Duration::from_secs(5);
/// Minimum time between the end of one check of a repo and the start of the next.
pub const MIN_GAP: Duration = Duration::from_secs(2);
/// A watcher overflow (rescan) checks every repo at most this often.
pub const RESCAN_MIN_GAP: Duration = Duration::from_secs(10);
pub const REPOSET_QUIET: Duration = Duration::from_secs(1);
pub const CONFIG_QUIET: Duration = Duration::from_millis(300);
/// ReadDirectoryChangesW delivers a fetch's own ref writes late.
pub const REMOTE_GRACE: Duration = Duration::from_millis(1500);

/// What an event means for the repo it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Ignore,
    Worktree,
    GitMeta,
    RemoteRefs,
    RepoSet,
    Config,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRoute {
    pub path: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    /// Ignored directory prefixes, relative to `path`, from `git status --ignored`.
    pub ignored: Vec<String>,
}

/// A route with its paths folded for comparison.
struct Keyed {
    route: RepoRoute,
    path: String,
    git_dir: String,
    common_dir: String,
    ignored: Vec<String>,
}

impl Keyed {
    fn new(route: RepoRoute) -> Self {
        Self {
            path: key(&route.path),
            git_dir: key(&route.git_dir),
            common_dir: key(&route.common_dir),
            ignored: fold_ignored(&route.ignored),
            route,
        }
    }
}

pub struct Router {
    root: String,
    config_file: String,
    routes: Vec<Keyed>,
}

impl Router {
    pub fn new(root: PathBuf, config_file: PathBuf) -> Self {
        Self {
            root: key(&root),
            config_file: key(&config_file),
            routes: Vec::new(),
        }
    }

    pub fn set_repos(&mut self, routes: Vec<RepoRoute>) {
        self.routes = routes.into_iter().map(Keyed::new).collect();
    }

    pub fn set_ignored(&mut self, repo: &Path, ignored: Vec<String>) {
        if let Some(keyed) = self.routes.iter_mut().find(|r| r.route.path == repo) {
            keyed.ignored = fold_ignored(&ignored);
            keyed.route.ignored = ignored;
        }
    }

    pub fn routes(&self) -> impl Iterator<Item = &RepoRoute> {
        self.routes.iter().map(|r| &r.route)
    }

    pub fn classify(&self, path: &Path, kind: &EventKind) -> Vec<(Option<PathBuf>, Class)> {
        let p = key(path);
        let mut out = Vec::new();
        // The config file may sit inside a repo, so keep classifying.
        if p == self.config_file {
            out.push((None, Class::Config));
        }
        let rel_root = under(&p, &self.root);

        // `<x>/.git` appearing or going away adds or removes a repo.
        if let Some(rel) = rel_root
            && let Some((_, name)) = rel.split_once('/')
            && name.eq_ignore_ascii_case(".git")
            && structural(kind)
        {
            out.push((None, Class::RepoSet));
            return out;
        }

        if let Some(found) = self.git_meta(&p) {
            out.extend(found);
            return out;
        }

        let Some(rel) = rel_root.filter(|rel| !rel.is_empty()) else {
            return out;
        };
        let Some((_, rest)) = rel.split_once('/') else {
            // A top-level folder or file. Modify is only attributes or
            // timestamps, such as our own PathMakeSystemFolder.
            let class = if structural(kind) {
                Class::RepoSet
            } else {
                Class::Ignore
            };
            out.push((None, class));
            return out;
        };
        let route = self
            .routes
            .iter()
            .filter(|r| under(&p, &r.path).is_some())
            .max_by_key(|r| r.path.len());
        let repo = route.map(|r| r.route.path.clone());
        let class = match route {
            // Our own folder-icon writes, and a linked worktree's `.git` file.
            _ if rest.eq_ignore_ascii_case("desktop.ini") || rest.eq_ignore_ascii_case(".git") => {
                Class::Ignore
            }
            Some(route) if is_ignored(route, under(&p, &route.path).unwrap_or_default()) => {
                Class::Ignore
            }
            Some(_) => Class::Worktree,
            None => Class::Ignore,
        };
        out.push((repo, class));
        out
    }

    /// Classifies a path inside some repo's git dir or common dir. Common-dir
    /// files (refs, config) belong to every repo sharing that dir; HEAD and
    /// index belong only to the repo whose git dir it is.
    fn git_meta(&self, p: &str) -> Option<Vec<(Option<PathBuf>, Class)>> {
        let dir = self
            .routes
            .iter()
            .flat_map(|r| [r.git_dir.as_str(), r.common_dir.as_str()])
            .filter(|dir| under(p, dir).is_some())
            .max_by_key(|dir| dir.len())?;
        let rel = under(p, dir)?;
        let class = git_class(rel);
        let per_worktree = is_per_worktree(rel) || !self.routes.iter().any(|r| r.common_dir == dir);
        let targets: Vec<PathBuf> = self
            .routes
            .iter()
            .filter(|r| {
                if per_worktree {
                    r.git_dir == dir
                } else {
                    r.common_dir == dir
                }
            })
            .map(|r| r.route.path.clone())
            .collect();
        if class == Class::Ignore || targets.is_empty() {
            return Some(vec![(targets.into_iter().next(), Class::Ignore)]);
        }
        Some(targets.into_iter().map(|t| (Some(t), class)).collect())
    }
}

fn structural(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Any
            | EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_))
    )
}

/// The whitelist of git-dir files that change what `git status` reports.
fn git_class(rel: &str) -> Class {
    let ci = |name: &str| rel.eq_ignore_ascii_case(name);
    if ci("HEAD") || ci("index") || ci("config") || ci("packed-refs") {
        return Class::GitMeta;
    }
    // Every lock ends in a rename to the real name, which is what counts.
    if ends_with_ci(rel, ".lock") {
        return Class::Ignore;
    }
    if starts_with_ci(rel, "refs/heads/") || starts_with_ci(rel, "refs/tags/") {
        Class::GitMeta
    } else if starts_with_ci(rel, "refs/remotes/") {
        Class::RemoteRefs
    } else {
        Class::Ignore
    }
}

fn is_per_worktree(rel: &str) -> bool {
    rel.eq_ignore_ascii_case("HEAD") || rel.eq_ignore_ascii_case("index")
}

fn is_ignored(route: &Keyed, rel: &str) -> bool {
    if rel.is_empty() {
        return false;
    }
    // `target` itself (a directory timestamp) as well as `target/x`.
    let dir = format!("{rel}/");
    route.ignored.iter().any(|prefix| dir.starts_with(prefix))
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn ends_with_ci(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len()
        && s.is_char_boundary(s.len() - suffix.len())
        && s[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// A path folded for comparison: on Windows case-insensitive and with `/`
/// and `\` alike, since git reports `C:/dev` and notify keeps `D:\dev`.
fn key(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    let text = text.replace('\\', "/").to_lowercase();
    #[cfg(not(windows))]
    let text = text.into_owned();
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() {
        text
    } else {
        trimmed.to_string()
    }
}

fn fold_ignored(ignored: &[String]) -> Vec<String> {
    ignored
        .iter()
        .map(|prefix| {
            #[cfg(windows)]
            let prefix = prefix.replace('\\', "/").to_lowercase();
            prefix.to_string()
        })
        .collect()
}

/// `p` relative to `dir`, both folded: `Some("")` for `dir` itself.
fn under<'a>(p: &'a str, dir: &str) -> Option<&'a str> {
    if p == dir {
        return Some("");
    }
    let rest = p.strip_prefix(dir)?;
    if dir.ends_with('/') {
        Some(rest)
    } else {
        rest.strip_prefix('/')
    }
}

/// The same path, compared the way the router compares them.
pub fn same_path(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

/// `path` is `dir` or inside it, compared the way the router compares them.
pub fn is_within(path: &Path, dir: &Path) -> bool {
    under(&key(path), &key(dir)).is_some()
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Need {
    Local,
    Fetch,
}

#[derive(Default)]
struct Slot {
    pending: Option<Need>,
    /// First event of the current burst, for `MAX_WAIT`.
    first: Option<Instant>,
    /// End of the trailing quiet window.
    quiet_until: Option<Instant>,
    in_flight: Option<Need>,
    /// Something happened during the running check.
    rerun: Option<Need>,
    last_end: Option<Instant>,
    fetch_end: Option<Instant>,
    /// Removed from the repo list while its check ran; dropped when it ends.
    orphan: bool,
}

impl Slot {
    fn ready_at(&self) -> Option<Instant> {
        self.pending?;
        if self.in_flight.is_some() {
            return None;
        }
        let mut at = self.quiet_until?;
        if let Some(end) = self.last_end {
            at = at.max(end + MIN_GAP);
        }
        Some(at)
    }
}

/// Keyed by path, never by index, so a changing repo list can't misroute.
#[derive(Default)]
pub struct Debouncer {
    slots: HashMap<PathBuf, Slot>,
    /// The repo list as of the last `retain`, for rescans.
    known: Vec<PathBuf>,
    last_rescan: Option<Instant>,
    /// A rate-limited rescan waiting for its turn.
    rescan_at: Option<Instant>,
}

impl Debouncer {
    pub fn on_event(&mut self, repo: &Path, class: Class, now: Instant) {
        let quiet = match class {
            Class::GitMeta | Class::RemoteRefs => QUIET_META,
            Class::Worktree => QUIET_WORKTREE,
            Class::Ignore | Class::RepoSet | Class::Config => return,
        };
        let slot = self.slots.entry(repo.to_path_buf()).or_default();
        if class == Class::RemoteRefs
            && (slot.in_flight == Some(Need::Fetch)
                || slot.fetch_end.is_some_and(|end| now < end + REMOTE_GRACE))
        {
            // Our own fetch writing refs; its status already reads them.
            return;
        }
        if slot.in_flight.is_some() {
            slot.rerun = slot.rerun.max(Some(Need::Local));
            return;
        }
        slot.pending = slot.pending.max(Some(Need::Local));
        let first = *slot.first.get_or_insert(now);
        let until = slot.quiet_until.map_or(now + quiet, |u| u.max(now + quiet));
        slot.quiet_until = Some(until.min(first + MAX_WAIT));
    }

    /// Sweep, rescan or manual refresh: due now, still after `MIN_GAP`.
    /// A running check of at least `need` already covers the request.
    pub fn request(&mut self, repo: &Path, need: Need, now: Instant) {
        let slot = self.slots.entry(repo.to_path_buf()).or_default();
        if let Some(running) = slot.in_flight {
            if need > running {
                slot.rerun = slot.rerun.max(Some(need));
            }
            return;
        }
        slot.pending = slot.pending.max(Some(need));
        slot.first.get_or_insert(now);
        slot.quiet_until = Some(slot.quiet_until.map_or(now, |u| u.min(now)));
    }

    /// The watcher lost events: a Local check of every known repo, at most
    /// once per `RESCAN_MIN_GAP`. Returns false when it was deferred.
    pub fn rescan(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last_rescan
            && now < last + RESCAN_MIN_GAP
        {
            self.rescan_at = Some(last + RESCAN_MIN_GAP);
            return false;
        }
        self.last_rescan = Some(now);
        self.rescan_at = None;
        for repo in self.known.clone() {
            self.request(&repo, Need::Local, now);
        }
        true
    }

    /// Checks that are due, marked in flight.
    pub fn due(&mut self, now: Instant, free_slots: usize) -> Vec<(PathBuf, Need)> {
        if self.rescan_at.is_some_and(|at| at <= now) {
            self.rescan(now);
        }
        if free_slots == 0 {
            return Vec::new();
        }
        let mut ready: Vec<(Instant, &PathBuf)> = self
            .slots
            .iter()
            .filter_map(|(path, slot)| slot.ready_at().filter(|at| *at <= now).map(|at| (at, path)))
            .collect();
        ready.sort();
        let picked: Vec<PathBuf> = ready
            .into_iter()
            .take(free_slots)
            .map(|(_, path)| path.clone())
            .collect();
        picked
            .into_iter()
            .filter_map(|path| {
                let slot = self.slots.get_mut(&path)?;
                let need = slot.pending.take()?;
                slot.in_flight = Some(need);
                slot.first = None;
                slot.quiet_until = None;
                Some((path, need))
            })
            .collect()
    }

    pub fn finished(&mut self, repo: &Path, need: Need, now: Instant) {
        let Some(slot) = self.slots.get_mut(repo) else {
            return;
        };
        slot.in_flight = None;
        slot.last_end = Some(now);
        if need == Need::Fetch {
            slot.fetch_end = Some(now);
        }
        if slot.orphan {
            self.slots.remove(repo);
            return;
        }
        if let Some(rerun) = slot.rerun.take() {
            slot.pending = slot.pending.max(Some(rerun));
            slot.first = Some(now);
            slot.quiet_until = Some(now + QUIET_META);
        }
    }

    /// Drops repos that are gone. A running check keeps its slot until it
    /// finishes, so a repo can't get a second concurrent check if it returns.
    pub fn retain(&mut self, repos: &[PathBuf]) {
        self.known = repos.to_vec();
        self.slots.retain(|path, slot| {
            if repos.contains(path) {
                slot.orphan = false;
                true
            } else if slot.in_flight.is_some() {
                slot.orphan = true;
                slot.pending = None;
                slot.rerun = None;
                true
            } else {
                false
            }
        });
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.slots
            .values()
            .filter_map(Slot::ready_at)
            .chain(self.rescan_at)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, MetadataKind, RemoveKind, RenameMode};

    const MODIFY: EventKind = EventKind::Modify(ModifyKind::Data(DataChange::Content));
    const ATTRIB: EventKind = EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any));
    const CREATE: EventKind = EventKind::Create(CreateKind::Any);
    const REMOVE: EventKind = EventKind::Remove(RemoveKind::Any);
    const RENAME: EventKind = EventKind::Modify(ModifyKind::Name(RenameMode::To));

    fn route(path: &str, git_dir: &str, common_dir: &str) -> RepoRoute {
        RepoRoute {
            path: path.into(),
            git_dir: git_dir.into(),
            common_dir: common_dir.into(),
            ignored: Vec::new(),
        }
    }

    /// `/dev` holds repo `r`, its linked worktree `wt`, and `gitstat` with the config.
    fn router() -> Router {
        let mut router = Router::new("/dev".into(), "/dev/gitstat/config.json".into());
        router.set_repos(vec![
            route("/dev/r", "/dev/r/.git", "/dev/r/.git"),
            route("/dev/wt", "/dev/r/.git/worktrees/wt", "/dev/r/.git"),
            route("/dev/gitstat", "/dev/gitstat/.git", "/dev/gitstat/.git"),
        ]);
        router
    }

    fn classes(router: &Router, path: &str, kind: EventKind) -> Vec<(Option<String>, Class)> {
        router
            .classify(Path::new(path), &kind)
            .into_iter()
            .map(|(repo, class)| (repo.map(|r| r.to_string_lossy().into_owned()), class))
            .collect()
    }

    fn one(repo: &str, class: Class) -> Vec<(Option<String>, Class)> {
        vec![(Some(repo.to_string()), class)]
    }

    #[test]
    fn git_dir_whitelist() {
        let router = router();
        for path in [
            "/dev/r/.git/index.lock",
            "/dev/r/.git/HEAD.lock",
            "/dev/r/.git/refs/heads/main.lock",
            "/dev/r/.git/objects/ab/cdef",
            "/dev/r/.git/objects/maintenance.lock",
            "/dev/r/.git/logs/HEAD",
            "/dev/r/.git/logs/refs/heads/main",
            "/dev/r/.git/FETCH_HEAD",
            "/dev/r/.git/ORIG_HEAD",
            "/dev/r/.git/MERGE_MSG",
            "/dev/r/.git/COMMIT_EDITMSG",
            "/dev/r/.git/rebase-merge/done",
            "/dev/r/.git/info/exclude",
            "/dev/r/.git/hooks/pre-commit",
            "/dev/r/.git/fsmonitor--daemon/cookies/1-2",
            "/dev/r/.git",
        ] {
            assert_eq!(
                classes(&router, path, MODIFY),
                one("/dev/r", Class::Ignore),
                "{path}"
            );
        }
        for path in [
            "/dev/r/.git/HEAD",
            "/dev/r/.git/index",
            "/dev/r/.git/config",
            "/dev/r/.git/packed-refs",
            "/dev/r/.git/refs/heads/main",
            "/dev/r/.git/refs/heads/feature/x",
            "/dev/r/.git/refs/tags/v1",
        ] {
            let expect = if path.ends_with("HEAD") || path.ends_with("index") {
                one("/dev/r", Class::GitMeta)
            } else {
                // Shared with the linked worktree.
                vec![
                    (Some("/dev/r".into()), Class::GitMeta),
                    (Some("/dev/wt".into()), Class::GitMeta),
                ]
            };
            assert_eq!(classes(&router, path, MODIFY), expect, "{path}");
        }
        assert_eq!(
            classes(&router, "/dev/r/.git/refs/remotes/origin/main", RENAME),
            vec![
                (Some("/dev/r".into()), Class::RemoteRefs),
                (Some("/dev/wt".into()), Class::RemoteRefs),
            ]
        );
    }

    #[test]
    fn linked_worktree_routing() {
        let router = router();
        assert_eq!(
            classes(&router, "/dev/r/.git/worktrees/wt/HEAD", MODIFY),
            one("/dev/wt", Class::GitMeta)
        );
        assert_eq!(
            classes(&router, "/dev/r/.git/worktrees/wt/index", MODIFY),
            one("/dev/wt", Class::GitMeta)
        );
        assert_eq!(
            classes(&router, "/dev/r/.git/worktrees/wt/index.lock", CREATE),
            one("/dev/wt", Class::Ignore)
        );
        // A worktree that isn't one of ours.
        assert_eq!(
            classes(&router, "/dev/r/.git/worktrees/other/HEAD", MODIFY),
            one("/dev/r", Class::Ignore)
        );
        assert_eq!(
            classes(&router, "/dev/wt/src/main.rs", MODIFY),
            one("/dev/wt", Class::Worktree)
        );
        // A `.git` file pointing elsewhere is just a worktree file.
        assert_eq!(
            classes(&router, "/dev/wt/.git", MODIFY),
            one("/dev/wt", Class::Ignore)
        );
    }

    #[test]
    fn git_dir_outside_the_root() {
        let mut router = Router::new("/dev".into(), "/cfg/config.json".into());
        router.set_repos(vec![route(
            "/dev/wt",
            "/elsewhere/main/.git/worktrees/wt",
            "/elsewhere/main/.git",
        )]);
        assert_eq!(
            classes(&router, "/elsewhere/main/.git/worktrees/wt/HEAD", MODIFY),
            one("/dev/wt", Class::GitMeta)
        );
        assert_eq!(
            classes(&router, "/elsewhere/main/.git/refs/heads/main", MODIFY),
            one("/dev/wt", Class::GitMeta)
        );
        // The main repo's own HEAD isn't the worktree's.
        assert_eq!(
            classes(&router, "/elsewhere/main/.git/HEAD", MODIFY),
            vec![(None, Class::Ignore)]
        );
        assert_eq!(classes(&router, "/elsewhere/main/file", MODIFY), vec![]);
    }

    #[test]
    fn top_level_and_repo_set() {
        let router = router();
        for kind in [CREATE, REMOVE, RENAME] {
            assert_eq!(
                classes(&router, "/dev/new", kind),
                vec![(None, Class::RepoSet)]
            );
            assert_eq!(
                classes(&router, "/dev/r", kind),
                vec![(None, Class::RepoSet)]
            );
            assert_eq!(
                classes(&router, "/dev/x/.git", kind),
                vec![(None, Class::RepoSet)],
                "{kind:?}"
            );
            assert_eq!(
                classes(&router, "/dev/r/.git", kind),
                vec![(None, Class::RepoSet)]
            );
        }
        for kind in [MODIFY, ATTRIB] {
            assert_eq!(
                classes(&router, "/dev/r", kind),
                vec![(None, Class::Ignore)]
            );
        }
        // Outside the root, or the root itself.
        assert_eq!(classes(&router, "/dev", CREATE), vec![]);
        assert_eq!(classes(&router, "/other/r/x", CREATE), vec![]);
        assert_eq!(classes(&router, "/devx/r/x", CREATE), vec![]);
        // Inside a folder that isn't a repo.
        assert_eq!(
            classes(&router, "/dev/notes/todo.txt", MODIFY),
            vec![(None, Class::Ignore)]
        );
        assert_eq!(
            classes(&router, "/dev/x/.git/HEAD", CREATE),
            vec![(None, Class::Ignore)]
        );
    }

    #[test]
    fn our_own_writes_are_ignored() {
        let router = router();
        for kind in [CREATE, MODIFY, ATTRIB, REMOVE, RENAME] {
            assert_eq!(
                classes(&router, "/dev/r/desktop.ini", kind),
                one("/dev/r", Class::Ignore)
            );
            assert_eq!(
                classes(&router, "/dev/r/Desktop.ini", kind),
                one("/dev/r", Class::Ignore)
            );
        }
        // Only the repo folder's own desktop.ini.
        assert_eq!(
            classes(&router, "/dev/r/sub/desktop.ini", CREATE),
            one("/dev/r", Class::Worktree)
        );
    }

    #[test]
    fn config_file_inside_a_repo() {
        let router = router();
        assert_eq!(
            classes(&router, "/dev/gitstat/config.json", MODIFY),
            vec![
                (None, Class::Config),
                (Some("/dev/gitstat".into()), Class::Worktree)
            ]
        );
        let outside = Router::new("/dev".into(), "/cfg/config.json".into());
        assert_eq!(
            classes(&outside, "/cfg/config.json", RENAME),
            vec![(None, Class::Config)]
        );
        assert_eq!(classes(&outside, "/cfg/config.json.tmp", CREATE), vec![]);
    }

    #[test]
    fn ignored_prefixes() {
        let mut router = router();
        router.set_ignored(
            Path::new("/dev/r"),
            vec!["target/".into(), "web/node_modules/".into()],
        );
        for path in [
            "/dev/r/target",
            "/dev/r/target/debug/x.o",
            "/dev/r/web/node_modules/a/b.js",
        ] {
            assert_eq!(
                classes(&router, path, CREATE),
                one("/dev/r", Class::Ignore),
                "{path}"
            );
        }
        for path in [
            "/dev/r/targets.txt",
            "/dev/r/web/index.js",
            "/dev/r/src/target",
        ] {
            assert_eq!(
                classes(&router, path, CREATE),
                one("/dev/r", Class::Worktree),
                "{path}"
            );
        }
        // Only that repo's prefixes.
        assert_eq!(
            classes(&router, "/dev/gitstat/target/x", CREATE),
            one("/dev/gitstat", Class::Worktree)
        );
        assert_eq!(router.routes().next().map(|r| r.ignored.len()), Some(2));
    }

    #[cfg(windows)]
    #[test]
    fn case_and_separators_on_windows() {
        let mut router = Router::new(r"D:\Dev".into(), "d:/dev/gitstat/CONFIG.json".into());
        router.set_repos(vec![
            route(r"D:\Dev\r", "D:/Dev/r/.git", "D:/Dev/r/.git"),
            route(
                r"D:\Dev\gitstat",
                "D:/dev/gitstat/.git",
                "D:/dev/gitstat/.git",
            ),
        ]);
        router.set_ignored(Path::new(r"D:\Dev\r"), vec!["Target/".into()]);
        assert_eq!(
            classes(&router, r"d:\dev\R\.GIT\head", MODIFY),
            one(r"D:\Dev\r", Class::GitMeta)
        );
        assert_eq!(
            classes(&router, r"D:/Dev\r/.git\refs/Remotes/origin/main", MODIFY),
            one(r"D:\Dev\r", Class::RemoteRefs)
        );
        assert_eq!(
            classes(&router, r"D:\DEV\r\target\debug\x.o", CREATE),
            one(r"D:\Dev\r", Class::Ignore)
        );
        assert_eq!(
            classes(&router, r"D:\DEV\r\src\main.rs", MODIFY),
            one(r"D:\Dev\r", Class::Worktree)
        );
        assert_eq!(
            classes(&router, r"D:\Dev\gitstat\config.json", MODIFY),
            vec![
                (None, Class::Config),
                (Some(r"D:\Dev\gitstat".into()), Class::Worktree)
            ]
        );
        assert_eq!(
            classes(&router, r"d:\dev\X\.Git", CREATE),
            vec![(None, Class::RepoSet)]
        );
        assert!(same_path(Path::new(r"D:\Dev\"), Path::new("d:/dev")));
        assert!(is_within(Path::new(r"D:\Dev\x"), Path::new("d:/dev")));
    }

    #[test]
    fn path_helpers() {
        assert!(same_path(Path::new("/dev/"), Path::new("/dev")));
        assert!(is_within(Path::new("/dev/a/b"), Path::new("/dev")));
        assert!(is_within(Path::new("/dev"), Path::new("/dev")));
        assert!(!is_within(Path::new("/devx"), Path::new("/dev")));
    }

    // Debouncer, with synthetic instants.

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn repo() -> PathBuf {
        PathBuf::from("/dev/r")
    }

    /// Runs one check to completion so the next one is subject to `MIN_GAP`.
    fn check(d: &mut Debouncer, at: Instant) -> Vec<(PathBuf, Need)> {
        let due = d.due(at, 6);
        for (repo, need) in &due {
            d.finished(repo, *need, at);
        }
        due
    }

    #[test]
    fn trailing_quiet_window() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.on_event(&repo(), Class::Worktree, t);
        d.on_event(&repo(), Class::Worktree, t + ms(500));
        assert_eq!(d.next_deadline(), Some(t + ms(1500)));
        assert!(d.due(t + ms(1499), 6).is_empty());
        assert_eq!(d.due(t + ms(1500), 6), vec![(repo(), Need::Local)]);
        assert_eq!(d.next_deadline(), None, "in flight");

        // Git-dir events settle faster.
        let mut d = Debouncer::default();
        d.on_event(&repo(), Class::GitMeta, t);
        assert_eq!(d.next_deadline(), Some(t + QUIET_META));
        // A worktree event in the same burst stretches the window.
        d.on_event(&repo(), Class::Worktree, t + ms(100));
        assert_eq!(d.next_deadline(), Some(t + ms(1100)));
        // Ignored classes do nothing.
        let mut d = Debouncer::default();
        for class in [Class::Ignore, Class::RepoSet, Class::Config] {
            d.on_event(&repo(), class, t);
        }
        assert_eq!(d.next_deadline(), None);
    }

    #[test]
    fn max_wait_under_continuous_events() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        let mut started = None;
        for i in 0..100 {
            let now = t + ms(i * 200);
            d.on_event(&repo(), Class::Worktree, now);
            if !d.due(now, 6).is_empty() {
                started = Some(now);
                break;
            }
        }
        assert_eq!(started, Some(t + MAX_WAIT));
    }

    #[test]
    fn min_gap_between_checks() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.on_event(&repo(), Class::GitMeta, t);
        assert_eq!(check(&mut d, t + QUIET_META).len(), 1);
        let end = t + QUIET_META;
        d.on_event(&repo(), Class::GitMeta, end + ms(10));
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
        assert!(d.due(end + MIN_GAP - ms(1), 6).is_empty());
        assert_eq!(d.due(end + MIN_GAP, 6), vec![(repo(), Need::Local)]);
    }

    #[test]
    fn rerun_while_in_flight() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Local, t);
        assert_eq!(d.due(t, 6), vec![(repo(), Need::Local)]);
        // Never two checks of one repo at once.
        d.on_event(&repo(), Class::Worktree, t + ms(100));
        d.request(&repo(), Need::Local, t + ms(100));
        assert!(d.due(t + ms(5000), 6).is_empty());
        assert_eq!(d.next_deadline(), None);
        let end = t + ms(3000);
        d.finished(&repo(), Need::Local, end);
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
        assert_eq!(d.due(end + MIN_GAP, 6), vec![(repo(), Need::Local)]);
        d.finished(&repo(), Need::Local, end + MIN_GAP);
        assert_eq!(d.next_deadline(), None, "one rerun, not a loop");
    }

    #[test]
    fn running_check_covers_requests() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Fetch, t);
        assert_eq!(d.due(t, 6), vec![(repo(), Need::Fetch)]);
        // A sweep or rescan during a fetch needs nothing more.
        d.request(&repo(), Need::Fetch, t);
        d.request(&repo(), Need::Local, t);
        d.finished(&repo(), Need::Fetch, t + ms(10));
        assert_eq!(d.next_deadline(), None);
        // A fetch requested during a local check runs after it.
        d.request(&repo(), Need::Local, t + MIN_GAP * 2);
        assert_eq!(d.due(t + MIN_GAP * 2, 6), vec![(repo(), Need::Local)]);
        d.request(&repo(), Need::Fetch, t + MIN_GAP * 2);
        d.finished(&repo(), Need::Local, t + MIN_GAP * 3);
        assert_eq!(d.due(t + MIN_GAP * 4, 6), vec![(repo(), Need::Fetch)]);
    }

    #[test]
    fn fetch_absorbs_local() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.on_event(&repo(), Class::Worktree, t);
        d.request(&repo(), Need::Fetch, t + ms(100));
        d.on_event(&repo(), Class::GitMeta, t + ms(200));
        let due = d.due(t + QUIET_WORKTREE + ms(200), 6);
        assert_eq!(due, vec![(repo(), Need::Fetch)]);
        // A request is due immediately.
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Local, t);
        d.request(&repo(), Need::Fetch, t);
        assert_eq!(d.due(t, 6), vec![(repo(), Need::Fetch)]);
    }

    #[test]
    fn remote_refs_grace() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Fetch, t);
        assert_eq!(d.due(t, 6).len(), 1);
        // The fetch's own ref writes, during and just after it.
        d.on_event(&repo(), Class::RemoteRefs, t + ms(100));
        let end = t + ms(500);
        d.finished(&repo(), Need::Fetch, end);
        assert_eq!(d.next_deadline(), None);
        d.on_event(&repo(), Class::RemoteRefs, end + REMOTE_GRACE - ms(1));
        assert_eq!(d.next_deadline(), None);
        // A later one (an accepted push) counts.
        d.on_event(&repo(), Class::RemoteRefs, end + REMOTE_GRACE);
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
        // Other events during a fetch still rerun.
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Fetch, t);
        d.due(t, 6);
        d.on_event(&repo(), Class::GitMeta, t + ms(100));
        d.finished(&repo(), Need::Fetch, end);
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
        // And remote refs during a local check rerun too.
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Local, t);
        d.due(t, 6);
        d.on_event(&repo(), Class::RemoteRefs, t + ms(100));
        d.finished(&repo(), Need::Local, end);
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
    }

    #[test]
    fn rescan_is_rate_limited() {
        let t = Instant::now();
        let repos = vec![PathBuf::from("/dev/a"), PathBuf::from("/dev/b")];
        let mut d = Debouncer::default();
        d.retain(&repos);
        assert!(d.rescan(t));
        let due = check(&mut d, t);
        assert_eq!(due.len(), 2);
        assert!(due.iter().all(|(_, need)| *need == Need::Local));
        // A second overflow soon after is deferred, not dropped.
        assert!(!d.rescan(t + ms(1000)));
        assert!(!d.rescan(t + ms(2000)));
        assert_eq!(d.next_deadline(), Some(t + RESCAN_MIN_GAP));
        assert!(d.due(t + RESCAN_MIN_GAP - ms(1), 6).is_empty());
        assert_eq!(d.due(t + RESCAN_MIN_GAP, 6).len(), 2);
        assert_eq!(d.next_deadline(), None);
    }

    #[test]
    fn free_slots_limit_and_order() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        let a = PathBuf::from("/dev/a");
        let b = PathBuf::from("/dev/b");
        d.request(&b, Need::Local, t);
        d.request(&a, Need::Local, t + ms(1));
        assert!(d.due(t + ms(5), 0).is_empty());
        assert_eq!(d.due(t + ms(5), 1), vec![(b.clone(), Need::Local)]);
        assert_eq!(d.next_deadline(), Some(t + ms(1)));
        assert_eq!(d.due(t + ms(5), 1), vec![(a, Need::Local)]);
    }

    #[test]
    fn retain_keeps_in_flight_slots() {
        let t = Instant::now();
        let a = PathBuf::from("/dev/a");
        let b = PathBuf::from("/dev/b");
        let mut d = Debouncer::default();
        d.request(&a, Need::Local, t);
        d.request(&b, Need::Local, t);
        assert_eq!(d.due(t, 1), vec![(a.clone(), Need::Local)]);
        d.retain(&[]);
        assert_eq!(d.next_deadline(), None, "b was idle and is gone");
        // `a` is still running: it comes back and must not start twice.
        d.on_event(&a, Class::Worktree, t);
        d.retain(std::slice::from_ref(&a));
        d.request(&a, Need::Local, t);
        assert!(d.due(t + ms(100), 6).is_empty());
        d.finished(&a, Need::Local, t + ms(100));
        assert_eq!(d.next_deadline(), Some(t + ms(100) + MIN_GAP));

        // Removed for good: its slot goes when the check ends.
        let mut d = Debouncer::default();
        d.request(&a, Need::Local, t);
        d.due(t, 6);
        d.retain(&[]);
        d.finished(&a, Need::Local, t + ms(100));
        assert!(d.slots.is_empty());
    }
}
