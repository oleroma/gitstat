//! Turns raw filesystem events into "this repo needs a check" decisions:
//! the `Router` classifies paths, the `Debouncer` decides when to check.

use notify::EventKind;
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use std::collections::{HashMap, HashSet};
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
/// Folder churn that never pauses still gets a rediscovery this long after
/// its first event (or after the walk gap, if that ends later).
pub const REPOSET_MAX_WAIT: Duration = Duration::from_secs(5);
pub const CONFIG_QUIET: Duration = Duration::from_millis(300);
/// How long after a repo's `.gitstat/icon.svg` changes the folder icons
/// get another look: an editor's save is done by then.
pub const REPO_ICON_QUIET: Duration = Duration::from_millis(500);

/// What an event means for the repo it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Ignore,
    Worktree,
    GitMeta,
    RemoteRefs,
    RepoSet,
    Config,
    /// The repo's own folder icon, `.gitstat/icon.svg`, or its folder.
    RepoIcon,
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
    /// How many levels below the root the repo walk looks.
    depth: usize,
    config_file: String,
    routes: Vec<Keyed>,
    /// Folders the walk didn't enter, folded (see `Discovery::pruned`).
    pruned: HashSet<String>,
    /// Folders the walk went into, folded (see `Discovery::entered`).
    entered: HashSet<String>,
    /// A cap cut the last walk short: events outside the listed repos
    /// can't make the next walk find more, so only sweeps look again.
    capped: bool,
    /// Whether a path that just appeared is a folder (or a link to one).
    is_folder: fn(&Path) -> bool,
}

impl Router {
    pub fn new(root: PathBuf, depth: usize, config_file: PathBuf) -> Self {
        Self {
            root: key(&root),
            depth,
            config_file: key(&config_file),
            routes: Vec::new(),
            pruned: HashSet::new(),
            entered: HashSet::new(),
            capped: false,
            is_folder: folder_on_disk,
        }
    }

    pub fn set_repos(&mut self, routes: Vec<RepoRoute>) {
        self.routes = routes.into_iter().map(Keyed::new).collect();
    }

    pub fn set_pruned(&mut self, pruned: &[PathBuf]) {
        self.pruned = pruned.iter().map(|p| key(p)).collect();
    }

    pub fn set_entered(&mut self, entered: &[PathBuf]) {
        self.entered = entered.iter().map(|p| key(p)).collect();
    }

    pub fn set_capped(&mut self, capped: bool) {
        self.capped = capped;
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

        // `<dir>/.git` appearing or going away adds or removes a repo. With
        // the list capped, only for a listed repo.
        if let Some(rel) = rel_root
            && structural(kind)
            && self.is_repo_marker(rel)
            && (!self.capped || self.repo_of(&p).is_some())
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
        let Some(route) = self.repo_of(&p) else {
            // Outside every repo only the folder tree the walk sees matters:
            // a folder appearing, going or renamed there may hold a repo.
            let class = if !self.capped && self.walked(rel) && self.folder_change(path, &p, kind) {
                Class::RepoSet
            } else {
                Class::Ignore
            };
            out.push((None, class));
            return out;
        };
        let inner = under(&p, &route.path).unwrap_or_default();
        if inner.is_empty() {
            // The repo folder itself. Modify is only attributes or
            // timestamps, such as our own PathMakeSystemFolder.
            let class = if structural(kind) {
                Class::RepoSet
            } else {
                Class::Ignore
            };
            out.push((None, class));
            return out;
        }
        let repo = Some(route.route.path.clone());
        // Whatever git thinks of it: it is often ignored, and a change to
        // a committed one may leave the status as it was. Our own writes
        // never go there.
        if is_repo_icon(inner) {
            out.push((repo.clone(), Class::RepoIcon));
        }
        // Our own folder-icon writes, and a linked worktree's `.git` file.
        let class = if inner.eq_ignore_ascii_case("desktop.ini")
            || inner.eq_ignore_ascii_case(".git")
            || is_ignored(route, inner)
        {
            Class::Ignore
        } else {
            Class::Worktree
        };
        out.push((repo, class));
        out
    }

    /// Whether a structural event at `path` (folded: `p`, outside every
    /// repo) may be a folder appearing or going. Windows reports every
    /// removal, and a creation whose path is already gone, without saying
    /// what it was, and renames don't say on any OS. So: a folder the walk
    /// went into going away (or being renamed away), or a path appearing
    /// (or renamed into place) that is a folder now.
    fn folder_change(&self, path: &Path, p: &str, kind: &EventKind) -> bool {
        use notify::event::RenameMode;
        let known = || self.entered.contains(p);
        let appeared = || (self.is_folder)(path);
        match kind {
            EventKind::Create(CreateKind::Folder) => true,
            EventKind::Create(CreateKind::File) | EventKind::Remove(RemoveKind::File) => false,
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                appeared()
            }
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => known(),
            // A rename of unknown direction (or both paths of one), or
            // anything at all.
            EventKind::Any | EventKind::Modify(ModifyKind::Name(_)) => known() || appeared(),
            _ => false,
        }
    }

    /// The repo `p` is in or is, by the longest repo path that holds it.
    fn repo_of(&self, p: &str) -> Option<&Keyed> {
        self.routes
            .iter()
            .filter(|r| under(p, &r.path).is_some())
            .max_by_key(|r| r.path.len())
    }

    /// Whether `rel` (below the root) is a folder's `.git` that the walk
    /// would look for: the folder is the root or a level the walk reaches,
    /// and not inside a repo (the repo itself is fine: its `.git` going
    /// away removes it).
    fn is_repo_marker(&self, rel: &str) -> bool {
        let dir = if rel.eq_ignore_ascii_case(".git") {
            ""
        } else {
            match rel.rsplit_once('/') {
                Some((dir, name)) if name.eq_ignore_ascii_case(".git") => dir,
                _ => return false,
            }
        };
        if dir.is_empty() {
            return true;
        }
        let full = format!("{}/{dir}", self.root.trim_end_matches('/'));
        let inside_repo = self
            .routes
            .iter()
            .any(|r| under(&full, &r.path).is_some_and(|inner| !inner.is_empty()));
        !inside_repo && self.walked(dir)
    }

    /// Whether the repo walk lists `rel` (below the root, not inside a
    /// repo): it is at most `depth` levels down, and every folder above it
    /// is one the walk enters. Its own name doesn't matter, since a folder
    /// holding `.git` is a repo whatever it is called.
    fn walked(&self, rel: &str) -> bool {
        if rel.split('/').count() > self.depth {
            return false;
        }
        let root = self.root.trim_end_matches('/');
        rel.match_indices('/').all(|(end, _)| {
            let parent = &rel[..end];
            let name = parent.rsplit('/').next().unwrap_or(parent);
            crate::git::walks_into(name)
                && (self.pruned.is_empty() || !self.pruned.contains(&format!("{root}/{parent}")))
        })
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

/// A folder, or a link that may point at one, as found on disk now.
fn folder_on_disk(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() || m.file_type().is_symlink())
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

/// `.gitstat/icon.svg` below a repo, or the `.gitstat` folder, which can
/// appear or go with the file inside (a checkout, a rename) and only
/// report itself. As exact as the file system: any case on Windows.
fn is_repo_icon(rel: &str) -> bool {
    let (dir, file) = (crate::repo_icon::DIR, crate::repo_icon::FILE);
    #[cfg(windows)]
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    #[cfg(not(windows))]
    let same = |a: &str, b: &str| a == b;
    match rel.split_once('/') {
        None => same(rel, dir),
        Some((d, f)) => same(d, dir) && same(f, file),
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
    /// Removed from the repo list while its check ran; dropped when it ends.
    orphan: bool,
    /// The running check belongs to an earlier root. Its result is dropped,
    /// so requests made meanwhile run after it rather than counting on it.
    stale: bool,
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
    /// A deferred rescan ran inside `due`; the scheduler rediscovers repos.
    rescanned: bool,
    /// Each repo's common dir, folded: repos sharing one share
    /// `refs/remotes`, and two fetches there race for the ref locks.
    groups: HashMap<PathBuf, String>,
}

impl Debouncer {
    pub fn on_event(&mut self, repo: &Path, class: Class, now: Instant) {
        let quiet = match class {
            Class::GitMeta | Class::RemoteRefs => QUIET_META,
            Class::Worktree => QUIET_WORKTREE,
            Class::Ignore | Class::RepoSet | Class::Config | Class::RepoIcon => return,
        };
        // Remote refs count even during our own fetch: a `git push` from a
        // terminal writes them too, and a Local recheck can't loop (it
        // never writes refs).
        let slot = self.slots.entry(repo.to_path_buf()).or_default();
        if slot.in_flight.is_some() {
            slot.rerun = slot.rerun.max(Some(Need::Local));
            return;
        }
        slot.pending = slot.pending.max(Some(Need::Local));
        let first = *slot.first.get_or_insert(now);
        let until = slot.quiet_until.map_or(now + quiet, |u| u.max(now + quiet));
        slot.quiet_until = Some(until.min(first + MAX_WAIT));
    }

    /// Sweep or manual refresh: due now, still after `MIN_GAP`. A running
    /// check of at least `need` already covers the request, unless it
    /// belongs to an earlier root.
    pub fn request(&mut self, repo: &Path, need: Need, now: Instant) {
        let slot = self.slots.entry(repo.to_path_buf()).or_default();
        if let Some(running) = slot.in_flight {
            if need > running || slot.stale {
                slot.rerun = slot.rerun.max(Some(need));
            }
            return;
        }
        slot.pending = slot.pending.max(Some(need));
        slot.first.get_or_insert(now);
        slot.quiet_until = Some(slot.quiet_until.map_or(now, |u| u.min(now)));
    }

    /// Like `request(repo, Local)`, for when events may have been lost: a
    /// running check may have read the status before them, so it reruns.
    pub fn recheck(&mut self, repo: &Path, now: Instant) {
        let slot = self.slots.entry(repo.to_path_buf()).or_default();
        if slot.in_flight.is_some() {
            slot.rerun = slot.rerun.max(Some(Need::Local));
        } else {
            self.request(repo, Need::Local, now);
        }
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
            self.recheck(&repo, now);
        }
        true
    }

    /// Whether a deferred rescan ran inside `due` since the last call.
    pub fn take_rescanned(&mut self) -> bool {
        std::mem::take(&mut self.rescanned)
    }

    /// Sets which repos share a common dir.
    pub fn set_groups(&mut self, routes: &[RepoRoute]) {
        self.groups = routes
            .iter()
            .map(|r| (r.path.clone(), key(&r.common_dir)))
            .collect();
    }

    /// Common dirs a fetch is running in.
    fn fetching_groups(&self) -> HashSet<&str> {
        self.slots
            .iter()
            .filter(|(_, slot)| slot.in_flight == Some(Need::Fetch))
            .filter_map(|(path, _)| self.groups.get(path).map(String::as_str))
            .collect()
    }

    /// A pending fetch waits while another fetch runs in its common dir.
    fn held(&self, path: &Path, slot: &Slot, fetching: &HashSet<&str>) -> bool {
        slot.pending == Some(Need::Fetch)
            && self
                .groups
                .get(path)
                .is_some_and(|g| fetching.contains(g.as_str()))
    }

    /// Checks that are due, marked in flight. Never two fetches in one
    /// common dir at once.
    pub fn due(&mut self, now: Instant, free_slots: usize) -> Vec<(PathBuf, Need)> {
        if self.rescan_at.is_some_and(|at| at <= now) {
            self.rescanned |= self.rescan(now);
        }
        if free_slots == 0 {
            return Vec::new();
        }
        let mut fetching: HashSet<String> = self
            .fetching_groups()
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut ready: Vec<(Instant, &PathBuf, &Slot)> = self
            .slots
            .iter()
            .filter_map(|(path, slot)| {
                let at = slot.ready_at().filter(|at| *at <= now)?;
                Some((at, path, slot))
            })
            .collect();
        ready.sort_by_key(|&(at, path, _)| (at, path));
        let mut picked: Vec<PathBuf> = Vec::new();
        for (_, path, slot) in ready {
            if picked.len() == free_slots {
                break;
            }
            if slot.pending == Some(Need::Fetch)
                && let Some(group) = self.groups.get(path)
                && !fetching.insert(group.clone())
            {
                continue;
            }
            picked.push(path.clone());
        }
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

    pub fn finished(&mut self, repo: &Path, now: Instant) {
        let Some(slot) = self.slots.get_mut(repo) else {
            return;
        };
        slot.in_flight = None;
        slot.stale = false;
        slot.last_end = Some(now);
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

    /// The root changed: forget everything but the checks still running,
    /// which keep their repo from starting a second one until they end.
    pub fn next_root(&mut self) {
        let slots = std::mem::take(&mut self.slots)
            .into_iter()
            .filter_map(|(path, slot)| {
                let running = Slot {
                    in_flight: Some(slot.in_flight?),
                    orphan: true,
                    stale: true,
                    ..Default::default()
                };
                Some((path, running))
            })
            .collect();
        *self = Debouncer {
            slots,
            ..Default::default()
        };
    }

    /// A held fetch has no deadline of its own: the end of the fetch it
    /// waits for wakes the scheduler.
    pub fn next_deadline(&self) -> Option<Instant> {
        let fetching = self.fetching_groups();
        self.slots
            .iter()
            .filter(|(path, slot)| !self.held(path, slot, &fetching))
            .filter_map(|(_, slot)| slot.ready_at())
            .chain(self.rescan_at)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{DataChange, MetadataKind, RenameMode};

    const MODIFY: EventKind = EventKind::Modify(ModifyKind::Data(DataChange::Content));
    const ATTRIB: EventKind = EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any));
    const CREATE: EventKind = EventKind::Create(CreateKind::Any);
    const REMOVE: EventKind = EventKind::Remove(RemoveKind::Any);
    const RENAME: EventKind = EventKind::Modify(ModifyKind::Name(RenameMode::To));
    const RENAME_FROM: EventKind = EventKind::Modify(ModifyKind::Name(RenameMode::From));
    const RENAME_ANY: EventKind = EventKind::Modify(ModifyKind::Name(RenameMode::Any));

    /// Test paths don't exist: a name with an extension is a file, the rest
    /// are folders.
    fn fake_folder(path: &Path) -> bool {
        path.extension().is_none()
    }

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
        let mut router = Router::new("/dev".into(), 3, "/dev/gitstat/config.json".into());
        router.is_folder = fake_folder;
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
        let mut router = Router::new("/dev".into(), 3, "/cfg/config.json".into());
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
        let mut router = router();
        // A folder the last walk went into, so its removal counts.
        router.set_entered(&[PathBuf::from("/dev/new")]);
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

    /// `/d` (a whole drive, say) with repos at depths 1, 2 and 3.
    fn deep_router(depth: usize) -> Router {
        let mut router = Router::new("/d".into(), depth, "/cfg/config.json".into());
        router.is_folder = fake_folder;
        // What the walk went into at depth 3.
        router.set_entered(&[
            PathBuf::from("/d/dev"),
            PathBuf::from("/d/dev/grp"),
            PathBuf::from("/d/old"),
        ]);
        router.set_repos(vec![
            route("/d/a", "/d/a/.git", "/d/a/.git"),
            route("/d/dev/b", "/d/dev/b/.git", "/d/dev/b/.git"),
            route("/d/dev/grp/c", "/d/dev/grp/c/.git", "/d/dev/grp/c/.git"),
        ]);
        router
    }

    const NEW_DIR: EventKind = EventKind::Create(CreateKind::Folder);
    const NEW_FILE: EventKind = EventKind::Create(CreateKind::File);
    const GONE_FILE: EventKind = EventKind::Remove(RemoveKind::File);
    const GONE_DIR: EventKind = EventKind::Remove(RemoveKind::Folder);

    #[test]
    fn routes_repos_at_any_depth() {
        let router = deep_router(3);
        assert_eq!(
            classes(&router, "/d/dev/grp/c/src/main.rs", MODIFY),
            one("/d/dev/grp/c", Class::Worktree)
        );
        assert_eq!(
            classes(&router, "/d/dev/grp/c/.git/HEAD", MODIFY),
            one("/d/dev/grp/c", Class::GitMeta)
        );
        assert_eq!(
            classes(&router, "/d/dev/grp/c/.git/refs/remotes/o/m", RENAME),
            one("/d/dev/grp/c", Class::RemoteRefs)
        );
        assert_eq!(
            classes(&router, "/d/dev/b/deep/er/x.rs", CREATE),
            one("/d/dev/b", Class::Worktree)
        );
        assert_eq!(
            classes(&router, "/d/dev/b/desktop.ini", CREATE),
            one("/d/dev/b", Class::Ignore)
        );
        // A repo folder's own attributes (our folder icon) don't count.
        assert_eq!(
            classes(&router, "/d/dev/grp/c", ATTRIB),
            vec![(None, Class::Ignore)]
        );
        // `c2` only shares a prefix with `c`.
        assert_eq!(
            classes(&router, "/d/dev/grp/c2/x", MODIFY),
            vec![(None, Class::Ignore)]
        );
        // Longest prefix wins, should a repo ever sit inside another.
        let mut nested = deep_router(3);
        let mut routes: Vec<RepoRoute> = nested.routes().cloned().collect();
        routes.push(route(
            "/d/dev/b/inner",
            "/d/dev/b/inner/.git",
            "/d/dev/b/inner/.git",
        ));
        nested.set_repos(routes);
        assert_eq!(
            classes(&nested, "/d/dev/b/inner/f", MODIFY),
            one("/d/dev/b/inner", Class::Worktree)
        );
        assert_eq!(
            classes(&nested, "/d/dev/b/f", MODIFY),
            one("/d/dev/b", Class::Worktree)
        );
    }

    #[test]
    fn repo_set_at_depth() {
        let router = deep_router(3);
        let repo_set = vec![(None, Class::RepoSet)];
        let ignore = vec![(None, Class::Ignore)];
        // Folders appearing at levels 1 to 3, whatever their name.
        for kind in [CREATE, NEW_DIR, RENAME, RENAME_ANY] {
            for path in [
                "/d/new",
                "/d/dev/new",
                "/d/dev/grp/new",
                "/d/dev/node_modules",
                "/d/dev/.config",
            ] {
                assert_eq!(classes(&router, path, kind), repo_set, "{path} {kind:?}");
            }
        }
        // Folders the walk went into going away; others can't take a repo
        // with them (a repo is routed, see below).
        for kind in [REMOVE, GONE_DIR, RENAME_FROM, RENAME_ANY] {
            for path in ["/d/dev", "/d/dev/grp", "/d/old"] {
                assert_eq!(classes(&router, path, kind), repo_set, "{path} {kind:?}");
            }
        }
        for kind in [REMOVE, GONE_DIR, RENAME_FROM] {
            for path in ["/d/dev/new", "/d/dev/node_modules", "/d/dev/grp/x"] {
                assert_eq!(classes(&router, path, kind), ignore, "{path} {kind:?}");
            }
        }
        for kind in [CREATE, NEW_DIR, REMOVE, RENAME] {
            // Known repos coming and going, and `.git` at levels 0 to 3.
            for path in [
                "/d/a",
                "/d/dev/grp/c",
                "/d/.git",
                "/d/new/.git",
                "/d/dev/new/.git",
                "/d/dev/grp/new/.git",
                "/d/dev/grp/c/.git",
                "/d/dev/node_modules/.git",
            ] {
                assert_eq!(classes(&router, path, kind), repo_set, "{path} {kind:?}");
            }
            // Too deep, inside a skipped folder, or inside a repo.
            for path in [
                "/d/dev/grp/new/x",
                "/d/dev/grp/new/deeper/.git",
                "/d/dev/node_modules/pkg",
                "/d/dev/node_modules/pkg/.git",
                "/d/Target/x",
                "/d/.cache/x",
                "/d/.cache/x/.git",
                "/d/$Recycle.Bin/S-1-5",
                "/d/dev/new/.git/HEAD",
            ] {
                assert_eq!(classes(&router, path, kind), ignore, "{path} {kind:?}");
            }
            assert_eq!(
                classes(&router, "/d/dev/grp/c/sub/.git", kind),
                one("/d/dev/grp/c", Class::Worktree),
                "{kind:?}"
            );
        }
        // Files don't make repos, however they are reported: Windows says
        // Remove(Any) for every removal and Create(Any) when the file is
        // already gone, and renames don't say on any OS.
        for kind in [
            NEW_FILE,
            GONE_FILE,
            MODIFY,
            CREATE,
            REMOVE,
            RENAME,
            RENAME_FROM,
            RENAME_ANY,
        ] {
            assert_eq!(
                classes(&router, "/d/dev/notes.txt", kind),
                ignore,
                "{kind:?}"
            );
            assert_eq!(
                classes(&router, "/d/Downloads/x.crdownload", kind),
                ignore,
                "{kind:?}"
            );
        }
        // A worktree `.git` file is created as a file.
        assert_eq!(classes(&router, "/d/dev/wt/.git", NEW_FILE), repo_set);

        // At depth 1 only the root's own folders count, as before.
        let shallow = deep_router(1);
        assert_eq!(classes(&shallow, "/d/dev", NEW_DIR), repo_set);
        assert_eq!(classes(&shallow, "/d/dev/new", NEW_DIR), ignore);
        assert_eq!(classes(&shallow, "/d/new/.git", NEW_DIR), repo_set);
        assert_eq!(classes(&shallow, "/d/dev/new/.git", NEW_DIR), ignore);
        // The repos deeper down still route.
        assert_eq!(
            classes(&shallow, "/d/dev/grp/c/x", MODIFY),
            one("/d/dev/grp/c", Class::Worktree)
        );
    }

    #[test]
    fn pruned_folders_are_ignored() {
        let mut router = deep_router(3);
        router.set_pruned(&[PathBuf::from("/d/link"), PathBuf::from("/d/dev/Hidden")]);
        let ignore = vec![(None, Class::Ignore)];
        for path in ["/d/link/x", "/d/link/x/.git", "/d/dev/Hidden/r"] {
            assert_eq!(classes(&router, path, NEW_DIR), ignore, "{path}");
        }
        // The folder itself may be replaced by a plain one. Its removal
        // can't take a repo with it: the walk didn't go in.
        assert_eq!(
            classes(&router, "/d/link", NEW_DIR),
            vec![(None, Class::RepoSet)]
        );
        assert_eq!(classes(&router, "/d/link", REMOVE), ignore);
        router.set_pruned(&[]);
        assert_eq!(
            classes(&router, "/d/link/x", NEW_DIR),
            vec![(None, Class::RepoSet)]
        );
    }

    #[test]
    fn capped_walk_ignores_what_it_did_not_list() {
        let mut router = deep_router(3);
        router.set_capped(true);
        let ignore = vec![(None, Class::Ignore)];
        let repo_set = vec![(None, Class::RepoSet)];
        // Repo 501 (unlisted), or a folder past the folder cap.
        for (path, kind) in [
            ("/d/dev/r501/target", NEW_DIR),
            ("/d/dev/r501/x.rs", REMOVE),
            ("/d/dev/new", NEW_DIR),
            ("/d/dev", REMOVE),
            ("/d/dev/new/.git", NEW_DIR),
            ("/d/.git", NEW_DIR),
        ] {
            assert_eq!(classes(&router, path, kind), ignore, "{path}");
        }
        // A listed repo going away may make room for another.
        assert_eq!(classes(&router, "/d/dev/grp/c", REMOVE), repo_set);
        assert_eq!(classes(&router, "/d/dev/grp/c/.git", REMOVE), repo_set);
        assert_eq!(
            classes(&router, "/d/a/x", MODIFY),
            one("/d/a", Class::Worktree)
        );
        router.set_capped(false);
        assert_eq!(classes(&router, "/d/dev/new", NEW_DIR), repo_set);
    }

    #[test]
    fn root_that_is_a_repo() {
        let mut router = Router::new("/r".into(), 3, "/cfg/config.json".into());
        router.set_repos(vec![route("/r", "/r/.git", "/r/.git")]);
        assert_eq!(
            classes(&router, "/r/src/x", NEW_DIR),
            one("/r", Class::Worktree)
        );
        assert_eq!(
            classes(&router, "/r/.git/index", MODIFY),
            one("/r", Class::GitMeta)
        );
        // A nested repo is part of the worktree; the root's own `.git` isn't.
        assert_eq!(
            classes(&router, "/r/sub/.git", NEW_DIR),
            one("/r", Class::Worktree)
        );
        assert_eq!(
            classes(&router, "/r/.git", REMOVE),
            vec![(None, Class::RepoSet)]
        );
        assert_eq!(classes(&router, "/r", REMOVE), vec![]);
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
    fn repo_icon_changes() {
        let mut router = router();
        let icon = |repo: &str, check: Class| {
            vec![
                (Some(repo.to_string()), Class::RepoIcon),
                (Some(repo.to_string()), check),
            ]
        };
        for kind in [CREATE, MODIFY, REMOVE, RENAME, RENAME_FROM] {
            assert_eq!(
                classes(&router, "/dev/r/.gitstat/icon.svg", kind),
                icon("/dev/r", Class::Worktree),
                "{kind:?}"
            );
            assert_eq!(
                classes(&router, "/dev/r/.gitstat", kind),
                icon("/dev/r", Class::Worktree),
                "{kind:?}"
            );
        }
        // Gitignored, it still counts, without a status check.
        router.set_ignored(Path::new("/dev/r"), vec![".gitstat/".into()]);
        assert_eq!(
            classes(&router, "/dev/r/.gitstat/icon.svg", MODIFY),
            icon("/dev/r", Class::Ignore)
        );
        assert_eq!(
            classes(&router, "/dev/r/.gitstat", CREATE),
            icon("/dev/r", Class::Ignore)
        );
        // Every repo has its own, at any depth.
        let deep = deep_router(3);
        assert_eq!(
            classes(&deep, "/d/dev/grp/c/.gitstat/icon.svg", RENAME),
            icon("/d/dev/grp/c", Class::Worktree)
        );
        // Other files there, deeper ones, and ones outside a repo don't.
        for path in [
            "/dev/r/.gitstat/icon.svg.tmp",
            "/dev/r/.gitstat/other.svg",
            "/dev/r/src/.gitstat/icon.svg",
            "/dev/r/.gitstat/icon.svg/x",
        ] {
            let found = classes(&router, path, MODIFY);
            assert!(
                !found.iter().any(|(_, c)| *c == Class::RepoIcon),
                "{path}: {found:?}"
            );
        }
        assert_eq!(
            classes(&router, "/dev/notes/.gitstat/icon.svg", MODIFY),
            vec![(None, Class::Ignore)]
        );
        #[cfg(not(windows))]
        assert_eq!(
            classes(&self::router(), "/dev/r/.gitstat/Icon.SVG", MODIFY),
            one("/dev/r", Class::Worktree)
        );
        // It never schedules a check of its own.
        let mut d = Debouncer::default();
        d.on_event(Path::new("/dev/r"), Class::RepoIcon, Instant::now());
        assert_eq!(d.next_deadline(), None);
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
        let outside = Router::new("/dev".into(), 3, "/cfg/config.json".into());
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
        let mut router = Router::new(r"D:\Dev".into(), 3, "d:/dev/gitstat/CONFIG.json".into());
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
        assert_eq!(
            classes(&router, r"D:\DEV\r\.GitStat\Icon.SVG", MODIFY),
            vec![
                (Some(r"D:\Dev\r".into()), Class::RepoIcon),
                (Some(r"D:\Dev\r".into()), Class::Worktree)
            ]
        );
        // A whole drive as the root.
        let mut drive = Router::new(r"D:\".into(), 3, r"D:\dev\gitstat\config.json".into());
        drive.is_folder = fake_folder;
        drive.set_repos(vec![route(r"D:\Dev\r", "D:/Dev/r/.git", "D:/Dev/r/.git")]);
        drive.set_pruned(&[PathBuf::from(r"D:\ProgramData")]);
        assert_eq!(
            classes(&drive, r"d:\dev\R\src\x.rs", MODIFY),
            one(r"D:\Dev\r", Class::Worktree)
        );
        assert_eq!(
            classes(&drive, r"D:\Dev\Group\new", CREATE),
            vec![(None, Class::RepoSet)]
        );
        assert_eq!(
            classes(&drive, r"D:\programdata\x\y", CREATE),
            vec![(None, Class::Ignore)]
        );
        assert_eq!(
            classes(&drive, r"D:\$RECYCLE.BIN\S-1\x", CREATE),
            vec![(None, Class::Ignore)]
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
        for (repo, _) in &due {
            d.finished(repo, at);
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
        d.finished(&repo(), end);
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
        assert_eq!(d.due(end + MIN_GAP, 6), vec![(repo(), Need::Local)]);
        d.finished(&repo(), end + MIN_GAP);
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
        d.finished(&repo(), t + ms(10));
        assert_eq!(d.next_deadline(), None);
        // A fetch requested during a local check runs after it.
        d.request(&repo(), Need::Local, t + MIN_GAP * 2);
        assert_eq!(d.due(t + MIN_GAP * 2, 6), vec![(repo(), Need::Local)]);
        d.request(&repo(), Need::Fetch, t + MIN_GAP * 2);
        d.finished(&repo(), t + MIN_GAP * 3);
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
    fn remote_refs_always_count() {
        let t = Instant::now();
        let end = t + ms(500);
        // A `git push` from a terminal while our fetch runs: the status
        // the fetch reads may predate it, so a Local check follows.
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Fetch, t);
        assert_eq!(d.due(t, 6).len(), 1);
        d.on_event(&repo(), Class::RemoteRefs, t + ms(100));
        d.finished(&repo(), end);
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
        assert_eq!(d.due(end + MIN_GAP, 6), vec![(repo(), Need::Local)]);
        d.finished(&repo(), end + MIN_GAP);
        assert_eq!(d.next_deadline(), None, "one rerun, not a loop");
        // Just after a fetch too.
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Fetch, t);
        d.due(t, 6);
        d.finished(&repo(), end);
        d.on_event(&repo(), Class::RemoteRefs, end + ms(10));
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
        // And during a local check.
        let mut d = Debouncer::default();
        d.request(&repo(), Need::Local, t);
        d.due(t, 6);
        d.on_event(&repo(), Class::RemoteRefs, t + ms(100));
        d.finished(&repo(), end);
        assert_eq!(d.next_deadline(), Some(end + MIN_GAP));
    }

    #[test]
    fn rescan_reruns_a_running_check() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.retain(&[repo()]);
        d.request(&repo(), Need::Local, t);
        assert_eq!(d.due(t, 6), vec![(repo(), Need::Local)]);
        // Events lost while the check runs: it may have read status first.
        assert!(d.rescan(t + ms(100)));
        let end = t + ms(500);
        d.finished(&repo(), end);
        assert_eq!(d.due(end + MIN_GAP, 6), vec![(repo(), Need::Local)]);
        // `recheck` does the same for one repo, and is plain otherwise.
        d.recheck(&repo(), end + MIN_GAP);
        d.finished(&repo(), end + MIN_GAP * 2);
        assert_eq!(d.due(end + MIN_GAP * 4, 6), vec![(repo(), Need::Local)]);
        let mut d = Debouncer::default();
        d.recheck(&repo(), t);
        assert_eq!(d.due(t, 6), vec![(repo(), Need::Local)]);
    }

    #[test]
    fn one_fetch_per_common_dir() {
        let t = Instant::now();
        let (r, wt, other) = (
            PathBuf::from("/dev/r"),
            PathBuf::from("/dev/wt"),
            PathBuf::from("/dev/other"),
        );
        let mut d = Debouncer::default();
        d.set_groups(&[
            route("/dev/r", "/dev/r/.git", "/dev/r/.git"),
            route("/dev/wt", "/dev/r/.git/worktrees/wt", "/dev/r/.git"),
            route("/dev/other", "/dev/other/.git", "/dev/other/.git"),
        ]);
        for repo in [&r, &wt, &other] {
            d.request(repo, Need::Fetch, t);
        }
        let first = d.due(t, 6);
        assert_eq!(first.len(), 2, "{first:?}");
        assert!(first.contains(&(other.clone(), Need::Fetch)));
        let (shared, _) = first.iter().find(|(p, _)| *p != other).unwrap().clone();
        let waiting = if shared == r { wt.clone() } else { r.clone() };
        // Held, not busy-waiting: no deadline until that fetch ends.
        assert_eq!(d.next_deadline(), None);
        assert!(d.due(t + ms(5000), 6).is_empty());
        // A local check in the same common dir isn't held.
        d.on_event(&waiting, Class::Worktree, t);
        assert!(d.due(t + ms(5000), 6).is_empty(), "still a pending fetch");
        let end = t + ms(6000);
        d.finished(&shared, end);
        assert_eq!(d.next_deadline(), Some(t + QUIET_WORKTREE));
        assert_eq!(d.due(end, 6), vec![(waiting, Need::Fetch)]);

        // Without groups (or in different ones) fetches run side by side.
        let mut d = Debouncer::default();
        d.request(&r, Need::Fetch, t);
        d.request(&wt, Need::Fetch, t);
        assert_eq!(d.due(t, 6).len(), 2);
    }

    #[test]
    fn root_change_waits_for_old_checks() {
        let t = Instant::now();
        let (a, b) = (PathBuf::from("/dev/a"), PathBuf::from("/dev/b"));
        let mut d = Debouncer::default();
        d.request(&a, Need::Fetch, t);
        d.request(&b, Need::Local, t + ms(1));
        assert_eq!(d.due(t, 1), vec![(a.clone(), Need::Fetch)]);
        d.next_root();
        assert_eq!(
            d.next_deadline(),
            None,
            "b's pending check was the old root's"
        );
        // The new root has `a` again (A -> B -> A): its sweep must not
        // start a second fetch while the old one runs, nor count on it.
        d.retain(std::slice::from_ref(&a));
        d.request(&a, Need::Fetch, t + ms(10));
        assert!(d.due(t + ms(10), 6).is_empty());
        let end = t + ms(100);
        d.finished(&a, end);
        assert_eq!(d.due(end + MIN_GAP, 6), vec![(a.clone(), Need::Fetch)]);
        // A new-root check is covered as usual.
        d.request(&a, Need::Fetch, end + MIN_GAP);
        d.finished(&a, end + MIN_GAP);
        assert_eq!(d.next_deadline(), None);

        // An old check of a repo the new root doesn't have just ends.
        let mut d = Debouncer::default();
        d.request(&a, Need::Local, t);
        d.due(t, 6);
        d.next_root();
        d.retain(&[]);
        d.finished(&a, t + ms(100));
        assert!(d.slots.is_empty());
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
        d.finished(&a, t + ms(100));
        assert_eq!(d.next_deadline(), Some(t + ms(100) + MIN_GAP));

        // Removed for good: its slot goes when the check ends.
        let mut d = Debouncer::default();
        d.request(&a, Need::Local, t);
        d.due(t, 6);
        d.retain(&[]);
        d.finished(&a, t + ms(100));
        assert!(d.slots.is_empty());
    }
}
