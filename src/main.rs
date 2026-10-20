//! gitstat: a tray icon showing whether the git repos in one folder are in
//! sync with their remotes. See README.md.

// No console window for the tray app. `--once` reattaches to the parent console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod git;
#[cfg(any(windows, test))]
mod icon;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod tray;

use std::path::PathBuf;

const USAGE: &str = "\
gitstat: shows whether the git repos in one folder are in sync with their remotes

Usage:
  gitstat                 run the tray app
  gitstat --once          print every repo's status and exit (exit code 1 if any is unsynced)
  gitstat --once --no-fetch
                          same, without running git fetch first
  gitstat --config FILE   use FILE instead of config.json next to gitstat.exe";

struct Args {
    once: bool,
    fetch: bool,
    config_file: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        once: false,
        fetch: true,
        config_file: config::default_file(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--once" => args.once = true,
            "--no-fetch" => args.fetch = false,
            "--config" => args.config_file = iter.next().ok_or("--config needs a file")?.into(),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            #[cfg(windows)]
            attach_parent_console();
            if !message.is_empty() {
                eprintln!("{message}\n");
            }
            println!("{USAGE}");
            std::process::exit(if message.is_empty() { 0 } else { 2 });
        }
    };

    if args.once || cfg!(not(windows)) {
        #[cfg(windows)]
        attach_parent_console();
        std::process::exit(run_once(&args));
    }

    #[cfg(windows)]
    {
        if !single_instance() {
            let _ = error_box(
                "gitstat is already running. Look for its icon in the notification area.".into(),
            )
            .join();
            return;
        }
        app::run(args.config_file);
    }
}

fn run_once(args: &Args) -> i32 {
    let config = match config::Config::load_or_create(&args.config_file) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let dir = config.repos_dir();
    let statuses = git::check_all(&dir, args.fetch);
    if statuses.is_empty() {
        println!("No git repos found in {}", dir.display());
    }
    let width = statuses
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0);
    for s in &statuses {
        println!("{:<width$}  {}", s.name, s.label());
    }
    if statuses.iter().all(|s| s.is_synced()) {
        0
    } else {
        1
    }
}

/// A release build has no console of its own; reuse the terminal it was
/// started from so `--once` and `--help` can print. Only for those: a tray
/// app attached to a terminal dies when that terminal closes.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Holds a named mutex for the life of the process. Returns false if another
/// gitstat already holds it.
#[cfg(windows)]
fn single_instance() -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\gitstat-tray"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    !handle.is_null() && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
}

/// Shows a warning on its own thread. A message box on the main thread would
/// pump GPUI's queued tasks while the app is borrowed, and panic.
#[cfg(windows)]
pub fn error_box(message: String) -> std::thread::JoinHandle<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };
    std::thread::spawn(move || {
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "gitstat".encode_utf16().chain(Some(0)).collect();
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONWARNING | MB_SETFOREGROUND,
            )
        };
    })
}
