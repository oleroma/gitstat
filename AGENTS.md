# Rules

## Builds
- All builds happen on Olex's PC. GitHub CI runs only lightweight checks (lint, quick tests); it never builds APKs, executables or other release files.
- Everything made by building, testing or running the program (build outputs, logs, screenshots, caches, virtual environments, downloads) goes inside the repo's gitignored `build/` folder, and nowhere else in the repo.
- The newest APK or exe sits directly in `build/`.
- `build/builds/` keeps the last 4 builds, one folder per build, named after its version and commit. Each folder holds that build's APK or exe and, in `src/`, the git worktree it was built from.
- When adding a build, delete build folders older than the last 4 (remove their worktrees with `git worktree remove`) and any old `debug-*` files.
