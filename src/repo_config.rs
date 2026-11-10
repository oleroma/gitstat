//! Repo config that would make gitstat's own git calls run a program.
//!
//! gitstat runs git on its own in every repo it finds. A fresh clone never
//! copies `.git/config`, but a repo folder copied in from a zip, another PC
//! or a download keeps it, and some keys there make git run a command:
//! `core.fsmonitor` on `status`, `core.sshCommand` and credential helpers on
//! `fetch`, a filter's `clean` command on `status`. Reading the config runs
//! nothing, so [`guard`] reads it before any other git call in the repo and
//! says what gitstat may do there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// What gitstat may run in a repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Guard {
    /// Nothing in the repo's config runs a program: behave as always.
    Safe,
    /// The key (as git lists it, lowercased) runs a program on fetch, or
    /// on status through `core.fsmonitor`: never fetch, and run every
    /// other call with [`SAFE_ARGS`].
    Risky(String),
    /// The key runs a program on status itself (a filter), which no
    /// override turns off: run no git at all.
    Blocked(String),
}

/// Prepended to every git call in a [`Guard::Risky`] repo. `false` turns
/// off both a hook command and Git's own daemon.
pub const SAFE_ARGS: [&str; 2] = ["-c", "core.fsmonitor=false"];

/// How every warning starts, so the label can show it as one.
pub const WARNING: &str = "unsafe .git/config";

impl Guard {
    /// The warning the repo shows instead of an error, if any.
    pub fn warning(&self) -> Option<String> {
        match self {
            Guard::Safe => None,
            Guard::Risky(key) => Some(format!("{WARNING} ({key}): not fetched")),
            Guard::Blocked(key) => Some(format!("{WARNING} ({key}): not checked")),
        }
    }
}

/// The repo's guard, read again whenever its config files change.
pub fn guard(repo: &Path) -> Guard {
    let stamp = stamp(repo);
    if let Some(stamp) = &stamp
        && let Some(hit) = cache()
            .lock()
            .ok()
            .and_then(|c| c.get(repo).filter(|h| &h.stamp == stamp).cloned())
    {
        return hit.guard;
    }
    let Some((guard, includes)) = read(repo) else {
        // git couldn't read the config, so it can't run anything from it
        // either; ask again next time.
        return Guard::Safe;
    };
    // An included file can change without `.git/config` changing, so a
    // repo with includes is read again every time.
    if let Some(stamp) = stamp.filter(|_| !includes)
        && let Ok(mut c) = cache().lock()
    {
        c.insert(
            repo.to_path_buf(),
            Cached {
                stamp,
                guard: guard.clone(),
            },
        );
    }
    guard
}

#[derive(Clone)]
struct Cached {
    stamp: Stamp,
    guard: Guard,
}

/// Modification times of `.git/config` and `.git/config.worktree`.
type Stamp = (Option<SystemTime>, Option<SystemTime>);

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// `None` when `.git` isn't a plain folder (a linked worktree or a
/// submodule keeps its config elsewhere): then the config is always read.
fn stamp(repo: &Path) -> Option<Stamp> {
    let dot_git = repo.join(".git");
    if !dot_git.is_dir() {
        return None;
    }
    let mtime = |name: &str| {
        std::fs::metadata(dot_git.join(name))
            .and_then(|m| m.modified())
            .ok()
    };
    Some((mtime("config"), mtime("config.worktree")))
}

/// `git config --list` of the repo's own config (local and worktree, with
/// their includes), classified; also whether it has includes.
fn read(repo: &Path) -> Option<(Guard, bool)> {
    let scoped = crate::git::supports_show_scope();
    let args: &[&str] = if scoped {
        &["config", "--list", "--includes", "--show-scope", "-z"]
    } else {
        &["config", "--local", "--list", "--includes", "-z"]
    };
    let out = crate::git::git_unguarded(repo, args).ok()?;
    if !out.status.success() {
        return None;
    }
    let entries = parse(&out.stdout, scoped);
    let includes = entries.iter().any(|(key, _)| is_include(key));
    Some((classify(&entries), includes))
}

/// Entries of `config --list -z`, `key\nvalue\0` each (`key\0` for a key
/// with no value), after a `scope\0` with `--show-scope`. With scopes, only
/// the repo's own (local and worktree) entries are kept.
fn parse(out: &[u8], scoped: bool) -> Vec<(String, Option<String>)> {
    let text = String::from_utf8_lossy(out);
    let mut fields = text.split('\0');
    let mut entries = Vec::new();
    loop {
        let scope = if scoped { fields.next() } else { Some("local") };
        let (Some(scope), Some(entry)) = (scope, fields.next()) else {
            break;
        };
        if entry.is_empty() {
            continue;
        }
        if scope != "local" && scope != "worktree" {
            continue;
        }
        let (key, value) = match entry.split_once('\n') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (entry, None),
        };
        entries.push((key.to_ascii_lowercase(), value));
    }
    entries
}

/// The worst key in `entries`: any blocking key wins over a risky one.
pub(crate) fn classify(entries: &[(String, Option<String>)]) -> Guard {
    let mut risky = None;
    for (key, value) in entries {
        match level(key, value.as_deref()) {
            Some(Level::Blocked) => return Guard::Blocked(key.clone()),
            Some(Level::Risky) if risky.is_none() => risky = Some(key.clone()),
            _ => {}
        }
    }
    risky.map_or(Guard::Safe, Guard::Risky)
}

enum Level {
    Risky,
    Blocked,
}

fn level(key: &str, value: Option<&str>) -> Option<Level> {
    let (section, rest) = key.split_once('.')?;
    // The variable name is after the last dot; a subsection may hold dots.
    let name = rest.rsplit('.').next().unwrap_or(rest);
    let sub = rest.len() > name.len();
    let value = value.unwrap_or("true").trim();
    let risky = match (section, name) {
        // `status` runs a filter's clean command on any file whose stat
        // info changed, which in a copied folder is every file.
        ("filter", "clean" | "process") if sub => {
            return (!value.is_empty() && !is_lfs(value)).then_some(Level::Blocked);
        }
        // A boolean picks Git's own daemon; anything else is a command.
        ("core", "fsmonitor") if !sub => !is_bool(value),
        ("core", "sshcommand" | "gitproxy" | "askpass" | "alternaterefscommand") if !sub => {
            !value.is_empty()
        }
        // A plain name runs `git-credential-<name>` from PATH, which is the
        // user's own; a `!` command or a path runs whatever the repo names.
        ("credential", "helper") => value.starts_with('!') || value.contains(['/', '\\']),
        ("remote", "uploadpack" | "receivepack") if sub => !value.is_empty(),
        // `ext::` URLs run a command on fetch, but only once allowed.
        ("protocol", "allow") => key == "protocol.allow" || key == "protocol.ext.allow",
        _ => is_include(key),
    };
    let risky = risky && !(matches!(name, "allow") && value.eq_ignore_ascii_case("never"));
    risky.then_some(Level::Risky)
}

fn is_include(key: &str) -> bool {
    key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path"))
}

/// What `git lfs install --local` writes, which runs git-lfs from PATH.
fn is_lfs(value: &str) -> bool {
    let value = value.strip_suffix(" --skip").unwrap_or(value);
    matches!(value, "git-lfs clean -- %f" | "git-lfs filter-process")
}

fn is_bool(value: &str) -> bool {
    ["", "true", "false", "yes", "no", "on", "off", "1", "0"]
        .iter()
        .any(|b| value.eq_ignore_ascii_case(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard_of(entries: &[(&str, Option<&str>)]) -> Guard {
        let entries: Vec<_> = entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
            .collect();
        classify(&entries)
    }

    #[test]
    fn normal_config_is_safe() {
        assert_eq!(
            guard_of(&[
                ("core.repositoryformatversion", Some("0")),
                ("core.fsmonitor", Some("true")),
                ("core.fsmonitor", None),
                ("core.fsmonitor", Some("")),
                ("remote.origin.url", Some("https://github.com/a/b")),
                ("credential.helper", Some("manager")),
                ("credential.helper", Some("")),
                ("filter.lfs.clean", Some("git-lfs clean -- %f")),
                ("filter.lfs.process", Some("git-lfs filter-process --skip")),
                ("protocol.ext.allow", Some("never")),
                ("core.hookspath", Some(".husky")),
            ]),
            Guard::Safe
        );
    }

    #[test]
    fn flags_keys_that_run_programs() {
        for (key, value) in [
            ("core.fsmonitor", Some("C:/evil.exe")),
            ("core.sshcommand", Some("ssh -i key")),
            ("core.gitproxy", Some("proxy")),
            ("core.askpass", Some("ask")),
            ("credential.helper", Some("!evil")),
            ("credential.https://github.com.helper", Some("/tmp/evil")),
            ("remote.origin.uploadpack", Some("evil")),
            ("remote.my.fork.receivepack", Some("evil")),
            ("protocol.ext.allow", Some("always")),
            ("include.path", Some("other.cfg")),
            ("includeif.gitdir:~/x/.path", Some("other.cfg")),
        ] {
            assert_eq!(
                guard_of(&[(key, value)]),
                Guard::Risky(key.to_string()),
                "{key} = {value:?}"
            );
        }
    }

    #[test]
    fn filters_block_everything() {
        assert_eq!(
            guard_of(&[
                ("core.sshcommand", Some("ssh")),
                ("filter.x.clean", Some("evil %f")),
            ]),
            Guard::Blocked("filter.x.clean".to_string())
        );
        assert_eq!(
            guard_of(&[("filter.lfs.process", Some("git-lfs filter-process; evil"))]),
            Guard::Blocked("filter.lfs.process".to_string())
        );
    }

    #[test]
    fn parses_scoped_list() {
        let out = b"system\0filter.lfs.clean\ngit-lfs clean\0global\0core.sshcommand\nevil\0\
local\0core.bare\nfalse\0local\0Core.FSMonitor\nx\0worktree\0include.path\na\0command\0x.y\0";
        assert_eq!(
            parse(out, true),
            vec![
                ("core.bare".to_string(), Some("false".to_string())),
                ("core.fsmonitor".to_string(), Some("x".to_string())),
                ("include.path".to_string(), Some("a".to_string())),
            ]
        );
        assert_eq!(
            parse(b"core.bare\nfalse\0core.fsmonitor\0", false),
            vec![
                ("core.bare".to_string(), Some("false".to_string())),
                ("core.fsmonitor".to_string(), None),
            ]
        );
    }
}
