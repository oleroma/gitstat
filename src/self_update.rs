//! Self-update. An install never stops or starts gitstat: it puts the new
//! build next to the installed exe as `gitstat.exe.new`. A running gitstat
//! notices it, swaps it in and restarts itself; one that isn't running swaps
//! it in at its next start.
//!
//! Windows won't overwrite or delete a running exe, but it will rename one:
//! the running exe becomes `gitstat.exe.old` and is removed by the next
//! gitstat once the old one has exited.
//!
//! A gitstat running from another folder, such as a build folder, isn't the
//! installed one. An install leaves it `gitstat.exe.handover` holding the
//! installed exe's path; it starts that exe and quits, so the installed
//! build takes over.

use std::io;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::Duration;

/// Given to the restarted gitstat, which waits for the old one to quit
/// instead of reporting it as already running.
pub const AFTER_UPDATE: &str = "--after-update";

/// How often the tray app looks for a new build. A metadata read, so cheap.
#[cfg(windows)]
pub const POLL: Duration = Duration::from_secs(3);

/// How long the restarted gitstat waits for the old one to quit.
#[cfg(windows)]
pub const HANDOVER: Duration = Duration::from_secs(20);

fn with_suffix(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Where an install puts a new build: `gitstat.exe.new`.
pub fn new_build(exe: &Path) -> PathBuf {
    with_suffix(exe, ".new")
}

/// Where the replaced exe goes: `gitstat.exe.old`.
pub fn old_build(exe: &Path) -> PathBuf {
    with_suffix(exe, ".old")
}

/// Where an install asks a gitstat running elsewhere to hand over:
/// `gitstat.exe.handover`, holding the installed exe's path.
pub fn handover_file(exe: &Path) -> PathBuf {
    with_suffix(exe, ".handover")
}

/// The exe an install asked this gitstat to hand over to, if it exists
/// and isn't this one.
pub fn handover(exe: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(handover_file(exe)).ok()?;
    let target = PathBuf::from(text.trim());
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    (target.is_file() && !same(&target, exe)).then_some(target)
}

/// Whether a new build is waiting.
pub fn pending(exe: &Path) -> bool {
    new_build(exe).is_file()
}

/// Moves `<exe>.new` into place and the current exe to `<exe>.old`.
/// `Ok(false)` when there is no new build, or the exe the last update
/// replaced is still running (its gitstat is quitting), so it can't make
/// room yet. On an error the current exe is left (or put back) where it
/// was, and `<exe>.new` stays for a later try.
pub fn swap(exe: &Path) -> io::Result<bool> {
    let new = new_build(exe);
    if !new.is_file() || !remove_old(exe) {
        return Ok(false);
    }
    let old = old_build(exe);
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e);
    }
    Ok(true)
}

/// Removes the exe the last update replaced, once nothing runs it.
/// True when it is gone.
pub fn remove_old(exe: &Path) -> bool {
    let old = old_build(exe);
    !old.exists() || std::fs::remove_file(&old).is_ok()
}

/// The arguments for the restarted gitstat: this one's, plus `AFTER_UPDATE`.
pub fn restart_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let mut out: Vec<_> = args.into_iter().filter(|a| a != AFTER_UPDATE).collect();
    out.push(AFTER_UPDATE.into());
    out
}

/// Starts the swapped-in exe with this process's arguments. The caller quits.
#[cfg(windows)]
pub fn restart(exe: &Path) -> io::Result<()> {
    std::process::Command::new(exe)
        .args(restart_args(std::env::args_os().skip(1)))
        .spawn()
        .map(drop)
}

/// Sends `found` once every `POLL` while a new build or a handover is
/// waiting, and removes the replaced exe once the old gitstat has exited.
#[cfg(windows)]
pub fn watch(exe: PathBuf, found: impl Fn() + Send + 'static) {
    std::thread::Builder::new()
        .name("self-update".into())
        .spawn(move || {
            let mut old_gone = false;
            loop {
                std::thread::sleep(POLL);
                if !old_gone {
                    old_gone = remove_old(&exe);
                }
                if pending(&exe) || handover(&exe).is_some() {
                    found();
                }
            }
        })
        .expect("spawn the self-update thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A fresh folder with `gitstat.exe` holding `text`.
    fn setup(name: &str, text: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gitstat-self-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("gitstat.exe");
        fs::write(&exe, text).unwrap();
        exe
    }

    #[test]
    fn names_sit_next_to_the_exe() {
        let exe = Path::new("C:/Programs/gitstat/gitstat.exe");
        assert_eq!(
            new_build(exe),
            Path::new("C:/Programs/gitstat/gitstat.exe.new")
        );
        assert_eq!(
            old_build(exe),
            Path::new("C:/Programs/gitstat/gitstat.exe.old")
        );
    }

    #[test]
    fn nothing_to_swap() {
        let exe = setup("none", "v1");
        assert!(!pending(&exe));
        assert!(!swap(&exe).unwrap());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v1");
        assert!(!old_build(&exe).exists());
        fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }

    #[test]
    fn swaps_the_new_build_in() {
        let exe = setup("swap", "v1");
        fs::write(new_build(&exe), "v2").unwrap();
        assert!(pending(&exe));
        assert!(swap(&exe).unwrap());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v2");
        assert_eq!(fs::read_to_string(old_build(&exe)).unwrap(), "v1");
        assert!(!pending(&exe));
        assert!(remove_old(&exe));
        assert!(!old_build(&exe).exists());
        fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_leftover_old_exe_is_replaced() {
        let exe = setup("leftover", "v2");
        fs::write(old_build(&exe), "v1").unwrap();
        fs::write(new_build(&exe), "v3").unwrap();
        assert!(swap(&exe).unwrap());
        assert_eq!(fs::read_to_string(&exe).unwrap(), "v3");
        assert_eq!(fs::read_to_string(old_build(&exe)).unwrap(), "v2");
        fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }

    #[test]
    fn hands_over_only_to_another_existing_exe() {
        let exe = setup("handover", "v1");
        let dir = exe.parent().unwrap();
        assert_eq!(handover(&exe), None);
        // Itself.
        fs::write(handover_file(&exe), exe.display().to_string()).unwrap();
        assert_eq!(handover(&exe), None);
        // An exe that isn't there.
        let installed = dir.join("installed.exe");
        fs::write(handover_file(&exe), installed.display().to_string()).unwrap();
        assert_eq!(handover(&exe), None);
        fs::write(&installed, "v2").unwrap();
        fs::write(handover_file(&exe), format!("{}\r\n", installed.display())).unwrap();
        assert_eq!(handover(&exe), Some(installed));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restart_marks_the_handover_once() {
        let args = restart_args(["--config".into(), "c.json".into(), AFTER_UPDATE.into()]);
        assert_eq!(args, ["--config", "c.json", AFTER_UPDATE]);
        assert_eq!(restart_args([]), [AFTER_UPDATE]);
    }
}
