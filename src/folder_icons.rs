//! Status icons on the repo folders in Explorer, through desktop.ini. A
//! ledger in `data_dir` remembers what we changed so it can be undone.
//!
//! [`plan`] decides what to do and is pure; the Windows [`Worker`] carries
//! it out off the UI thread.

use crate::git::RepoStatus;
use crate::icon::{FolderState, ICON_VERSION};
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Apply {
        path: PathBuf,
        state: FolderState,
        tip: String,
    },
    Revert(PathBuf),
    /// Drop the ledger entry without touching the folder.
    Forget(PathBuf),
    Skip(PathBuf, &'static str),
}

/// What to change so the folders match `statuses`. Folders already showing
/// the right icon cost no probing beyond one `ini_present`, and the cheap
/// probes run before the one that spawns git.
///
/// Entries no longer wanted are undone first, so a folder that comes back
/// under another spelling (a new key) is applied to a clean folder.
pub fn plan(ledger: &Ledger, statuses: &[RepoStatus], enabled: bool, probe: &dyn Probe) -> Vec<Op> {
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
            // `git clean -X` removes desktop.ini without telling the ledger.
            if entry.is_some_and(|e| e.state == state && e.tip == tip && probe.ini_present(path)) {
                continue;
            }
            if !probe.eligible(path) {
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

/// desktop.ini text. The shell writes UTF-16 with a BOM, people write ANSI.
fn decode_ini(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
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

    type Job = (Vec<RepoStatus>, bool);
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
                            engine.sync(&job.0, job.1).0
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

        pub fn sync(&self, statuses: Vec<RepoStatus>, enabled: bool) {
            let _ = self.tx.send((statuses, enabled));
        }
    }

    /// `gitstat --remove-folder-icons`: reverts every ledger entry and
    /// returns how many folders were restored.
    pub fn revert_all_blocking(data_dir: &Path) -> Result<usize, String> {
        let data_dir = data_dir.to_path_buf();
        // Its own thread, so COM on the caller's thread stays as it was.
        let joined = std::thread::spawn(move || {
            let _com = Com::init();
            Engine::new(data_dir).sync(&[], false)
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
        notifier: Notifier,
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
                notifier: Notifier::default(),
            }
        }

        /// Returns the folders left alone (or that failed) with the reason,
        /// and how many were reverted.
        fn sync(&mut self, statuses: &[RepoStatus], enabled: bool) -> (Skips, usize) {
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
            let ops = plan(&self.ledger, statuses, enabled, &probe);
            let (mut skips, mut reverted) = (Vec::new(), 0);
            for op in ops {
                match op {
                    Op::Apply { path, state, tip } => {
                        if let Err(e) = self.apply(&path, state, &tip) {
                            skips.push((path, e));
                        }
                    }
                    Op::Revert(path) => match self.revert(&path) {
                        Ok(Reverted::Restored) => reverted += 1,
                        Ok(Reverted::Tracked) => skips.push((path, SKIP_TRACKED.to_string())),
                        Err(e) => skips.push((path, e)),
                    },
                    Op::Forget(path) => {
                        self.ledger.entries.remove(&key(&path));
                        if let Err(e) = self.ledger.save(&self.ledger_file) {
                            skips.push((path, e));
                        }
                    }
                    Op::Skip(path, why) => skips.push((path, why.to_string())),
                }
                self.notifier.pump(false);
            }
            self.notifier.pump(true);
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
                let file = icon_file(&self.data_dir, state);
                let bytes = icon::folder_ico(state);
                if std::fs::read(&file).ok().as_deref() == Some(&bytes[..]) {
                    continue;
                }
                let tmp = file.with_extension("ico.tmp");
                std::fs::write(&tmp, &bytes).map_err(fail)?;
                std::fs::rename(&tmp, &file).map_err(fail)?;
            }
            self.icons_ready = true;
            Ok(())
        }

        fn apply(&mut self, path: &Path, state: FolderState, tip: &str) -> Result<(), String> {
            self.ensure_icons()?;
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
            };
            // Record first, so a crash mid-write still leaves something to revert.
            if prev.is_none() {
                self.ledger.entries.insert(key(path), entry.clone());
                if let Err(e) = self.ledger.save(&self.ledger_file) {
                    self.ledger.entries.remove(&key(path));
                    return Err(e);
                }
            }
            if let Err(e) = write_settings(path, &ini, ini_existed, state, tip, &self.data_dir) {
                let _ = self.revert(path);
                return Err(e);
            }
            self.ledger.entries.insert(key(path), entry);
            self.notifier.push(path);
            self.ledger.save(&self.ledger_file)
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
                self.ledger.entries.remove(&key(path));
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

    /// Points desktop.ini at the state's icon and sets the tip, then marks
    /// the folder so the shell reads it.
    fn write_settings(
        folder: &Path,
        ini: &Path,
        ini_existed: bool,
        state: FolderState,
        tip: &str,
        data_dir: &Path,
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
        let mut icon = wide(icon_file(data_dir, state).as_os_str());
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
            let (skips, _) = engine.sync(std::slice::from_ref(&status), true);
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
                engine.sync(std::slice::from_ref(&status), true),
                (vec![], 0)
            );

            // The ledger lost track (renamed folder, corrupt ledger): our
            // desktop.ini is adopted as ours, so revert still restores all.
            std::fs::remove_file(data.join(LEDGER_FILE)).unwrap();
            let mut engine = Engine::new(data.clone());
            let (skips, _) = engine.sync(std::slice::from_ref(&status), true);
            assert!(skips.is_empty(), "{skips:?}");
            let entry = engine.ledger.get(&repo).unwrap().clone();
            assert!(entry.created_ini && !entry.had_readonly, "{entry:?}");
            drop(engine);

            assert_eq!(revert_all_blocking(&data), Ok(1));
            assert!(!ini.exists());
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
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        ineligible: HashSet<PathBuf>,
        tracked: HashSet<PathBuf>,
        foreign: HashSet<PathBuf>,
        missing: HashSet<PathBuf>,
        no_ini: HashSet<PathBuf>,
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
                },
            );
        }
        ledger
    }

    fn apply(s: &RepoStatus) -> Op {
        Op::Apply {
            path: s.path.clone(),
            state: FolderState::of(s),
            tip: tip(s),
        }
    }

    #[test]
    fn applies_only_on_change() {
        let (a, mut b) = (repo("a"), repo("b"));
        let fake = Fake::default();
        assert_eq!(
            plan(&Ledger::default(), &[a.clone(), b.clone()], true, &fake),
            vec![apply(&a), apply(&b)]
        );

        let ledger = ledger_of(&[a.clone(), b.clone()]);
        fake.calls.borrow_mut().clear();
        assert_eq!(plan(&ledger, &[a.clone(), b.clone()], true, &fake), vec![]);
        assert!(fake.calls.borrow().is_empty(), "up to date: no probing");

        b.ahead = 2;
        assert_eq!(
            plan(&ledger, &[a.clone(), b.clone()], true, &fake),
            vec![apply(&b)]
        );
        // The tip alone changing counts too.
        let mut b3 = b.clone();
        b3.ahead = 3;
        let ledger = ledger_of(&[a.clone(), b.clone()]);
        assert_eq!(
            plan(&ledger, &[a, b3.clone()], true, &fake),
            vec![apply(&b3)]
        );
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
            plan(&ledger, std::slice::from_ref(&a), true, &fake),
            vec![apply(&a)]
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
            plan(&Ledger::default(), &all, true, &fake),
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
            plan(&ledger_of(&all), &changed, true, &fake),
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
            plan(&ledger, std::slice::from_ref(&new), true, &Fake::default()),
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
            plan(&ledger, &[offline.clone()], true, &Fake::default()),
            vec![]
        );

        // A local change still shows while offline.
        offline.dirty = true;
        let mut local = a.clone();
        local.dirty = true;
        assert_eq!(
            plan(&ledger, &[offline], true, &Fake::default()),
            vec![apply(&local)]
        );

        // A real error does change the icon.
        let mut broken = a.clone();
        broken.error = Some("not a git repository".into());
        let ops = plan(&ledger, &[broken], true, &Fake::default());
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
            plan(&ledger, std::slice::from_ref(&a), true, &fake),
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
            plan(&ledger, &[a.clone(), b.clone()], false, &fake),
            vec![Op::Revert(a.path.clone()), Op::Forget(b.path.clone())]
        );
        assert_eq!(plan(&Ledger::default(), &[a, b], false, &fake), vec![]);
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
