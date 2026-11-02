<!-- shared rules: start -->
# Shared rules for all of Olex's repos

## How agents work
- `main` is the only long-lived branch. Agents always work on a feature branch made from `main`, and change `main` only through `agent-merge`, after Olex says the feature works.
- Work in a cloud session unless Olex asks for local work or no cloud session is available. If the cloud session lacks something it needs, ask Olex whether to work locally. Local work follows the same steps; say that you're working locally and why.
- Scripts in `.agents/` do the branching, committing, pushing, building, installing and merging. The agent does what needs judgment: code, commit messages, conflicts, and talking to Olex. Never do by hand what a script does; if a script is missing something or seems wrong, tell Olex.
- Run the scripts with `bash`. Each ends with `result: <word>` and a short summary, and logs to `build/logs/`. Read the log only when the result is `failed`.

## Results
| Result | Script | Agent does |
|---|---|---|
| `done` | all | Next step |
| `failed` | all | Read the log, explain to Olex, stop |
| `other-agent` | `agent-start` | Tell Olex which draft PR is open, ask whether to wait |
| `checks-failed` | `agent-build` | Fix it, go back to step 2. Olex doesn't test this build |
| `conflicts` | `agent-merge` | Resolve them, run `agent-merge` again |
| `needs-testing` | `agent-merge` | The merge touched this feature: go back to step 3 |
| `not-built` | `agent-merge` | Something was committed after the last build: go back to step 3 |
| `stuck` | `agent-merge` | Show Olex the files, ask what to do, stop |

## Steps
1. **Start.** `bash .agents/agent-start <feature-name>` makes the branch and a draft PR. If Olex says to go on despite `other-agent`, run it with `--continue`.
2. **Edit.** Write the code, then `bash .agents/agent-commit "<message>"`.
3. **Build.** Every build runs the `quick-test`, `build` and `full-check` hooks once and marks the commit checked; nothing runs them again. Try the PC first through Remote Control (actually try, don't guess):
   - `pc`: in `D:\dev\<repo>`, run `git fetch origin <branch>`, then `git -C build/src checkout --detach origin/<branch>` (the first time: `git worktree add --detach build/src origin/<branch>`), then `bash build/src/.agents/agent-build pc`. Report its summary. If it says a build file wasn't installed because it isn't an APK, ask Olex whether to install it. To install again without building, run `bash build/src/.agents/agent-install [<version>-<commit>]`.
   - `ci`, only when the PC can't be reached: `bash .agents/agent-build ci`. Post the download links in the project thread first, then upload the files.
   - If checks fail in code this feature didn't change, tell Olex instead of fixing it in the feature.
4. **Check.** Wait for Olex. If he asks for changes, go back to 2. When he says the feature works, go to 5.
5. **Merge.** `bash .agents/agent-merge`. It merges the latest `main` into the branch, then the branch into `main`, keeping the branch.

## Scripts and hooks
- The scripts in `.agents/` are the same in every repo, synced with these rules, and changed only by the "rulesetter" thread. Agents never edit them.
- Each repo's hooks in `.agents/hooks/` are its only repo-specific part of the pipeline, and part of its code: an agent may change them when its task changes how the repo builds or tests, and says so in its summary.
  - `quick-test`: the quick tests, run before the build.
  - `full-check`: the slower checks, run after the build. Every repo has one, and it doesn't repeat the quick tests.
  - `build`: builds the repo and prints `build-file: <path> <version>` per build file (path relative to the repo). Several build files share one version, made from `BUILD_NUMBER`.
  - A repo made of separate parts (such as an app and its companion) tests, builds and checks only the parts the branch changed since `main`, so the others aren't rebuilt or reinstalled. A change outside every part builds them all. The hooks decide this; a part that isn't built has no build file.
  - Optionally `install`: run on the PC after a build with the build id and file paths. It installs the build files that aren't APKs and prints `note: <text>` lines for the summary. It never starts or restarts an app; a running app updates itself from `<exe>.new`.
- `.github/workflows/fallback.yml` is the PC build's fallback: run by hand on a branch, never on a push. It runs the same hooks (each on the runner it needs), claims the build number, uploads each build file unzipped, and marks the commit checked only when every job passed. Every job has a `timeout-minutes`. Keep its actions on the Node 24 runtime.
- `.github/workflows/cache.yml` runs after each push to `main` and only compiles (just the parts the push changed, where a repo has parts), with the same cache setup as `fallback.yml`, so a new branch's first fallback build starts from `main`'s caches. It runs no tests or checks, sets no status, and nothing waits for it, `agent-merge` included. Its jobs have a `timeout-minutes` too.

## Builds
- Everything made by building, testing or running the program (outputs, logs, screenshots, caches, downloads) goes in the repo's gitignored `build/` folder, nowhere else in the repo. Leave files other tools keep in the repo (such as `.delta/` and `.gitstat/`) alone.
- `build/src` is the one PC build worktree, reused so builds stay incremental. Nothing is ever edited in it.
- Every build gets the repo's next build number, pushed as a `build/<n>` tag by `.agents/build-number`; never push, move or delete those tags by hand. The version is the hand-bumped version name plus the number, `<name>-<n>` (or `0.1.<n>` where the name holds it), and Android's `versionCode` is the number. A repo may start its count at the number in `.agents/hooks/first-build-number`.
- Keep builds fast: build caches on and never cleaned, build daemons kept running on the PC, quicker settings for branch builds as long as the build file runs the same way.

## Destructive actions
- Destructive actions are allowed only inside the repo's `build/` folder, and only what the task needs. Never wipe `build/`.
- Everywhere else (the rest of the repo, other repos, the PC, the phone), never do anything that loses data git can't bring back: no deleting files or folders, `git reset --hard`, `git clean`, `git checkout`/`git restore` over uncommitted changes, `git stash drop`, force pushes, deleting branches or tags, rewriting pushed history, or uninstalling apps. If one is needed, ask Olex first. Normal edits as part of the task are fine.

## Agent instruction files
- These shared rules sit at the top of every repo's `AGENTS.md`, between the "shared rules" markers. Their master copy is `rules/AGENTS.md` in the DevDoctor project's files, and only the "rulesetter" thread changes them. When Olex asks a repo thread for a shared-rule change, pass his message to the rulesetter thread.
- Rules for one repo go below the end marker, written by Olex or by an agent when Olex asks. Where they disagree with a shared rule, the repo rule wins in that repo. Otherwise leave all agent instruction files (`AGENTS.md`, `CLAUDE.md` and the like) alone and bring any needed change to Olex.

## On the PC
- Build tools are used through the `PATH` and their standard variables (such as `JAVA_HOME`, `ANDROID_HOME`), never by a hardcoded path. A tool installed by hand goes in `D:\tools` (one folder each) and on the user `PATH`, never inside `D:\dev` or a repo. Java 17 is in `D:\tools\jdk17`; the Android SDK is in `%LOCALAPPDATA%\Android\Sdk`; adb is on the `PATH`.
- In Git Bash, set `MSYS_NO_PATHCONV=1` for adb paths like `/sdcard`.
- Agents never put anything of their own in AppData; an `install` hook may install a program into `%LOCALAPPDATA%\Programs\<app>` with its shortcuts.
- Agents never start the repo's apps on the PC; Olex tests everything that can't be checked in the background.

## Phone
- The phone is Olex's daily phone. Never inject test touches (`adb input`) on it, and don't use an emulator.
<!-- shared rules: end -->
