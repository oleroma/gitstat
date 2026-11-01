#!/usr/bin/env bash
# Shared helpers for the scripts in .agents/. Sourced by them, never run on its own.
# A script ends by printing "result: <word>" and a short summary. Everything
# else, including each command it runs, goes to its log in build/logs/.

set -euo pipefail
shopt -s inherit_errexit

AGENTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKTREE="$(git -C "$AGENTS_DIR" rev-parse --show-toplevel)"
# The main checkout, which holds build/, also when running from the build/src worktree.
MAIN_REPO="$(dirname "$(git -C "$WORKTREE" rev-parse --path-format=absolute --git-common-dir)")"
BUILD_DIR="$MAIN_REPO/build"
SCRIPT_NAME="$(basename "$0")"
mkdir -p "$BUILD_DIR/logs"
LOG_FILE="$BUILD_DIR/logs/$SCRIPT_NAME-$(date +%Y%m%d-%H%M%S).log"

# fd 3 keeps the terminal for the result; stdout and stderr go to the log.
exec 3>&1 >>"$LOG_FILE" 2>&1
set -x

FINISHED=0

# Scripts redefine this to undo things such as a lock; it runs on every exit.
cleanup() { :; }

# finish <word> [summary line...]: print the result and exit.
finish() {
  set +x
  FINISHED=1
  local word="$1" out
  shift
  out="result: $word"
  if [ $# -gt 0 ]; then out="$out"$'\n'"$(printf '%s\n' "$@")"; fi
  out="$out"$'\n'"log: $LOG_FILE"
  printf '%s\n' "$out"
  printf '%s\n' "$out" >&3
  case "$word" in
    done) exit 0 ;;
    failed) exit 1 ;;
    *) exit 3 ;;
  esac
}

die() { finish failed "$@"; }

on_exit() {
  local code=$?
  set +x
  cleanup || true
  if [ "$FINISHED" = 0 ]; then
    printf 'result: failed\nUnexpected error (exit %s); the last command in the log failed.\nlog: %s\n' "$code" "$LOG_FILE" >&3
    exit 1
  fi
}
trap on_exit EXIT

# GitHub calls go through the REST API (gh api): cloud sessions can't reach GitHub's
# GraphQL API, which gh pr, gh repo view and most other gh commands use.

# owner/repo of origin, read from its URL.
SLUG="$(git -C "$WORKTREE" remote get-url origin | sed -E 's#\.git$##; s#.*[:/]([^/]+/[^/]+)$#\1#')"

# pr_number <branch>: the open PR from <branch> into main, empty if there is none.
pr_number() {
  gh api "repos/$SLUG/pulls?head=${SLUG%%/*}:$1&base=main&state=open" --jq '.[0].number // empty'
}

# pr_ready <number>: marks a draft PR ready for review. REST has no call for this; cloud
# sessions have their own route for it, and on the PC gh pr ready works.
pr_ready() {
  [ "$(gh api "repos/$SLUG/pulls/$1" --jq .draft)" = true ] || return 0
  gh api -X POST "repos/$SLUG/pulls/$1/ccr/ready_for_review" >/dev/null || gh pr ready "$1" -R "$SLUG"
}

# run_fallback <mode>: runs fallback.yml on BRANCH in mode build or check and waits for it.
# Sets RUN (the run id) and RUN_URL. Returns 1 if the run failed; stops if it can't be found.
run_fallback() {
  local mode="$1" sha since i
  sha="$(git -C "$WORKTREE" rev-parse HEAD)"
  since="$(date -u -d '1 minute ago' +%Y-%m-%dT%H:%M:%SZ)"
  gh workflow run fallback.yml -R "$SLUG" --ref "$BRANCH" -f mode="$mode"
  RUN=""
  for i in $(seq 1 30); do
    sleep 4
    RUN="$(gh run list -R "$SLUG" --workflow fallback.yml --branch "$BRANCH" --event workflow_dispatch --limit 10 \
      --json databaseId,headSha,createdAt \
      --jq "[.[] | select(.headSha == \"$sha\" and .createdAt >= \"$since\")][0].databaseId // empty")"
    if [ -n "$RUN" ]; then break; fi
  done
  [ -n "$RUN" ] || die "Started the fallback workflow ($mode) but couldn't find its run."
  RUN_URL="https://github.com/$SLUG/actions/runs/$RUN"
  gh run watch -R "$SLUG" "$RUN" --exit-status --interval 15
}

# The full checks that passed on a commit are recorded on it as a GitHub commit status named
# full-check, by PC builds and by fallback.yml's check mode, so agent-merge can skip them.

# checked <sha>: true if the full checks already passed on <sha>.
checked() {
  [ -n "$(gh api "repos/$SLUG/commits/$1/status" \
    --jq '.statuses[] | select(.context == "full-check" and .state == "success") | .context')" ]
}

# mark_checked <sha> <description>: records that the full checks passed on <sha>.
mark_checked() {
  gh api -X POST "repos/$SLUG/statuses/$1" -f state=success -f context=full-check -f description="$2" >/dev/null
}

# reuse_check: true if HEAD merges main into a feature head that passed the full checks, and the
# merge brought in only files that can't change the build or the checks: the shared rules and
# scripts (not .agents/hooks/), Markdown files at the repo root, and docs/.
reuse_check() {
  git rev-parse -q --verify HEAD^2 >/dev/null || return 1
  checked "$(git rev-parse HEAD^1)" || return 1
  ! git diff --name-only HEAD^1 HEAD | grep -vE '^([^/]+\.md|docs/.*|\.agents/[^/]+)$' | grep -q .
}

# main_red: prints the link of the latest finished fallback run on main if it failed, else nothing.
main_red() {
  gh api "repos/$SLUG/actions/workflows/fallback.yml/runs?branch=main&event=push&status=completed&per_page=1" \
    --jq '.workflow_runs[0] | select(.conclusion == "failure") | .html_url'
}

# Sets BRANCH to the current branch, or stops if it isn't a feature branch.
require_feature_branch() {
  BRANCH="$(git -C "$WORKTREE" symbolic-ref --quiet --short HEAD)" || die "Not on a branch."
  case "$BRANCH" in
    main) die "On $BRANCH: run this on a feature branch." ;;
  esac
}
