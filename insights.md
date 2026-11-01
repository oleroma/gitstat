## gitstat-1: Agents never write to AppData, except to install a program
Added: 2026-11-01. Status: open
What happened: Agents in Claude desktop sessions put build worktrees, caches and other files under AppData. Claude desktop is an MSIX app, so its writes to AppData get silently redirected into `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache`. That left clutter in two places and split settings between the real AppData folder and the redirected copy (for example an old `gitstat-build` worktree under AppData).
Proposed rule: Agents never write to AppData (`%LOCALAPPDATA%` or `%APPDATA%`), with one exception: a repo's `install` hook may install a program into `AppData\Local\Programs\<app>`, together with what that program needs to run, plus its Start menu shortcut and its start-with-Windows entry. "Programs and what they need, not entire builds": the installed app may keep its own settings and caches in AppData, but agents never put builds, worktrees, downloads or tool caches there. Those go in the repo's gitignored `build\` folder.

## gitstat-2: Start test apps outside the Claude package
Added: 2026-11-01. Status: open
What happened: A cleanup of `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache` (where Windows redirects AppData writes from the MSIX Claude desktop app) found: an old gitstat build worktree (`gitstat-build`, Oct 20-22) that gitstat showed as a second worktree; two GyroPhone CI builds with APKs (198 MB); NuGet, kotlin and matplotlib caches; and split app settings, because apps started from a Claude session ran inside the package too: an Oryx started that way saved its newest config and launches only in the package folder, and GyroPhone got a second, different `host.key` there. Installs through `install` hooks into `AppData\Local\Programs` did land in the real folder. See gitstat-1 for the AppData rule itself.
Proposed rule: Apps an agent starts for testing are started outside the Claude app's package, for example from the Start menu shortcut or Explorer, so they read and write their real settings in AppData instead of a redirected copy.

## gitstat-3: Insight branches don't merge automatically
Added: 2026-11-01. Status: open
What happened: The shared rules tell an agent to run `agent-merge` straight after committing an insight on its own `insights-<topic>` branch, taking Olex's ask as his OK. Olex wants to look at insight entries before they reach `main`.
Proposed rule: An `insights-<topic>` branch is not merged automatically. The agent commits the entry with `agent-commit`, leaves the PR open, and runs `agent-merge` only when Olex says to merge it.

## gitstat-4: Skip CI for insight-only changes
Added: 2026-11-01. Status: open
What happened: `agent-merge` runs the full checks in GitHub CI and waits for them before merging, even when the branch only changes `insights.md`. Merging two lines of notes took minutes and looked to Olex like a stuck thread. The push to `main` then runs the full checks again for nothing.
Proposed rule: When a branch changes only `insights.md`, `agent-merge` skips the CI checks on the branch and merges with `[skip ci]` so the push to `main` doesn't run them either.
