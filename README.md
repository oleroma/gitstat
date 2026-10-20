# gitstat

A Windows tray app that watches every git repo directly inside one folder and
shows at a glance whether they're in sync with their remotes. It's built
with [GPUI](https://www.gpui.rs/), the UI framework from Zed Industries.

- **Green ✓**: every repo is current.
- **Red with a number**: that many repos are unsynced.
- **Gray …**: the first check is still running.

**Left-click** the icon to open the status window. It lists every repo with its
status (`⬆2`, `⬇1`, `diverged`, `dirty`, `no upstream`, `✓ current`, or a fetch
error) and has a **Refresh now** button. Click a repo to open its folder.

**Right-click** the icon for the same list as a menu, plus **Show status
window**, **Refresh now**, **Start with Windows** and **Quit**.

Every `interval_minutes`, gitstat runs `git fetch` in each repo. It only
fetches; it never pulls, merges or pushes. A repo counts as unsynced if it is
ahead, behind, diverged, has uncommitted changes, has no upstream branch, or
its fetch failed.

## Install

1. Download `gitstat-windows-x64` from the latest successful
   [build](../../actions/workflows/build.yml) and unzip it. Or build it
   yourself, see below.
2. Put `gitstat.exe` in a folder of its own, e.g. `D:\dev\gitstat\`.
3. Run it. The first time, it writes `config.json` next to itself.
4. Right-click the tray icon and tick **Start with Windows** to have it start
   on every login.

git must be on your `PATH` (Git for Windows puts it there).

## Configure

`config.json` lives next to `gitstat.exe`:

```json
{
  "path": "D:/dev",
  "interval_minutes": 5.0
}
```

`path` is the parent folder whose direct subfolders are your repos. On first
run it's set to the folder that holds gitstat's own folder, so
`D:\dev\gitstat\gitstat.exe` watches `D:\dev`. `~` and variables like
`%USERPROFILE%` are expanded. Use forward slashes or doubled backslashes.

gitstat re-reads the file before every check, so edits apply on the next
refresh without a restart. **Edit config.json** in the status window opens it.

Fetches never prompt for credentials. A repo whose remote needs a password
shows `fetch failed`, so use SSH keys or Git Credential Manager for those.

## Command line

```
gitstat --once              print every repo's status and exit
gitstat --once --no-fetch   same, without fetching first
gitstat --config FILE       use another config file
```

`--once` exits with code 1 if any repo is unsynced. gitstat.exe is a
windowed app, so run it from PowerShell as `.\gitstat.exe --once | Out-Host`
to have the terminal wait for its output.

## Build from source

You need [Rust](https://rustup.rs/) and the Visual Studio C++ build tools with
a Windows SDK. GPUI compiles its shaders with the SDK's `fxc.exe`.

```
cargo build --release
```

The executable is `target\release\gitstat.exe`. `cargo test` runs the status
detection tests. They also run on macOS and Linux, where only `--once` is
available.

## How it works

- `src/git.rs` runs `git fetch --all --prune`, `git status --porcelain` and
  `git rev-list --left-right --count HEAD...@{upstream}` in each repo, six
  repos at a time, with no console windows.
- `src/tray.rs` owns the notification-area icon and menu
  ([tray-icon](https://crates.io/crates/tray-icon)). `src/icon.rs` draws the
  icon.
- `src/app.rs` is the GPUI app: shared state, the refresh timer, and the
  status window.
- `src/autostart.rs` adds or removes gitstat under
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
