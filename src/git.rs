//! Fetching repos and working out how each one compares to its upstream.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// Parallel `git fetch`es. Enough to hide network latency without
/// hammering a remote that hosts most of the repos. The timeline caps its
/// history loads at this too.
pub(crate) const MAX_PARALLEL: usize = 6;
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const LOCAL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoStatus {
    pub name: String,
    pub path: PathBuf,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
    pub has_upstream: bool,
    pub error: Option<String>,
    /// `error` came from `git fetch`, so the local state is still worth keeping.
    pub fetch_failed: bool,
    /// The fetch failed, whether or not the local state could be read (then
    /// `fetch_failed` is false). Drives the retry backoff, and lets a later
    /// local-only recheck keep the fetch error.
    pub fetch_errored: bool,
}

impl RepoStatus {
    pub fn is_synced(&self) -> bool {
        self.error.is_none()
            && self.has_upstream
            && !self.dirty
            && self.ahead == 0
            && self.behind == 0
    }

    pub fn is_diverged(&self) -> bool {
        self.ahead > 0 && self.behind > 0
    }

    /// Short status text such as `⬆2 ⬇1 diverged dirty` or `✓ current`.
    pub fn label(&self) -> String {
        if let Some(error) = &self.error {
            return format!("error: {error}");
        }
        let mut parts = Vec::new();
        if !self.has_upstream {
            parts.push("no upstream".to_string());
        }
        if self.ahead > 0 {
            parts.push(format!("⬆{}", self.ahead));
        }
        if self.behind > 0 {
            parts.push(format!("⬇{}", self.behind));
        }
        if self.is_diverged() {
            parts.push("diverged".to_string());
        }
        if self.dirty {
            parts.push("dirty".to_string());
        }
        if parts.is_empty() {
            "✓ current".to_string()
        } else {
            parts.join(" ")
        }
    }
}

/// How deep `find_repos` looks by default, and the most it will.
pub const DEFAULT_DEPTH: usize = 3;
pub const MAX_DEPTH: usize = 8;
/// A walk stops after examining this many folders...
const MAX_DIRS: usize = 50_000;
/// ...or finding this many repos, and says so.
const MAX_REPOS: usize = 500;

/// Folders the walk never descends into: build output, package caches and
/// system folders, where repos don't live and churn is heavy. Compared
/// case-insensitively. A folder holding `.git` is still a repo whatever its
/// name, so a repo called `build` or `bin` isn't lost.
const SKIP_DIRS: [&str; 13] = [
    // `$RECYCLE.BIN` on older Windows; the comparison ignores case.
    "$recycle.bin",
    "system volume information",
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    "vendor",
    "bin",
    "obj",
];

/// The repos `find_repos` found, and why it stopped early if it did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    /// Sorted by name (see [`repo_name`]), case-insensitively.
    pub repos: Vec<PathBuf>,
    /// Set when a cap cut the walk short, so the list may be incomplete.
    pub capped: Option<String>,
    /// Folders above `depth` the walk didn't enter for a reason an event
    /// path doesn't show: links, and on Windows hidden, system and other
    /// reparse-point folders. The watcher ignores what happens inside them.
    pub pruned: Vec<PathBuf>,
    /// The folders below `root` the walk went into. Only removing or
    /// renaming one of these can take repos away, so the watcher ignores
    /// other removals (which Windows doesn't say are files or folders).
    pub entered: Vec<PathBuf>,
}

/// Every git repo in `root` or up to `depth` levels below it (1: direct
/// subfolders only), breadth-first. A repo is a folder holding `.git` (a
/// folder, or a file for a linked worktree); repos are not descended into,
/// so nested repos and submodules are not listed. If `root` is itself a repo
/// it is the only one.
///
/// Never descended into: names starting with `.` or in [`SKIP_DIRS`],
/// symlinks and junctions (no cycles), on Windows hidden or system folders
/// and other reparse points (OneDrive placeholders), and folders that can't
/// be read.
pub fn find_repos(root: &Path, depth: usize) -> Discovery {
    find_repos_capped(root, depth, MAX_DIRS, MAX_REPOS)
}

fn find_repos_capped(root: &Path, depth: usize, max_dirs: usize, max_repos: usize) -> Discovery {
    let depth = depth.clamp(1, MAX_DEPTH);
    let mut found: Vec<(PathBuf, bool)> = Vec::new();
    let mut capped = None;
    let mut pruned = Vec::new();
    let mut entered = Vec::new();
    if is_repo(root) {
        found.push((root.to_path_buf(), false));
    } else {
        let mut queue = VecDeque::from([(root.to_path_buf(), 1)]);
        let mut visited = 0;
        'walk: while let Some((dir, level)) = queue.pop_front() {
            // Unreadable (access denied, gone meanwhile): skipped silently.
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(link) = folder_kind(&entry) else {
                    continue;
                };
                visited += 1;
                if visited > max_dirs {
                    capped = Some(format!(
                        "stopped looking after {max_dirs} folders; lower \"depth\" or \
                         point \"path\" at a smaller folder"
                    ));
                    break 'walk;
                }
                let path = entry.path();
                if is_repo(&path) {
                    if found.len() == max_repos {
                        capped = Some(format!("showing the first {max_repos} repos found"));
                        break 'walk;
                    }
                    found.push((path, link));
                } else if level < depth && walks_into(&entry.file_name().to_string_lossy()) {
                    if !link && plain_folder(&entry) {
                        entered.push(path.clone());
                        queue.push_back((path, level + 1));
                    } else {
                        pruned.push(path);
                    }
                }
            }
        }
    }
    let mut repos = drop_linked_duplicates(found);
    repos.sort_by_cached_key(|p| repo_name(root, p).to_lowercase());
    Discovery {
        repos,
        capped,
        pruned,
        entered,
    }
}

fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// `Some(is_link)` for a folder or a link that may point at one (a
/// dangling link too), `None` for anything else, a link to a file included.
fn folder_kind(entry: &std::fs::DirEntry) -> Option<bool> {
    let kind = entry.file_type().ok()?;
    // On Windows a junction is a symlink here too.
    if !kind.is_symlink() {
        return kind.is_dir().then_some(false);
    }
    #[cfg(windows)]
    {
        // From the directory listing: junctions and folder links are
        // symlink_dir. Following the link could block on an offline share.
        use std::os::windows::fs::FileTypeExt;
        (!kind.is_symlink_file()).then_some(true)
    }
    #[cfg(not(windows))]
    {
        match std::fs::metadata(entry.path()) {
            Ok(meta) if !meta.is_dir() => None,
            _ => Some(true),
        }
    }
}

/// A folder that isn't hidden, system or another kind of reparse point.
/// Always true off Windows, where hidden is the leading `.` [`walks_into`]
/// already checks.
fn plain_folder(entry: &std::fs::DirEntry) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SYSTEM,
        };
        // From the directory listing itself: no extra file-system call.
        let skip = FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM | FILE_ATTRIBUTE_REPARSE_POINT;
        entry
            .metadata()
            .is_ok_and(|meta| meta.file_attributes() & skip == 0)
    }
    #[cfg(not(windows))]
    {
        let _ = entry;
        true
    }
}

/// Whether the walk goes into a (non-repo) folder with this name, by name
/// alone: what the watcher can tell from an event path.
pub fn walks_into(name: &str) -> bool {
    !name.starts_with('.') && !SKIP_DIRS.iter().any(|skip| name.eq_ignore_ascii_case(skip))
}

/// A link to a repo is listed (as at depth 1 before), unless the walk also
/// found the repo it points at, or another link to it.
fn drop_linked_duplicates(found: Vec<(PathBuf, bool)>) -> Vec<PathBuf> {
    if !found.iter().any(|(_, link)| *link) {
        return found.into_iter().map(|(path, _)| path).collect();
    }
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut seen: HashSet<PathBuf> = found
        .iter()
        .filter(|(_, link)| !link)
        .map(|(path, _)| real(path))
        .collect();
    found
        .into_iter()
        .filter(|(path, link)| !link || seen.insert(real(path)))
        .map(|(path, _)| path)
        .collect()
}

/// A repo's display name: its path below `root` with the platform's
/// separator (`dev\gitstat`), or the root's own name when it is the root.
pub fn repo_name(root: &Path, repo: &Path) -> String {
    match repo.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => {
            let name = file_name(root);
            if name.is_empty() {
                root.display().to_string()
            } else {
                name
            }
        }
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join(std::path::MAIN_SEPARATOR_STR),
        Err(_) => file_name(repo),
    }
}

/// Checks `repos` (found under `root`), fetching first when `fetch` is set.
pub fn check_all(root: &Path, repos: &[PathBuf], fetch: bool) -> Vec<RepoStatus> {
    let mut results: Vec<Option<RepoStatus>> = vec![None; repos.len()];
    let (tx, rx) = mpsc::channel();
    let mut next = 0;
    let mut running = 0;
    while next < repos.len() || running > 0 {
        while running < MAX_PARALLEL && next < repos.len() {
            let tx = tx.clone();
            let (index, base, repo) = (next, root.to_path_buf(), repos[next].clone());
            let spawned = thread::Builder::new().spawn(move || {
                // Always report back, even if the check panics, so the loop can't wait forever.
                let status = std::panic::catch_unwind(|| check_repo(&base, &repo, fetch))
                    .unwrap_or_else(|_| failed(&base, &repo, "internal error"));
                let _ = tx.send((index, status));
            });
            if let Err(e) = spawned {
                results[index] = Some(failed(root, &repos[index], &e.to_string()));
                next += 1;
                continue;
            }
            next += 1;
            running += 1;
        }
        let Ok((index, status)) = rx.recv() else {
            break;
        };
        results[index] = Some(status);
        running -= 1;
    }
    results.into_iter().flatten().collect()
}

/// A repo whose check couldn't run.
pub fn failed(root: &Path, repo: &Path, error: &str) -> RepoStatus {
    RepoStatus {
        name: repo_name(root, repo),
        path: repo.to_path_buf(),
        error: Some(error.to_string()),
        ..Default::default()
    }
}

/// Checks `repo`, which `find_repos` found under `root` (for its name).
pub fn check_repo(root: &Path, repo: &Path, fetch: bool) -> RepoStatus {
    check_repo_detailed(root, repo, fetch).0
}

/// Like [`check_repo`], but also returns the ignored directory prefixes
/// (`target/`, `node_modules/`) the watcher uses to drop worktree noise.
///
/// A failed fetch still gets a local check, so the result carries fresh
/// local state alongside the fetch error (`fetch_failed`).
pub fn check_repo_detailed(root: &Path, repo: &Path, fetch: bool) -> (RepoStatus, Vec<String>) {
    let mut status = RepoStatus {
        name: repo_name(root, repo),
        path: repo.to_path_buf(),
        ..Default::default()
    };
    let fetch_error = if fetch { fetch_repo(repo).err() } else { None };
    status.fetch_errored = fetch_error.is_some();

    match local_status(repo, supports(STATUS_V2)) {
        Ok(local) => {
            status.dirty = local.dirty;
            status.has_upstream = local.has_upstream;
            status.ahead = local.ahead;
            status.behind = local.behind;
            status.fetch_failed = fetch_error.is_some();
            status.error = fetch_error;
            (status, local.ignored)
        }
        // The fetch error usually says more (it carries git's stderr).
        Err(error) => {
            status.error = Some(fetch_error.unwrap_or(error));
            (status, Vec::new())
        }
    }
}

/// A local-only recheck can't tell whether the network is back, so it keeps
/// the last fetch error until the next fetch replaces it. Callers apply this
/// only to local results, never to fetch results.
#[cfg(any(windows, test))]
pub fn keep_fetch_error(prev: &RepoStatus, mut local: RepoStatus) -> RepoStatus {
    if prev.fetch_errored && local.error.is_none() {
        local.error = prev.error.clone();
        local.fetch_failed = true;
        local.fetch_errored = true;
    }
    local
}

/// `--no-write-fetch-head` and `--no-auto-maintenance` arrived in 2.29.
const FETCH_QUIET_FLAGS: (u32, u32) = (2, 29);
/// `status --porcelain=v2` is 2.11, `--ignored=matching` is 2.16.
const STATUS_V2: (u32, u32) = (2, 16);
/// `rev-parse --path-format=absolute`.
#[cfg(any(windows, test))]
const PATH_FORMAT: (u32, u32) = (2, 31);
/// Beyond this many ignored directories nothing extra is filtered; the
/// watcher just does a few more checks.
const MAX_IGNORED: usize = 2048;

fn fetch_repo(repo: &Path) -> Result<(), String> {
    // Without these, every fetch writes FETCH_HEAD and may start a background
    // gc in the user's repo, both of which the watcher would see.
    let mut args = vec!["fetch", "--quiet", "--all", "--prune"];
    if supports(FETCH_QUIET_FLAGS) {
        args.extend(["--no-write-fetch-head", "--no-auto-maintenance"]);
    }
    let out = git(repo, &args, FETCH_TIMEOUT)?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(match stderr.trim().lines().last() {
        Some(line) => format!("fetch failed ({})", line.trim()),
        None => "fetch failed".to_string(),
    })
}

fn local_status(repo: &Path, v2: bool) -> Result<Parsed, String> {
    if v2 {
        let base = ["status", "--porcelain=v2", "-z", "--branch"];
        let out = git(
            repo,
            &[&base[..], &["--ignored=matching"]].concat(),
            LOCAL_TIMEOUT,
        )?;
        let out = if out.status.success() {
            out
        } else {
            // `status.showUntrackedFiles=no` rejects `--ignored=matching`.
            // The ignored dirs only spare the watcher some checks.
            git(repo, &base, LOCAL_TIMEOUT)?
        };
        if !out.status.success() {
            return Err("not readable".to_string());
        }
        return Ok(parse_status_v2(&out.stdout));
    }

    let mut parsed = Parsed::default();
    let out = git(repo, &["status", "--porcelain"], LOCAL_TIMEOUT)?;
    if !out.status.success() {
        return Err("not readable".to_string());
    }
    parsed.dirty = !out.stdout.trim_ascii().is_empty();
    let out = git(
        repo,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        LOCAL_TIMEOUT,
    )?;
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let mut counts = text.split_whitespace().map(|n| n.parse().unwrap_or(0));
        parsed.has_upstream = true;
        parsed.ahead = counts.next().unwrap_or(0);
        parsed.behind = counts.next().unwrap_or(0);
    }
    Ok(parsed)
}

/// What one `git status --porcelain=v2 -z --branch --ignored=matching` says.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
    /// `# branch.ab` is present only when the upstream exists, so a gone
    /// upstream counts as none, as `rev-list ...@{upstream}` failing did.
    pub has_upstream: bool,
    /// Ignored directories, relative to the repo, with their trailing `/`.
    pub ignored: Vec<String>,
}

pub(crate) fn parse_status_v2(out: &[u8]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut fields = out.split(|&b| b == 0);
    while let Some(field) = fields.next() {
        match field.first() {
            None => {}
            Some(b'#') => {
                if let Some(ab) = field.strip_prefix(b"# branch.ab ") {
                    let ab = String::from_utf8_lossy(ab);
                    let mut counts = ab.split_whitespace();
                    let mut count = |sign: char| {
                        counts
                            .next()
                            .and_then(|c| c.strip_prefix(sign))
                            .and_then(|c| c.parse().ok())
                            .unwrap_or(0)
                    };
                    parsed.ahead = count('+');
                    parsed.behind = count('-');
                    parsed.has_upstream = true;
                }
            }
            Some(b'!') => {
                if let Some(path) = field.strip_prefix(b"! ")
                    && path.ends_with(b"/")
                    && parsed.ignored.len() < MAX_IGNORED
                {
                    parsed
                        .ignored
                        .push(String::from_utf8_lossy(path).into_owned());
                }
            }
            Some(_) => {
                parsed.dirty = true;
                // A rename or copy carries its original path as the next
                // field, which could otherwise look like a `#` or `!` record.
                if field.starts_with(b"2 ") {
                    fields.next();
                }
            }
        }
    }
    parsed
}

/// The repo's git dir and common dir, both absolute. They differ for a
/// linked worktree, whose refs and config live in the main repo's git dir.
#[cfg(any(windows, test))]
pub fn git_dirs(repo: &Path) -> Option<(PathBuf, PathBuf)> {
    let modern = supports(PATH_FORMAT);
    let args: &[&str] = if modern {
        &[
            "rev-parse",
            "--path-format=absolute",
            "--absolute-git-dir",
            "--git-common-dir",
        ]
    } else {
        &["rev-parse", "--absolute-git-dir", "--git-common-dir"]
    };
    let out = git(repo, args, LOCAL_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let git_dir = git_path(repo, lines.next()?)?;
    let common_dir = git_path(repo, lines.next()?)?;
    Some((git_dir, common_dir))
}

/// Makes sure `pattern` is in the repo's `info/exclude`, so a file gitstat
/// writes into the worktree (desktop.ini) never shows up as untracked.
/// Returns whether it added the lines.
#[cfg(any(windows, test))]
pub fn ensure_excluded(repo: &Path, pattern: &str) -> Result<bool, String> {
    use std::io::Write;

    if pattern.trim().is_empty() || pattern.contains(['\n', '\r']) {
        return Err(format!("bad exclude pattern {pattern:?}"));
    }
    // `--git-path` follows `.git` files and linked worktrees to the right file.
    let out = git(
        repo,
        &["rev-parse", "--git-path", "info/exclude"],
        LOCAL_TIMEOUT,
    )?;
    if !out.status.success() {
        return Err("not a git repo".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let file = text
        .lines()
        .next()
        .and_then(|l| git_path(repo, l))
        .ok_or("no info/exclude path")?;

    let existing = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let present = String::from_utf8_lossy(&existing)
        .lines()
        .any(|l| l.trim_end() == pattern);
    if present {
        return Ok(false);
    }

    // The comment has to be its own line: git has no trailing comments, so
    // `/desktop.ini # gitstat` would be a pattern that matches nothing.
    let mut add = String::new();
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        add.push('\n');
    }
    add.push_str("# gitstat folder icon\n");
    add.push_str(pattern);
    add.push('\n');

    let fail = |e: std::io::Error| format!("{}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut f| f.write_all(add.as_bytes()))
        .map_err(fail)?;
    Ok(true)
}

/// Whether git tracks `rel` (relative to the repo). Case-insensitive, and
/// any answer other than a clean "no" counts as tracked, so a caller that
/// skips tracked files never writes into a repo it couldn't read.
#[cfg(any(windows, test))]
pub fn is_tracked(repo: &Path, rel: &Path) -> bool {
    let rel = rel.to_string_lossy().replace('\\', "/");
    let spec = format!(":(literal,icase){rel}");
    match git(
        repo,
        &["ls-files", "--error-unmatch", "--", &spec],
        LOCAL_TIMEOUT,
    ) {
        Ok(out) => out.status.code() != Some(1),
        Err(_) => true,
    }
}

/// A path git printed, made absolute against `repo` and given native
/// separators so it compares cleanly with paths from the watcher.
#[cfg(any(windows, test))]
fn git_path(repo: &Path, line: &str) -> Option<PathBuf> {
    if line.is_empty() {
        return None;
    }
    #[cfg(windows)]
    let line = line.replace('/', "\\");
    // Older git prints the common dir and `--git-path` relative to the repo.
    Some(repo.join(line))
}

/// Whether the installed git is at least `min`. An unknown version counts
/// as old, since the old commands work everywhere.
fn supports(min: (u32, u32)) -> bool {
    git_version().is_some_and(|v| v >= min)
}

/// Cached once known; a failure (git not on PATH yet) is retried next time.
fn git_version() -> Option<(u32, u32)> {
    static VERSION: OnceLock<(u32, u32)> = OnceLock::new();
    if let Some(v) = VERSION.get() {
        return Some(*v);
    }
    let out = run_git(None, &["--version"], LOCAL_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let v = parse_version(&String::from_utf8_lossy(&out.stdout))?;
    Some(*VERSION.get_or_init(|| v))
}

/// `git version 2.45.1.windows.1` -> `(2, 45)`.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let version = text.split_whitespace().nth(2)?;
    let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??))
}

/// Runs git in `repo`; see [`run_git`].
pub(crate) fn git(repo: &Path, args: &[&str], timeout: Duration) -> Result<Output, String> {
    run_git(Some(repo), args, timeout)
}

/// Runs git (in `repo` when given) with no console window, no credential
/// prompts and no optional locks, killing it if it runs past `timeout`.
///
/// `GIT_OPTIONAL_LOCKS=0` stops `status` and `diff` from refreshing
/// `.git/index`: that write would wake the watcher and loop forever, and it
/// could collide with the user's own `index.lock`.
fn run_git(repo: Option<&Path>, args: &[&str], timeout: Duration) -> Result<Output, String> {
    let mut command = Command::new("git");
    if let Some(repo) = repo {
        command.arg("-C").arg(repo);
    }
    command
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "git not found on PATH".to_string(),
        _ => e.to_string(),
    })?;
    #[cfg(windows)]
    let job = job::Job::assign(&child);

    // Drain the pipes on helper threads so a chatty git can't block on a full pipe.
    let stdout = spawn_reader(child.stdout.take());
    let stderr = spawn_reader(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(exit)) => break exit,
            Ok(None) if Instant::now() >= deadline => {
                // The whole tree: Git for Windows' `cmd\git.exe` is a
                // launcher, and `fetch --all` starts a fetch per remote.
                #[cfg(windows)]
                if let Some(job) = &job {
                    job.terminate();
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".to_string());
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    // Something git started (an ssh master, a credential helper) can keep the
    // pipes open after git exits, so don't wait on them for long.
    let collect =
        |rx: mpsc::Receiver<Vec<u8>>| rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    Ok(Output {
        status: exit,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// A job object holding one git child and everything it starts, so a
/// timeout can end them all. No kill-on-close: what git leaves running on
/// purpose (an fsmonitor daemon, a credential cache) outlives a normal exit.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    impl Job {
        /// `None` if the job can't be made or joined; a timeout then kills
        /// only the child. The launcher starts the real git only once it
        /// has loaded, well after this runs.
        pub fn assign(child: &std::process::Child) -> Option<Job> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return None;
            }
            let job = Job(handle);
            let process = child.as_raw_handle() as HANDLE;
            (unsafe { AssignProcessToJobObject(job.0, process) } != 0).then_some(job)
        }

        pub fn terminate(&self) {
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn spawn_reader(pipe: Option<impl std::io::Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    fn commit(dir: &Path, msg: &str) {
        run(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
    }

    fn output(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed in {}",
            dir.display()
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// A fresh empty directory for one test; tests run in parallel.
    fn temp_root(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("gitstat-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        run(dir, &["init", "-q"]);
        commit(dir, "init");
    }

    fn parse(records: &[&str]) -> Parsed {
        let mut out = records.join("\0");
        out.push('\0');
        parse_status_v2(out.as_bytes())
    }

    const HEADER: [&str; 2] = ["# branch.oid 1234567890abcdef", "# branch.head main"];

    fn with_header(rest: &[&'static str]) -> Vec<&'static str> {
        HEADER.iter().copied().chain(rest.iter().copied()).collect()
    }

    #[test]
    fn parses_status_v2() {
        let upstream = "# branch.upstream origin/main";
        let even = "# branch.ab +0 -0";
        let clean = parse(&with_header(&[upstream, even]));
        assert_eq!(
            clean,
            Parsed {
                has_upstream: true,
                ..Default::default()
            }
        );
        assert_eq!(parse_status_v2(b""), Parsed::default());

        let modified = "1 .M N... 100644 100644 100644 aaaa bbbb src/main.rs";
        let dirty = parse(&with_header(&[upstream, even, modified]));
        assert!(dirty.dirty && dirty.has_upstream);

        let untracked = parse(&with_header(&[upstream, even, "? notes.txt"]));
        assert!(untracked.dirty);

        // The original path of a rename is its own field and must not be
        // read as a record, even when it looks like one.
        let rename = "2 R. N... 100644 100644 100644 aaaa aaaa R100 new name.rs";
        let renamed = parse(&with_header(&[rename, "! old/", "! target/"]));
        assert!(renamed.dirty);
        assert_eq!(renamed.ignored, vec!["target/".to_string()]);

        let noup = parse(&HEADER);
        assert!(!noup.has_upstream && !noup.dirty);

        // A gone upstream keeps branch.upstream but drops branch.ab.
        let gone = parse(&with_header(&[upstream]));
        assert!(!gone.has_upstream);

        let diverged = parse(&with_header(&[upstream, "# branch.ab +2 -1"]));
        assert_eq!((diverged.ahead, diverged.behind), (2, 1));
        assert!(diverged.has_upstream && !diverged.dirty);

        let ignored = parse(&with_header(&[
            upstream,
            even,
            "! target/",
            "! debug.log",
            "! sub/node_modules/",
        ]));
        assert!(!ignored.dirty);
        assert_eq!(ignored.ignored, vec!["target/", "sub/node_modules/"]);

        let submodule = "1 .M S.M. 160000 160000 160000 aaaa aaaa vendor/lib";
        assert!(parse(&with_header(&[upstream, even, submodule])).dirty);

        let conflict = "u UU N... 100644 100644 100644 100644 a b c both.rs";
        assert!(parse(&with_header(&[conflict])).dirty);
    }

    #[test]
    fn caps_ignored_prefixes() {
        let records: Vec<String> = (0..MAX_IGNORED + 10).map(|i| format!("! d{i}/")).collect();
        let records: Vec<&str> = records.iter().map(String::as_str).collect();
        let parsed = parse(&records);
        assert_eq!(parsed.ignored.len(), MAX_IGNORED);
        assert!(!parsed.dirty);
    }

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("git version 2.43.0\n"), Some((2, 43)));
        assert_eq!(parse_version("git version 2.45.1.windows.1"), Some((2, 45)));
        assert_eq!(
            parse_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39))
        );
        assert_eq!(parse_version("git version"), None);
        assert_eq!(parse_version("nonsense"), None);
        assert!(git_version().is_some());
    }

    #[test]
    fn keeps_fetch_error_until_next_fetch() {
        let prev = RepoStatus {
            name: "r".into(),
            error: Some("fetch failed (offline)".into()),
            fetch_failed: true,
            fetch_errored: true,
            ..Default::default()
        };
        let local = RepoStatus {
            name: "r".into(),
            dirty: true,
            has_upstream: true,
            ..Default::default()
        };
        let kept = keep_fetch_error(&prev, local.clone());
        assert_eq!(kept.error, prev.error);
        assert!(kept.fetch_failed && kept.dirty);

        // A local error is newer and wins.
        let broken = RepoStatus {
            error: Some("not readable".into()),
            ..local.clone()
        };
        assert_eq!(keep_fetch_error(&prev, broken.clone()), broken);

        // A fetch that failed alongside the local status still carries over.
        let both = RepoStatus {
            fetch_failed: false,
            ..prev.clone()
        };
        let kept = keep_fetch_error(&both, local.clone());
        assert_eq!(kept.error, prev.error);
        assert!(kept.fetch_failed && kept.fetch_errored);

        // Only fetch errors carry over.
        let local_error = RepoStatus {
            fetch_failed: false,
            fetch_errored: false,
            error: Some("not readable".into()),
            ..prev
        };
        assert_eq!(keep_fetch_error(&local_error, local.clone()), local);
    }

    #[test]
    fn fetch_error_keeps_local_state() {
        let root = temp_root("fetch-error");
        let repo = root.join("r");
        init_repo(&repo);
        let missing = root.join("missing.git");
        run(
            &repo,
            &["remote", "add", "origin", missing.to_str().unwrap()],
        );
        std::fs::write(repo.join("x.txt"), "x").unwrap();
        let (status, _) = check_repo_detailed(&root, &repo, true);
        assert!(status.fetch_failed && status.fetch_errored);
        assert!(status.error.as_deref().unwrap().starts_with("fetch failed"));
        assert!(status.dirty);

        // Both failing: no local state, but the fetch failure still shows.
        std::fs::write(repo.join(".git").join("index"), "garbage").unwrap();
        let (status, _) = check_repo_detailed(&root, &repo, true);
        let _ = std::fs::remove_dir_all(&root);
        assert!(!status.fetch_failed && status.fetch_errored, "{status:?}");
        assert!(status.error.as_deref().unwrap().starts_with("fetch failed"));
    }

    #[test]
    fn status_without_untracked_files() {
        // `status.showUntrackedFiles=no` rejects `--ignored=matching`.
        let root = temp_root("no-untracked");
        let repo = root.join("r");
        init_repo(&repo);
        run(&repo, &["config", "status.showUntrackedFiles", "no"]);
        std::fs::write(repo.join("new.txt"), "x").unwrap();
        let clean = check_repo(&root, &repo, false);
        let file = repo.join("tracked.txt");
        std::fs::write(&file, "a").unwrap();
        run(&repo, &["add", "tracked.txt"]);
        commit(&repo, "c");
        std::fs::write(&file, "b").unwrap();
        let dirty = check_repo(&root, &repo, false);
        let _ = std::fs::remove_dir_all(&root);
        // The user's setting decides: untracked files don't count.
        assert_eq!(clean.error, None, "{clean:?}");
        assert!(!clean.dirty, "{clean:?}");
        assert_eq!(dirty.error, None);
        assert!(dirty.dirty);
    }

    #[test]
    fn check_leaves_index_untouched() {
        let root = temp_root("index-mtime");
        let repo = root.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q"]);
        let file = repo.join("tracked.txt");
        std::fs::write(&file, "x").unwrap();
        run(&repo, &["add", "tracked.txt"]);
        commit(&repo, "a");
        // Same content, new stat data: a plain `git status` would refresh the index.
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let index = repo.join(".git").join("index");
        let mtime = || std::fs::metadata(&index).unwrap().modified().unwrap();
        let before = mtime();

        let (status, _) = check_repo_detailed(&root, &repo, false);
        let after_check = mtime();
        // Control: without GIT_OPTIONAL_LOCKS=0 the same status does write.
        thread::sleep(Duration::from_millis(20));
        run(&repo, &["status", "--porcelain"]);
        let after_plain = mtime();
        let _ = std::fs::remove_dir_all(&root);

        assert!(!status.dirty);
        assert_eq!(before, after_check, "gitstat's check rewrote .git/index");
        assert_ne!(
            after_check, after_plain,
            "control: plain status should refresh the index"
        );
    }

    #[test]
    fn excludes_desktop_ini() {
        let root = temp_root("exclude");
        let repo = root.join("r");
        init_repo(&repo);
        std::fs::remove_dir_all(repo.join(".git").join("info")).unwrap_or_default();
        std::fs::write(repo.join("desktop.ini"), "[.ShellClassInfo]\r\n").unwrap();
        assert!(check_repo(&root, &repo, false).dirty);

        assert_eq!(ensure_excluded(&repo, "/desktop.ini"), Ok(true));
        assert_eq!(ensure_excluded(&repo, "/desktop.ini"), Ok(false));
        let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude, "# gitstat folder icon\n/desktop.ini\n");
        assert_eq!(output(&repo, &["status", "--porcelain"]), "");
        assert!(!check_repo(&root, &repo, false).dirty);

        // A linked worktree shares the main repo's info/exclude.
        let wt = root.join("wt");
        run(&repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        std::fs::write(wt.join("desktop.ini"), "x").unwrap();
        assert_eq!(ensure_excluded(&wt, "/desktop.ini"), Ok(false));
        assert_eq!(output(&wt, &["status", "--porcelain"]), "");
        assert!(ensure_excluded(&root.join("not-a-repo"), "/desktop.ini").is_err());
        assert!(ensure_excluded(&repo, "a\nb").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exclude_fixes_missing_newline() {
        let root = temp_root("exclude-newline");
        let repo = root.join("r");
        init_repo(&repo);
        let file = repo.join(".git").join("info").join("exclude");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "*.tmp").unwrap();
        assert_eq!(ensure_excluded(&repo, "/desktop.ini"), Ok(true));
        let exclude = std::fs::read_to_string(&file).unwrap();
        std::fs::write(repo.join("desktop.ini"), "x").unwrap();
        std::fs::write(repo.join("a.tmp"), "x").unwrap();
        let porcelain = output(&repo, &["status", "--porcelain"]);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(exclude, "*.tmp\n# gitstat folder icon\n/desktop.ini\n");
        assert_eq!(porcelain, "");
    }

    #[test]
    fn finds_git_dirs_and_tracked_files() {
        let root = temp_root("git-dirs");
        let repo = root.join("r");
        init_repo(&repo);
        std::fs::write(repo.join("Desktop.ini"), "x").unwrap();
        run(&repo, &["add", "Desktop.ini"]);
        commit(&repo, "ini");
        let wt = root.join("wt");
        run(&repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);

        let canon = |p: PathBuf| p.canonicalize().unwrap();
        let (git_dir, common) = git_dirs(&repo).unwrap();
        let main_git = canon(repo.join(".git"));
        assert_eq!(
            (canon(git_dir), canon(common.clone())),
            (main_git.clone(), main_git.clone())
        );
        let (wt_git, wt_common) = git_dirs(&wt).unwrap();
        assert!(wt_git.is_absolute() && wt_common.is_absolute());
        assert_eq!(canon(wt_git), main_git.join("worktrees").join("wt"));
        assert_eq!(canon(wt_common), main_git);
        assert_eq!(git_dirs(&root), None);

        assert!(is_tracked(&repo, Path::new("Desktop.ini")));
        assert!(is_tracked(&repo, Path::new("desktop.ini")));
        assert!(!is_tracked(&repo, Path::new("other.ini")));
        // Literal pathspec: a glob in the name matches nothing.
        assert!(!is_tracked(&repo, Path::new("*.ini")));
        std::fs::write(repo.join("untracked.ini"), "x").unwrap();
        assert!(!is_tracked(&repo, Path::new("untracked.ini")));
        // Can't tell, so assume tracked and leave the folder alone.
        assert!(is_tracked(&root.join("missing"), Path::new("desktop.ini")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Makes each folder (relative to `root`) a repo, as far as discovery
    /// can tell: it holds a `.git` folder.
    fn fake_repos(root: &Path, repos: &[&str]) {
        for repo in repos {
            std::fs::create_dir_all(root.join(repo).join(".git")).unwrap();
        }
    }

    fn names(root: &Path, found: &Discovery) -> Vec<String> {
        found
            .repos
            .iter()
            .map(|r| repo_name(root, r).replace(std::path::MAIN_SEPARATOR, "/"))
            .collect()
    }

    #[test]
    fn discovery_stops_at_depth() {
        let root = temp_root("depth");
        fake_repos(
            &root,
            &["a", "g/B", "g/h/c", "g/h/i/d", "g/h/i/j/k/l/m/n/e"],
        );
        std::fs::create_dir_all(root.join("empty/x/y")).unwrap();
        std::fs::write(root.join("g/file.txt"), "x").unwrap();
        let at = |depth| names(&root, &find_repos(&root, depth));
        assert_eq!(at(1), ["a"]);
        assert_eq!(at(2), ["a", "g/B"]);
        assert_eq!(at(3), ["a", "g/B", "g/h/c"]);
        assert_eq!(at(4), ["a", "g/B", "g/h/c", "g/h/i/d"]);
        // Clamped to 1..=MAX_DEPTH.
        assert_eq!(at(0), at(1));
        assert_eq!(at(100), at(MAX_DEPTH));
        assert_eq!(at(MAX_DEPTH).len(), 4, "e is 9 levels down");
        let found = find_repos(&root, 3);
        assert_eq!(found.capped, None);
        assert!(found.pruned.is_empty(), "{:?}", found.pruned);
        // Only folders above the last level are entered, repos never.
        let mut entered = found.entered.clone();
        entered.sort();
        let expect = ["empty", "empty/x", "g", "g/h"].map(|p| root.join(p));
        assert_eq!(entered, expect);
        // Names use the platform's separator.
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(repo_name(&root, &found.repos[2]), format!("g{sep}h{sep}c"));
        assert!(find_repos(&root.join("missing"), 3).repos.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_sorts_by_relative_path_ignoring_case() {
        let root = temp_root("sort");
        fake_repos(&root, &["b", "apricot/y", "Apple/z", "C"]);
        assert_eq!(
            names(&root, &find_repos(&root, 2)),
            ["Apple/z", "apricot/y", "b", "C"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_skips_build_and_hidden_folders() {
        let root = temp_root("skip");
        fake_repos(
            &root,
            &[
                "node_modules/pkg",
                "web/node_modules/pkg",
                "Target/x",
                "build/x",
                "dist/x",
                "out/x",
                ".venv/x",
                "venv/x",
                "__pycache__/x",
                "vendor/x",
                "BIN/x",
                "obj/x",
                "$RECYCLE.BIN/x",
                "System Volume Information/x",
                ".hidden/x",
                "web/app",
                // A repo is a repo whatever its name; it just isn't entered.
                "dist",
                ".dotfiles",
            ],
        );
        assert_eq!(
            names(&root, &find_repos(&root, 3)),
            [".dotfiles", "dist", "web/app"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_does_not_enter_repos() {
        let root = temp_root("nested");
        fake_repos(&root, &["r", "r/sub", "r/x/y", "g/r2", "g/r2/vendor/lib"]);
        // A linked worktree's `.git` is a file.
        std::fs::create_dir_all(root.join("g/wt")).unwrap();
        std::fs::write(root.join("g/wt/.git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(names(&root, &find_repos(&root, 4)), ["g/r2", "g/wt", "r"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn root_that_is_a_repo_is_the_only_one() {
        let root = temp_root("root-repo");
        fake_repos(&root, &["", "a", "b/c"]);
        let found = find_repos(&root, 3);
        assert_eq!(found.repos, std::slice::from_ref(&root));
        assert_eq!(repo_name(&root, &root), file_name(&root));
        assert_eq!(repo_name(Path::new("/"), Path::new("/")), "/");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_survives_symlink_loops() {
        use std::os::unix::fs::symlink;
        let root = temp_root("links");
        let outside = temp_root("links-outside");
        fake_repos(&root, &["g/r"]);
        fake_repos(&outside, &["ext", "deep/r"]);
        // Loops: to the folder itself and to the root.
        symlink(root.join("g"), root.join("g/loop")).unwrap();
        symlink(&root, root.join("g/up")).unwrap();
        // A second name for a repo the walk finds anyway: listed once.
        symlink(root.join("g/r"), root.join("alias")).unwrap();
        // A link to a repo elsewhere is listed, as at depth 1 before; a
        // link to a folder of repos isn't entered.
        symlink(outside.join("ext"), root.join("ext")).unwrap();
        symlink(outside.join("deep"), root.join("deep")).unwrap();
        symlink(root.join("missing"), root.join("dangling")).unwrap();
        // Links to files are not folders: not counted, not pruned.
        std::fs::write(root.join("g/file"), "x").unwrap();
        symlink(root.join("g/file"), root.join("file-link")).unwrap();
        symlink(root.join("g/file"), root.join("g/file-link")).unwrap();
        let found = find_repos(&root, MAX_DEPTH);
        assert_eq!(names(&root, &found), ["ext", "g/r"]);
        let mut pruned = found.pruned.clone();
        pruned.sort();
        let mut expect = vec![
            root.join("dangling"),
            root.join("deep"),
            root.join("g/loop"),
            root.join("g/up"),
        ];
        expect.sort();
        assert_eq!(pruned, expect);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn file_links_use_no_folder_budget() {
        use std::os::unix::fs::symlink;
        let root = temp_root("file-links");
        fake_repos(&root, &["r"]);
        std::fs::write(root.join("f"), "x").unwrap();
        for i in 0..10 {
            symlink(root.join("f"), root.join(format!("l{i}"))).unwrap();
        }
        // One folder allowed: the repo; the ten file links don't count.
        let found = find_repos_capped(&root, 3, 1, 100);
        assert_eq!(names(&root, &found), ["r"]);
        assert_eq!(found.capped, None);
        assert!(found.pruned.is_empty(), "{:?}", found.pruned);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_caps_say_so() {
        let root = temp_root("caps");
        fake_repos(&root, &["a", "b", "c", "g/d", "g/e"]);
        std::fs::create_dir_all(root.join("h/i")).unwrap();
        let all = find_repos_capped(&root, 3, 100, 100);
        assert_eq!(all.repos.len(), 5);
        assert_eq!(all.capped, None);

        let few = find_repos_capped(&root, 3, 100, 2);
        assert_eq!(few.repos.len(), 2);
        assert!(few.capped.as_deref().unwrap().contains("first 2 repos"));

        // 5 top-level folders, then g's two: the folder cap stops the walk.
        let short = find_repos_capped(&root, 3, 5, 100);
        assert!(short.repos.len() <= 5);
        assert!(short.capped.as_deref().unwrap().contains("5 folders"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A drive-sized tree: about 5000 folders, 3 levels deep.
    #[test]
    fn discovery_of_5000_folders_is_fast() {
        let root = temp_root("big");
        for a in 0..10 {
            for b in 0..20 {
                for c in 0..25 {
                    let dir = root.join(format!("a{a}/b{b}/c{c}"));
                    std::fs::create_dir_all(&dir).unwrap();
                    if c == 0 {
                        std::fs::create_dir(dir.join(".git")).unwrap();
                    }
                }
            }
        }
        // Churn the walk must not enter.
        for a in 0..10 {
            std::fs::create_dir_all(root.join(format!("a{a}/node_modules/x/y"))).unwrap();
        }
        let started = Instant::now();
        let found = find_repos(&root, 3);
        let took = started.elapsed();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.repos.len(), 200);
        assert_eq!(found.capped, None);
        eprintln!("walked ~5200 folders in {took:?}");
        assert!(took < Duration::from_secs(1), "{took:?}");
    }

    #[test]
    fn detects_every_state() {
        let root = std::env::temp_dir().join(format!("gitstat-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dev = root.join("dev");
        std::fs::create_dir_all(&dev).unwrap();

        let origin = root.join("origin.git");
        run(&root, &["init", "-q", "--bare", origin.to_str().unwrap()]);
        let seed = root.join("seed");
        run(
            &root,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        commit(&seed, "a");
        run(&seed, &["push", "-q", "origin", "HEAD"]);

        for name in ["behind", "diverged", "dirty"] {
            run(&dev, &["clone", "-q", origin.to_str().unwrap(), name]);
        }
        commit(&dev.join("diverged"), "b");
        commit(&seed, "c");
        run(&seed, &["push", "-q", "origin", "HEAD"]);
        for name in ["current", "ahead"] {
            run(&dev, &["clone", "-q", origin.to_str().unwrap(), name]);
        }
        commit(&dev.join("ahead"), "d");
        run(&dev.join("dirty"), &["pull", "-q"]);
        std::fs::write(dev.join("dirty").join("x.txt"), "x").unwrap();
        let noup = dev.join("noup");
        std::fs::create_dir_all(&noup).unwrap();
        run(&noup, &["init", "-q"]);
        commit(&noup, "e");
        std::fs::create_dir_all(dev.join("not-a-repo")).unwrap();

        let found = find_repos(&dev, 1);
        assert_eq!(found.capped, None);
        let statuses = check_all(&dev, &found.repos, true);
        // The pre-2.16 fallback must agree with the porcelain v2 parser.
        for s in &statuses {
            let v2 = local_status(&s.path, true).unwrap();
            let legacy = local_status(&s.path, false).unwrap();
            assert_eq!(
                (
                    legacy.dirty,
                    legacy.ahead,
                    legacy.behind,
                    legacy.has_upstream
                ),
                (v2.dirty, v2.ahead, v2.behind, v2.has_upstream),
                "{}",
                s.name
            );
        }
        let labels: Vec<(String, String, bool)> = statuses
            .into_iter()
            .map(|s| (s.name.clone(), s.label(), s.is_synced()))
            .collect();
        let _ = std::fs::remove_dir_all(&root);

        let expect =
            |name: &str, label: &str, synced: bool| (name.to_string(), label.to_string(), synced);
        assert_eq!(
            labels,
            vec![
                expect("ahead", "⬆1", false),
                expect("behind", "⬇1", false),
                expect("current", "✓ current", true),
                expect("dirty", "dirty", false),
                expect("diverged", "⬆1 ⬇1 diverged", false),
                expect("noup", "no upstream", false),
            ]
        );
    }
}
