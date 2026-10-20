//! gitstat: a tray icon showing whether the git repos in one folder are in
//! sync with their remotes. See README.md.

// No console window for the tray app. `--once` reattaches to the parent console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod git;

// Platform-neutral, but only the tray app uses them; tests cover them on Linux.
#[cfg(any(windows, test))]
mod folder_icons;
#[cfg(any(windows, test))]
mod icon;
#[cfg(any(windows, test))]
mod layout;
#[cfg(any(windows, test))]
mod monitor;
#[cfg(any(windows, test))]
mod state;
#[cfg(any(windows, test))]
mod watch;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod panel;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod view;
#[cfg(windows)]
mod win;

use std::path::PathBuf;

const USAGE: &str = "\
gitstat: shows whether the git repos in one folder are in sync with their remotes

Usage:
  gitstat                 run the tray app
  gitstat --once          print every repo's status and exit (exit code 1 if any is unsynced)
  gitstat --once --no-fetch
                          same, without running git fetch first
  gitstat --config FILE   use FILE instead of config.json next to gitstat.exe
  gitstat --remove-folder-icons
                          turn folder icons off in the config, restore every repo
                          folder gitstat gave a status icon, and exit";

struct Args {
    once: bool,
    remove_folder_icons: bool,
    fetch: bool,
    config_file: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        once: false,
        remove_folder_icons: false,
        fetch: true,
        config_file: config::default_file(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--once" => args.once = true,
            "--no-fetch" => args.fetch = false,
            "--remove-folder-icons" => args.remove_folder_icons = true,
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

    if args.remove_folder_icons {
        #[cfg(windows)]
        {
            attach_parent_console();
            std::process::exit(remove_folder_icons(&args.config_file));
        }
        #[cfg(not(windows))]
        {
            eprintln!("--remove-folder-icons is only available on Windows");
            std::process::exit(2);
        }
    }

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
    let found = git::find_repos(&dir, config.depth());
    if let Some(capped) = &found.capped {
        eprintln!("The repo list is incomplete: {capped}.");
    }
    let statuses = git::check_all(&dir, &found.repos, args.fetch);
    if statuses.is_empty() {
        println!(
            "No git repos found in {} (depth {})",
            dir.display(),
            config.depth()
        );
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

/// `--remove-folder-icons`: turns `folder_icons` off in config.json, so
/// the next tray start doesn't put the icons back, then reverts every folder
/// in the ledger. Folders that can't be restored stay in the ledger for the
/// next try (or for the tray app, which reverts them with the option off).
#[cfg(windows)]
fn remove_folder_icons(config_file: &std::path::Path) -> i32 {
    // A running tray app would put the icons straight back.
    if !single_instance() {
        eprintln!("gitstat is running. Quit it from its tray menu first.");
        return 2;
    }
    // Only an existing file: load_or_create would write a new one.
    if config_file.exists() {
        let saved = config::Config::load_or_create(config_file).and_then(|mut config| {
            if !config.folder_icons {
                return Ok(false);
            }
            config.folder_icons = false;
            config.save(config_file).map(|()| true)
        });
        match saved {
            Ok(true) => println!("Turned folder_icons off in {}.", config_file.display()),
            Ok(false) => {}
            Err(e) => eprintln!(
                "Couldn't turn folder_icons off; set it to false in config.json, or gitstat \
                 will restore the icons at its next start.\n{e}"
            ),
        }
    }
    match folder_icons::revert_all_blocking(&config::data_dir()) {
        Ok(n) => {
            println!("Restored {n} folder(s).");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
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
