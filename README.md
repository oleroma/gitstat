<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./media/logo_light.svg">
    <source media="(prefers-color-scheme: light)" srcset="./media/logo_dark.svg">
    <!-- The fallback image for older browsers -->
    <img alt="gitstat" src="./media/logo_light.svg" width="200">
  </picture>
  <br>
</h1>

# gitstat

A Windows tray app that watches every git repo in one folder, down to a few
levels below it (a whole development drive works too), and shows at a glance
whether they're in sync with their remotes. It's built with
[GPUI](https://www.gpui.rs/), the UI framework from Zed Industries.

- **Green ✓**: every repo is current.
- **Red with a number**: that many repos are unsynced.
- **Gray …**: the first check is still running.

**Settings ▸ Icon and badge ▸ Number on the icon and badge** picks what the number counts: unsynced repos
(the default), repos with uncommitted changes, repos to push, repos to pull,
open pull requests, pull requests needing you (a review asked of you, failing
checks, conflicts or requested changes), or reviews asked of you. The colour
still follows sync: red while any repo is unsynced, green with the number
once all are current, and the ✓ when that number is 0 too. The tooltip says
what the number counts.

**Settings ▸ Status views ▸ Sort repos by** sets the order of the repos, the same in the
panel and the big view: name (the default), status (worst first), last
commit on any branch, last worked on (where HEAD last moved, from the
reflog), last pull request activity, most open pull requests (the ones
needing you count first), or most hanging branches. The panel keeps the
unsynced repos above the synced ones and sorts within each group; the
GitHub-only repos stay at the end, sorted the same way.

The icon is drawn at the exact size the notification area uses at your
display scale, with smooth edges and Segoe UI digits, so it stays crisp at
125%, 150% and 200%. Above 99 it shows `99+`.

A repo counts as unsynced if it is ahead, behind, diverged, has uncommitted
changes, has no upstream branch, its fetch failed, a rebase, merge,
cherry-pick, revert or bisect is half done, or another of its local branches
has commits no remote has (ahead of its upstream, or never pushed; a branch
whose upstream was deleted, as after a merged pull request, doesn't count).
Status texts are `⬆2`, `⬇1`, `diverged`, `dirty`, `no upstream`,
`detached at 1a2b3c4`, `rebasing 3/7`, `merging`, `cherry-picking`,
`reverting`, `bisecting`, `wip unpushed` or `3 branches unpushed`,
`✓ current`, or the error. Stashes are shown (`2 stashed`) but don't make a
repo unsynced. Hover a repo for the details: the unpushed branches by name,
and the last lines git printed when a fetch failed.

gitstat only fetches. It never pulls, merges or pushes.

## Using it

**Left-click** the tray icon to open the **status panel**, a flyout next to
the taskbar. Unsynced repos come first, worst first; the synced ones are
folded into one "✓ N synced" row that expands on click. A repo that needs
something has an edge in its status colour, and the header sums them up: a
thin bar and counts of synced (✓), local-work (●) and needs-a-look (!)
repos. Click a repo to open it in the status window, scrolled to its column
(or its folder, with the window turned off). ↻ fetches every repo now, ⧉
turns the panel into the status window and ⚙ opens the settings. The panel
closes when you click elsewhere, press Esc or click the tray icon again.

**Double-click** the tray icon to open the **status window**, a normal window
with every repo and a **Refresh now** button. Opening it from the panel
turns the panel into it (**Settings ▸ Status views ▸ Flip animation**, at
the **Flip speed** picked there; **Very slow** takes 5 seconds, for looking
at each frame). Closing or minimising it turns it back into the panel.

**Type** in the panel or the window to filter the repos: only those whose
name or branch holds what you typed are listed, with every section open.
Backspace takes a letter off, Enter opens the first repo listed, and Esc
clears the filter (a second Esc closes).

**Right-click** a repo, in the panel or the window, for its menu: **Open
folder**, **Open terminal here** (Windows Terminal, or PowerShell without
it), **Open in VS Code** (when `code` is on the `PATH`), **Open on GitHub**,
**Copy branch name**, **Copy path**, **Fetch now** (also the way to retry a
failed fetch) and **Hide this repo**, which adds it to `ignore`. A GitHub
repo with no clone here offers only its page and hiding.

In the window each repo is a column. Its header shows the name and checkout,
then one row that sums the repo up: a small bar and coloured counts of its
branches (local and remote copies of a branch count once, the default branch
not at all), its worktrees and how long ago the last merge was. Purple
branches are **merged** into the default branch, by git or through a merged
GitHub pull request, which also catches squash merges. Green ones are **in
review**, with an open or draft pull request. Amber ones are **hanging**: not
merged and with no open pull request, so work that didn't make it or hasn't
yet. Click the row to open the details, which list the hanging and in-review
branches by name with their age, the worktrees and the last merge; click
again to fold them.

Below the header runs the repo's history, newest at the top. The wheel
over it scrolls it; anywhere else in the window, column headers included, the
wheel scrolls the table sideways. A double-click goes back to the top. With
**Settings ▸ Status views ▸ Scroll columns together** (on by default) every column scrolls with it, row
for row like the panel's strips, and thin lines between the rows line them
up across the table; a column with fewer commits stops at its last. Hover a
commit for its card, which shows the commit's whole message.

A repo's name is its path below `path`, such as `dev\gitstat`, and lists
everywhere sort by it.

**Right-click** the tray icon for the menu:

- the summary and every repo (click one to open its folder)
- **Open status panel**, **Open status window**
- **Refresh now**
- **Settings…**, which opens the settings window: General (start with
  Windows, the folders to watch, folder depth, watching for changes, how
  often to fetch), Status views, Icon and badge (notifications, the corner
  badge, the number on the icon), Folder icons and GitHub
- **Quit**

With the panel turned off, a left-click opens the window instead; with both
off, a left-click shows the menu. The right-click menu is always there.

### Notifications

**Settings ▸ Icon and badge ▸ Notifications** can have Windows tell you when
a pull request in your repos asks for your review, when one is opened, when
an open pull request's checks fail, when a fetch finds a repo behind its
remote, when a fetch brings in a branch that wasn't there, or when it brings
in new commits on any remote branch (with their count; your own pushes
don't count). Each is off until you turn it on. They come from the tray icon, several changes
at once as one notification, and Windows holds them back in Do not disturb.
Starting gitstat says nothing about what was already so.

### Corner badge

**Settings ▸ Icon and badge ▸ Show the corner badge** puts a small always-on-top circle in a corner of
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
Picking a **Corner** in the settings puts it back `margin` from the edges. By default it hides while a fullscreen app or a presentation
is running; it can also hide while everything is synced.

### Folder icons in Explorer

**Settings ▸ Folder icons** gives each repo folder an icon that
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

#### Repo icons

The **Repo icon** style (`"folder_icon_mode": "repo-svg"`)
shows a repo's own picture instead, for every repo that has a
`.gitstat/icon.svg` (that exact name; any case on Windows). Repos without
one keep the status folder. The picture takes one of three colours:

- **green**: synced;
- **amber**: work that is only local: uncommitted changes, unpushed commits,
  or no upstream;
- **red**: needs you: behind, diverged, or an error.

The same picture, in the same colour, sits beside the repo's name in the
status panel and the window, whatever the folder icon style.

Every fill or stroke that is `currentColor` gets that colour, and every other
colour stays as drawn. A file that never mentions `currentColor` is drawn as
a silhouette instead: its shape, all in that colour. For example, this
icon.svg is a coloured disc with a white check mark:

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">
  <circle cx="8" cy="8" r="8" fill="currentColor"/>
  <path d="M4 8.5l2.5 2.5L12 5.5" fill="none" stroke="#fff" stroke-width="2"/>
</svg>
```

The picture is scaled to fill the icon square, keeping its proportions,
centred. gitstat draws it into `%LOCALAPPDATA%\gitstat\icons` (never into
the repo) whenever the file, the repo's colour or the style changes; saving
the file updates the folder within a second or so, and each sweep has
another look too. Old drawings are deleted once no folder uses them.

The file comes from the repo, so gitstat treats it with care: files over
1 MiB, links, DTD entities, deep nesting, filters, references that loop and
more than 10,000 shapes are refused, counting every copy that `<use>`,
patterns, gradients, clip paths, masks and markers make. Nothing it refers
to is loaded (other files, web addresses, embedded images, fonts: text isn't
drawn), and like any viewer gitstat shows only what is inside the `viewBox`.
If the file can't be used, the folder shows the status icon and the panel
counts it as skipped until the file changes. If it can't be read, or takes
more than 10 seconds to draw, gitstat tries again on a later check.

### GitHub pull requests

**Settings ▸ GitHub ▸ Pull requests** (`"github_prs": true`, on by default)
lists the open pull requests of every repo whose `origin` is on GitHub at the
bottom of the panel: review requests for you first, then ones with failing
checks, conflicts or requested changes, then the rest. Each row shows the
repo's icon, the number and title, and its checks, review and merge state;
hover for the branch and author, click to open it in the browser.

gitstat reads them through the [GitHub CLI](https://cli.github.com/), so
`gh` must be on the `PATH` and signed in (`gh auth login`); gitstat stores no
token. It asks again every `interval_minutes` and on the refresh button, in
one GraphQL query per 20 repos. Like everything in gitstat this only reads:
nothing on GitHub or in the repos is ever changed.

### GitHub repos not cloned here

**Settings ▸ GitHub ▸ Repos not cloned here** (`"github_repos": true`, on by
default) adds every repo of the account `gh` is signed in to, and of its
organisations, that has no clone under the watched folders (archived repos are
left out; a clone counts when its `origin` points at the repo). They come
after the cloned repos: in the panel behind a "◌ N on GitHub only" row that
folds like the synced one, in the window as the last columns. Their trees
come from GitHub: the 30 most recently committed branches, 100 commits back
from each, read again when the repo is pushed to. Being remote only, they
have no dirty, unpushed or worktree marks, and never count as unsynced.
Clicking one opens it on GitHub.

The list is one more read-only GraphQL query per 100 repos every
`interval_minutes`, plus one per repo for its tree when it is first shown or
pushed to. gitstat never clones anything.

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

Repos named in `ignore` are left out altogether: never checked, fetched or
listed. **Hide this repo** in a repo's right-click menu adds its name. A
pattern is a name as gitstat shows it, such as `dev\gitstat\build\src`, or
`owner/name` for a GitHub repo with no clone here; `*` stands for any run of
characters (`*\build\src` hides every `build\src` worktree), `/` and `\`
are the same, and case doesn't matter.

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

While **GitHub pull requests** is on, the same poll reads when GitHub last
saw a push to each repo whose `origin` is on GitHub. A sweep then fetches
only the repos pushed to since their last fetch, and gives the others a
local check; a repo the poll finds newly pushed to is fetched right away,
without waiting for the sweep. Every sixth sweep, the first one and
**Refresh now** still fetch every repo, which also covers other remotes
and deleted branches.

If the watch fails, the panel's footer says *Polling* and gitstat checks every
repo once a minute until it can watch again. Untick **Watch for changes** (or
set `"watch": false`) to check on the interval only, as before v0.3.

## Install

git must be on your `PATH` (Git for Windows puts it there).

Every build is published as a pre-release on its `build/<n>` tag, and the
newest build of `main` as the **Development build** release; its
`gitstat.exe` runs from any folder of its own. The first time, it writes
`config.json` next to itself; tick **Start with Windows** in the settings
to have it start on every login. To build it yourself, see
[Build from source](#build-from-source).

Each build on the PC installs itself to `%LOCALAPPDATA%\Programs\gitstat`
with a **gitstat** shortcut in the Start menu. The install never stops or
starts gitstat: it leaves the new build next to the installed one as
`gitstat.exe.new`. A running gitstat notices it within a few seconds, swaps
it in and restarts itself; one that isn't running switches at its next start.
A gitstat running from anywhere else, such as `build\gitstat.exe`, is handed
`gitstat.exe.handover` next to its exe: it starts the installed build and
quits, so the installed one takes over. Starting gitstat while one already
runs asks whether to restart it: **Yes** quits the running one and goes on
with the new start, so an older gitstat that doesn't hand over can be
replaced from the Start menu in one step.
The replaced exe is kept as `gitstat.exe.old` until the new gitstat removes
it. `config.json` there is kept between installs; the first install copies
`build\config.json`. If **Start with Windows** is on, it is pointed at the
installed exe.

## Configure

`config.json` lives next to `gitstat.exe`. Every key is optional, and a file
from an older version keeps working:

```json
{
  "path": "D:/dev",
  "more_paths": [],
  "depth": 3,
  "interval_minutes": 5.0,
  "watch": true,
  "panel": true,
  "window": true,
  "icons_only": false,
  "overlay": {
    "enabled": false,
    "corner": "bottom-right",
    "size": 44,
    "margin": 16,
    "hide_when_synced": false,
    "hide_in_fullscreen": true
  },
  "folder_icons": false,
  "folder_icon_mode": "status",
  "github_prs": true,
  "github_repos": true,
  "repo_click": "folder",
  "badge_number": "unsynced",
  "sort": "name",
  "scroll_together": true,
  "flip_animation": true,
  "flip_ms": 560,
  "panel_slide_ms": 150,
  "ignore": [],
  "notify": {
    "review_requested": false,
    "new_pr": false,
    "checks_failed": false,
    "behind": false,
    "new_branch": false,
    "new_commits": false
  }
}
```

| key | default | meaning |
|---|---|---|
| `path` | the folder above gitstat's own | The folder your repos are in, at any level down to `depth`; a whole drive such as `D:/` works. `~` and variables like `%USERPROFILE%` are expanded. Use forward slashes or doubled backslashes. |
| `more_paths` | `[]` | More folders to watch besides `path`, each down to `depth`, such as `["E:/work", "~/src"]`. A folder inside or around one already listed is skipped. With more than one folder, repo names start with their folder's name (`dev\gitstat`, `work\api`). |
| `depth` | 3 | How many folder levels below `path` to look for repos, 1 to 8. 1 is only `path`'s direct subfolders, which is all earlier versions looked at: set it to keep an older config's repo list as it was, with two exceptions. A `path` that is itself a repo now shows only that repo, not the repos inside it, and a link to a repo that is listed anyway is dropped. See [Which repos](#which-repos). |
| `interval_minutes` | 5 | How often to fetch, 1 to 1440. |
| `watch` | true | Pick up local changes from file-system events. false checks on the interval only. |
| `panel` | true | A tray left-click (and a badge click) opens the status panel. |
| `window` | true | The status window is available from the menu, a double-click and the panel's ⧉ button. |
| `icons_only` | false | **Settings ▸ Status views ▸ Icons only**: the panel and the badge show pictures, no words. Each repo is its `.gitstat/icon.svg` (or a status dot) beside its graph, the header a bar of green, amber and red repos, the badge a ring of the same colours around a dot in the most urgent one. Hover any of them for the words. |
| `scroll_together` | true | **Settings ▸ Status views ▸ Scroll columns together**: every column of the big view scrolls with the one under the mouse, with thin lines between the rows. Off, each column scrolls on its own. |
| `overlay.enabled` | false | Show the corner badge. |
| `overlay.corner` | `bottom-right` | `top-left`, `top-right`, `bottom-left` or `bottom-right`. |
| `overlay.offset_x`, `overlay.offset_y` | unset | Where the badge was dragged to: px at 100% scale from the left or right, and the top or bottom, edge of the corner. Unset is `margin`. Written by dragging, removed by picking a corner from the menu. |
| `overlay.size` | 44 | Diameter in px at 100% scale, 24 to 128. |
| `overlay.margin` | 16 | Distance from the screen edges in px at 100% scale, 0 to 200, unless the badge was dragged. |
| `overlay.hide_when_synced` | false | Hide the badge while every repo is synced. |
| `overlay.hide_in_fullscreen` | true | Hide the badge while a fullscreen app or a presentation runs. |
| `folder_icons` | false | Status icons on the repo folders, see above. Setting it by hand skips the confirmation. |
| `folder_icon_mode` | `status` | `status` for the status folders, `repo-svg` for each repo's own `.gitstat/icon.svg` where it has one, see [Repo icons](#repo-icons). Only matters while `folder_icons` is on. |
| `github_prs` | true | **Settings ▸ GitHub ▸ Pull requests**: open pull requests in the panel and big mode, read through `gh`, see [GitHub pull requests](#github-pull-requests). Also lets a sweep skip fetching repos with no new push, see [How status stays current](#how-status-stays-current). |
| `github_repos` | true | **Settings ▸ GitHub ▸ Repos not cloned here**: the signed-in account's GitHub repos with no clone under `path`, read through `gh`, see [GitHub repos not cloned here](#github-repos-not-cloned-here). |
| `badge_number` | `unsynced` | **Settings ▸ Icon and badge ▸ Number on the icon and badge**: what the number on the tray icon and the badge counts. `unsynced`, `dirty`, `ahead`, `behind`, `pull-requests`, `pull-requests-for-you` or `review-requests`. |
| `sort` | `name` | **Settings ▸ Status views ▸ Sort repos by**: the order of the repos in the panel and the big view. `name`, `status`, `last-commit`, `last-worked`, `last-pull-request`, `open-pull-requests` or `hanging-branches`. |
| `repo_click` | `folder` | **Settings ▸ Status views ▸ Clicking a repo opens**: what a click on a repo (its name in big mode, or in the menu) opens. `folder` for its folder, `github` for its GitHub page; a repo whose `origin` isn't on GitHub opens its folder either way. |
| `flip_animation` | true | **Settings ▸ Status views ▸ Flip animation**: opening the window from the panel turns the panel into it, and closing turns it back. |
| `flip_ms` | 560 | **Flip speed**: how long the flip takes, in ms. Fast 350, Normal 560, Slow 900, Very slow 5000 (for looking at each frame). |
| `panel_slide_ms` | 150 | **Panel slides in**: how long the panel takes to slide in from the taskbar, in ms; 0 shows it at once. Very slow is 1500. |
| `ignore` | `[]` | Repos to leave out entirely, by name, with `*` for any run of characters, see [Which repos](#which-repos). **Hide this repo** in a repo's right-click menu adds to it. |
| `notify.review_requested`, `notify.new_pr`, `notify.checks_failed`, `notify.behind`, `notify.new_branch`, `notify.new_commits` | false | **Settings ▸ Icon and badge ▸ Notifications**, see [Notifications](#notifications). |

Edits apply within about a second, without a restart. **Edit config.json** in
the panel's footer opens the file. The menu's toggles write the file too;
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

Agents build it with the shared pipeline (`.agents/agent-build pc`, see
`AGENTS.md`): it runs the tests, builds the committed branch in the `build/src`
worktree and puts the newest `gitstat.exe` in `build\` (ignored by git). The
last 4 builds stay in `build\builds\<version>-<commit>\`. The repo's own steps
are in `.agents/hooks/` (`quick-test`, `build`, `full-check`). To run a build,
put a `config.json` next to `build\gitstat.exe` whose `path` points at your
repos folder, since a new config defaults to the folder above the exe's
folder. Plain `cargo build --release` still works for a quick check.

`cargo test` runs the status detection, file-watching, layout, config and
folder-icon tests. They also run on Linux, where only `--once` is available.

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
  or a built-in stroke font without it), `src/repo_icon.rs` draws a repo's
  own `.gitstat/icon.svg` ([resvg](https://crates.io/crates/resvg), with no
  text, fonts or image loading), and `src/folder_icons.rs` applies and
  reverts the desktop.ini icons.
- `src/repo_menu.rs` is a repo's right-click menu, run on a thread of its
  own; `src/notify.rs` works out and shows the notifications;
  `src/settings.rs` is the settings window.
- `src/config.rs` reads and writes `config.json`. `src/autostart.rs` adds or
  removes gitstat under
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
