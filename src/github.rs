//! Pull requests from GitHub, read through the `gh` CLI that is already
//! signed in on this machine; gitstat stores no token.
//!
//! Strictly read-only, like the rest of gitstat: each poll is one GraphQL
//! `query` (never a mutation) per batch of repos, and a repo is only ever
//! read (`git remote get-url origin`). GraphQL reads go out as a POST to
//! `/graphql`; that is how GitHub's GraphQL API takes every request.
//!
//! The poller thread asks again every sweep interval and when the refresh
//! button is pressed, and reports every repo's pull requests at once.

// Only the Windows app polls; Linux builds this for its tests.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::git;
use crate::repo_info::github_web;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// Repos per GraphQL query: keeps each one well under GitHub's node limits.
const BATCH: usize = 20;
/// Open pull requests read per repo, newest activity first.
const MAX_OPEN: usize = 30;
/// Merged and closed ones, for the graph's colours.
const MAX_DONE: usize = 30;
const GH_TIMEOUT: Duration = Duration::from_secs(45);
/// A new repo list waits this long for more changes before a poll.
const SETTLE: Duration = Duration::from_secs(3);

/// The graph colours branches by the same states.
pub use crate::graph::PrState;

/// The head commit's combined checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    Passing,
    Failing,
    Pending,
    /// No checks ran.
    None,
}

/// GitHub's review decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review {
    Approved,
    ChangesRequested,
    Required,
    /// No review needed by branch protection, and none given.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mergeable {
    Yes,
    Conflicts,
    /// GitHub hasn't worked it out yet.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub author: String,
    /// The head branch's name, as in the head repo (maybe a fork).
    pub head_branch: String,
    pub head_oid: String,
    pub review: Review,
    /// The signed-in user is asked to review it.
    pub review_requested: bool,
    pub checks: Checks,
    pub mergeable: Mergeable,
    /// ISO 8601, so it sorts as text.
    pub updated_at: String,
}

impl Pr {
    pub fn is_open(&self) -> bool {
        matches!(self.state, PrState::Open | PrState::Draft)
    }

    /// Something for the user to act on: a review asked of them, failing
    /// checks, conflicts or requested changes.
    pub fn needs_attention(&self) -> bool {
        self.is_open()
            && (self.review_requested
                || self.checks == Checks::Failing
                || self.mergeable == Mergeable::Conflicts
                || self.review == Review::ChangesRequested)
    }
}

/// One poll's result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Every polled GitHub repo, with its open pull requests first (newest
    /// activity first), then the recently merged and closed ones. A repo
    /// GitHub couldn't show (no access, renamed) is left out.
    pub prs: HashMap<PathBuf, Vec<Pr>>,
    /// Why nothing could be read: `gh` missing or signed out. Repos whose
    /// earlier pull requests are still worth showing are kept by the app.
    pub problem: Option<String>,
}

enum Cmd {
    Repos(Vec<PathBuf>),
    Settings { enabled: bool, every: Duration },
    Refresh,
    Shutdown,
}

pub struct Poller {
    tx: mpsc::Sender<Cmd>,
}

impl Poller {
    /// Starts disabled until `settings` turns it on.
    pub fn start(sink: impl Fn(Report) + Send + 'static) -> Poller {
        let (tx, rx) = mpsc::channel();
        let _ = thread::Builder::new()
            .name("gitstat-github".into())
            .spawn(move || run(rx, sink));
        Poller { tx }
    }

    pub fn set_repos(&self, repos: Vec<PathBuf>) {
        let _ = self.tx.send(Cmd::Repos(repos));
    }

    pub fn settings(&self, enabled: bool, every: Duration) {
        let _ = self.tx.send(Cmd::Settings { enabled, every });
    }

    pub fn refresh(&self) {
        let _ = self.tx.send(Cmd::Refresh);
    }
}

impl Drop for Poller {
    /// A running `gh` is left to finish or time out.
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

fn run(rx: mpsc::Receiver<Cmd>, sink: impl Fn(Report)) {
    let mut repos: Vec<PathBuf> = Vec::new();
    let mut enabled = false;
    let mut every = Duration::from_secs(5 * 60);
    let mut next: Option<Instant> = None;
    loop {
        let msg = match next {
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match msg {
            Ok(Cmd::Repos(r)) => {
                let grew = r.iter().any(|p| !repos.contains(p));
                repos = r;
                if enabled && grew {
                    next = Some(Instant::now() + SETTLE);
                }
            }
            Ok(Cmd::Settings {
                enabled: on,
                every: e,
            }) => {
                let turned_on = on && !enabled;
                enabled = on;
                every = e;
                next = if !enabled {
                    // Off: the app clears what it shows.
                    None
                } else if turned_on {
                    Some(Instant::now())
                } else {
                    next.map(|at| at.min(Instant::now() + every))
                };
            }
            Ok(Cmd::Refresh) if enabled => next = Some(Instant::now()),
            Ok(Cmd::Refresh) => {}
            Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                sink(poll(&repos));
                next = Some(Instant::now() + every);
            }
        }
    }
}

/// `owner/name` of a repo's GitHub `origin`, read without changing anything.
fn slug(repo: &Path) -> Option<String> {
    let out = git::git(repo, &["remote", "get-url", "origin"], git::LOCAL_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    let web = github_web(String::from_utf8_lossy(&out.stdout).trim())?;
    web.strip_prefix("https://github.com/").map(str::to_string)
}

fn poll(repos: &[PathBuf]) -> Report {
    let targets: Vec<(PathBuf, String)> = repos
        .iter()
        .filter_map(|r| slug(r).map(|s| (r.clone(), s)))
        .collect();
    let mut report = Report::default();
    for batch in targets.chunks(BATCH) {
        let slugs: Vec<&str> = batch.iter().map(|(_, s)| s.as_str()).collect();
        match gh_graphql(&query(&slugs)) {
            Ok(json) => {
                for (i, prs) in parse(&json, batch.len()).into_iter().enumerate() {
                    if let Some(prs) = prs {
                        report.prs.insert(batch[i].0.clone(), prs);
                    }
                }
            }
            Err(e) => {
                report.problem = Some(e);
                break;
            }
        }
    }
    report
}

fn gh_graphql(query: &str) -> Result<Value, String> {
    let mut command = Command::new("gh");
    command
        .args(["api", "graphql", "-f"])
        .arg(format!("query={query}"))
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1");
    let out = git::run_quiet(command, GH_TIMEOUT).map_err(|e| {
        if e.contains("not found on PATH") {
            "GitHub CLI (gh) not found; install it and run gh auth login".to_string()
        } else {
            format!("gh: {e}")
        }
    })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // A repo GitHub can't resolve fails the exit code but still returns the
    // others' data, so the JSON wins over the status.
    if let Ok(json) = serde_json::from_str::<Value>(&stdout)
        && json.get("data").is_some_and(|d| !d.is_null())
    {
        return Ok(json);
    }
    Err(gh_error(&String::from_utf8_lossy(&out.stderr), &stdout))
}

fn gh_error(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    if text.contains("gh auth login") || text.contains("not logged") {
        return "gh isn't signed in; run gh auth login".to_string();
    }
    let line = text.lines().map(str::trim).find(|l| !l.is_empty());
    format!("gh: {}", line.unwrap_or("failed"))
}

/// The one GraphQL query for `slugs` (`owner/name`, already checked to be
/// plain names by `github_web`), each repo under alias `r<i>`. A read-only
/// `query`; see the module docs.
fn query(slugs: &[&str]) -> String {
    let mut q = String::from("query{viewer{login}");
    for (i, slug) in slugs.iter().enumerate() {
        let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
        q.push_str(&format!(
            "r{i}:repository(owner:\"{owner}\",name:\"{name}\"){{\
             open:pullRequests(states:OPEN,first:{MAX_OPEN},\
             orderBy:{{field:UPDATED_AT,direction:DESC}}){{nodes{{...pr}}}}\
             done:pullRequests(states:[MERGED,CLOSED],first:{MAX_DONE},\
             orderBy:{{field:UPDATED_AT,direction:DESC}}){{nodes{{...pr}}}}}}"
        ));
    }
    q.push_str(
        "}fragment pr on PullRequest{number title url state isDraft updatedAt \
         author{login} headRefName headRefOid reviewDecision mergeable \
         reviewRequests(first:20){nodes{requestedReviewer{...on User{login}}}} \
         commits(last:1){nodes{commit{statusCheckRollup{state}}}}}",
    );
    q
}

/// Each alias's pull requests, `None` where GitHub returned no repo.
fn parse(json: &Value, count: usize) -> Vec<Option<Vec<Pr>>> {
    let data = &json["data"];
    let viewer = data["viewer"]["login"].as_str().unwrap_or("");
    (0..count)
        .map(|i| {
            let repo = &data[format!("r{i}")];
            if !repo.is_object() {
                return None;
            }
            let nodes = |key: &str| {
                repo[key]["nodes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
            };
            Some(
                nodes("open")
                    .chain(nodes("done"))
                    .filter_map(|n| parse_pr(&n, viewer))
                    .collect(),
            )
        })
        .collect()
}

fn parse_pr(n: &Value, viewer: &str) -> Option<Pr> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let state = match (
        n["state"].as_str()?,
        n["isDraft"].as_bool().unwrap_or(false),
    ) {
        ("OPEN", true) => PrState::Draft,
        ("OPEN", false) => PrState::Open,
        ("MERGED", _) => PrState::Merged,
        ("CLOSED", _) => PrState::Closed,
        _ => return None,
    };
    let review = match n["reviewDecision"].as_str() {
        Some("APPROVED") => Review::Approved,
        Some("CHANGES_REQUESTED") => Review::ChangesRequested,
        Some("REVIEW_REQUIRED") => Review::Required,
        _ => Review::None,
    };
    let mergeable = match n["mergeable"].as_str() {
        Some("MERGEABLE") => Mergeable::Yes,
        Some("CONFLICTING") => Mergeable::Conflicts,
        _ => Mergeable::Unknown,
    };
    let checks = match n["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"].as_str() {
        Some("SUCCESS") => Checks::Passing,
        Some("FAILURE" | "ERROR") => Checks::Failing,
        Some("PENDING" | "EXPECTED") => Checks::Pending,
        _ => Checks::None,
    };
    let review_requested = !viewer.is_empty()
        && n["reviewRequests"]["nodes"].as_array().is_some_and(|a| {
            a.iter()
                .any(|r| r["requestedReviewer"]["login"].as_str() == Some(viewer))
        });
    Some(Pr {
        number: n["number"].as_u64()?,
        title: s(&n["title"]),
        url: s(&n["url"]),
        state,
        author: s(&n["author"]["login"]),
        head_branch: s(&n["headRefName"]),
        head_oid: s(&n["headRefOid"]),
        review,
        review_requested,
        checks,
        mergeable,
        updated_at: s(&n["updatedAt"]),
    })
}

/// The open pull requests to list in the panel, most pressing first: a
/// review asked of the user, then other attention, then the rest, newest
/// activity first within each.
pub fn panel_order<'a>(prs: impl Iterator<Item = (&'a Path, &'a Pr)>) -> Vec<(&'a Path, &'a Pr)> {
    let mut open: Vec<(&Path, &Pr)> = prs.filter(|(_, p)| p.is_open()).collect();
    open.sort_by(|(_, a), (_, b)| {
        let rank = |p: &Pr| {
            (
                !p.review_requested,
                !p.needs_attention(),
                p.state == PrState::Draft,
            )
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn query_is_read_only_and_aliases_each_repo() {
        let q = query(&["oleroma/gitstat", "a-b/c.d"]);
        assert!(q.starts_with("query{"));
        assert!(!q.to_lowercase().contains("mutation"));
        assert!(q.contains("r0:repository(owner:\"oleroma\",name:\"gitstat\")"));
        assert!(q.contains("r1:repository(owner:\"a-b\",name:\"c.d\")"));
        // Braces balance, or GitHub rejects the whole batch.
        let open = q.matches('{').count();
        assert_eq!(open, q.matches('}').count());
    }

    fn node(number: u64, state: &str, draft: bool) -> Value {
        json!({
            "number": number, "title": format!("PR {number}"),
            "url": format!("https://github.com/o/r/pull/{number}"),
            "state": state, "isDraft": draft, "updatedAt": format!("2026-11-0{number}T00:00:00Z"),
            "author": {"login": "someone"}, "headRefName": format!("b{number}"),
            "headRefOid": "abc", "reviewDecision": null, "mergeable": "UNKNOWN",
            "reviewRequests": {"nodes": []},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]}
        })
    }

    #[test]
    fn parses_states_reviews_checks_and_missing_repos() {
        let mut open = node(1, "OPEN", false);
        open["reviewDecision"] = json!("CHANGES_REQUESTED");
        open["mergeable"] = json!("CONFLICTING");
        open["reviewRequests"]["nodes"] = json!([
            {"requestedReviewer": {"login": "oleroma"}},
            {"requestedReviewer": {}}
        ]);
        open["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = json!({"state": "FAILURE"});
        let mut draft = node(2, "OPEN", true);
        draft["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = json!({"state": "PENDING"});
        let json = json!({"data": {
            "viewer": {"login": "oleroma"},
            "r0": {"open": {"nodes": [open, draft]},
                   "done": {"nodes": [node(3, "MERGED", false), node(4, "CLOSED", false)]}},
            "r1": null
        }});
        let parsed = parse(&json, 2);
        assert_eq!(parsed[1], None);
        let prs = parsed[0].as_ref().unwrap();
        let states: Vec<PrState> = prs.iter().map(|p| p.state).collect();
        assert_eq!(
            states,
            [
                PrState::Open,
                PrState::Draft,
                PrState::Merged,
                PrState::Closed
            ]
        );
        let p = &prs[0];
        assert!(p.review_requested);
        assert_eq!(p.review, Review::ChangesRequested);
        assert_eq!(p.mergeable, Mergeable::Conflicts);
        assert_eq!(p.checks, Checks::Failing);
        assert_eq!(p.head_branch, "b1");
        assert!(p.needs_attention());
        assert_eq!(prs[1].checks, Checks::Pending);
        assert!(!prs[1].review_requested);
        assert_eq!(prs[2].checks, Checks::None);
        assert!(!prs[2].needs_attention(), "merged is never pressing");
    }

    #[test]
    fn panel_order_puts_review_requests_first_and_drops_closed() {
        let base = parse_pr(&node(1, "OPEN", false), "").unwrap();
        let mk = |n: u64, f: &dyn Fn(&mut Pr)| {
            let mut p = base.clone();
            p.number = n;
            p.updated_at = format!("2026-11-0{n}");
            f(&mut p);
            p
        };
        let prs = [
            mk(1, &|_| {}),
            mk(2, &|p| p.state = PrState::Draft),
            mk(3, &|p| p.checks = Checks::Failing),
            mk(4, &|p| p.review_requested = true),
            mk(5, &|p| p.state = PrState::Merged),
            mk(6, &|_| {}),
        ];
        let repo = Path::new("r");
        let order: Vec<u64> = panel_order(prs.iter().map(|p| (repo, p)))
            .into_iter()
            .map(|(_, p)| p.number)
            .collect();
        assert_eq!(order, [4, 3, 6, 1, 2]);
    }

    #[test]
    fn gh_errors_read_plainly() {
        assert_eq!(
            gh_error(
                "To get started with GitHub CLI, please run:  gh auth login",
                ""
            ),
            "gh isn't signed in; run gh auth login"
        );
        assert_eq!(
            gh_error("\nHTTP 502: Bad gateway\n", ""),
            "gh: HTTP 502: Bad gateway"
        );
        assert_eq!(gh_error("", ""), "gh: failed");
    }
}
