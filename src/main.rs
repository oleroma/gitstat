//! gitstat: a tray icon showing whether the git repos in one folder are in
//! sync with their remotes. See README.md.

// No console window for the tray app. `--once` reattaches to the parent console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod git;
mod repo_config;

// Platform-neutral, but only the tray app uses them; tests cover them on Linux.
#[cfg(any(windows, test))]
mod folder_icons;
#[cfg(any(windows, test))]
mod github;
#[cfg(any(windows, test))]
mod graph;
#[cfg(any(windows, test))]
mod history;
#[cfg(any(windows, test))]
mod icon;
#[cfg(any(windows, test))]
mod layout;
#[cfg(any(windows, test))]
mod monitor;
#[cfg(any(windows, test))]
mod repo_graph;
#[cfg(any(windows, test))]
mod repo_icon;
#[cfg(any(windows, test))]
mod repo_info;
#[cfg(any(windows, test))]
mod self_update;
#[cfg(any(windows, test))]
mod state;
#[cfg(any(windows, test))]
mod watch;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod morph;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod panel;
#[cfg(windows)]
mod repo_marks;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod timeline;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod turned_text;
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
    /// Started by a gitstat that just swapped in a new build (self_update.rs).
    after_update: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        once: false,
        remove_folder_icons: false,
        fetch: true,
        config_file: config::default_file(),
        after_update: false,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--once" => args.once = true,
            "--no-fetch" => args.fetch = false,
            "--remove-folder-icons" => args.remove_folder_icons = true,
            "--after-update" => args.after_update = true,
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
        // A build an install left while gitstat wasn't running.
        if let Ok(exe) = std::env::current_exe()
            && self_update::swap(&exe).unwrap_or(false)
            && self_update::restart(&exe).is_ok()
        {
            return;
        }
        let first = if args.after_update {
            wait_for_single_instance(self_update::HANDOVER)
        } else {
            single_instance()
        };
        if !first
            && !(ask_restart()
                && stop_other_gitstats()
                && wait_for_single_instance(self_update::HANDOVER))
        {
            return;
        }
        log_panics();
        app::run(args.config_file);
    }
}

/// Appends every panic to `%LOCALAPPDATA%\gitstat\crash.log` before the
/// default hook runs. The tray app has no console, and a panic in a GPUI
/// window callback can't unwind into Windows, so it ends the process with
/// nothing to say why.
#[cfg(windows)]
fn log_panics() {
    use std::io::Write;
    /// The log is started afresh past this size.
    const MAX_LOG: u64 = 256 * 1024;
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = config::data_dir();
        let file = dir.join("crash.log");
        let fresh = std::fs::metadata(&file).is_ok_and(|m| m.len() > MAX_LOG);
        let _ = std::fs::create_dir_all(&dir);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(!fresh)
            .truncate(fresh)
            .open(&file);
        if let Ok(mut log) = log {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let thread = std::thread::current();
            let _ = writeln!(
                log,
                "--- {} (unix {secs}), thread {}\n{info}\n{}",
                env!("CARGO_PKG_VERSION"),
                thread.name().unwrap_or("unnamed"),
                std::backtrace::Backtrace::force_capture()
            );
        }
        default(info);
    }));
}

fn run_once(args: &Args) -> i32 {
    let config = match config::Config::load_or_create(&args.config_file) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let roots = config.roots();
    let mut statuses = Vec::new();
    for dir in &roots {
        let found = git::find_repos(dir, config.depth());
        if let Some(capped) = &found.capped {
            eprintln!(
                "The repo list of {} is incomplete: {capped}.",
                dir.display()
            );
        }
        statuses.extend(git::check_all(
            &config.name_base(dir),
            &found.repos,
            args.fetch,
        ));
    }
    if statuses.is_empty() {
        let dirs: Vec<String> = roots.iter().map(|d| d.display().to_string()).collect();
        println!(
            "No git repos found in {} (depth {})",
            dirs.join(", "),
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
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\gitstat-tray"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return false;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // Not ours to keep: an open handle would keep the mutex alive after
        // the other gitstat quits, so a retry could never get it.
        unsafe { CloseHandle(handle) };
        return false;
    }
    true
}

/// `single_instance`, retried until the gitstat that started this one after
/// an update has quit, or `timeout` passes.
#[cfg(windows)]
fn wait_for_single_instance(timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if single_instance() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Says gitstat is already running and asks whether to restart it: quit the
/// running one and go on with this one. True for Restart.
#[cfg(windows)]
fn ask_restart() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONINFORMATION, MB_SETFOREGROUND, MB_YESNO, MessageBoxW,
    };
    let text: Vec<u16> = "gitstat is already running. Look for its icon in the notification \
         area.\n\nRestart it? Yes quits the running gitstat and starts this one."
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let title: Vec<u16> = "gitstat".encode_utf16().chain(Some(0)).collect();
    let answer = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONINFORMATION | MB_SETFOREGROUND,
        )
    };
    answer == IDYES
}

/// Ends every other gitstat.exe process and waits for each to be gone.
/// False when one couldn't be ended, with a warning saying so.
#[cfg(windows)]
fn stop_other_gitstats() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
        WaitForSingleObject,
    };
    let me = unsafe { GetCurrentProcessId() };
    let mut others = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snap, &mut entry) != 0;
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case("gitstat.exe") && entry.th32ProcessID != me {
                others.push(entry.th32ProcessID);
            }
            more = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
    }
    let mut stopped = true;
    for pid in others {
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
            if process.is_null() {
                stopped = false;
                continue;
            }
            if TerminateProcess(process, 0) != 0 {
                WaitForSingleObject(process, 5000);
            } else {
                stopped = false;
            }
            CloseHandle(process);
        }
    }
    if !stopped {
        let _ = error_box(
            "gitstat couldn't quit the gitstat that is running. Quit it from its \
             tray menu, then start gitstat again."
                .into(),
        )
        .join();
    }
    stopped
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
