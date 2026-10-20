//! Fetching repos and working out how each one compares to its upstream.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
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
    let mut status = RepoStatus {
        name: file_name(repo),
        path: repo.to_path_buf(),
        ..Default::default()
    };

    if fetch {
        match git(
            repo,
            &["fetch", "--quiet", "--all", "--prune"],
            FETCH_TIMEOUT,
        ) {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                status.error = Some(match stderr.trim().lines().last() {
                    Some(line) => format!("fetch failed ({})", line.trim()),
                    None => "fetch failed".to_string(),
                });
                return status;
            }
            Err(e) => {
                status.error = Some(e);
                return status;
            }
        }
    }

    match git(repo, &["status", "--porcelain"], LOCAL_TIMEOUT) {
        Ok(out) if out.status.success() => status.dirty = !out.stdout.trim_ascii().is_empty(),
        Ok(_) => {
            status.error = Some("not readable".to_string());
            return status;
        }
        Err(e) => {
            status.error = Some(e);
            return status;
        }
    }

    match git(
        repo,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        LOCAL_TIMEOUT,
    ) {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut counts = text.split_whitespace().map(|n| n.parse().unwrap_or(0));
            status.has_upstream = true;
            status.ahead = counts.next().unwrap_or(0);
            status.behind = counts.next().unwrap_or(0);
        }
        Ok(_) => status.has_upstream = false,
        Err(e) => status.error = Some(e),
    }
    status
}

/// Runs git in `repo` with no console window and no credential prompts,
/// killing it if it runs past `timeout`.
fn git(repo: &Path, args: &[&str], timeout: Duration) -> Result<Output, String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
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

        let labels: Vec<(String, String, bool)> = check_all(&dev, true)
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
