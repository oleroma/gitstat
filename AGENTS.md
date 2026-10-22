# Rules

## Builds
- All builds happen on Olex's PC. GitHub CI runs only lightweight checks (lint, quick tests); it never builds APKs, executables or other release files.
- Everything made by building, testing or running the program (build outputs, logs, screenshots, caches, virtual environments, downloads) goes inside the repo's gitignored `build/` folder, and nowhere else in the repo.
- The newest APK or exe sits directly in `build/`.
- `build/builds/` keeps the last 4 builds, one folder per build, named after its version and commit. Each folder holds that build's APK or exe and, in `src/`, the git worktree it was built from.
- When adding a build, delete build folders older than the last 4 (remove their worktrees with `git worktree remove`) and any old `debug-*` files.

## Destructive actions
- Destructive actions are allowed only inside the repo's gitignored `build/` folder. There you may delete, overwrite or move anything.
- Everywhere else (the rest of the repo, other repos, the rest of the PC, the phone), never do anything that loses data git can't bring back: no deleting files or folders, `git reset --hard`, `git clean`, `git checkout`/`git restore` over uncommitted changes, `git stash drop`, force pushes, deleting branches or tags, rewriting pushed history, or uninstalling apps. If one is needed, ask Olex first.
- Normal edits to files, as part of the task, are not destructive.
