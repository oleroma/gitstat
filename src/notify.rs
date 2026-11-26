//! Windows notifications for the changes the config's `notify` asks for:
//! a review asked of you, an open pull request's checks failing, a repo
//! falling behind. Shown as the tray icon's balloon, which Windows 10 and
//! 11 turn into a notification and hold back in Do not disturb.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::NotifyConfig;
use crate::git::RepoStatus;
use crate::github::{Checks, Pr};

/// One notification's lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub title: String,
    pub body: String,
}

/// Repos that a fetch put behind: they were known, read cleanly, not
/// behind, and now are, on the same branch, with its remote branch moved.
/// A failed check (which reads as not behind) or your own checkout of a
/// branch that is behind says nothing.
pub fn behind(cfg: &NotifyConfig, old: Option<&RepoStatus>, new: &RepoStatus) -> Option<String> {
    let old = old.filter(|o| o.error.is_none() && !o.tips.is_empty())?;
    let fetched = old.branch.is_some() && old.branch == new.branch && upstream_moved(old, new);
    (cfg.behind && !new.remote_only && old.behind == 0 && new.behind > 0 && fetched)
        .then(|| format!("{} ⬇{}", new.name, new.behind))
}

/// Whether the checked-out branch's remote branch moved between two
/// checks: `refs/remotes/<remote>/<branch>`, else, when no remote branch
/// has its name (an upstream named otherwise), any remote branch.
fn upstream_moved(old: &RepoStatus, new: &RepoStatus) -> bool {
    let Some(branch) = &new.branch else {
        return false;
    };
    let suffix = format!("/{branch}");
    let remote = |r: &str| r.starts_with("refs/remotes/");
    let named = |r: &str| remote(r) && r.ends_with(&suffix);
    let pick: &dyn Fn(&str) -> bool = if new.tips.iter().any(|(r, _)| named(r)) {
        &named
    } else {
        &remote
    };
    new.tips
        .iter()
        .filter(|(r, _)| pick(r))
        .any(|(r, oid)| !old.tips.iter().any(|(o, before)| o == r && before == oid))
}

/// The pull request changes since the last poll that `cfg` asks about. A
/// repo with no earlier poll is left out, so starting gitstat says nothing.
pub fn pr_changes(
    cfg: &NotifyConfig,
    names: &HashMap<PathBuf, String>,
    old: &HashMap<PathBuf, Vec<Pr>>,
    new: &HashMap<PathBuf, Vec<Pr>>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for (path, prs) in new {
        let Some(before) = old.get(path) else {
            continue;
        };
        let name = names.get(path).map_or("", String::as_str);
        // Only the newest open PRs are read, so one missing before may just
        // have been updated into the list; numbers only grow, so a new one
        // is past every number seen before.
        let newest = before.iter().map(|b| b.number).max().unwrap_or(0);
        for pr in prs.iter().filter(|p| p.is_open()) {
            let was = before.iter().find(|b| b.number == pr.number);
            let what = if cfg.review_requested
                && pr.review_requested
                && !was.is_some_and(|b| b.review_requested)
            {
                "review asked"
            } else if cfg.new_pr && was.is_none() && pr.number > newest {
                "new pull request"
            } else if cfg.checks_failed
                && (was.is_some() || pr.number > newest)
                && pr.checks == Checks::Failing
                && !was.is_some_and(|b| b.checks == Checks::Failing && b.head_oid == pr.head_oid)
            {
                "checks failed"
            } else {
                continue;
            };
            lines.push(format!("{name} #{}: {what} · {}", pr.number, pr.title));
        }
    }
    lines.sort();
    lines
}

/// What a fetch brought in on one remote branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteChange {
    /// A branch that wasn't there.
    New { branch: String },
    /// A branch that moved: its old and new commits.
    Moved {
        branch: String,
        from: String,
        to: String,
    },
}

/// The remote branches that appeared or moved between two checks of one
/// repo, as far as `cfg` asks. Ones that now match a local branch are left
/// out: that is your own push. The repo's first check says nothing.
pub fn remote_changes(
    cfg: &NotifyConfig,
    old: Option<&RepoStatus>,
    new: &RepoStatus,
) -> Vec<RemoteChange> {
    let Some(old) = old.filter(|o| !o.tips.is_empty()) else {
        return Vec::new();
    };
    if !(cfg.new_branch || cfg.new_commits) || new.remote_only {
        return Vec::new();
    }
    let local: Vec<&str> = new
        .tips
        .iter()
        .filter(|(r, _)| r.starts_with("refs/heads/"))
        .map(|(_, oid)| oid.as_str())
        .collect();
    let mut changes = Vec::new();
    for (r, oid) in &new.tips {
        let Some(branch) = r.strip_prefix("refs/remotes/") else {
            continue;
        };
        if local.contains(&oid.as_str()) {
            continue;
        }
        match old.tips.iter().find(|(o, _)| o == r) {
            None if cfg.new_branch => changes.push(RemoteChange::New {
                branch: branch.to_string(),
            }),
            Some((_, before)) if cfg.new_commits && before != oid => {
                changes.push(RemoteChange::Moved {
                    branch: branch.to_string(),
                    from: before.clone(),
                    to: oid.clone(),
                })
            }
            _ => {}
        }
    }
    changes
}

/// Several lines become one notification.
pub fn note(lines: &[String]) -> Option<Note> {
    match lines {
        [] => None,
        [one] => Some(Note {
            title: "gitstat".into(),
            body: one.clone(),
        }),
        more => Some(Note {
            title: format!("gitstat: {} changes", more.len()),
            body: more.join("\n"),
        }),
    }
}

/// Shows `note` as the balloon of the tray icon in `hwnd`.
#[cfg(windows)]
pub fn show(hwnd: isize, note: &Note) {
    use windows_sys::Win32::UI::Shell::{
        NIF_INFO, NIIF_INFO, NIIF_RESPECT_QUIET_TIME, NIM_MODIFY, NOTIFYICONDATAW,
        NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect, Shell_NotifyIconW,
    };
    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
    // tray-icon numbers its icons from 1 and doesn't say which is ours:
    // the one this window has is the one the shell knows a rect for.
    let id = (1..=32u32).find(|&id| {
        let ident = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: hwnd,
            uID: id,
            ..unsafe { std::mem::zeroed() }
        };
        let mut rect = unsafe { std::mem::zeroed() };
        unsafe { Shell_NotifyIconGetRect(&ident, &mut rect) == 0 }
    });
    let Some(id) = id else {
        return;
    };
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = id;
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
    copy_wide(&mut nid.szInfoTitle, &note.title);
    copy_wide(&mut nid.szInfo, &note.body);
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

/// `text` into a fixed, nul-terminated UTF-16 buffer, cut to fit.
#[cfg(windows)]
fn copy_wide(buf: &mut [u16], text: &str) {
    let max = buf.len() - 1;
    let wide: Vec<u16> = text.encode_utf16().take(max).collect();
    buf[..wide.len()].copy_from_slice(&wide);
    buf[wide.len()] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, review: bool, checks: Checks) -> Pr {
        Pr {
            number,
            review_requested: review,
            checks,
            title: format!("t{number}"),
            ..crate::github::tests_pr()
        }
    }

    #[test]
    fn remote_branches_that_appear_or_move() {
        let on = NotifyConfig {
            new_branch: true,
            new_commits: true,
            ..Default::default()
        };
        let tips = |t: &[(&str, &str)]| RepoStatus {
            tips: t
                .iter()
                .map(|(r, o)| (r.to_string(), o.to_string()))
                .collect(),
            ..Default::default()
        };
        let old = tips(&[
            ("refs/heads/main", "a"),
            ("refs/remotes/origin/main", "a"),
            ("refs/remotes/origin/x", "x1"),
        ]);
        let new = tips(&[
            ("refs/heads/main", "a"),
            ("refs/heads/mine", "m"),
            ("refs/remotes/origin/main", "b"),
            ("refs/remotes/origin/x", "x1"),
            ("refs/remotes/origin/y", "y1"),
            ("refs/remotes/origin/mine", "m"),
        ]);
        assert_eq!(
            remote_changes(&on, Some(&old), &new),
            [
                RemoteChange::Moved {
                    branch: "origin/main".into(),
                    from: "a".into(),
                    to: "b".into()
                },
                RemoteChange::New {
                    branch: "origin/y".into()
                },
            ]
        );
        assert!(remote_changes(&on, None, &new).is_empty());
        assert!(remote_changes(&NotifyConfig::default(), Some(&old), &new).is_empty());
    }

    #[test]
    fn only_new_changes_notify() {
        let on = NotifyConfig {
            review_requested: true,
            checks_failed: true,
            behind: true,
            new_pr: true,
            ..Default::default()
        };
        let path = PathBuf::from("r");
        let names = HashMap::from([(path.clone(), "repo".to_string())]);
        let old = HashMap::from([(
            path.clone(),
            vec![pr(1, false, Checks::Passing), pr(2, true, Checks::Failing)],
        )]);
        let new = HashMap::from([(
            path.clone(),
            vec![
                pr(1, true, Checks::Passing),
                pr(2, true, Checks::Failing),
                pr(3, false, Checks::Failing),
            ],
        )]);
        assert_eq!(
            pr_changes(&on, &names, &old, &new),
            [
                "repo #1: review asked · t1",
                "repo #3: new pull request · t3"
            ]
        );
        // The first poll says nothing.
        assert!(pr_changes(&on, &names, &HashMap::new(), &new).is_empty());
        // An older PR updated into the newest read isn't new.
        let later = HashMap::from([(
            path.clone(),
            vec![pr(3, false, Checks::Failing), pr(1, false, Checks::Failing)],
        )]);
        let before = HashMap::from([(path.clone(), vec![pr(3, false, Checks::Failing)])]);
        assert!(pr_changes(&on, &names, &before, &later).is_empty());
        assert!(pr_changes(&NotifyConfig::default(), &names, &old, &new).is_empty());

        let tip = |r: &str, oid: &str| (r.to_string(), oid.to_string());
        let mut a = RepoStatus {
            name: "x".into(),
            branch: Some("main".into()),
            tips: vec![tip("refs/heads/main", "1"), tip("refs/remotes/origin/main", "1")],
            ..Default::default()
        };
        let mut b = a.clone();
        b.behind = 2;
        // Behind with origin/main where it was: a checkout or reset, not a fetch.
        assert_eq!(behind(&on, Some(&a), &b), None);
        b.tips[1].1 = "3".into();
        assert_eq!(behind(&on, Some(&a), &b).as_deref(), Some("x ⬇2"));
        assert_eq!(behind(&on, None, &b), None);
        // A failed check reads as not behind: it is no baseline.
        let mut failed = a.clone();
        failed.error = Some("not readable".into());
        assert_eq!(behind(&on, Some(&failed), &b), None);
        // Switching to a branch that is behind.
        let mut other = a.clone();
        other.branch = Some("dev".into());
        assert_eq!(behind(&on, Some(&other), &b), None);
        a.behind = 1;
        assert_eq!(behind(&on, Some(&a), &b), None);
        assert_eq!(note(&[]), None);
        assert_eq!(note(&["a".into(), "b".into()]).unwrap().body, "a\nb");
    }
}
