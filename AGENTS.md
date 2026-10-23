<!-- shared rules: start. Synced from D:\dev\AGENTS.md by tools\sync-agents.ps1, edit there, not here. -->
# Shared rules for all of Olex's repos

## Where changes are made
- All changes to a repo's code and files are made in cloud sessions and pushed to GitHub. The local checkouts in D:\dev stay clean: sessions on the PC don't edit, commit or merge in them.
- To build on the PC, fetch the branch or commit from GitHub and check it out into a worktree inside the repo's `build/` folder (`build/builds/<version>-<commit>/src`), then build there.

## Builds
- All builds happen on Olex's PC. GitHub CI runs only lightweight checks (lint, quick tests); it never builds APKs, executables or other release files.
- Everything made by building, testing or running the program (build outputs, logs, screenshots, caches, virtual environments, downloads) goes inside the repo's gitignored `build/` folder, and nowhere else in the repo.
- The newest APK or exe sits directly in `build/`.
- `build/builds/` keeps the last 4 builds, one folder per build, named after its version and commit. Each folder holds that build's APK or exe and, in `src/`, the git worktree it was built from.
- When adding a build, delete build folders older than the last 4 (remove their worktrees with `git worktree remove`) and any old `debug-*` files.

## After a build
- If Olex's phone is connected over adb (`adb devices` lists it), install the newest APK on it with `adb install -r`, unless there's a good reason not to (for example a downgrade that would wipe app data).
- Right before installing, vibrate the phone once (`adb shell cmd vibrator_manager synced oneshot 200`) and show a toast naming the built version and saying it is installing now. Plain adb can't show a toast: use the app's own debug toast receiver if it has one, otherwise post that message as a notification instead.
- After the install, post a phone notification saying it completed or failed: `adb shell cmd notification post -t "<app> <version>" build "Install complete"` (or "Install failed").
- If it wasn't installed (no phone connected, a good reason to skip, the install failed, or the build is an exe), upload the newest build file from the PC to the project thread you're working in, so Olex can download it there, and say why.

## Destructive actions
- Destructive actions are allowed only inside the repo's gitignored `build/` folder, and only what the task needs there, such as replacing the newest build or removing builds older than the last 4. Never wipe `build/` or remove anything in it the task doesn't need gone.
- Everywhere else (the rest of the repo, other repos, the rest of the PC, the phone), never do anything that loses data git can't bring back: no deleting files or folders, `git reset --hard`, `git clean`, `git checkout`/`git restore` over uncommitted changes, `git stash drop`, force pushes, deleting branches or tags, rewriting pushed history, or uninstalling apps. If one is needed, ask Olex first.
- Normal edits to files, as part of the task, are not destructive.

## Repo rules
- These shared rules sit at the top of every repo's `AGENTS.md`, between the "shared rules" marker lines. Rules that apply to one repo go below the end marker.
- Where a repo's own rules disagree with the shared rules, the repo's rules win inside that repo. They can't loosen "Destructive actions" or "Agent instruction files".

## Agent instruction files
- Only the "rulesetter" thread changes the shared rules. Their master copy is `D:\dev\AGENTS.md`, and `D:\dev\tools\sync-agents.ps1` copies it into every repo. Olex writes each repo's own rules; rulesetter only reviews them for anything risky.
- Other sessions leave all agent instruction files (`AGENTS.md`, `CLAUDE.md` and the like) alone and bring any needed change to Olex.

## Tools on this PC
- Java: set `JAVA_HOME=D:\dev\tools\jdk17` for Gradle. Android SDK: `D:\dev\tools\android-sdk`; adb: `D:\dev\tools\android-sdk\platform-tools\adb.exe`.
- In Git Bash, set `MSYS_NO_PATHCONV=1` for adb paths like `/sdcard`.

## Phone
- The phone is Olex's daily phone. Never inject test touches (`adb input`) on it, and don't use an emulator.
<!-- shared rules: end. Repo-specific rules go below this line. -->
