# Project: Git Sync Tray

## Goal
A small tray/menu-bar app that monitors all git repos in one parent folder and shows at a glance whether they're in sync with their remotes.

## Behavior
- Watch every git repo directly under: [PATH, e.g. ~/dev]
- Every [N] minutes, run `git fetch` on each repo (fetch only — never pull automatically)
- For each repo, determine status vs. its upstream: current / ahead / behind / diverged / dirty (uncommitted changes) / no upstream
- A repo counts as "unsynced" if it's anything other than current

## Icon
- All repos current → green icon with a checkmark
- Any unsynced → red icon showing the number of unsynced repos
- Click/menu → list each repo with its status (e.g. ⬆2 ⬇1, dirty), plus "Refresh now" and "Quit"

## Environment
- OS: [Windows / macOS / Linux + desktop environment]
- Language: Python (pystray + Pillow) unless there's a better native option for my OS
- Should start automatically on login

## Preferences
- Keep it a single small script plus a config file (path, interval)
- Explain how to install and set up autostart
