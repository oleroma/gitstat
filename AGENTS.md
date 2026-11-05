<!-- shared rules: start -->
# Shared rules for all of Olex's repos

## How agents work
- `main` is the only long-lived branch. Agents commit only on a feature branch made from `main`, never on `main` itself, and change `main` only through `agent-merge`, when Olex says "merge".
- Work in a cloud session unless Olex asks for local work or no cloud session is available. If the cloud session lacks something it needs, ask Olex whether to work locally. Local work follows the same steps; say that you're working locally and why.
- Scripts in `.agents/` do the branching, committing, pushing, building, installing and merging. The agent does what needs judgment: code, commit messages, conflicts, and talking to Olex. Never do by hand what a script does; if a script is missing something or seems wrong, tell Olex.
- Run the scripts with `bash`. Each ends with `result: <word>` and a short summary, and logs to `build/logs/`. Read the log only when the result is `failed`.

## Results
| Result | Script | Agent does |
|---|---|---|
| `done` | all | Next step |
| `failed` | all | Read the log, explain to Olex, stop |
| `other-agent` | `agent-start` | Another feature holds the repo. If your task isn't a feature (it changes only files no build uses, see `workflow-change`), run `agent-start --continue <name>`. Otherwise make no branch and no edits: work out the task, ask Olex what you need, then ask whether to wait for that feature to merge or to send the task to its thread |
| `checks-failed` | `agent-build` | Fix it, go back to step 2. Olex doesn't test this build |
| `other-build` | `agent-build` | Tell Olex which build of which feature it would replace. If he says go on, build again with `--replace` |
| `workflow-change` | `agent-build` | Only files no build uses (the pipeline `.agents/` and `.github/workflows/`, docs, rules, `.gitignore`, `.claude/` and the like, listed in `lib.sh` `pipeline_file`) are all the branch changed since `main`: Olex tests them himself. Tell him what changed and how to test it, and don't run it. `agent-merge` accepts them without a build, and also accepts such commits made after the branch's last build |
| `conflicts` | `agent-start`, `agent-merge` | Resolve them, run the same script again |
| `needs-testing` | `agent-merge` | The merge touched this feature: go back to step 3 |
| `not-built` | `agent-merge` | Something was committed after the last build: go back to step 3 |
| `stuck` | `agent-merge` | Show Olex the files, ask what to do, stop |

## Steps
1. **Start.** Before editing, `bash .agents/agent-start <feature-name>` makes the branch from the latest `main` and opens a draft PR. Watch that PR's activity (subscribe to it) until it's merged: every build's result shows there.
2. **Edit.** Write the code, then `bash .agents/agent-commit "<message>"`.
3. **Build.** Every build runs the `quick-test`, `build` and `full-check` hooks once and marks the commit checked. Feature threads don't build: the project's "builder" thread runs every build, on the PC or in CI, one at a time. If the project has none, ask the coordinator to start one.
   - **Feature thread:** first run `bash .agents/hooks/quick-test` in your session and fix what fails; if the session can't run it, say so in your request. Then ask the builder to build your branch and say what the build should show. The result shows on your PR (the PC build's comment or the CI checks): as soon as it does, post only what was done, in one line, such as "Build 334 installed." or "Build 334 published." Pass on to Olex what the builder sends him or asks of him, and send his answer back.
   - **Builder thread:** it keeps a Remote Control session on the PC, which Olex approves once. For each request:
     - `pc`: run `bash .agents/agent-build pc <branch>`. To install again without building, run `bash .agents/agent-install [<version>-<commit>]`. If the phone dropped off wireless adb, ask for Olex to turn wireless debugging off and on, then run it again.
     - `ci`: when Remote Control can't start (the PC is offline or doesn't pick up), run `bash .agents/agent-build ci <branch>` right away and say so in the feature thread. Send it the CI run link as soon as it shows.
     - When a build fails, send the feature thread one message with the failing step and the log lines that matter. The feature fixes it and asks again.
     - Never edit code or merge.
   - If checks fail in code this feature didn't change, tell Olex instead of fixing it in the feature.
   - Run `agent-build` and `agent-install` in the background with output to a file, never through a pipe: the build daemons they start keep a pipe open, so the command would never return.
   - Build only what the code needs to work: a branch that changes only files no build uses needs no build, and `agent-build` stops with `workflow-change`.
4. **Check.** Wait for Olex. If he asks for changes, go back to 2. Merge only when he says "merge".
5. **Merge.** `bash .agents/agent-merge`. It merges the latest `main` into the branch, then the branch into `main`, keeping the branch.

## Scripts and hooks
- The scripts in `.agents/` are the same in every repo. When an agent finds a problem in them, the shared rules or its repo's workflows, it suggests a fix to Olex, and makes it on its feature branch only when he asks. Olex tests such changes himself: the agent pushes them and says what changed and how to test it, without running them. After the merge, Olex has the "rulesetter" thread apply it to every repo.
- Each repo's hooks in `.agents/hooks/` are its only repo-specific part of the pipeline. They are pipeline, not code: a branch that changes only hooks (with other pipeline files or docs) isn't built, and Olex tests it himself like any pipeline change. An agent may change them when its task changes how the repo builds or tests, and says so in its summary.
  - `quick-test`: the quick tests, run before the build.
  - `full-check`: the slower checks, run after the build. Every repo has one, and it doesn't repeat the quick tests.
  - `build`: builds the repo and prints `build-file: <path> <version>` per build file (path relative to the repo). Several build files share one version, made from `BUILD_NUMBER`.
  - Optionally `install`: run on the PC after a build with the build id and file paths. It installs the build files that aren't APKs and prints `note: <text>` lines for the summary. It never starts or restarts an app; a running app updates itself from `<exe>.new`.
- `.github/workflows/fallback.yml` is the PC build's fallback, started by `agent-build ci`. Every build, PC or CI, publishes its build files as a pre-release on its `build/<n>` tag, so Obtainium on the phone can install an APK; a PC build also installs on the phone when it's reachable.
- `.github/workflows/cache.yml` keeps `main`'s build caches warm after each push to `main`.

## Builds
- Everything made by building, testing or running the program (outputs, logs, screenshots, caches, downloads) goes in the repo's gitignored `build/` folder, nowhere else in the repo. Leave files other tools keep in the repo (such as `.delta/` and `.gitstat/`) alone.
- `build/src` is the one PC build worktree, reused so builds stay incremental. Nothing is ever edited in it.
- Every build gets the repo's next build number, pushed as a `build/<n>` tag by `.agents/build-number`. The version is the hand-bumped version name plus the number, `<name>-<n>` (or `0.1.<n>` where the name holds it), and Android's `versionCode` is the number. A repo may start its count at the number in `.agents/hooks/first-build-number`.
- When Olex asks to publish builds, ask the builder to run `bash .agents/agent-publish [<n>]` on the PC. It publishes build `<n>` (by default `main`'s newest build) as a pre-release on its `build/<n>` tag, with every part's current file.
- Keep builds fast: build caches on and never cleaned, build daemons kept running on the PC, quicker settings for branch builds as long as the build file runs the same way.

## Destructive actions
- Destructive actions are allowed only inside the repo's `build/` folder, and only what the task needs. Never wipe `build/`.
- Everywhere else (the rest of the repo, other repos, the PC, the phone), never do anything that loses data git can't bring back: no deleting files or folders, `git reset --hard`, `git clean`, `git checkout`/`git restore` over uncommitted changes, `git stash drop`, force pushes, deleting branches or tags, rewriting pushed history, or uninstalling apps. If one is needed, ask Olex first. Normal edits as part of the task are fine.

## Agent instruction files
- These shared rules sit at the top of every repo's `AGENTS.md`, between the "shared rules" markers. Their master copy is `rules/AGENTS.md` in the DevDoctor project's files, kept by the "rulesetter" thread, which applies it to every repo. A repo agent suggests shared-rule changes to Olex; when he approves, it makes them in its own `AGENTS.md` on its feature branch, and after the merge Olex has the rulesetter apply them everywhere.
- `AGENTS.md` is the main rules file; project instructions and project memory refer to it. When Olex says "check memory", compare project memory with `AGENTS.md`, and for each line that conflicts with it, suggest to Olex either removing the line from memory or moving it into this repo's rules below the end marker. Change nothing until he answers. A coordinator passes Olex's words on by attaching his message, not by rewording it.
- Rules for one repo go below the end marker, written by Olex or by an agent when Olex asks. Where they disagree with a shared rule, the repo rule wins in that repo. Otherwise leave all agent instruction files (`AGENTS.md`, `CLAUDE.md` and the like) alone and bring any needed change to Olex.

## On the PC
- Build tools are used through the `PATH` and their standard variables (such as `JAVA_HOME`, `ANDROID_HOME`), never by a hardcoded path. A tool installed by hand goes in `D:\tools` (one folder each) and on the user `PATH`, never inside `D:\dev` or a repo. Java 17 is in `D:\tools\jdk17`; the Android SDK is in `%LOCALAPPDATA%\Android\Sdk`; adb is on the `PATH`.
- In Git Bash, set `MSYS_NO_PATHCONV=1` for adb paths like `/sdcard`.
- Agents never put anything of their own in AppData; an `install` hook may install a program into `%LOCALAPPDATA%\Programs\<app>` with its shortcuts.
- Agents never start the repo's apps on the PC; Olex tests everything that can't be checked in the background.

## Phone
- The phone is Olex's daily phone. Never inject test touches (`adb input`) on it, and don't use an emulator.
<!-- shared rules: end -->
