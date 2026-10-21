//! Status icons on the repo folders in Explorer, through desktop.ini. A
//! ledger in `data_dir` remembers what we changed so it can be undone.
//!
//! In the `repo-svg` mode a repo with a `.gitstat/icon.svg` gets that
//! picture in its status colour instead (see repo_icon.rs), one .ico per
//! repo, colour and picture.
//!
//! [`plan`] decides what to do and is pure; the Windows [`Worker`] carries
//! it out off the UI thread.

use crate::config::FolderIconMode;
use crate::git::RepoStatus;
use crate::icon::{FolderState, ICON_VERSION};
use crate::repo_icon;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const LEDGER_FILE: &str = "folder-icons.json";
const SECTION: &str = ".ShellClassInfo";
const TIP_PREFIX: &str = "gitstat:";

pub const SKIP_INELIGIBLE: &str = "not on a local fixed drive, or inside OneDrive";
pub const SKIP_TRACKED: &str = "desktop.ini is tracked by git";
pub const SKIP_FOREIGN: &str = "folder already has its own desktop.ini";

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Ledger {
    /// Keyed by [`key`]: the lowercased folder path.
    pub entries: BTreeMap<String, Entry>,
    /// Repo icons that folders we let go of still show, see [`Kept`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<Kept>,
}

/// A repo icon whose folder left the ledger with its desktop.ini as it was
/// (git tracks the file now), so the file still points at the icon. It is
/// kept until it doesn't, rather than deleted from under it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Kept {
    pub path: PathBuf,
    pub repo_icon: String,
}

/// A folder whose icon we applied, and what to restore when reverting.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub state: FolderState,
    pub tip: String,
    /// desktop.ini didn't exist before, so reverting may delete it.
    pub created_ini: bool,
    /// The folder already had ReadOnly or System, so reverting leaves it.
    pub had_readonly: bool,
    /// The [`ICON_VERSION`] desktop.ini points at. Ledgers from before the
    /// field read 0, so their folders move to the new artwork once.
    #[serde(default)]
    pub icon_version: u32,
    /// The mode it was applied in; older ledgers knew only `Status`.
    #[serde(default)]
    pub mode: FolderIconMode,
    /// In `RepoSvg` mode, the repo's icon.svg as `repo_icon::fingerprint`
    /// saw it. With `state` and `mode`, what decides whether to re-apply.
    /// `None` after a failure worth another try, so the next sync has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub svg: Option<String>,
    /// The icon.svg couldn't be used, so the status folder shows: why. Only
    /// the file's own faults, which last until it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub svg_error: Option<String>,
    /// The repo icon's file name in the icons dir, for cleaning up the
    /// ones no folder points at any more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_icon: Option<String>,
}

impl Ledger {
    /// A missing ledger is empty. So is a corrupt one, which is moved aside
    /// to `*.bad` so nothing overwrites it before someone can look. One that
    /// exists but can't be read (locked, no access) is an error: starting
    /// empty would overwrite it on the next save and lose every entry.
    pub fn load(file: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("can't read {}: {e}", file.display())),
        };
        let parsed = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| serde_json::from_str(text).ok());
        Ok(parsed.unwrap_or_else(|| {
            let _ = std::fs::rename(file, file.with_extension("json.bad"));
            Self::default()
        }))
    }

    /// Writes a temp file and renames it over the ledger, so a crash never
    /// leaves half a ledger.
    pub fn save(&self, file: &Path) -> Result<(), String> {
        let fail = |e: std::io::Error| format!("{}: {e}", file.display());
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(fail)?;
        std::fs::rename(&tmp, file).map_err(fail)
    }

    pub fn get(&self, path: &Path) -> Option<&Entry> {
        self.entries.get(&key(path))
    }

    /// Drops `path`'s entry and leaves the folder as it is, keeping the
    /// repo icon its desktop.ini points at.
    fn forget(&mut self, path: &Path) {
        if let Some(entry) = self.entries.remove(&key(path))
            && let Some(repo_icon) = entry.repo_icon
        {
            let kept = Kept {
                path: entry.path,
                repo_icon,
            };
            if !self.kept.contains(&kept) {
                self.kept.push(kept);
            }
        }
    }

    /// Lets go of the kept repo icons that `points_at(folder, icon)` says
    /// their folder no longer shows. Whether there were any.
    fn prune_kept(&mut self, points_at: impl Fn(&Path, &str) -> bool) -> bool {
        let before = self.kept.len();
        self.kept.retain(|k| points_at(&k.path, &k.repo_icon));
        self.kept.len() != before
    }
}

/// Ledger key: Windows paths compare case-insensitively.
pub fn key(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    let text = text.replace('/', "\\");
    text.to_lowercase()
}

/// The filesystem questions `plan` asks, faked in tests.
pub trait Probe {
    /// A local fixed drive, not UNC, not under OneDrive.
    fn eligible(&self, p: &Path) -> bool;
    fn ini_tracked(&self, p: &Path) -> bool;
    /// desktop.ini exists and its icon isn't one of ours.
    fn foreign_ini(&self, p: &Path) -> bool;
    fn exists(&self, p: &Path) -> bool;
    fn ini_present(&self, p: &Path) -> bool;
    /// `repo_icon::fingerprint`: the repo's icon.svg, `None` without one.
    fn repo_svg(&self, p: &Path) -> Option<String>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Apply {
        path: PathBuf,
        state: FolderState,
        tip: String,
        mode: FolderIconMode,
        /// The icon.svg to draw (`RepoSvg` mode only), by fingerprint.
        svg: Option<String>,
    },
    Revert(PathBuf),
    /// Drop the ledger entry without touching the folder.
    Forget(PathBuf),
    Skip(PathBuf, &'static str),
    /// Up to date, but showing the status folder because the repo's
    /// icon.svg couldn't be used: why, reported like a skip.
    Note(PathBuf, String),
}

/// What to change so the folders match `statuses`. Folders already showing
/// the right icon cost no probing beyond one `ini_present`, and the cheap
/// probes run before the one that spawns git.
///
/// Entries no longer wanted are undone first, so a folder that comes back
/// under another spelling (a new key) is applied to a clean folder.
///
/// A folder is up to date when its state, tip, `mode` and (in `RepoSvg`
/// mode) icon.svg are what was applied, so a change to any of them
/// re-applies it once, and a mode switch re-applies every folder.
pub fn plan(
    ledger: &Ledger,
    statuses: &[RepoStatus],
    enabled: bool,
    mode: FolderIconMode,
    probe: &dyn Probe,
) -> Vec<Op> {
    let mut ops = Vec::new();
    let wanted: HashSet<String> = if enabled {
        statuses.iter().map(|s| key(&s.path)).collect()
    } else {
        HashSet::new()
    };
    for (k, e) in &ledger.entries {
        if wanted.contains(k) {
            continue;
        }
        ops.push(if probe.exists(&e.path) {
            Op::Revert(e.path.clone())
        } else {
            Op::Forget(e.path.clone())
        });
    }
    if enabled {
        for s in statuses {
            let path = &s.path;
            let entry = ledger.entries.get(&key(path));
            let view = local_view(s);
            let (state, tip) = (FolderState::of(&view), tip(&view));
            let read_svg = || match mode {
                FolderIconMode::RepoSvg => probe.repo_svg(path),
                FolderIconMode::Status => None,
            };
            // `git clean -X` removes desktop.ini without telling the ledger.
            // A new ICON_VERSION means new file names to point at. The
            // icon.svg is read only for a folder that may be up to date,
            // or is about to be applied.
            let mut svg = None;
            if let Some(e) = entry.filter(|e| {
                e.state == state && e.tip == tip && e.icon_version == ICON_VERSION && e.mode == mode
            }) {
                let now = read_svg();
                if e.svg == now && probe.ini_present(path) {
                    if let Some(why) = &e.svg_error {
                        ops.push(Op::Note(path.clone(), why.clone()));
                    }
                    continue;
                }
                svg = Some(now);
            }
            // A drive root (`D:\` as the root and a repo itself) has no
            // folder icon to replace.
            if path.parent().is_none() || !probe.eligible(path) {
                if entry.is_some() {
                    ops.push(Op::Revert(path.clone()));
                }
                ops.push(Op::Skip(path.clone(), SKIP_INELIGIBLE));
                continue;
            }
            // The git probe is the costly one: skip it for a foreign file
            // we have nothing to undo in.
            let foreign = probe.foreign_ini(path);
            let tracked = (!foreign || entry.is_some()) && probe.ini_tracked(path);
            let reason = if foreign {
                Some(SKIP_FOREIGN)
            } else if tracked {
                Some(SKIP_TRACKED)
            } else {
                None
            };
            if let Some(reason) = reason {
                if entry.is_some() {
                    // Never write into a file git tracks, even to undo ours.
                    // An untracked one with someone else's icon (Properties >
                    // Customize) still has our InfoTip, which revert removes
                    // on its own.
                    ops.push(if tracked {
                        Op::Forget(path.clone())
                    } else {
                        Op::Revert(path.clone())
                    });
                }
                ops.push(Op::Skip(path.clone(), reason));
                continue;
            }
            ops.push(Op::Apply {
                path: path.clone(),
                state,
                tip,
                mode,
                svg: svg.unwrap_or_else(read_svg),
            });
        }
    }
    ops
}

/// A failed fetch says nothing about the folder itself, and a flaky network
/// shouldn't flip every icon to red and back. Show the local state instead.
fn local_view(s: &RepoStatus) -> RepoStatus {
    let mut view = s.clone();
    if view.fetch_failed {
        view.error = None;
        view.fetch_failed = false;
    }
    view
}

/// The folder's InfoTip. No timestamps, so it only changes with the status.
pub fn tip(s: &RepoStatus) -> String {
    if let Some(error) = &s.error {
        let line = error.lines().next().unwrap_or_default().trim();
        let short: String = line.chars().take(120).collect();
        return format!("{TIP_PREFIX} error: {short}");
    }
    let mut parts = Vec::new();
    if !s.has_upstream {
        parts.push("no upstream".to_string());
    }
    if s.ahead > 0 {
        parts.push(format!("⬆{}", s.ahead));
    }
    if s.behind > 0 {
        parts.push(format!("⬇{}", s.behind));
    }
    if s.dirty {
        parts.push("uncommitted changes".to_string());
    }
    if parts.is_empty() {
        parts.push("in sync".to_string());
    }
    format!("{TIP_PREFIX} {}", parts.join(", "))
}

/// Versioned per state: Explorer caches icons by path, so new artwork needs
/// a new name rather than new bytes.
pub fn icon_file(dir: &Path, s: FolderState) -> PathBuf {
    icons_dir(dir).join(format!("{}-v{ICON_VERSION}.ico", s.name()))
}

fn icons_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("icons")
}

/// Repo icons (and their temp files) among `names`, the icons dir's
/// files, that no ledger entry points at and none is kept: their repo is
/// gone, or now shows another colour or picture.
fn stale_repo_icons(ledger: &Ledger, names: impl IntoIterator<Item = String>) -> Vec<String> {
    let used: HashSet<&str> = ledger
        .entries
        .values()
        .filter_map(|e| e.repo_icon.as_deref())
        .chain(ledger.kept.iter().map(|k| k.repo_icon.as_str()))
        .collect();
    names
        .into_iter()
        .filter(|name| repo_icon::is_repo_icon_file(name) && !used.contains(name.as_str()))
        .collect()
}

/// What an apply in `RepoSvg` mode records about the repo's icon.svg.
#[derive(Debug, Default, PartialEq)]
struct SvgOutcome {
    /// For the ledger entry's fields of the same names.
    svg: Option<String>,
    svg_error: Option<String>,
    repo_icon: Option<String>,
    /// Why the status folder shows instead, to report this time.
    note: Option<String>,
}

/// Loads and draws the icon.svg that `plan` saw as `planned`; `draw`
/// returns the .ico's file name.
///
/// The file's own faults are recorded with its fingerprint, so it isn't
/// tried again until it changes. Any other failure (it couldn't be read,
/// drawing it took too long, the .ico couldn't be saved) records no
/// fingerprint, so the next sync tries again, this run or the next.
fn draw_repo_svg(
    planned: String,
    load: impl FnOnce() -> Result<Option<Vec<u8>>, repo_icon::Error>,
    draw: impl FnOnce(&[u8]) -> Result<String, repo_icon::Error>,
) -> SvgOutcome {
    let (svg, drawn) = match load() {
        // Gone since: the status folder is right.
        Ok(None) => return SvgOutcome::default(),
        Ok(Some(bytes)) => (repo_icon::fingerprint_of(&bytes), draw(&bytes)),
        Err(e) => (planned, Err(e)),
    };
    match drawn {
        Ok(name) => SvgOutcome {
            svg: Some(svg),
            repo_icon: Some(name),
            ..SvgOutcome::default()
        },
        Err(e) if e.transient => SvgOutcome {
            note: Some(e.message),
            ..SvgOutcome::default()
        },
        Err(e) => SvgOutcome {
            svg: Some(svg),
            svg_error: Some(e.message.clone()),
            repo_icon: None,
            note: Some(e.message),
        },
    }
}

/// desktop.ini text. The shell writes UTF-16 with a BOM, people write ANSI.
fn decode_ini(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// A value from an INI file; sections and keys are case-insensitive.
fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = name.trim().eq_ignore_ascii_case(section);
        } else if in_section
            && let Some((k, v)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case(key)
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Whether any key is left, so an ini we created can be deleted when not.
fn ini_has_keys(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .any(|l| !l.is_empty() && !l.starts_with(';') && !(l.starts_with('[') && l.ends_with(']')))
}

/// The icon desktop.ini points at: `IconResource`, or the older `IconFile`.
fn ini_icon(text: &str) -> Option<String> {
    ini_value(text, SECTION, "IconResource").or_else(|| ini_value(text, SECTION, "IconFile"))
}

/// Whether desktop.ini's icon is the file named `name`, wherever it is.
fn ini_points_at(text: &str, name: &str) -> bool {
    ini_icon(text).is_some_and(|icon| norm(&icon).contains(&norm(name)))
}

/// Whether an `IconResource` value (`path,index`, maybe quoted or with
/// `%VARS%`) points into our icons dir.
fn is_ours(value: &str, icons_dir: &Path, env: &dyn Fn(&str) -> Option<String>) -> bool {
    let value = value.trim();
    let file = match value.rsplit_once(',') {
        Some((file, index)) if index.trim().parse::<i32>().is_ok() => file,
        _ => value,
    };
    let file = expand_env(file.trim().trim_matches('"'), env);
    let dir = norm(&icons_dir.to_string_lossy());
    norm(&file)
        .strip_prefix(&dir)
        .is_some_and(|rest| rest.starts_with('\\'))
}

/// Any desktop.ini that doesn't point at our icons belongs to someone else,
/// even one without an icon: reverting would delete its other keys.
fn foreign(text: &str, icons_dir: &Path, env: &dyn Fn(&str) -> Option<String>) -> bool {
    !ini_icon(text).is_some_and(|v| is_ours(&v, icons_dir, env))
}

fn expand_env(text: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else { break };
        out.push_str(&rest[..start]);
        match env(&after[..end]) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[start..start + end + 2]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Windows path text for comparing: one separator, no case, no trailing slash.
fn norm(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Whether `path` is `root` or inside it.
fn under(path: &str, root: &str) -> bool {
    let (path, root) = (norm(path), norm(root));
    !root.is_empty()
        && path
            .strip_prefix(&root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
}

/// `C:\` for a path on a drive letter (verbatim `\\?\C:\` too), `None` for
/// UNC and anything else Explorer may not trust.
fn drive_root(path: &str) -> Option<String> {
    let path = path.replace('/', "\\");
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    let mut chars = path.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => Some(format!("{letter}:\\")),
        _ => None,
    }
}

/// At most `BURST` shell notifications per `WINDOW`: enabling the feature on
/// a big folder must not flood Explorer with refreshes.
#[derive(Default)]
struct Throttle {
    window: Option<Instant>,
    sent: usize,
}

impl Throttle {
    const BURST: usize = 20;
    const WINDOW: Duration = Duration::from_millis(100);

    /// Takes a slot now, or says how long until the next one.
    fn admit(&mut self, now: Instant) -> Result<(), Duration> {
        match self.window {
            Some(start) if now.duration_since(start) < Self::WINDOW => {
                if self.sent < Self::BURST {
                    self.sent += 1;
                    Ok(())
                } else {
                    Err(Self::WINDOW - now.duration_since(start))
                }
            }
            _ => {
                self.window = Some(now);
                self.sent = 1;
                Ok(())
            }
        }
    }
}

#[cfg(windows)]
pub use shell::{Worker, revert_all_blocking};

#[cfg(windows)]
mod shell {
    use super::*;
    use crate::icon;
    use std::collections::VecDeque;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::mpsc;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
        FILE_ATTRIBUTE_SYSTEM, GetDriveTypeW, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
        SetFileAttributesW,
    };
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, WritePrivateProfileStringW};
    use windows_sys::Win32::UI::Shell::{
        FCS_FORCEWRITE, FCSM_ICONFILE, FCSM_INFOTIP, PathMakeSystemFolderW,
        PathUnmakeSystemFolderW, SHCNE_UPDATEITEM, SHCNF_FLUSHNOWAIT, SHCNF_PATHW, SHChangeNotify,
        SHFOLDERCUSTOMSETTINGS, SHGetSetFolderCustomSettings,
    };

    const INI: &str = "desktop.ini";

    type Job = (Vec<RepoStatus>, bool, FolderIconMode);
    type Skips = Vec<(PathBuf, String)>;

    /// Applies plans on its own thread, coalescing to the latest request.
    /// Dropping it ends the thread once the current batch is done.
    pub struct Worker {
        tx: mpsc::Sender<Job>,
    }

    impl Worker {
        /// `report` gets the skipped folders and why, whenever that list changes.
        pub fn start(data_dir: PathBuf, report: impl Fn(Skips) + Send + 'static) -> Self {
            let (tx, rx) = mpsc::channel::<Job>();
            let _ = std::thread::Builder::new()
                .name("folder-icons".into())
                .spawn(move || {
                    let _com = Com::init();
                    let mut engine = Engine::new(data_dir);
                    let mut last: Option<Skips> = None;
                    while let Ok(mut job) = rx.recv() {
                        while let Ok(newer) = rx.try_recv() {
                            job = newer;
                        }
                        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            engine.sync(&job.0, job.1, job.2).0
                        }));
                        if let Ok(skips) = run
                            && last.as_ref() != Some(&skips)
                        {
                            report(skips.clone());
                            last = Some(skips);
                        }
                    }
                });
            Self { tx }
        }

        /// Repo icons are drawn here too, on the worker's thread.
        pub fn sync(&self, statuses: Vec<RepoStatus>, enabled: bool, mode: FolderIconMode) {
            let _ = self.tx.send((statuses, enabled, mode));
        }
    }

    /// `gitstat --remove-folder-icons`: reverts every ledger entry and
    /// returns how many folders were restored.
    pub fn revert_all_blocking(data_dir: &Path) -> Result<usize, String> {
        let data_dir = data_dir.to_path_buf();
        // Its own thread, so COM on the caller's thread stays as it was.
        let joined = std::thread::spawn(move || {
            let _com = Com::init();
            Engine::new(data_dir).sync(&[], false, FolderIconMode::Status)
        })
        .join();
        let (failed, reverted) = joined.map_err(|_| "internal error".to_string())?;
        if failed.is_empty() {
            return Ok(reverted);
        }
        let list: Vec<String> = failed
            .iter()
            .map(|(p, why)| format!("{}: {why}", p.display()))
            .collect();
        Err(format!(
            "restored {reverted} folders, but not these:\n{}",
            list.join("\n")
        ))
    }

    /// Single-threaded COM for the shell calls; uninitialised on drop.
    struct Com(bool);

    impl Com {
        fn init() -> Self {
            let flags = (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32;
            Self(unsafe { CoInitializeEx(std::ptr::null(), flags) } >= 0)
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    fn attributes(path: &Path) -> Option<u32> {
        let attrs = unsafe { GetFileAttributesW(wide(path.as_os_str()).as_ptr()) };
        (attrs != INVALID_FILE_ATTRIBUTES).then_some(attrs)
    }

    fn env(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    struct ShellProbe {
        icons_dir: PathBuf,
        onedrive: Vec<String>,
    }

    impl ShellProbe {
        fn new(icons_dir: PathBuf) -> Self {
            let onedrive = ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"]
                .iter()
                .filter_map(|v| env(v))
                .collect();
            Self {
                icons_dir,
                onedrive,
            }
        }
    }

    impl Probe for ShellProbe {
        fn eligible(&self, p: &Path) -> bool {
            let text = p.to_string_lossy();
            if self.onedrive.iter().any(|root| under(&text, root)) {
                return false;
            }
            drive_root(&text).is_some_and(|root| {
                let root = wide(root.as_ref());
                (unsafe { GetDriveTypeW(root.as_ptr()) }) == DRIVE_FIXED
            })
        }

        fn ini_tracked(&self, p: &Path) -> bool {
            crate::git::is_tracked(p, Path::new(INI))
        }

        fn foreign_ini(&self, p: &Path) -> bool {
            match std::fs::read(p.join(INI)) {
                Ok(bytes) => foreign(&decode_ini(&bytes), &self.icons_dir, &env),
                Err(e) => e.kind() != std::io::ErrorKind::NotFound,
            }
        }

        fn exists(&self, p: &Path) -> bool {
            p.is_dir()
        }

        fn ini_present(&self, p: &Path) -> bool {
            p.join(INI).exists()
        }

        fn repo_svg(&self, p: &Path) -> Option<String> {
            repo_icon::fingerprint(p)
        }
    }

    /// Queued `SHCNE_UPDATEITEM`s, sent through the [`Throttle`].
    #[derive(Default)]
    struct Notifier {
        queue: VecDeque<Vec<u16>>,
        throttle: Throttle,
    }

    impl Notifier {
        fn push(&mut self, path: &Path) {
            self.queue.push_back(wide(path.as_os_str()));
        }

        /// Sends what the throttle allows; with `wait`, sleeps until all are sent.
        fn pump(&mut self, wait: bool) {
            while let Some(path) = self.queue.front() {
                match self.throttle.admit(Instant::now()) {
                    Ok(()) => {
                        unsafe {
                            SHChangeNotify(
                                SHCNE_UPDATEITEM as i32,
                                SHCNF_PATHW | SHCNF_FLUSHNOWAIT,
                                path.as_ptr().cast(),
                                std::ptr::null(),
                            )
                        };
                        self.queue.pop_front();
                    }
                    Err(delay) if wait => std::thread::sleep(delay),
                    Err(_) => return,
                }
            }
        }
    }

    struct Engine {
        data_dir: PathBuf,
        ledger_file: PathBuf,
        ledger: Ledger,
        /// The ledger exists but couldn't be read: nothing is applied or
        /// saved until it can, or it would be overwritten.
        load_error: Option<String>,
        icons_ready: bool,
        /// Repo icons may have fallen out of use since the last cleanup.
        /// True at first, for what an earlier run left.
        repo_icons_dirty: bool,
        notifier: Notifier,
        renderer: repo_icon::Renderer,
    }

    impl Engine {
        fn new(data_dir: PathBuf) -> Self {
            let ledger_file = data_dir.join(LEDGER_FILE);
            let (ledger, load_error) = match Ledger::load(&ledger_file) {
                Ok(ledger) => (ledger, None),
                Err(e) => (Ledger::default(), Some(e)),
            };
            Self {
                ledger,
                load_error,
                ledger_file,
                data_dir,
                icons_ready: false,
                repo_icons_dirty: true,
                notifier: Notifier::default(),
                renderer: repo_icon::Renderer::default(),
            }
        }

        /// Returns the folders left alone (or that failed) with the reason,
        /// and how many were reverted.
        fn sync(
            &mut self,
            statuses: &[RepoStatus],
            enabled: bool,
            mode: FolderIconMode,
        ) -> (Skips, usize) {
            if self.load_error.is_some() {
                // Maybe it was only locked for a moment.
                match Ledger::load(&self.ledger_file) {
                    Ok(ledger) => {
                        self.ledger = ledger;
                        self.load_error = None;
                    }
                    Err(e) => return (vec![(self.ledger_file.clone(), e)], 0),
                }
            }
            let probe = ShellProbe::new(icons_dir(&self.data_dir));
            let ops = plan(&self.ledger, statuses, enabled, mode, &probe);
            let (mut skips, mut reverted) = (Vec::new(), 0);
            for op in ops {
                if !matches!(op, Op::Skip(..) | Op::Note(..)) {
                    self.repo_icons_dirty = true;
                }
                match op {
                    Op::Apply {
                        path,
                        state,
                        tip,
                        mode,
                        svg,
                    } => match self.apply(&path, state, &tip, mode, svg) {
                        Ok(None) => {}
                        Ok(Some(note)) => skips.push((path, note)),
                        Err(e) => skips.push((path, e)),
                    },
                    Op::Revert(path) => match self.revert(&path) {
                        Ok(Reverted::Restored) => reverted += 1,
                        Ok(Reverted::Tracked) => skips.push((path, SKIP_TRACKED.to_string())),
                        Err(e) => skips.push((path, e)),
                    },
                    Op::Forget(path) => {
                        self.ledger.forget(&path);
                        if let Err(e) = self.ledger.save(&self.ledger_file) {
                            skips.push((path, e));
                        }
                    }
                    Op::Skip(path, why) => skips.push((path, why.to_string())),
                    Op::Note(path, why) => skips.push((path, why)),
                }
                self.notifier.pump(false);
            }
            self.notifier.pump(true);
            self.remove_stale_repo_icons();
            skips.sort_by(|a, b| a.0.cmp(&b.0));
            (skips, reverted)
        }

        /// Writes every state's .ico, touching only files whose bytes differ.
        fn ensure_icons(&mut self) -> Result<(), String> {
            if self.icons_ready {
                return Ok(());
            }
            let dir = icons_dir(&self.data_dir);
            let fail = |e: std::io::Error| format!("can't write icons in {}: {e}", dir.display());
            std::fs::create_dir_all(&dir).map_err(fail)?;
            for state in FolderState::ALL {
                write_if_changed(&icon_file(&self.data_dir, state), &icon::folder_ico(state))
                    .map_err(fail)?;
            }
            self.icons_ready = true;
            Ok(())
        }

        /// Draws the repo's icon.svg, `svg`, in `state`'s colour into its
        /// own .ico and returns that file's name. Errors say why the status
        /// folder shows instead.
        fn write_repo_icon(
            &self,
            path: &Path,
            state: FolderState,
            svg: &[u8],
        ) -> Result<String, repo_icon::Error> {
            let tone = state.tone();
            let ico = self.renderer.render(&key(path), svg, tone)?;
            let name = repo_icon::file_name(path, tone, svg);
            let file = icons_dir(&self.data_dir).join(&name);
            write_if_changed(&file, &ico).map_err(|e| repo_icon::Error {
                message: format!("{}: {e}", file.display()),
                transient: true,
            })?;
            Ok(name)
        }

        /// Deletes the repo icons no ledger entry points at, and lets go of
        /// kept ones whose folder shows another icon now.
        fn remove_stale_repo_icons(&mut self) {
            if !std::mem::take(&mut self.repo_icons_dirty) {
                return;
            }
            if self.ledger.prune_kept(shows_icon) {
                // Or with the next change.
                let _ = self.ledger.save(&self.ledger_file);
            }
            let dir = icons_dir(&self.data_dir);
            let Ok(listing) = std::fs::read_dir(&dir) else {
                return;
            };
            let names = listing
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok());
            for name in stale_repo_icons(&self.ledger, names) {
                // Maybe in use by Explorer this moment: next time, then.
                if std::fs::remove_file(dir.join(&name)).is_err() {
                    self.repo_icons_dirty = true;
                }
            }
        }

        /// Returns why the status folder shows instead of the repo's
        /// icon.svg, if it does in `RepoSvg` mode.
        fn apply(
            &mut self,
            path: &Path,
            state: FolderState,
            tip: &str,
            mode: FolderIconMode,
            svg: Option<String>,
        ) -> Result<Option<String>, String> {
            self.ensure_icons()?;
            let drawn = match svg {
                Some(planned) => draw_repo_svg(
                    planned,
                    || repo_icon::load(path),
                    |svg| self.write_repo_icon(path, state, svg),
                ),
                None => SvgOutcome::default(),
            };
            let icon = match &drawn.repo_icon {
                Some(name) => icons_dir(&self.data_dir).join(name),
                None => icon_file(&self.data_dir, state),
            };
            // Before desktop.ini exists, or the repo shows as dirty.
            crate::git::ensure_excluded(path, &format!("/{INI}"))
                .map_err(|e| format!("can't exclude desktop.ini: {e}"))?;

            let ini = path.join(INI);
            let ini_existed = ini.exists();
            let folder_attrs = attributes(path).ok_or("folder is gone")?;
            let prev = self.ledger.get(path).cloned();
            // `plan` applies only to a desktop.ini that is missing or points
            // at our icons, so an existing one with no entry is ours from
            // before the ledger lost it (folder renamed, ledger corrupt):
            // the file and the folder's ReadOnly mark both came from us.
            let adopted = prev.is_none() && ini_existed;
            let entry = Entry {
                path: path.to_path_buf(),
                state,
                tip: tip.to_string(),
                created_ini: !ini_existed
                    || adopted
                    || prev.as_ref().is_some_and(|e| e.created_ini),
                had_readonly: match &prev {
                    Some(e) => e.had_readonly,
                    None if adopted => false,
                    None => folder_attrs & (FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_SYSTEM) != 0,
                },
                icon_version: ICON_VERSION,
                mode,
                svg: drawn.svg,
                svg_error: drawn.svg_error,
                repo_icon: drawn.repo_icon,
            };
            // Record first, so a crash mid-write still leaves something to revert.
            if prev.is_none() {
                self.ledger.entries.insert(key(path), entry.clone());
                if let Err(e) = self.ledger.save(&self.ledger_file) {
                    self.ledger.entries.remove(&key(path));
                    return Err(e);
                }
            }
            if let Err(e) = write_settings(path, &ini, ini_existed, &icon, tip) {
                let _ = self.revert(path);
                return Err(e);
            }
            self.ledger.entries.insert(key(path), entry);
            self.notifier.push(path);
            self.ledger.save(&self.ledger_file)?;
            Ok(drawn.note)
        }

        /// Undoes our changes to a folder and drops its entry. An error
        /// keeps the entry, so the next sync tries again.
        fn revert(&mut self, path: &Path) -> Result<Reverted, String> {
            let Some(entry) = self.ledger.get(path).cloned() else {
                return Ok(Reverted::Restored);
            };
            let ini = path.join(INI);
            // Committed or pulled in since: the user owns it now. Drop our
            // record and leave the file and the folder alone.
            if ini.exists()
                && path.join(".git").exists()
                && crate::git::is_tracked(path, Path::new(INI))
            {
                self.ledger.forget(path);
                self.ledger.save(&self.ledger_file)?;
                return Ok(Reverted::Tracked);
            }
            // Whatever is left in desktop.ini still needs the folder marked.
            let mut ini_in_use = false;
            let bytes = match std::fs::read(&ini) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                // Locked or unreadable: it may still hold our keys.
                Err(e) => return Err(format!("{}: {e}", ini.display())),
            };
            if let Some(bytes) = bytes {
                let text = decode_ini(&bytes);
                let icons = icons_dir(&self.data_dir);
                let icon_ours = ini_icon(&text).is_some_and(|v| is_ours(&v, &icons, &env));
                let tip_ours =
                    ini_value(&text, SECTION, "InfoTip").is_some_and(|t| t.starts_with(TIP_PREFIX));
                let mut keys = Vec::new();
                if icon_ours {
                    keys.extend(["IconResource", "IconFile", "IconIndex"]);
                }
                if tip_ours {
                    keys.push("InfoTip");
                }
                clear_keys(&ini, &keys)?;
                let empty = std::fs::read(&ini).is_ok_and(|b| !ini_has_keys(&decode_ini(&b)));
                if entry.created_ini && empty {
                    let ini_w = wide(ini.as_os_str());
                    unsafe { SetFileAttributesW(ini_w.as_ptr(), FILE_ATTRIBUTE_NORMAL) };
                    std::fs::remove_file(&ini).map_err(|e| format!("{}: {e}", ini.display()))?;
                }
                ini_in_use = !empty;
            }
            if !entry.had_readonly && !ini_in_use {
                unsafe { PathUnmakeSystemFolderW(wide(path.as_os_str()).as_ptr()) };
            }
            self.notifier.push(path);
            self.ledger.entries.remove(&key(path));
            self.ledger.save(&self.ledger_file)?;
            Ok(Reverted::Restored)
        }
    }

    enum Reverted {
        Restored,
        /// desktop.ini is tracked by git now, so it was left as it is.
        Tracked,
    }

    /// Whether `folder`'s desktop.ini points at the icon file `name`, or
    /// may: one that can't be read this moment counts.
    fn shows_icon(folder: &Path, name: &str) -> bool {
        match std::fs::read(folder.join(INI)) {
            Ok(bytes) => ini_points_at(&decode_ini(&bytes), name),
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        }
    }

    /// Writes `bytes` to `file` through a temp file and a rename, unless
    /// it holds them already.
    fn write_if_changed(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
        if std::fs::read(file).ok().as_deref() == Some(bytes) {
            return Ok(());
        }
        let tmp = file.with_extension("ico.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, file)
    }

    /// Points desktop.ini at `icon` and sets the tip, then marks the
    /// folder so the shell reads it.
    fn write_settings(
        folder: &Path,
        ini: &Path,
        ini_existed: bool,
        icon: &Path,
        tip: &str,
    ) -> Result<(), String> {
        if !ini_existed {
            // Profile APIs keep a file's encoding, and an ANSI ini would turn
            // the tip's arrows into `?`. Seed it as UTF-16.
            let seed: Vec<u8> = format!("\u{FEFF}[{SECTION}]\r\n")
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            std::fs::write(ini, seed).map_err(|e| format!("{}: {e}", ini.display()))?;
        }
        let mut icon = wide(icon.as_os_str());
        let mut tip = wide(tip.as_ref());
        let folder_w = wide(folder.as_os_str());
        // All-zero is valid for this plain C struct: null pointers and zero counts.
        let mut fcs: SHFOLDERCUSTOMSETTINGS = unsafe { std::mem::zeroed() };
        fcs.dwSize = size_of::<SHFOLDERCUSTOMSETTINGS>() as u32;
        fcs.dwMask = FCSM_ICONFILE | FCSM_INFOTIP;
        fcs.pszIconFile = icon.as_mut_ptr();
        fcs.iIconIndex = 0;
        fcs.pszInfoTip = tip.as_mut_ptr();
        let hr =
            unsafe { SHGetSetFolderCustomSettings(&mut fcs, folder_w.as_ptr(), FCS_FORCEWRITE) };
        if hr < 0 {
            return Err(format!("can't write desktop.ini (0x{:08X})", hr as u32));
        }
        // The shell usually hides it already; make sure, it's our clutter.
        let ini_w = wide(ini.as_os_str());
        if let Some(attrs) = attributes(ini) {
            let attrs = attrs | FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM;
            unsafe { SetFileAttributesW(ini_w.as_ptr(), attrs) };
        }
        if unsafe { PathMakeSystemFolderW(folder_w.as_ptr()) } == 0 {
            return Err("can't mark the folder as customised".to_string());
        }
        Ok(())
    }

    /// Removes `keys` from the `.ShellClassInfo` section and flushes the
    /// profile cache so a following read sees the file as it is on disk.
    fn clear_keys(ini: &Path, keys: &[&str]) -> Result<(), String> {
        let ini_w = wide(ini.as_os_str());
        let section = wide(SECTION.as_ref());
        for k in keys {
            let k = wide(k.as_ref());
            let ok = unsafe {
                WritePrivateProfileStringW(
                    section.as_ptr(),
                    k.as_ptr(),
                    std::ptr::null(),
                    ini_w.as_ptr(),
                )
            };
            if ok == 0 {
                return Err(format!("can't edit {}", ini.display()));
            }
        }
        unsafe {
            WritePrivateProfileStringW(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                ini_w.as_ptr(),
            )
        };
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::Command;

        fn git(dir: &Path, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        /// The Windows CI round trip: apply leaves the repo clean and the
        /// folder customised, revert restores both.
        #[test]
        fn apply_then_revert() {
            let root = std::env::temp_dir().join(format!("gitstat-icons-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let repo = root.join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            let data = root.join("data");
            let status = RepoStatus {
                name: "repo".into(),
                path: repo.clone(),
                dirty: true,
                ..Default::default()
            };
            let _com = Com::init();
            let mut engine = Engine::new(data.clone());
            let status_mode = FolderIconMode::Status;
            let (skips, _) = engine.sync(std::slice::from_ref(&status), true, status_mode);
            assert!(skips.is_empty(), "{skips:?}");

            let ini = repo.join(INI);
            let ini_attrs = attributes(&ini).unwrap();
            assert_ne!(ini_attrs & FILE_ATTRIBUTE_HIDDEN, 0);
            assert_ne!(ini_attrs & FILE_ATTRIBUTE_SYSTEM, 0);
            let folder = attributes(&repo).unwrap();
            assert_ne!(
                folder & (FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_SYSTEM),
                0
            );
            assert_eq!(git(&repo, &["status", "--porcelain"]), "");
            let text = decode_ini(&std::fs::read(&ini).unwrap());
            assert!(is_ours(&ini_icon(&text).unwrap(), &icons_dir(&data), &env));
            assert_eq!(ini_value(&text, SECTION, "InfoTip").unwrap(), tip(&status));
            assert_eq!(
                Ledger::load(&data.join(LEDGER_FILE)).unwrap().entries.len(),
                1
            );

            // Unchanged status: nothing to do.
            assert_eq!(
                engine.sync(std::slice::from_ref(&status), true, status_mode),
                (vec![], 0)
            );

            // The ledger lost track (renamed folder, corrupt ledger): our
            // desktop.ini is adopted as ours, so revert still restores all.
            std::fs::remove_file(data.join(LEDGER_FILE)).unwrap();
            let mut engine = Engine::new(data.clone());
            let (skips, _) = engine.sync(std::slice::from_ref(&status), true, status_mode);
            assert!(skips.is_empty(), "{skips:?}");
            let entry = engine.ledger.get(&repo).unwrap().clone();
            assert!(entry.created_ini && !entry.had_readonly, "{entry:?}");

            // The repo's own icon: its own .ico, which a new picture
            // replaces, and a broken one falls back with a note.
            let svg_mode = FolderIconMode::RepoSvg;
            let ini_icon_of = || ini_icon(&decode_ini(&std::fs::read(&ini).unwrap())).unwrap();
            let repo_icons = || {
                let mut names: Vec<String> = std::fs::read_dir(icons_dir(&data))
                    .unwrap()
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| repo_icon::is_repo_icon_file(n))
                    .collect();
                names.sort();
                names
            };
            std::fs::create_dir_all(repo.join(repo_icon::DIR)).unwrap();
            let svg = |fill: &str| {
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8" fill="{fill}"/></svg>"#
                )
            };
            std::fs::write(repo_icon::svg_path(&repo), svg("currentColor")).unwrap();
            let (skips, _) = engine.sync(std::slice::from_ref(&status), true, svg_mode);
            assert!(skips.is_empty(), "{skips:?}");
            let first = engine.ledger.get(&repo).unwrap().repo_icon.clone().unwrap();
            assert!(ini_icon_of().contains(&first), "{}", ini_icon_of());
            assert_eq!(repo_icons(), std::slice::from_ref(&first));
            assert_eq!(
                engine.sync(std::slice::from_ref(&status), true, svg_mode),
                (vec![], 0),
                "applied once"
            );
            std::fs::write(repo_icon::svg_path(&repo), svg("#123456")).unwrap();
            let (skips, _) = engine.sync(std::slice::from_ref(&status), true, svg_mode);
            assert!(skips.is_empty(), "{skips:?}");
            let second = engine.ledger.get(&repo).unwrap().repo_icon.clone().unwrap();
            assert_ne!(first, second);
            assert_eq!(repo_icons(), [second], "the old picture is cleaned up");
            std::fs::write(repo_icon::svg_path(&repo), "<svg").unwrap();
            for _ in 0..2 {
                let (skips, _) = engine.sync(std::slice::from_ref(&status), true, svg_mode);
                assert_eq!(skips.len(), 1, "{skips:?}");
                assert!(skips[0].1.contains("icon.svg"), "{skips:?}");
            }
            let entry = engine.ledger.get(&repo).unwrap().clone();
            assert!(entry.svg_error.is_some() && entry.repo_icon.is_none());
            assert!(is_ours(&ini_icon_of(), &icons_dir(&data), &env));
            assert!(repo_icons().is_empty());
            std::fs::write(repo_icon::svg_path(&repo), svg("currentColor")).unwrap();
            let (skips, _) = engine.sync(std::slice::from_ref(&status), true, svg_mode);
            assert!(skips.is_empty(), "{skips:?}");
            assert_eq!(repo_icons().len(), 1);

            // desktop.ini committed: the folder is let go of as it is, and
            // the icon it shows is kept. Untracked again, it is adopted.
            git(&repo, &["add", "-f", INI]);
            let mut changed = status.clone();
            changed.ahead = 1;
            let (skips, _) = engine.sync(std::slice::from_ref(&changed), true, svg_mode);
            assert_eq!(skips, [(repo.clone(), SKIP_TRACKED.to_string())]);
            assert!(engine.ledger.get(&repo).is_none());
            assert_eq!(engine.ledger.kept.len(), 1, "{:?}", engine.ledger.kept);
            let icons = repo_icons();
            assert_eq!(icons.len(), 1);
            assert!(ini_icon_of().contains(&icons[0]), "{}", ini_icon_of());
            git(&repo, &["rm", "--cached", "-q", INI]);
            let (skips, _) = engine.sync(std::slice::from_ref(&status), true, svg_mode);
            assert!(skips.is_empty(), "{skips:?}");
            assert_eq!(repo_icons(), icons);
            drop(engine);

            assert_eq!(revert_all_blocking(&data), Ok(1));
            assert!(!ini.exists());
            assert!(repo_icons().is_empty(), "no folder uses them now");
            let folder = attributes(&repo).unwrap();
            assert_eq!(
                folder & (FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_SYSTEM),
                0
            );
            assert!(
                Ledger::load(&data.join(LEDGER_FILE))
                    .unwrap()
                    .entries
                    .is_empty()
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FolderIconMode::{RepoSvg, Status};
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        ineligible: HashSet<PathBuf>,
        tracked: HashSet<PathBuf>,
        foreign: HashSet<PathBuf>,
        missing: HashSet<PathBuf>,
        no_ini: HashSet<PathBuf>,
        svgs: HashMap<PathBuf, String>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl Probe for Fake {
        fn eligible(&self, p: &Path) -> bool {
            self.calls.borrow_mut().push("eligible");
            !self.ineligible.contains(p)
        }
        fn ini_tracked(&self, p: &Path) -> bool {
            self.calls.borrow_mut().push("tracked");
            self.tracked.contains(p)
        }
        fn foreign_ini(&self, p: &Path) -> bool {
            self.calls.borrow_mut().push("foreign");
            self.foreign.contains(p)
        }
        fn exists(&self, p: &Path) -> bool {
            !self.missing.contains(p)
        }
        fn ini_present(&self, p: &Path) -> bool {
            !self.no_ini.contains(p)
        }
        fn repo_svg(&self, p: &Path) -> Option<String> {
            self.svgs.get(p).cloned()
        }
    }

    fn repo(name: &str) -> RepoStatus {
        RepoStatus {
            name: name.into(),
            path: PathBuf::from(format!("D:/dev/{name}")),
            has_upstream: true,
            ..Default::default()
        }
    }

    fn ledger_of(statuses: &[RepoStatus]) -> Ledger {
        let mut ledger = Ledger::default();
        for s in statuses {
            ledger.entries.insert(
                key(&s.path),
                Entry {
                    path: s.path.clone(),
                    state: FolderState::of(s),
                    tip: tip(s),
                    created_ini: true,
                    had_readonly: false,
                    icon_version: ICON_VERSION,
                    mode: Status,
                    svg: None,
                    svg_error: None,
                    repo_icon: None,
                },
            );
        }
        ledger
    }

    fn apply(s: &RepoStatus) -> Op {
        apply_in(s, Status, None)
    }

    fn apply_in(s: &RepoStatus, mode: FolderIconMode, svg: Option<&str>) -> Op {
        Op::Apply {
            path: s.path.clone(),
            state: FolderState::of(s),
            tip: tip(s),
            mode,
            svg: svg.map(str::to_string),
        }
    }

    /// The ledger after applying `op`, as the engine records it.
    fn record(ledger: &mut Ledger, op: &Op, svg_error: Option<&str>) {
        let Op::Apply {
            path,
            state,
            tip,
            mode,
            svg,
        } = op
        else {
            panic!("not an apply: {op:?}");
        };
        let repo_icon = (svg.is_some() && svg_error.is_none()).then(|| format!("repo-{state:?}"));
        ledger.entries.insert(
            key(path),
            Entry {
                path: path.clone(),
                state: *state,
                tip: tip.clone(),
                created_ini: true,
                had_readonly: false,
                icon_version: ICON_VERSION,
                mode: *mode,
                svg: svg.clone(),
                svg_error: svg_error.map(str::to_string),
                repo_icon,
            },
        );
    }

    #[test]
    fn repo_svg_changes_apply_once() {
        let (a, b) = (repo("a"), repo("b"));
        let mut fake = Fake {
            svgs: HashMap::from([(a.path.clone(), "h1".to_string())]),
            ..Default::default()
        };
        let both = [a.clone(), b.clone()];
        // Switching modes re-applies every folder, with or without an icon.svg.
        let mut ledger = ledger_of(&both);
        let ops = plan(&ledger, &both, true, RepoSvg, &fake);
        assert_eq!(
            ops,
            vec![
                apply_in(&a, RepoSvg, Some("h1")),
                apply_in(&b, RepoSvg, None)
            ]
        );
        for op in &ops {
            record(&mut ledger, op, None);
        }
        assert_eq!(plan(&ledger, &both, true, RepoSvg, &fake), vec![]);

        // A new picture, once.
        fake.svgs.insert(a.path.clone(), "h2".into());
        let ops = plan(&ledger, &both, true, RepoSvg, &fake);
        assert_eq!(ops, vec![apply_in(&a, RepoSvg, Some("h2"))]);
        record(&mut ledger, &ops[0], None);
        assert_eq!(plan(&ledger, &both, true, RepoSvg, &fake), vec![]);

        // One that can't be drawn: applied once, then noted every time.
        fake.svgs.insert(a.path.clone(), "bad".into());
        let ops = plan(&ledger, &both, true, RepoSvg, &fake);
        assert_eq!(ops, vec![apply_in(&a, RepoSvg, Some("bad"))]);
        record(&mut ledger, &ops[0], Some("icon.svg isn't usable SVG"));
        for _ in 0..2 {
            assert_eq!(
                plan(&ledger, &both, true, RepoSvg, &fake),
                vec![Op::Note(a.path.clone(), "icon.svg isn't usable SVG".into())]
            );
        }

        // An icon.svg added or removed counts too; the state as well.
        fake.svgs.insert(b.path.clone(), "h3".into());
        fake.svgs.remove(&a.path);
        let mut dirty_b = b.clone();
        dirty_b.dirty = true;
        let changed = [a.clone(), dirty_b.clone()];
        let ops = plan(&ledger, &changed, true, RepoSvg, &fake);
        assert_eq!(
            ops,
            vec![
                apply_in(&a, RepoSvg, None),
                apply_in(&dirty_b, RepoSvg, Some("h3"))
            ]
        );
        for op in &ops {
            record(&mut ledger, op, None);
        }
        assert_eq!(plan(&ledger, &changed, true, RepoSvg, &fake), vec![]);

        // And back to the status folders: all again, the icon.svg unread.
        let ops = plan(&ledger, &changed, true, Status, &fake);
        assert_eq!(ops, vec![apply(&a), apply(&dirty_b)]);
        for op in &ops {
            record(&mut ledger, op, None);
        }
        assert_eq!(plan(&ledger, &changed, true, Status, &fake), vec![]);
        // Off reverts whatever the mode.
        assert_eq!(
            plan(&ledger, &changed, false, RepoSvg, &fake),
            vec![Op::Revert(a.path.clone()), Op::Revert(b.path.clone())]
        );
    }

    /// Counts icon.svg reads.
    struct Reads<'a>(&'a Fake, RefCell<usize>);

    impl Probe for Reads<'_> {
        fn eligible(&self, p: &Path) -> bool {
            self.0.eligible(p)
        }
        fn ini_tracked(&self, p: &Path) -> bool {
            self.0.ini_tracked(p)
        }
        fn foreign_ini(&self, p: &Path) -> bool {
            self.0.foreign_ini(p)
        }
        fn exists(&self, p: &Path) -> bool {
            self.0.exists(p)
        }
        fn ini_present(&self, p: &Path) -> bool {
            self.0.ini_present(p)
        }
        fn repo_svg(&self, p: &Path) -> Option<String> {
            *self.1.borrow_mut() += 1;
            self.0.repo_svg(p)
        }
    }

    #[test]
    fn icon_svgs_are_read_only_when_needed() {
        let (a, b, c) = (repo("a"), repo("b"), repo("c"));
        let fake = Fake {
            ineligible: HashSet::from([b.path.clone()]),
            foreign: HashSet::from([c.path.clone()]),
            svgs: HashMap::from([(a.path.clone(), "h".to_string())]),
            ..Default::default()
        };
        let all = [a.clone(), b.clone(), c.clone()];
        let reads = Reads(&fake, RefCell::new(0));
        // Skipped folders aren't read; the one applied is, once.
        let ops = plan(&Ledger::default(), &all, true, RepoSvg, &reads);
        assert_eq!(ops[0], apply_in(&a, RepoSvg, Some("h")));
        assert_eq!(*reads.1.borrow(), 1);
        // Up to date: one read each to tell. Status mode: none.
        let mut ledger = Ledger::default();
        record(&mut ledger, &ops[0], None);
        *reads.1.borrow_mut() = 0;
        assert_eq!(
            plan(&ledger, std::slice::from_ref(&a), true, RepoSvg, &reads),
            vec![]
        );
        assert_eq!(*reads.1.borrow(), 1);
        *reads.1.borrow_mut() = 0;
        plan(&ledger, &all, true, Status, &reads);
        assert_eq!(*reads.1.borrow(), 0);
    }

    #[test]
    fn old_ledgers_mean_status_mode() {
        let old = r#"{ "entries": { "x": { "path": "x", "state": "synced", "tip": "t",
            "created_ini": true, "had_readonly": false, "icon_version": 2 } } }"#;
        let old: Ledger = serde_json::from_str(old).unwrap();
        let e = &old.entries["x"];
        assert_eq!(
            (e.mode, &e.svg, &e.svg_error, &e.repo_icon),
            (Status, &None, &None, &None)
        );
        assert!(old.kept.is_empty());
        // Status-mode entries are written as before.
        let text = serde_json::to_string(&ledger_of(&[repo("a")])).unwrap();
        assert!(
            !text.contains("svg") && !text.contains("repo_icon") && !text.contains("kept"),
            "{text}"
        );
    }

    #[test]
    fn a_new_render_version_redraws_once() {
        let a = repo("a");
        let svg = b"<svg/>";
        let fake = Fake {
            svgs: HashMap::from([(a.path.clone(), repo_icon::fingerprint_of(svg))]),
            ..Default::default()
        };
        // Drawn by the version before.
        let old = repo_icon::fingerprint_at(repo_icon::RENDER_VERSION.wrapping_sub(1), svg);
        let mut ledger = Ledger::default();
        record(&mut ledger, &apply_in(&a, RepoSvg, Some(&old)), None);
        let ops = plan(&ledger, std::slice::from_ref(&a), true, RepoSvg, &fake);
        let now = repo_icon::fingerprint_of(svg);
        assert_eq!(ops, vec![apply_in(&a, RepoSvg, Some(&now))]);
        record(&mut ledger, &ops[0], None);
        assert_eq!(
            plan(&ledger, std::slice::from_ref(&a), true, RepoSvg, &fake),
            vec![]
        );
    }

    fn file_error(message: &str) -> repo_icon::Error {
        repo_icon::Error {
            message: message.into(),
            transient: false,
        }
    }

    fn transient(message: &str) -> repo_icon::Error {
        repo_icon::Error {
            message: message.into(),
            transient: true,
        }
    }

    #[test]
    fn only_the_files_own_faults_are_remembered() {
        let bytes = b"<svg/>".to_vec();
        let seen = repo_icon::fingerprint_of(&bytes);
        let planned = || "planned".to_string();
        let loaded = || Ok(Some(bytes.clone()));
        let unreachable = |_: &[u8]| -> Result<String, repo_icon::Error> { panic!("drawn") };
        // Drawn.
        assert_eq!(
            draw_repo_svg(planned(), loaded, |_| Ok("repo-1.ico".into())),
            SvgOutcome {
                svg: Some(seen.clone()),
                repo_icon: Some("repo-1.ico".into()),
                ..Default::default()
            }
        );
        // The file's fault: remembered with what was drawn, not what plan
        // saw, so it waits for the file to change.
        assert_eq!(
            draw_repo_svg(planned(), loaded, |_| Err(file_error("bad"))),
            SvgOutcome {
                svg: Some(seen),
                svg_error: Some("bad".into()),
                repo_icon: None,
                note: Some("bad".into()),
            }
        );
        assert_eq!(
            draw_repo_svg(planned(), || Err(file_error("a link")), unreachable),
            SvgOutcome {
                svg: Some(planned()),
                svg_error: Some("a link".into()),
                repo_icon: None,
                note: Some("a link".into()),
            }
        );
        // Anything else: noted this time, and no fingerprint, so the next
        // sync tries again.
        let retry = |why: &str| SvgOutcome {
            note: Some(why.into()),
            ..Default::default()
        };
        assert_eq!(
            draw_repo_svg(planned(), loaded, |_| Err(transient("too long"))),
            retry("too long")
        );
        assert_eq!(
            draw_repo_svg(planned(), || Err(transient("locked")), unreachable),
            retry("locked")
        );
        // Gone since: nothing to note.
        assert_eq!(
            draw_repo_svg(planned(), || Ok(None), unreachable),
            SvgOutcome::default()
        );

        // As plan sees them: a remembered fault is only noted, a transient
        // one is tried again, every time until it works.
        let a = repo("a");
        let fake = Fake {
            svgs: HashMap::from([(a.path.clone(), "h".to_string())]),
            ..Default::default()
        };
        let mut ledger = Ledger::default();
        let op = apply_in(&a, RepoSvg, Some("h"));
        record(&mut ledger, &op, Some("bad"));
        assert_eq!(
            plan(&ledger, std::slice::from_ref(&a), true, RepoSvg, &fake),
            vec![Op::Note(a.path.clone(), "bad".into())]
        );
        let retried = apply_in(&a, RepoSvg, None);
        record(&mut ledger, &retried, None);
        for _ in 0..2 {
            assert_eq!(
                plan(&ledger, std::slice::from_ref(&a), true, RepoSvg, &fake),
                vec![op.clone()]
            );
        }
    }

    #[test]
    fn a_folder_let_go_of_keeps_its_repo_icon() {
        let (a, b) = (repo("a"), repo("b"));
        let mut ledger = ledger_of(&[a.clone(), b.clone()]);
        ledger.entries.get_mut(&key(&a.path)).unwrap().repo_icon =
            Some("repo-1-green-2.ico".into());
        let names = || ["repo-1-green-2.ico".to_string()];
        assert!(stale_repo_icons(&ledger, names()).is_empty());
        // Forgotten (desktop.ini tracked now): still used.
        ledger.forget(&a.path);
        ledger.forget(&b.path);
        assert!(ledger.entries.is_empty());
        let kept = Kept {
            path: a.path.clone(),
            repo_icon: "repo-1-green-2.ico".into(),
        };
        assert_eq!(ledger.kept, std::slice::from_ref(&kept));
        assert!(stale_repo_icons(&ledger, names()).is_empty());
        // Applied and forgotten again: kept once.
        let mut again = ledger_of(std::slice::from_ref(&a));
        again.entries.get_mut(&key(&a.path)).unwrap().repo_icon = Some("repo-1-green-2.ico".into());
        ledger.entries.append(&mut again.entries);
        ledger.forget(&a.path);
        assert_eq!(ledger.kept, std::slice::from_ref(&kept));
        // Saved and loaded with the ledger.
        let text = serde_json::to_string(&ledger).unwrap();
        assert_eq!(serde_json::from_str::<Ledger>(&text).unwrap(), ledger);
        // Until its folder shows another icon.
        assert!(!ledger.prune_kept(|_, _| true));
        assert_eq!(ledger.kept, [kept]);
        assert!(ledger.prune_kept(|path, name| path != a.path || name != "repo-1-green-2.ico"));
        assert!(ledger.kept.is_empty());
        assert_eq!(stale_repo_icons(&ledger, names()), names());
    }

    #[test]
    fn desktop_ini_pointing_at_an_icon() {
        let ini = "[.ShellClassInfo]\r\nIconResource=C:\\Users\\me\\AppData\\Local\\gitstat\\icons\\Repo-1-GREEN-2.ico,0\r\n";
        assert!(ini_points_at(ini, "repo-1-green-2.ico"));
        assert!(!ini_points_at(ini, "repo-1-amber-2.ico"));
        assert!(ini_points_at(
            "[.ShellClassInfo]\nIconFile=D:/x/repo-1-green-2.ico\n",
            "repo-1-green-2.ico"
        ));
        assert!(!ini_points_at(
            "[.ShellClassInfo]\nInfoTip=repo-1-green-2.ico\n",
            "repo-1-green-2.ico"
        ));
    }

    #[test]
    fn unused_repo_icons_are_stale() {
        let (a, b) = (repo("a"), repo("b"));
        let mut ledger = ledger_of(&[a.clone(), b.clone()]);
        ledger.entries.get_mut(&key(&a.path)).unwrap().repo_icon =
            Some("repo-1-green-2.ico".into());
        let names = [
            "repo-1-green-2.ico",
            "repo-1-amber-2.ico",
            "repo-3-red-4.ico",
            "repo-1-green-2.ico.tmp",
            "synced-v2.ico",
            "dirty-v2.ico.tmp",
            "notes.txt",
        ]
        .map(String::from);
        assert_eq!(
            stale_repo_icons(&ledger, names),
            [
                "repo-1-amber-2.ico",
                "repo-3-red-4.ico",
                "repo-1-green-2.ico.tmp"
            ]
        );
    }

    #[test]
    fn applies_only_on_change() {
        let (a, mut b) = (repo("a"), repo("b"));
        let fake = Fake::default();
        assert_eq!(
            plan(
                &Ledger::default(),
                &[a.clone(), b.clone()],
                true,
                Status,
                &fake
            ),
            vec![apply(&a), apply(&b)]
        );

        let ledger = ledger_of(&[a.clone(), b.clone()]);
        fake.calls.borrow_mut().clear();
        assert_eq!(
            plan(&ledger, &[a.clone(), b.clone()], true, Status, &fake),
            vec![]
        );
        assert!(fake.calls.borrow().is_empty(), "up to date: no probing");

        b.ahead = 2;
        assert_eq!(
            plan(&ledger, &[a.clone(), b.clone()], true, Status, &fake),
            vec![apply(&b)]
        );
        // The tip alone changing counts too.
        let mut b3 = b.clone();
        b3.ahead = 3;
        let ledger = ledger_of(&[a.clone(), b.clone()]);
        assert_eq!(
            plan(&ledger, &[a, b3.clone()], true, Status, &fake),
            vec![apply(&b3)]
        );
    }

    #[test]
    fn new_icon_version_is_reapplied_once() {
        let (a, b) = (repo("a"), repo("b"));
        let mut ledger = ledger_of(&[a.clone(), b.clone()]);
        ledger.entries.get_mut(&key(&a.path)).unwrap().icon_version = ICON_VERSION - 1;
        let fake = Fake::default();
        assert_eq!(
            plan(&ledger, &[a.clone(), b.clone()], true, Status, &fake),
            vec![apply(&a)]
        );
        // A ledger written before the field existed reads as version 0.
        let old = r#"{ "entries": { "x": { "path": "x", "state": "synced", "tip": "t",
            "created_ini": true, "had_readonly": false } } }"#;
        let old: Ledger = serde_json::from_str(old).unwrap();
        assert_eq!(old.entries["x"].icon_version, 0);
    }

    #[test]
    fn missing_ini_is_reapplied() {
        let a = repo("a");
        let ledger = ledger_of(std::slice::from_ref(&a));
        let fake = Fake {
            no_ini: HashSet::from([a.path.clone()]),
            ..Default::default()
        };
        assert_eq!(
            plan(&ledger, std::slice::from_ref(&a), true, Status, &fake),
            vec![apply(&a)]
        );
    }

    #[test]
    fn drive_root_is_never_given_an_icon() {
        let root = RepoStatus {
            path: PathBuf::from(if cfg!(windows) { "D:\\" } else { "/" }),
            ..repo("root")
        };
        let fake = Fake::default();
        assert_eq!(
            plan(
                &Ledger::default(),
                std::slice::from_ref(&root),
                true,
                Status,
                &fake
            ),
            vec![Op::Skip(root.path.clone(), SKIP_INELIGIBLE)]
        );
    }

    #[test]
    fn skip_reasons() {
        let (a, b, c) = (repo("a"), repo("b"), repo("c"));
        let fake = Fake {
            ineligible: HashSet::from([a.path.clone()]),
            tracked: HashSet::from([b.path.clone()]),
            foreign: HashSet::from([c.path.clone()]),
            ..Default::default()
        };
        let all = [a.clone(), b.clone(), c.clone()];
        assert_eq!(
            plan(&Ledger::default(), &all, true, Status, &fake),
            vec![
                Op::Skip(a.path.clone(), SKIP_INELIGIBLE),
                Op::Skip(b.path.clone(), SKIP_TRACKED),
                Op::Skip(c.path.clone(), SKIP_FOREIGN),
            ]
        );
        // The git probe only runs when the cheap ones pass.
        assert_eq!(
            *fake.calls.borrow(),
            [
                "eligible", "eligible", "foreign", "tracked", "eligible", "foreign"
            ]
        );

        // Folders we had applied: undo where the file is untracked (for a
        // foreign icon, revert removes only our InfoTip), else let go.
        let d = repo("d");
        let mut fake = fake;
        fake.foreign.insert(d.path.clone());
        fake.tracked.insert(d.path.clone());
        let all = [a.clone(), b.clone(), c.clone(), d.clone()];
        let mut changed = all.clone();
        for s in &mut changed {
            s.dirty = true;
        }
        assert_eq!(
            plan(&ledger_of(&all), &changed, true, Status, &fake),
            vec![
                Op::Revert(a.path.clone()),
                Op::Skip(a.path.clone(), SKIP_INELIGIBLE),
                Op::Forget(b.path.clone()),
                Op::Skip(b.path.clone(), SKIP_TRACKED),
                Op::Revert(c.path.clone()),
                Op::Skip(c.path.clone(), SKIP_FOREIGN),
                Op::Forget(d.path.clone()),
                Op::Skip(d.path.clone(), SKIP_FOREIGN),
            ]
        );
    }

    #[test]
    fn stale_entries_are_undone_first() {
        // The same folder under a new spelling: the old entry's revert must
        // run before the new key's apply, or it deletes what apply wrote.
        let old = repo("a");
        let mut new = old.clone();
        new.path = PathBuf::from("S:/a");
        let ledger = ledger_of(std::slice::from_ref(&old));
        assert_eq!(
            plan(
                &ledger,
                std::slice::from_ref(&new),
                true,
                Status,
                &Fake::default()
            ),
            vec![Op::Revert(old.path.clone()), apply(&new)]
        );
    }

    #[test]
    fn fetch_errors_do_not_flap() {
        let a = repo("a");
        let ledger = ledger_of(std::slice::from_ref(&a));
        let mut offline = a.clone();
        offline.error = Some("could not resolve host".into());
        offline.fetch_failed = true;
        assert_eq!(
            plan(&ledger, &[offline.clone()], true, Status, &Fake::default()),
            vec![]
        );

        // A local change still shows while offline.
        offline.dirty = true;
        let mut local = a.clone();
        local.dirty = true;
        assert_eq!(
            plan(&ledger, &[offline], true, Status, &Fake::default()),
            vec![apply(&local)]
        );

        // A real error does change the icon.
        let mut broken = a.clone();
        broken.error = Some("not a git repository".into());
        let ops = plan(&ledger, &[broken], true, Status, &Fake::default());
        assert!(matches!(
            &ops[..],
            [Op::Apply {
                state: FolderState::Error,
                ..
            }]
        ));
    }

    #[test]
    fn removed_repos_revert_or_forget() {
        let (a, b, c) = (repo("a"), repo("b"), repo("c"));
        let ledger = ledger_of(&[a.clone(), b.clone(), c.clone()]);
        let fake = Fake {
            missing: HashSet::from([c.path.clone()]),
            ..Default::default()
        };
        assert_eq!(
            plan(&ledger, std::slice::from_ref(&a), true, Status, &fake),
            vec![Op::Revert(b.path.clone()), Op::Forget(c.path.clone())]
        );
    }

    #[test]
    fn disabled_reverts_everything() {
        let (a, b) = (repo("a"), repo("b"));
        let ledger = ledger_of(&[a.clone(), b.clone()]);
        let fake = Fake {
            missing: HashSet::from([b.path.clone()]),
            ..Default::default()
        };
        assert_eq!(
            plan(&ledger, &[a.clone(), b.clone()], false, Status, &fake),
            vec![Op::Revert(a.path.clone()), Op::Forget(b.path.clone())]
        );
        assert_eq!(
            plan(&Ledger::default(), &[a, b], false, Status, &fake),
            vec![]
        );
    }

    #[test]
    fn tips_are_stable() {
        let mut s = repo("a");
        assert_eq!(tip(&s), "gitstat: in sync");
        s.ahead = 2;
        s.dirty = true;
        assert_eq!(tip(&s), "gitstat: ⬆2, uncommitted changes");
        s.has_upstream = false;
        s.ahead = 0;
        assert_eq!(tip(&s), "gitstat: no upstream, uncommitted changes");
        s.error = Some("fatal: bad\nsecond line".into());
        assert_eq!(tip(&s), "gitstat: error: fatal: bad");
        // Nothing time-dependent: the same status gives the same tip.
        assert_eq!(tip(&s), tip(&s.clone()));
        assert!(!tip(&s).chars().any(|c| c.is_ascii_digit()));
    }

    #[test]
    fn icon_files_are_versioned_per_state() {
        let dir = Path::new("/data");
        assert_eq!(
            icon_file(dir, FolderState::NoUpstream),
            Path::new("/data/icons").join(format!("no-upstream-v{ICON_VERSION}.ico"))
        );
        let names: HashSet<_> = FolderState::ALL
            .iter()
            .map(|&s| icon_file(dir, s))
            .collect();
        assert_eq!(names.len(), FolderState::ALL.len());
    }

    #[test]
    fn ledger_round_trips_and_survives_corruption() {
        let dir = std::env::temp_dir().join(format!("gitstat-ledger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join(LEDGER_FILE);
        assert_eq!(Ledger::load(&file), Ok(Ledger::default()));

        let ledger = ledger_of(&[repo("a"), repo("B")]);
        ledger.save(&file).unwrap();
        assert_eq!(Ledger::load(&file), Ok(ledger));
        assert!(
            Ledger::load(&file)
                .unwrap()
                .get(Path::new("D:/DEV/b"))
                .is_some()
        );
        assert!(!file.with_extension("json.tmp").exists());

        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(Ledger::load(&file), Ok(Ledger::default()));
        assert!(
            file.with_extension("json.bad").exists(),
            "kept for inspection"
        );
        std::fs::write(&file, b"\xff\xfe{}").unwrap();
        assert_eq!(Ledger::load(&file), Ok(Ledger::default()), "not UTF-8");

        // Exists but can't be read: an error, not an empty ledger that the
        // next save would write over it.
        let _ = std::fs::remove_file(&file);
        std::fs::create_dir_all(&file).unwrap();
        assert!(Ledger::load(&file).is_err());
        assert!(file.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_ini_in_both_encodings() {
        let text = "[.ShellClassInfo]\r\nIconResource=C:\\x\\a.ico,0\r\nInfoTip=gitstat: ⬆1\r\n";
        let utf16: Vec<u8> = "\u{FEFF}"
            .chars()
            .chain(text.chars())
            .collect::<String>()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        for bytes in [text.as_bytes().to_vec(), utf16] {
            let decoded = decode_ini(&bytes);
            assert_eq!(
                ini_value(&decoded, ".shellclassinfo", "iconresource").as_deref(),
                Some("C:\\x\\a.ico,0")
            );
            assert_eq!(
                ini_value(&decoded, SECTION, "InfoTip").as_deref(),
                Some("gitstat: ⬆1")
            );
        }
        let other = "[ViewState]\nIconResource=nope\n[.ShellClassInfo]\nIconFile=old.ico\n";
        assert_eq!(ini_icon(other).as_deref(), Some("old.ico"));
        assert!(!ini_has_keys("[.ShellClassInfo]\r\n\r\n; note\r\n"));
        assert!(ini_has_keys("[.ShellClassInfo]\r\nConfirmFileOp=0\r\n"));
    }

    #[test]
    fn recognises_our_icons() {
        let icons = Path::new(r"C:\Users\me\AppData\Local\gitstat\icons");
        let env =
            |name: &str| (name == "LOCALAPPDATA").then(|| r"C:\Users\me\AppData\Local".to_string());
        let ours = |v: &str| is_ours(v, icons, &env);
        assert!(ours(
            r"C:\Users\me\AppData\Local\gitstat\icons\dirty-v1.ico,0"
        ));
        assert!(ours(
            r#""c:/users/ME/appdata/local/gitstat/icons/dirty-v1.ico",-3"#
        ));
        assert!(ours(r"%LOCALAPPDATA%\gitstat\icons\synced-v1.ico,0"));
        assert!(!ours(r"C:\Users\me\AppData\Local\gitstat\icons2\x.ico,0"));
        assert!(!ours(r"%SystemRoot%\system32\SHELL32.dll,4"));
        assert!(!ours(""));

        let mine = "[.ShellClassInfo]\nIconResource=C:\\Users\\me\\AppData\\Local\\gitstat\\icons\\a.ico,0\n";
        assert!(!foreign(mine, icons, &env));
        assert!(foreign(
            "[.ShellClassInfo]\nIconResource=D:\\art\\x.ico,0\n",
            icons,
            &env
        ));
        assert!(foreign(
            "[.ShellClassInfo]\nLocalizedResourceName=Stuff\n",
            icons,
            &env
        ));
    }

    #[test]
    fn eligibility_helpers() {
        assert_eq!(drive_root(r"D:\dev\a").as_deref(), Some(r"D:\"));
        assert_eq!(drive_root("d:/dev/a").as_deref(), Some(r"d:\"));
        assert_eq!(drive_root(r"\\?\C:\dev").as_deref(), Some(r"C:\"));
        assert_eq!(drive_root(r"\\server\share\a"), None);
        assert_eq!(drive_root(r"\\?\UNC\server\share"), None);
        assert_eq!(drive_root("/home/me"), None);

        let od = r"C:\Users\me\OneDrive";
        assert!(under(r"c:\users\me\onedrive\dev\a", od));
        assert!(under("C:/Users/me/OneDrive", od));
        assert!(!under(r"C:\Users\me\OneDrive - Work\a", od));
        assert!(!under(r"C:\dev", ""));
    }

    #[test]
    fn throttle_bursts_then_waits() {
        let start = Instant::now();
        let mut t = Throttle::default();
        for _ in 0..Throttle::BURST {
            assert_eq!(t.admit(start), Ok(()));
        }
        let later = start + Duration::from_millis(30);
        assert_eq!(t.admit(later), Err(Duration::from_millis(70)));
        assert_eq!(t.admit(start + Throttle::WINDOW), Ok(()));
    }

    #[test]
    fn expands_known_variables_only() {
        let env = |n: &str| (n == "A").then(|| "x".to_string());
        assert_eq!(expand_env("%A%\\b", &env), "x\\b");
        assert_eq!(expand_env("%B%\\%A%", &env), "%B%\\x");
        assert_eq!(expand_env("50%", &env), "50%");
    }
}
