<!-- shared rules: start -->
# Shared rules for all of Olex's repos

## How agents work
- `main` is the only long-lived branch. Agents always work on a feature branch made from `main`.
- Agents may work in a cloud session or locally on the PC; the cloud is preferred. Work in the cloud unless:
  - Olex asks for local work: work locally.
  - No cloud session is available: work locally.
  - The cloud session lacks something it really needs: ask Olex what to do; if he doesn't answer within 2 minutes, work locally.
- Local work follows the same steps; say that you're working locally and why. Olex can work and change anything anywhere.
- Scripts in `.agents/` run the pipeline: branching, committing, pushing, building, installing and merging. The agent does only what needs judgment: writing code and commit messages, resolving conflicts, trying the PC before the fallback build, posting in the project thread, and talking to Olex.
- Never do by hand what a script does. If a script is missing something or seems wrong, tell Olex instead of working around it.
- Run the scripts with `bash`, as in the steps below. Each script ends with one line, `result: <word>`, followed by a short summary, and writes its full log to `build/logs/`. Read the log only when the result is `failed`.

## Results
| Result | Script | Meaning | Agent does |
|---|---|---|---|
| `done` | all | Finished | Next step |
| `failed` | all | Something unexpected went wrong | Read the log, explain to Olex, stop |
| `main-red` | `agent-start` | The last checks on `main` failed | Tell Olex with the link, ask whether to fix `main` first |
| `other-agent` | `agent-start` | Another agent's draft PR is open | Tell Olex which one, ask whether to wait |
| `conflicts` | `agent-merge` | Conflicts (attempt 1 to 3) | Resolve them, run `agent-merge` again |
| `needs-testing` | `agent-merge` | The merge touched this feature | Go back to step 3 |
| `checks-failed` | `agent-merge` | The full checks failed on the branch | Fix it, go back to step 2 |
| `stuck` | `agent-merge` | Conflicts left after 3 attempts, or conflict markers left in files | Show Olex the files, ask what to do, stop |

## Steps
1. **Start.** Run `bash .agents/agent-start <feature-name>`. It makes a branch named after the feature from the latest `main`, pushes, and opens a draft PR into `main` so other agents can see the work.
   - `main-red`: tell Olex the last checks on `main` failed, with the link, and ask whether to fix `main` first (as its own feature) or go on.
   - `other-agent`: tell Olex which draft PR is open and ask whether to wait.
   - If he says to go on, run `bash .agents/agent-start --continue <feature-name>`, which skips both checks.
2. **Edit.** Write the code, then run `bash .agents/agent-commit "<message>"` with a commit message; the script commits and pushes.
3. **Build.** Try to reach Olex's PC through Remote Control and run the `pc` build there. Don't decide from the cloud session's own tools whether the PC is there: actually try. If the PC can't be reached (offline, no access, no answer), run `bash .agents/agent-build ci` in the cloud instead.
   - `pc`: on the PC, in `D:\dev\<repo>`, fetch the branch and check it out in `build/src` (`git fetch origin <branch>`, then `git -C build/src checkout --detach origin/<branch>`, or `git worktree add --detach build/src origin/<branch>` the first time). Then run `bash build/src/.agents/agent-build pc`, so the script always comes from the branch being built. It builds in `build/src`, stores the build files, then installs them with `agent-install`: each APK on the phone when one is connected and it isn't a downgrade, then the repo's `install` hook if it has one. A failed install doesn't fail the build. The summary says what was installed and why not. If it says a build file isn't installed because it isn't an APK, ask Olex whether to install it. Last it runs the full checks; when they pass, the commit is marked checked on GitHub so `agent-merge` doesn't run them again. If they fail, fix them before Olex tests.
      - To install without building (for example once the phone is connected), run only `bash build/src/.agents/agent-install`, which installs the newest build, or `agent-install <version>-<commit>` for an older one from `build/builds/`.
   - `ci`: runs the fallback workflow on the branch and waits, then prints a direct download link per build file and downloads them. Post the links in the project thread first, then upload the files. Nothing is installed.
4. **Check.** Wait for Olex. If he asks for changes, go back to 2. When he says the feature works, go to 5.
5. **Merge.** Run `bash .agents/agent-merge`. It merges the latest `main` into the feature branch and pushes, runs the full checks on the branch in GitHub CI and waits for them (a few minutes) unless they already passed on that commit, then marks the PR ready and merges it into `main`, keeping the feature branch.
   - `conflicts`: resolve them, then run it again; it commits the resolution.
   - `needs-testing`: go back to 3. Once Olex says it still works, run it again.
   - `checks-failed`: read the run, fix the cause on the branch (step 2), build again (step 3), and tell Olex what changed.
   - `stuck`: show Olex the files and ask what to do.

## After merging into main
- Agents change `main` only through `agent-merge`, after Olex says the feature works. Never commit or push to `main` directly.
- A merge into `main` builds nothing. GitHub CI then runs the repo's full checks again on `main`. Nothing waits for it; the next `agent-start` reports a red `main`, and the fix goes on a new feature branch.

## Scripts and hooks
- The scripts in `.agents/` (and `.agents/pipeline.md`, the diagram of these steps) are shared: the same in every repo, synced with these rules, and changed only by the "rulesetter" thread. Agents never edit them.
- Each repo has hooks in `.agents/hooks/`, the only repo-specific part of the pipeline. They are part of the repo's code: an agent may change them when its task changes how the repo builds or tests, and says so in its summary.
  - `quick-test`: the quick tests a branch build runs.
  - `full-check`: the slower checks run on every push to `main`.
  - `build`: builds the repo and prints one line `build-file: <path> <version>` per build file, with the path relative to the repo. A repo may have several build files (for example an app and a desktop companion); they share one version.
    - Optionally `install`: run on the PC by `agent-install`, with the build id (`<version>-<commit>`) and the paths of the build files. It installs the build files that aren't APKs and does any setup after an install. Its `note: <text>` lines go into the build summary.
- `.github/workflows/fallback.yml` calls the same hooks. It takes a `mode` input when run by hand on a branch: `build` (the default, used by `agent-build ci`) runs `quick-test` and `build` and uploads each build file unzipped (`archive: false`), one artifact per file; `check` (used by `agent-merge`) runs `full-check` only and, when it passes, marks the commit checked with a `full-check` commit status, as PC builds do. On a push to `main`, it runs `full-check` only and builds nothing. A repo without `full-check` has `check` mode do nothing and doesn't run on `main`.
- `full-check` runs on Linux. Code that builds only on Windows is checked by PC builds and by the fallback's Windows build job; a repo may add a Windows job to `check` mode.
- `fallback.yml` uses actions on the Node 24 runtime: `actions/checkout@v7`, `actions/setup-java@v6`, `actions/setup-dotnet@v6`, `android-actions/setup-android@v4`, `gradle/actions/setup-gradle@v6` (with `cache-provider: basic`), `actions/cache@v6`, `actions/upload-artifact@v7`. Bump them when GitHub warns about a runtime.

## Builds
- Everything an agent or its scripts make by building, testing or running the program (build outputs, logs, screenshots, caches, virtual environments, downloads) goes inside the repo's gitignored `build/` folder, and nowhere else in the repo.
- Files other tools keep in the repo (such as `.delta/` and `.gitstat/`) aren't covered by this rule: leave them alone.
- Each repo has one build worktree, `build/src`, reused by every PC build so builds stay incremental. Only one build runs in it at a time; nothing is ever edited in it.
- The newest build files sit directly in `build/`. `build/builds/` keeps the last 4 builds, one folder per build named `<version>-<commit>` holding its build files.
- Builds are set up to be as fast as possible:
  - The build tool's caches are on, and its output and caches are never cleaned between builds.
  - PC builds keep the build tool's background processes running between builds.
  - Branch builds may use quicker build settings than a final build, as long as the build file still runs the same way.
  - `fallback.yml` restores the build tool's cache on every run. Branch runs save it too; runs on `main` only read it, so `main` doesn't fill the repo's cache space.

## Destructive actions
- Destructive actions are allowed only inside the repo's gitignored `build/` folder, and only what the task needs there, such as the scripts' build cleanup. Never wipe `build/` or remove anything in it the task doesn't need gone.
- Everywhere else (the rest of the repo, other repos, the rest of the PC, the phone), never do anything that loses data git can't bring back: no deleting files or folders, `git reset --hard`, `git clean`, `git checkout`/`git restore` over uncommitted changes, `git stash drop`, force pushes, deleting branches or tags, rewriting pushed history, or uninstalling apps. If one is needed, ask Olex first.
- Normal edits to files, as part of the task, are not destructive.

## Agent instruction files
- These shared rules sit at the top of every repo's `AGENTS.md`, between the "shared rules" marker lines, and are the same in every repo. Only the "rulesetter" thread changes them. Their master copy is `rules/AGENTS.md` in the DevDoctor project's files (`/mnt/project-files/rules/AGENTS.md` in a cloud session), with the shared scripts next to it in `rules/.agents/`; a cloud session runs `rules/sync-agents.sh` from there to copy both into every repo. `D:\dev\AGENTS.md` and `D:\dev\tools\sync-agents.ps1` are retired: don't use them.
- Rules that apply to one repo go below the end marker, without repeating the shared rules. Where a repo rule disagrees with a shared rule, the repo rule wins inside that repo. Olex writes them; rulesetter only reviews them for anything risky.
- Other sessions leave all agent instruction files (`AGENTS.md`, `CLAUDE.md` and the like) alone and bring any needed change to Olex.

## Tools on this PC
- Build tools (JDKs, SDKs and the like) are used through the `PATH` and their standard environment variables (such as `JAVA_HOME` and `ANDROID_HOME`), never by a hardcoded path in scripts or hooks. A tool installed by hand goes in `D:\tools` (one folder each, a portable copy where possible) and is added to the user `PATH`; never inside `D:\dev` or a repo.
- Java 17: `D:\tools\jdk17`, on the user `PATH`, with `JAVA_HOME` set. Android SDK: `%LOCALAPPDATA%\Android\Sdk`, with `ANDROID_HOME` set. adb: on the `PATH`.
- In Git Bash, set `MSYS_NO_PATHCONV=1` for adb paths like `/sdcard`.

## Phone
- The phone is Olex's daily phone. Never inject test touches (`adb input`) on it, and don't use an emulator.
<!-- shared rules: end -->
