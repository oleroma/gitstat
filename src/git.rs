//! Fetching repos and working out how each one compares to its upstream.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// Parallel `git fetch`es. Enough to hide network latency without
/// hammering a remote that hosts most of the repos.
const MAX_PARALLEL: usize = 6;
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const LOCAL_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Every direct subfolder of `parent` that is a git repo, sorted by name.
pub fn find_repos(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut repos: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    repos.sort_by_key(|p| file_name(p).to_lowercase());
    repos
}

/// Checks every repo under `parent`, fetching first when `fetch` is set.
pub fn check_all(parent: &Path, fetch: bool) -> Vec<RepoStatus> {
    let repos = find_repos(parent);
    let mut results: Vec<Option<RepoStatus>> = vec![None; repos.len()];
    let (tx, rx) = mpsc::channel();
    let mut next = 0;
    let mut running = 0;
    while next < repos.len() || running > 0 {
        while running < MAX_PARALLEL && next < repos.len() {
            let tx = tx.clone();
            let (index, repo) = (next, repos[next].clone());
            let spawned = thread::Builder::new().spawn(move || {
                // Always report back, even if the check panics, so the loop can't wait forever.
                let status = std::panic::catch_unwind(|| check_repo(&repo, fetch))
                    .unwrap_or_else(|_| failed(&repo, "internal error"));
                let _ = tx.send((index, status));
            });
            if let Err(e) = spawned {
                results[index] = Some(failed(&repos[index], &e.to_string()));
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

fn failed(repo: &Path, error: &str) -> RepoStatus {
    RepoStatus {
        name: file_name(repo),
        path: repo.to_path_buf(),
        error: Some(error.to_string()),
        ..Default::default()
    }
}

pub fn check_repo(repo: &Path, fetch: bool) -> RepoStatus {
    check_repo_detailed(repo, fetch).0
}

/// Like [`check_repo`], but also returns the ignored directory prefixes
/// (`target/`, `node_modules/`) the watcher uses to drop worktree noise.
///
/// A failed fetch still gets a local check, so the result carries fresh
/// local state alongside the fetch error (`fetch_failed`).
pub fn check_repo_detailed(repo: &Path, fetch: bool) -> (RepoStatus, Vec<String>) {
    let mut status = RepoStatus {
        name: file_name(repo),
        path: repo.to_path_buf(),
        ..Default::default()
    };
    let fetch_error = if fetch { fetch_repo(repo).err() } else { None };

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
    if prev.fetch_failed && local.error.is_none() {
        local.error = prev.error.clone();
        local.fetch_failed = true;
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
        let out = git(
            repo,
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--ignored=matching",
            ],
            LOCAL_TIMEOUT,
        )?;
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
fn git(repo: &Path, args: &[&str], timeout: Duration) -> Result<Output, String> {
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

    // Drain the pipes on helper threads so a chatty git can't block on a full pipe.
    let stdout = spawn_reader(child.stdout.take());
    let stderr = spawn_reader(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(exit)) => break exit,
            Ok(None) if Instant::now() >= deadline => {
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

        // Only fetch errors carry over.
        let local_error = RepoStatus {
            fetch_failed: false,
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
        let (status, _) = check_repo_detailed(&repo, true);
        let _ = std::fs::remove_dir_all(&root);
        assert!(status.fetch_failed);
        assert!(status.error.as_deref().unwrap().starts_with("fetch failed"));
        assert!(status.dirty);
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

        let (status, _) = check_repo_detailed(&repo, false);
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
        assert!(check_repo(&repo, false).dirty);

        assert_eq!(ensure_excluded(&repo, "/desktop.ini"), Ok(true));
        assert_eq!(ensure_excluded(&repo, "/desktop.ini"), Ok(false));
        let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude, "# gitstat folder icon\n/desktop.ini\n");
        assert_eq!(output(&repo, &["status", "--porcelain"]), "");
        assert!(!check_repo(&repo, false).dirty);

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

        let statuses = check_all(&dev, true);
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
