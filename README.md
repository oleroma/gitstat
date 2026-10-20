# gitstat

A Windows tray app that watches every git repo in one folder, down to a few
levels below it (a whole development drive works too), and shows at a glance
whether they're in sync with their remotes. It's built with
[GPUI](https://www.gpui.rs/), the UI framework from Zed Industries.

- **Green ✓**: every repo is current.
- **Red with a number**: that many repos are unsynced.
- **Gray …**: the first check is still running.

The icon is drawn at the exact size the notification area uses at your
display scale, with smooth edges and Segoe UI digits, so it stays crisp at
125%, 150% and 200%. Above 99 it shows `99+`.

A repo counts as unsynced if it is ahead, behind, diverged, has uncommitted
changes, has no upstream branch, or its fetch failed. Status texts are `⬆2`,
`⬇1`, `diverged`, `dirty`, `no upstream`, `✓ current`, or the error.

gitstat only fetches. It never pulls, merges or pushes.

## Using it

**Left-click** the tray icon to open the **status panel**, a flyout next to
the taskbar. Unsynced repos come first, worst first; the synced ones are
folded into one "✓ N synced" row that expands on click. Click a repo to open
its folder. ↻ fetches every repo now and ⧉ pops the list out into the status
window. The panel closes when you click elsewhere, press Esc or click the tray
icon again.

**Double-click** the tray icon to open the **status window**, a normal window
with every repo in name order and a **Refresh now** button.

A repo's name is its path below `path`, such as `dev\gitstat`, and lists
everywhere sort by it.

**Right-click** the tray icon for the menu:

- the summary and every repo (click one to open its folder)
- **Open status panel**, **Open status window**
- **Refresh now**
- **Display ▸**: Status panel on click, Status window, Corner badge, Badge
  corner ▸, Hide badge when all synced, Hide badge in fullscreen, Folder icons
  in Explorer
- **Watch for changes**, **Start with Windows**, **Edit config.json**, **Quit**

With the panel turned off, a left-click opens the window instead; with both
off, a left-click shows the menu. The right-click menu is always there.

### Corner badge

**Display ▸ Corner badge** puts a small always-on-top circle in a corner of
the primary monitor's work area, showing the unsynced count (or ✓, ! for a
problem, … while checking). Clicking it opens the panel (or the window, if the
panel is off). It never takes focus, clicks outside the circle go to the
window underneath, and it follows resolution, DPI and taskbar changes within
about 2 seconds.

**Drag it** with the left mouse button to put it anywhere on the primary
monitor; a press that doesn't move is still a click. gitstat remembers the
spot as the nearest corner plus the distance from that corner's edges
(`overlay.corner`, `overlay.offset_x`, `overlay.offset_y`), so the badge keeps
its place relative to that corner when the resolution or the taskbar changes.
Picking a corner from **Badge corner ▸** puts it back `margin` from the edges. By default it hides while a fullscreen app or a presentation
is running; it can also hide while everything is synced.

### Folder icons in Explorer

**Display ▸ Folder icons in Explorer** gives each repo folder an icon that
shows its status (✓ synced, ⬆ ahead, ⬇ behind, ⬆⬇ diverged, ● dirty, – no
upstream, ! error) and a tooltip such as `gitstat: ⬆2, uncommitted changes`.
gitstat asks before turning it on.

What it changes in each repo folder:

- writes a hidden, system `desktop.ini` with `IconResource` and `InfoTip`
  pointing at icons in `%LOCALAPPDATA%\gitstat\icons`;
- sets the folder's read-only (or system) attribute, which is how Windows
  turns on custom folder icons;
- adds two lines to the repo's `.git/info/exclude`, `# gitstat folder icon`
  and `/desktop.ini`, so `git status` stays clean. These lines stay after the
  icons are removed; they are harmless.

It skips folders on network or removable drives, folders inside OneDrive,
repos that track a `desktop.ini`, and folders that already have a
`desktop.ini` of their own, which is never touched. The panel's footer shows
how many folders were skipped. The icon replaces the folder icon rather than
adding a small overlay, and icons don't update while gitstat isn't running.

To undo: untick the option, or quit gitstat and run
`gitstat --remove-folder-icons`, which also turns the option off in
config.json. Either restores every folder gitstat changed, from the list it
keeps in `%LOCALAPPDATA%\gitstat\folder-icons.json`.

The icons show in Explorer in every view and in the Files app. File Pilot is
**unverified**: it uses the shell's icons, so it should show them, possibly
only after F5.

## Which repos

gitstat looks in `path` and up to `depth` folder levels below it (3 by
default; 1 is just the direct subfolders). A folder holding `.git` is a repo,
and gitstat doesn't look inside it, so nested repos and submodules aren't
listed separately. If `path` is itself a repo, it is the only one.

It never looks inside folders named `node_modules`, `target`, `build`,
`dist`, `out`, `venv`, `__pycache__`, `vendor`, `bin` or `obj`, folders whose
name starts with `.`, `$Recycle.Bin` and `System Volume Information`, hidden
or system folders, symlinks, junctions and other reparse points such as
OneDrive or other cloud-synced folders, or folders it can't read. (A folder
with one of those names, or of those kinds, that holds `.git` is still a
repo, so repos directly inside `path` are found even under OneDrive; repos in
a group folder below it are not.) A link straight to a repo is listed, unless
the repo itself is found too.

It stops after looking at 50,000 folders or finding 500 repos. The menu and
the panel then show the count with a `+` (for example `500+ repos`), and the
panel's footer says which limit was hit; lower `depth` or point `path` at a
smaller folder. While the list is cut short, only the sweeps (and a removed
repo) make gitstat look again: new folders can't bring in more repos.

## How status stays current

**Local changes** (an edit, a commit, a checkout, a push, a new or removed
repo) are picked up from file-system events, usually within a second of the
last write. gitstat ignores what git ignores (so `cargo build` or
`npm install` churn costs nothing after the first check), and outside the
repos it only notices folders appearing or going within `depth` levels, so
the rest of a watched drive costs next to nothing. The search for new repos
runs in the background, within about 5 seconds of a folder change even while
the disk stays busy, and is spaced out so that on a big folder it is never
searching more than a tenth of the time. It never checks the
same repo more than once every 2 seconds, and doesn't wake up at all while
nothing changes. Its own `git status` runs with `GIT_OPTIONAL_LOCKS=0`, so it
never rewrites `.git/index` or collides with your `index.lock`.

**Remote changes** can't be seen locally, so every `interval_minutes` gitstat
runs `git fetch --all --prune` in each repo (about 15 seconds after the PC
wakes from sleep, on **Refresh now**, and with a 1, 2, then 5 minute backoff
while a fetch keeps failing). Each sweep also rechecks local state, which
covers network drives and `\\wsl$`, where events are unreliable.

If the watch fails, the panel's footer says *Polling* and gitstat checks every
repo once a minute until it can watch again. Untick **Watch for changes** (or
set `"watch": false`) to check on the interval only, as before v0.3.

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

`config.json` lives next to `gitstat.exe`. Every key is optional, and a file
from an older version keeps working:

```json
{
  "path": "D:/dev",
  "depth": 3,
  "interval_minutes": 5.0,
  "watch": true,
  "panel": true,
  "window": true,
  "overlay": {
    "enabled": false,
    "corner": "bottom-right",
    "size": 44,
    "margin": 16,
    "hide_when_synced": false,
    "hide_in_fullscreen": true
  },
  "folder_icons": false
}
```

| key | default | meaning |
|---|---|---|
| `path` | the folder above gitstat's own | The folder your repos are in, at any level down to `depth`; a whole drive such as `D:/` works. `~` and variables like `%USERPROFILE%` are expanded. Use forward slashes or doubled backslashes. |
| `depth` | 3 | How many folder levels below `path` to look for repos, 1 to 8. 1 is only `path`'s direct subfolders, which is all earlier versions looked at: set it to keep an older config's repo list as it was, with two exceptions. A `path` that is itself a repo now shows only that repo, not the repos inside it, and a link to a repo that is listed anyway is dropped. See [Which repos](#which-repos). |
| `interval_minutes` | 5 | How often to fetch, 1 to 1440. |
| `watch` | true | Pick up local changes from file-system events. false checks on the interval only. |
| `panel` | true | A tray left-click (and a badge click) opens the status panel. |
| `window` | true | The status window is available from the menu, a double-click and the panel's ⧉ button. |
| `overlay.enabled` | false | Show the corner badge. |
| `overlay.corner` | `bottom-right` | `top-left`, `top-right`, `bottom-left` or `bottom-right`. |
| `overlay.offset_x`, `overlay.offset_y` | unset | Where the badge was dragged to: px at 100% scale from the left or right, and the top or bottom, edge of the corner. Unset is `margin`. Written by dragging, removed by picking a corner from the menu. |
| `overlay.size` | 44 | Diameter in px at 100% scale, 24 to 128. |
| `overlay.margin` | 16 | Distance from the screen edges in px at 100% scale, 0 to 200, unless the badge was dragged. |
| `overlay.hide_when_synced` | false | Hide the badge while every repo is synced. |
| `overlay.hide_in_fullscreen` | true | Hide the badge while a fullscreen app or a presentation runs. |
| `folder_icons` | false | Status icons on the repo folders, see above. Setting it by hand skips the confirmation. |

Edits apply within about a second, without a restart. **Edit config.json** in
the menu or the panel opens the file. The menu's toggles write the file too;
if it doesn't parse (say, an edit in progress), they leave it alone and the
change lasts until gitstat quits.

Fetches never prompt for credentials. A repo whose remote needs a password
shows `fetch failed`, so use SSH keys or Git Credential Manager for those.

## Command line

```
gitstat --once                  print every repo's status and exit
gitstat --once --no-fetch       same, without fetching first
gitstat --config FILE           use another config file
gitstat --remove-folder-icons   turn folder icons off, restore every folder that got one, and exit
```

`--once` exits with code 1 if any repo is unsynced. `--remove-folder-icons`
exits with code 1 if some folder couldn't be restored (it lists them, and they
stay on the list for the next try), and refuses to run while the tray app is
running. gitstat.exe is a windowed app, so run it from PowerShell as
`.\gitstat.exe --once | Out-Host` to have the terminal wait for its output.

## Build from source

You need [Rust](https://rustup.rs/) and the Visual Studio C++ build tools with
a Windows SDK. GPUI compiles its shaders with the SDK's `fxc.exe`.

```
cargo build --release
```

The executable is `target\release\gitstat.exe`. To run your own build, copy it
into `builds\` (ignored by git) along with a `config.json` whose `path` points
at your repos folder, since a new config defaults to the folder above the
exe's folder.

`cargo test` runs the status detection, file-watching, layout, config and
folder-icon tests. They also run on Linux (CI runs them there too), where only
`--once` is available.

## How it works

- `src/git.rs` finds the repos (a breadth-first walk down to `depth`) and
  runs `git fetch` and `git status --porcelain=v2 --branch` in each one, with
  no console windows and no optional locks.
- `src/monitor.rs` is the scheduler thread: one recursive watch on the root
  ([notify](https://crates.io/crates/notify)), the fetch sweep, and up to six
  checks at a time. `src/watch.rs` decides which events matter and debounces
  them.
- `src/app.rs` is the GPUI app: startup, and one loop that handles tray,
  monitor and badge input in batches. It wakes only on input.
- `src/view.rs`, `src/panel.rs` and `src/overlay.rs` are the status window,
  the flyout panel and the corner badge; `src/layout.rs` and `src/win.rs`
  place them.
- `src/tray.rs` owns the notification-area icon and menu
  ([tray-icon](https://crates.io/crates/tray-icon)). `src/icon.rs` draws the
  tray and folder icons, anti-aliased at their exact pixel sizes (the tray's
  digits with [ab_glyph](https://crates.io/crates/ab_glyph) from Segoe UI,
  or a built-in stroke font without it), and `src/folder_icons.rs` applies
  and reverts the desktop.ini icons.
- `src/config.rs` reads and writes `config.json`. `src/autostart.rs` adds or
  removes gitstat under
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
