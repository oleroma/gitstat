<!-- shared rules: start -->
# Shared rules for all of Olex's repos

## How agents work
- `main` is the only long-lived branch. Agents commit only on a feature branch made from `main` (or a `handoff/` branch, step 1), never on `main` itself, and change `main` only through `agent-merge`, after Olex says the feature works.
- Work in a cloud session unless Olex asks for local work or no cloud session is available. If the cloud session lacks something it needs, ask Olex whether to work locally. Local work follows the same steps; say that you're working locally and why.
- Scripts in `.agents/` do the branching, committing, pushing, building, installing and merging. The agent does what needs judgment: code, commit messages, conflicts, and talking to Olex. Never do by hand what a script does; if a script is missing something or seems wrong, tell Olex.
- Run the scripts with `bash`. Each ends with `result: <word>` and a short summary, and logs to `build/logs/`. Read the log only when the result is `failed`.

## Results
| Result | Script | Agent does |
|---|---|---|
| `done` | all | Next step |
| `failed` | all | Read the log, explain to Olex, stop |
| `other-agent` | `agent-start` | Run `agent-handoff`, then ask Olex: join that feature or wait (step 1) |
| `checks-failed` | `agent-build` | Fix it, go back to step 2. Olex doesn't test this build |
| `other-build` | `agent-build` | Tell Olex which build of which feature it would replace, ask whether to go on |
| `workflow-change` | `agent-build` | Only pipeline files or docs changed since the last build: tell Olex what changed and how to test it, and don't run it |
| `conflicts` | `agent-start`, `agent-take`, `agent-merge` | Resolve them, run the same script again |
| `needs-testing` | `agent-merge` | The merge touched this feature: go back to step 3 |
| `not-built` | `agent-merge` | Something was committed after the last build: go back to step 3 |
| `stuck` | `agent-merge` | Show Olex the files, ask what to do, stop |

## Steps
1. **Start.** You may edit on `main` first, without building. When ready to branch, `bash .agents/agent-start <feature-name>` makes the branch from where you are, with your edits as its first commit and the latest `main` merged in, and opens a draft PR. Watch that PR's activity (subscribe to it) until it's merged: PC builds report there.
   - `other-agent` means another feature holds the repo. Run `bash .agents/agent-handoff <feature-name>`, which pushes your work as `handoff/<feature-name>` (no PR, no build), then ask Olex: join or wait.
   - Join: send the other feature's thread Olex's answer, asking it to run `bash .agents/agent-take handoff/<feature-name>`. It merges your work into its branch, and its next build tests both. Your part is then done.
   - Wait: once the other feature is merged, run `agent-start <feature-name>` on your handoff branch. If Olex says to work in parallel anyway, run `agent-start --continue <feature-name>`.
2. **Edit.** Write the code, then `bash .agents/agent-commit "<message>"`.
3. **Build.** Every build runs the `quick-test`, `build` and `full-check` hooks once and marks the commit checked; nothing runs them again. Feature threads don't build: the project's builder thread (its id is in project memory) runs every build, on the PC or in CI, one at a time. If the project has no builder thread yet, ask the coordinator to start one and save its id to project memory.
   - **Feature thread:** ask the builder thread to build your branch, and say what the build should show. Add `--replace` only when Olex said to go on after `other-build`. The builder answers in your thread, and a PC build also posts its result as a comment on your PR, which wakes you. As soon as it arrives, post only what was done, in one line, such as "Build 334 installed." or "Build 334 published." Only what is posted in the thread reaches Olex. Never end a turn waiting on Olex without telling him there what you're waiting for. Pass on to Olex what the builder asks of him (installing a build file that isn't an APK, going on after `other-build`, turning wireless debugging off and on), and send his answer back to the builder. A check on a device that needs Olex to act first (such as opening the app) is a separate request to the builder, sent after he says he's done it.
   - **Builder thread:** it keeps a Remote Control session on the PC, in `D:\dev\<repo>`, which Olex approves once. For each request:
     - Before building, check what changed since the last build and that the build can show what was asked (for example, that the phone is on adb when installing is the point). If it can't, say so in the feature thread instead of building.
     - `pc`: in `D:\dev\<repo>`, run `git fetch origin <branch>`, then `git -C build/src checkout --detach origin/<branch>` (the first time: `git worktree add --detach build/src origin/<branch>`), then `bash build/src/.agents/agent-build pc`. Send its summary to the feature thread. If it says a build file wasn't installed because it isn't an APK, ask the feature thread whether Olex wants it installed. To install again without building, run `bash build/src/.agents/agent-install [<version>-<commit>]`. If the install failed (`agent-install` ends `failed`; `agent-build` still ends `done` and says why in its summary), say why; when the phone dropped off wireless adb, ask for Olex to turn wireless debugging off and on, then run `agent-install` again.
     - `ci`, only when the PC can't be reached: check out the branch (`git fetch origin <branch>`, `git checkout <branch>`), then `bash .agents/agent-build ci`. As soon as its output shows the CI run link, send it to the feature thread, and send the build's links when it finishes. The PC can't be reached when it's offline (`list_devices`), when the Remote Control session hasn't started 5 minutes after it was asked (set a reminder for then), when the device doesn't pick up the session, or when the session ends without a result. Then run `agent-build ci` at once, without asking Olex, and say in the feature thread that the PC didn't answer. If the session ended without a result, first check the PR for the PC build's result comment and the head commit for a `build/<n>` tag: if either is there, the PC did build, so report that instead of building again.
     - Don't rely on the Remote Control session's report alone: it can fail to arrive. When a PC build starts, set one reminder for after it should be done. If no result has arrived by then, check whether the head commit has a `build/<n>` tag and a passed check, and tell the feature thread what you find (or that it's still running, and since when).
     - Never edit code or merge. Requests that arrive while a build runs wait their turn.
   - `agent-build` stops with `other-build` when the newest build came from another feature whose PR is still open, since Olex may still be testing it. If Olex says to go on, it's run again with `--replace`.
   - If checks fail in code this feature didn't change, tell Olex instead of fixing it in the feature.
   - Run every script that builds (`agent-build`, `agent-install`) in the background with its output going to a file, never in the foreground or through a pipe such as `| tail`. Build daemons it starts can hold a pipe open after it finishes, so the command never returns, and an agent stuck in a foreground command can't read messages.
   - A branch that changes only documentation (`*.md` files and `docs/`) needs no build or test: go straight to step 5 when Olex says so.
   - Commits that change only the pipeline (`.agents/`, `.github/workflows/`) or documentation need no build: Olex tests them himself ("Scripts and hooks"). `agent-build` stops with `workflow-change`, and `agent-merge` accepts them without one.
4. **Check.** Wait for Olex. If he asks for changes, go back to 2. When he says the feature works, go to 5.
5. **Merge.** `bash .agents/agent-merge`. It merges the latest `main` into the branch, then the branch into `main`, keeping the branch.

## Scripts and hooks
- The scripts in `.agents/` are the same in every repo, synced with these rules. When an agent finds a problem in them, the shared rules or its repo's workflows, or a useful insight from its work, it suggests the change to Olex. When Olex asks, it may change them, the shared rules, or its repo's workflows in its own repo. It makes the change on its feature branch like any other code and says so in its summary. Olex tests every change to the scripts, hooks and workflows himself: the agent commits and pushes it, says in the thread what changed and how to test it, and neither builds nor runs it to check it. After the merge it passes the change to the "rulesetter" thread, which copies it to the master and makes the same change in every other repo's rules, scripts and workflows.
- Each repo's hooks in `.agents/hooks/` are its only repo-specific part of the pipeline, and part of its code: an agent may change them when its task changes how the repo builds or tests, and says so in its summary.
  - `quick-test`: the quick tests, run before the build.
  - `full-check`: the slower checks, run after the build. Every repo has one, and it doesn't repeat the quick tests.
  - `build`: builds the repo and prints `build-file: <path> <version>` per build file (path relative to the repo). Several build files share one version, made from `BUILD_NUMBER`.
  - A repo made of separate parts (such as an app and its companion) tests, builds and checks only the parts the branch changed since `main`, so the others aren't rebuilt or reinstalled. A change outside every part builds them all. The hooks decide this; a part that isn't built has no build file.
  - Optionally `install`: run on the PC after a build with the build id and file paths. It installs the build files that aren't APKs and prints `note: <text>` lines for the summary. It never starts or restarts an app; a running app updates itself from `<exe>.new`.
- `.github/workflows/fallback.yml` is the PC build's fallback: run by hand on a branch, never on a push. It runs the same hooks (each on the runner it needs), claims the build number, uploads each build file unzipped, and marks the commit checked only when every job passed. It then publishes the build files as a pre-release on that build's `build/<n>` tag, so Obtainium on the phone can install an APK. Every PC build publishes the same way after its full checks pass, and also installs on the phone when it's reachable. Every job has a `timeout-minutes`. Keep its actions on the Node 24 runtime.
- `.github/workflows/cache.yml` runs after each push to `main` and only compiles (just the parts the push changed, where a repo has parts), with the same cache setup as `fallback.yml`, so a new branch's first fallback build starts from `main`'s caches. It runs no tests or checks, sets no status, and nothing waits for it, `agent-merge` included, except `agent-build ci`, which first waits up to 30 minutes for a running one. Its jobs have a `timeout-minutes` too.

## Builds
- Everything made by building, testing or running the program (outputs, logs, screenshots, caches, downloads) goes in the repo's gitignored `build/` folder, nowhere else in the repo. Leave files other tools keep in the repo (such as `.delta/` and `.gitstat/`) alone.
- `build/src` is the one PC build worktree, reused so builds stay incremental. Nothing is ever edited in it.
- Every build gets the repo's next build number, pushed as a `build/<n>` tag by `.agents/build-number`; never push, move or delete those tags by hand. The version is the hand-bumped version name plus the number, `<name>-<n>` (or `0.1.<n>` where the name holds it), and Android's `versionCode` is the number. A repo may start its count at the number in `.agents/hooks/first-build-number`.
- When Olex asks to publish builds, ask the builder thread to run `bash .agents/agent-publish [<n>]` on the PC (from `build/src` when the checkout in `D:\dev\<repo>` doesn't have it yet). It publishes build `<n>` (by default `main`'s newest build) as a GitHub pre-release on its `build/<n>` tag, with every part's current file: a part the build didn't remake comes from the older build it was last made in. Never create releases with `gh release` by hand.
- Keep builds fast: build caches on and never cleaned, build daemons kept running on the PC, quicker settings for branch builds as long as the build file runs the same way.

## Destructive actions
- Destructive actions are allowed only inside the repo's `build/` folder, and only what the task needs. Never wipe `build/`.
- Everywhere else (the rest of the repo, other repos, the PC, the phone), never do anything that loses data git can't bring back: no deleting files or folders, `git reset --hard`, `git clean`, `git checkout`/`git restore` over uncommitted changes, `git stash drop`, force pushes, deleting branches or tags, rewriting pushed history, or uninstalling apps. If one is needed, ask Olex first. Normal edits as part of the task are fine.

## Agent instruction files
- These shared rules sit at the top of every repo's `AGENTS.md`, between the "shared rules" markers. Their master copy is `rules/AGENTS.md` in the DevDoctor project's files, kept and synced by the "rulesetter" thread. Apart from the fixes and insights above, when Olex asks a repo thread for a shared-rule change, pass his message to the rulesetter thread.
- Rules for one repo go below the end marker, written by Olex or by an agent when Olex asks. Where they disagree with a shared rule, the repo rule wins in that repo. Otherwise leave all agent instruction files (`AGENTS.md`, `CLAUDE.md` and the like) alone and bring any needed change to Olex.

## On the PC
- Build tools are used through the `PATH` and their standard variables (such as `JAVA_HOME`, `ANDROID_HOME`), never by a hardcoded path. A tool installed by hand goes in `D:\tools` (one folder each) and on the user `PATH`, never inside `D:\dev` or a repo. Java 17 is in `D:\tools\jdk17`; the Android SDK is in `%LOCALAPPDATA%\Android\Sdk`; adb is on the `PATH`.
- In Git Bash, set `MSYS_NO_PATHCONV=1` for adb paths like `/sdcard`.
- Agents never put anything of their own in AppData; an `install` hook may install a program into `%LOCALAPPDATA%\Programs\<app>` with its shortcuts.
- Agents never start the repo's apps on the PC; Olex tests everything that can't be checked in the background.

## Phone
- The phone is Olex's daily phone. Never inject test touches (`adb input`) on it, and don't use an emulator.
<!-- shared rules: end -->
