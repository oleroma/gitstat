//! Pull requests from GitHub, read through the `gh` CLI that is already
//! signed in on this machine; gitstat stores no token.
//!
//! Strictly read-only, like the rest of gitstat: each poll is one GraphQL
//! `query` (never a mutation) per batch of repos, and a repo is only ever
//! read (`git remote get-url origin`). GraphQL reads go out as a POST to
//! `/graphql`; that is how GitHub's GraphQL API takes every request.
//!
//! It also lists the signed-in account's own repos (and its organisations'),
//! so the app can show the ones not cloned under the watched folder. That is
//! one more read-only `query` per poll, a page of 100 repos at a time.
//!
//! The poller thread asks again every sweep interval and when the refresh
//! button is pressed, and reports every repo's pull requests at once.

// Only the Windows app polls; Linux builds this for its tests.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::git;
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
/// Pages of 100 account repos read per poll, at most.
const MAX_REPO_PAGES: usize = 10;
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

/// A repo of the signed-in account (or one of its organisations) on GitHub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRepo {
    /// `owner/name`.
    pub slug: String,
    pub url: String,
    pub private: bool,
    pub fork: bool,
    /// ISO 8601 of the last push, empty when never pushed.
    pub pushed_at: String,
    pub default_branch: String,
    /// The default branch's latest commit's combined checks.
    pub checks: Checks,
    pub open_prs: u64,
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
    /// The account's repos with no clone among the polled repos, most
    /// recently pushed first. `None` when not asked for, or when they
    /// couldn't be read (then the app keeps the last list).
    pub remote_only: Option<Vec<RemoteRepo>>,
}

/// What a poll reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wants {
    /// Pull requests of the cloned repos.
    pub prs: bool,
    /// The account's repos that aren't cloned here.
    pub remote_repos: bool,
}

impl Wants {
    fn any(self) -> bool {
        self.prs || self.remote_repos
    }
}

enum Cmd {
    Repos(Vec<PathBuf>),
    Settings { wants: Wants, every: Duration },
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

    pub fn settings(&self, wants: Wants, every: Duration) {
        let _ = self.tx.send(Cmd::Settings { wants, every });
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
    let mut wants = Wants::default();
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
                if wants.any() && grew {
                    next = Some(Instant::now() + SETTLE);
                }
            }
            Ok(Cmd::Settings { wants: w, every: e }) => {
                // Something newly asked for is read right away.
                let turned_on = (w.prs && !wants.prs) || (w.remote_repos && !wants.remote_repos);
                wants = w;
                every = e;
                next = if !wants.any() {
                    // Off: the app clears what it shows.
                    None
                } else if turned_on {
                    Some(Instant::now())
                } else {
                    next.map(|at| at.min(Instant::now() + every))
                };
            }
            Ok(Cmd::Refresh) if wants.any() => next = Some(Instant::now()),
            Ok(Cmd::Refresh) => {}
            Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                sink(poll(&repos, wants));
                next = Some(Instant::now() + every);
            }
        }
    }
}

/// `owner/name` of a repo's GitHub `origin`, read without changing anything.
fn slug(repo: &Path) -> Option<String> {
    let web = crate::repo_info::origin_web(repo)?;
    web.strip_prefix("https://github.com/").map(str::to_string)
}

fn poll(repos: &[PathBuf], wants: Wants) -> Report {
    let targets: Vec<(PathBuf, String)> = repos
        .iter()
        .filter_map(|r| slug(r).map(|s| (r.clone(), s)))
        .collect();
    let mut report = Report::default();
    // Before the first discovery every repo would look not cloned.
    if wants.remote_repos && !repos.is_empty() {
        match account_repos() {
            Ok(all) => {
                let cloned: Vec<&str> = targets.iter().map(|(_, s)| s.as_str()).collect();
                report.remote_only = Some(not_cloned(all, &cloned));
            }
            Err(e) => {
                // Signed out or offline: the pull requests would fail alike.
                report.problem = Some(e);
                return report;
            }
        }
    }
    if !wants.prs {
        return report;
    }
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

/// Every repo the signed-in account owns or reaches through an
/// organisation it belongs to, archived ones left out, a page at a time.
fn account_repos() -> Result<Vec<RemoteRepo>, String> {
    let mut all = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_REPO_PAGES {
        let json = gh_graphql(&repos_query(after.as_deref()))?;
        let (page, next) = parse_repos(&json);
        all.extend(page);
        match next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    Ok(all)
}

/// The account's repos that aren't among `cloned` (`owner/name`, matched
/// ignoring case as GitHub does), most recently pushed first.
fn not_cloned(all: Vec<RemoteRepo>, cloned: &[&str]) -> Vec<RemoteRepo> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<RemoteRepo> = all
        .into_iter()
        .filter(|r| !cloned.iter().any(|c| c.eq_ignore_ascii_case(&r.slug)))
        .filter(|r| seen.insert(r.slug.to_lowercase()))
        .collect();
    out.sort_by(|a, b| b.pushed_at.cmp(&a.pushed_at));
    out
}

/// One page of the account's repos, after `cursor`. A read-only `query`.
fn repos_query(cursor: Option<&str>) -> String {
    // Cursors are base64-ish; anything else is dropped rather than quoted.
    let after = cursor
        .filter(|c| {
            c.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'=' | b'+' | b'/' | b'-' | b'_' | b':')
            })
        })
        .map(|c| format!(",after:\"{c}\""))
        .unwrap_or_default();
    format!(
        "query{{viewer{{repositories(first:100{after},\
         ownerAffiliations:[OWNER,ORGANIZATION_MEMBER],isArchived:false,\
         orderBy:{{field:PUSHED_AT,direction:DESC}}){{\
         pageInfo{{hasNextPage endCursor}}\
         nodes{{nameWithOwner url isPrivate isFork pushedAt \
         pullRequests(states:OPEN){{totalCount}} \
         defaultBranchRef{{name target{{...on Commit{{statusCheckRollup{{state}}}}}}}}}}}}}}}}"
    )
}

/// A page of repos and the cursor of the next page, if there is one.
fn parse_repos(json: &Value) -> (Vec<RemoteRepo>, Option<String>) {
    let repos = &json["data"]["viewer"]["repositories"];
    let page = repos["nodes"]
        .as_array()
        .map(|nodes| nodes.iter().filter_map(parse_repo).collect())
        .unwrap_or_default();
    let info = &repos["pageInfo"];
    let next = info["hasNextPage"]
        .as_bool()
        .unwrap_or(false)
        .then(|| info["endCursor"].as_str().map(str::to_string))
        .flatten();
    (page, next)
}

fn parse_repo(n: &Value) -> Option<RemoteRepo> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let slug = n["nameWithOwner"].as_str()?.to_string();
    let branch = &n["defaultBranchRef"];
    Some(RemoteRepo {
        url: n["url"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://github.com/{slug}")),
        slug,
        private: n["isPrivate"].as_bool().unwrap_or(false),
        fork: n["isFork"].as_bool().unwrap_or(false),
        pushed_at: s(&n["pushedAt"]),
        default_branch: s(&branch["name"]),
        checks: checks(branch["target"]["statusCheckRollup"]["state"].as_str()),
        open_prs: n["pullRequests"]["totalCount"].as_u64().unwrap_or(0),
    })
}

/// A path that stands for a GitHub repo with no clone here, so the
/// timeline can key it like a cloned one: `github:owner/name`.
pub fn remote_path(slug: &str) -> PathBuf {
    PathBuf::from(format!("{REMOTE_PREFIX}{slug}"))
}

/// The `owner/name` a [`remote_path`] stands for; `None` for a real folder.
pub fn remote_slug(path: &Path) -> Option<&str> {
    path.to_str()?.strip_prefix(REMOTE_PREFIX)
}

const REMOTE_PREFIX: &str = "github:";
/// Branches read per uncloned repo, most recently committed to first.
const REMOTE_BRANCHES: usize = 30;
/// Commits read back from each of those branches' tips.
const REMOTE_DEPTH: usize = 100;

/// An uncloned repo's recent history as GitHub has it: its default branch,
/// each branch's tip, and the commits reachable within `REMOTE_DEPTH` of
/// those tips, each once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteHistory {
    pub default_branch: Option<String>,
    /// (branch, tip commit id).
    pub branches: Vec<(String, String)>,
    pub commits: Vec<crate::history::Commit>,
}

/// Reads `slug`'s branches and recent commits: one read-only `query`.
pub fn remote_history(slug: &str) -> Result<RemoteHistory, String> {
    let (owner, name) = slug.split_once('/').ok_or("not owner/name")?;
    let plain = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if !plain(owner) || !plain(name) {
        return Err(format!("odd repo name {slug}"));
    }
    let json = gh_graphql(&history_query(owner, name))?;
    parse_history(&json).ok_or_else(|| format!("GitHub has no repo {slug}"))
}

fn history_query(owner: &str, name: &str) -> String {
    format!(
        "query{{repository(owner:\"{owner}\",name:\"{name}\"){{defaultBranchRef{{name}} \
         refs(refPrefix:\"refs/heads/\",first:{REMOTE_BRANCHES},\
         orderBy:{{field:TAG_COMMIT_DATE,direction:DESC}}){{nodes{{name \
         target{{...on Commit{{oid history(first:{REMOTE_DEPTH}){{nodes{{oid \
         committedDate messageHeadline author{{name email}} \
         parents(first:3){{nodes{{oid}}}}}}}}}}}}}}}}}}}}"
    )
}

fn parse_history(json: &Value) -> Option<RemoteHistory> {
    let repo = &json["data"]["repository"];
    if !repo.is_object() {
        return None;
    }
    let mut out = RemoteHistory {
        default_branch: repo["defaultBranchRef"]["name"]
            .as_str()
            .map(str::to_string),
        ..Default::default()
    };
    let mut seen = std::collections::HashSet::new();
    for r in repo["refs"]["nodes"].as_array().into_iter().flatten() {
        let (Some(branch), Some(tip)) = (r["name"].as_str(), r["target"]["oid"].as_str()) else {
            continue;
        };
        out.branches.push((branch.to_string(), tip.to_string()));
        for c in r["target"]["history"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(id) = c["oid"].as_str() else {
                continue;
            };
            if !seen.insert(id.to_string()) {
                continue;
            }
            out.commits.push(crate::history::Commit {
                id: id.to_string(),
                parents: c["parents"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["oid"].as_str().map(str::to_string))
                    .collect(),
                author: c["author"]["name"].as_str().unwrap_or("").to_string(),
                email: c["author"]["email"].as_str().unwrap_or("").to_string(),
                time: c["committedDate"].as_str().and_then(unix_time).unwrap_or(0),
                subject: c["messageHeadline"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    out.commits = date_order(out.commits);
    Some(out)
}

/// Seconds since 1970 for an ISO 8601 UTC time as GitHub writes it:
/// `2026-11-01T14:05:00Z`.
fn unix_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from 1970-01-01 to y-m-d, by the civil-from-days algorithm.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

/// Newest first, with every commit after all of its children (what the
/// graph layout needs, as `git log --date-order` gives): of the commits
/// whose children are all placed, the newest goes next.
fn date_order(commits: Vec<crate::history::Commit>) -> Vec<crate::history::Commit> {
    use std::collections::{BinaryHeap, HashMap};
    let index: HashMap<&str, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    let mut children = vec![0usize; commits.len()];
    for c in &commits {
        for p in &c.parents {
            if let Some(&i) = index.get(p.as_str()) {
                children[i] += 1;
            }
        }
    }
    let mut ready: BinaryHeap<(i64, std::cmp::Reverse<usize>)> = children
        .iter()
        .enumerate()
        .filter(|(_, n)| **n == 0)
        .map(|(i, _)| (commits[i].time, std::cmp::Reverse(i)))
        .collect();
    let mut order = Vec::with_capacity(commits.len());
    while let Some((_, std::cmp::Reverse(i))) = ready.pop() {
        order.push(i);
        for p in &commits[i].parents {
            if let Some(&j) = index.get(p.as_str()) {
                children[j] -= 1;
                if children[j] == 0 {
                    ready.push((commits[j].time, std::cmp::Reverse(j)));
                }
            }
        }
    }
    let mut slots: Vec<Option<crate::history::Commit>> = commits.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

fn checks(state: Option<&str>) -> Checks {
    match state {
        Some("SUCCESS") => Checks::Passing,
        Some("FAILURE" | "ERROR") => Checks::Failing,
        Some("PENDING" | "EXPECTED") => Checks::Pending,
        _ => Checks::None,
    }
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
    let checks = checks(n["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"].as_str());
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
    fn repos_query_is_read_only_and_pages() {
        for q in [repos_query(None), repos_query(Some("Y3Vyc29yOjEwMA=="))] {
            assert!(q.starts_with("query{"));
            assert!(!q.to_lowercase().contains("mutation"));
            assert_eq!(q.matches('{').count(), q.matches('}').count());
        }
        assert!(!repos_query(None).contains("after:"));
        assert!(repos_query(Some("Y3Vyc29yOjEwMA==")).contains(",after:\"Y3Vyc29yOjEwMA==\""));
        // Anything that could break out of the string is dropped.
        assert!(!repos_query(Some("x\"){evil}")).contains("after:"));
    }

    fn repo_node(slug: &str, pushed: &str) -> Value {
        json!({
            "nameWithOwner": slug, "url": format!("https://github.com/{slug}"),
            "isPrivate": true, "isFork": false, "pushedAt": pushed,
            "pullRequests": {"totalCount": 2},
            "defaultBranchRef": {"name": "main",
                "target": {"statusCheckRollup": {"state": "FAILURE"}}}
        })
    }

    #[test]
    fn parses_account_repos_and_pages() {
        let json = json!({"data": {"viewer": {"repositories": {
            "pageInfo": {"hasNextPage": true, "endCursor": "abc"},
            "nodes": [repo_node("o/a", "2026-10-01T00:00:00Z"), {"url": "no name"},
                      {"nameWithOwner": "o/empty", "defaultBranchRef": null}]
        }}}});
        let (page, next) = parse_repos(&json);
        assert_eq!(next.as_deref(), Some("abc"));
        assert_eq!(page.len(), 2);
        let a = &page[0];
        assert_eq!(a.slug, "o/a");
        assert!(a.private && !a.fork);
        assert_eq!(a.default_branch, "main");
        assert_eq!(a.checks, Checks::Failing);
        assert_eq!(a.open_prs, 2);
        let empty = &page[1];
        assert_eq!(empty.url, "https://github.com/o/empty");
        assert_eq!(empty.checks, Checks::None);
        assert_eq!(empty.pushed_at, "");

        let last = json!({"data": {"viewer": {"repositories": {
            "pageInfo": {"hasNextPage": false, "endCursor": "def"}, "nodes": []
        }}}});
        assert_eq!(parse_repos(&last), (Vec::new(), None));
    }

    #[test]
    fn not_cloned_drops_clones_ignoring_case_and_sorts_newest_first() {
        let all = ["o/Old", "o/cloned", "o/new", "o/new"]
            .iter()
            .zip(["2026-01", "2026-05", "2026-09", "2026-09"])
            .map(|(s, at)| parse_repo(&repo_node(s, at)).unwrap())
            .collect();
        let left: Vec<String> = not_cloned(all, &["O/Cloned"])
            .into_iter()
            .map(|r| r.slug)
            .collect();
        assert_eq!(left, ["o/new", "o/Old"]);
    }

    #[test]
    fn remote_paths_round_trip() {
        let p = remote_path("o/r");
        assert_eq!(remote_slug(&p), Some("o/r"));
        assert_eq!(remote_slug(Path::new("/dev/r")), None);
    }

    #[test]
    fn history_query_is_read_only() {
        let q = history_query("o", "r");
        assert!(q.starts_with("query{"));
        assert!(!q.to_lowercase().contains("mutation"));
        assert_eq!(q.matches('{').count(), q.matches('}').count());
        assert!(remote_history("o/r\"x").is_err());
    }

    #[test]
    fn reads_iso_times() {
        assert_eq!(unix_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_time("2026-11-01T14:05:00Z"), Some(1_793_541_900));
        assert_eq!(unix_time("2000-02-29T12:00:00Z"), Some(951_825_600));
        assert_eq!(unix_time("nope"), None);
    }

    #[test]
    fn parses_history_once_per_commit_children_first() {
        let c = |id: &str, t: &str, parents: &[&str]| {
            json!({"oid": id, "committedDate": t, "messageHeadline": id,
                   "author": {"name": "a", "email": "a@x"},
                   "parents": {"nodes": parents.iter().map(|p| json!({"oid": p})).collect::<Vec<_>>()}})
        };
        // main: m2 <- m1 <- root; feat: f1 <- root, with a clock that puts
        // the child f1 before its parent root, which must still come after.
        let json = json!({"data": {"repository": {
            "defaultBranchRef": {"name": "main"},
            "refs": {"nodes": [
                {"name": "main", "target": {"oid": "m2", "history": {"nodes": [
                    c("m2", "2026-01-03T00:00:00Z", &["m1"]),
                    c("m1", "2026-01-02T00:00:00Z", &["root"]),
                    c("root", "2026-01-05T00:00:00Z", &[])]}}},
                {"name": "feat", "target": {"oid": "f1", "history": {"nodes": [
                    c("f1", "2026-01-04T00:00:00Z", &["root", "gone"]),
                    c("root", "2026-01-05T00:00:00Z", &[])]}}}
            ]}
        }}});
        let h = parse_history(&json).unwrap();
        assert_eq!(h.default_branch.as_deref(), Some("main"));
        assert_eq!(h.branches.len(), 2);
        let ids: Vec<&str> = h.commits.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["f1", "m2", "m1", "root"]);
        assert_eq!(h.commits[0].parents, ["root", "gone"]);
        assert!(parse_history(&json!({"data": {"repository": null}})).is_none());
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
