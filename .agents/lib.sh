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

# Scripts redefine this to pass on the result (its full text as $1); it runs once, on every
# exit, after the result is printed. Its failure never changes the result.
report() { :; }

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
  report "$out" || true
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
    local out
    out="$(printf 'result: failed\nUnexpected error (exit %s); the last command in the log failed.\nlog: %s' "$code" "$LOG_FILE")"
    printf '%s\n' "$out" >&3
    FINISHED=1
    report "$out" || true
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

# wait_main_cache: when cache.yml is still compiling main, waits for it (at most 30 minutes),
# so the fallback run starts from main's warm caches instead of building cold. Never fails.
wait_main_cache() {
  local i
  for i in $(seq 1 60); do
    [ -n "$(gh run list -R "$SLUG" --workflow cache.yml --branch main --limit 5 --json status \
      --jq '[.[] | select(.status != "completed")][0].status // empty' 2>/dev/null)" ] || return 0
    if [ "$i" = 1 ]; then echo "Waiting for the main cache run to finish first." >&2; fi
    sleep 30
  done
  return 0
}

# run_fallback: runs fallback.yml on BRANCH (the same tests and build as a PC build) and waits
# for it. Sets RUN (the run id) and RUN_URL. Returns 1 if the run failed; stops if it can't be found.
run_fallback() {
  local sha since i
  sha="$(git -C "$WORKTREE" rev-parse HEAD)"
  wait_main_cache
  since="$(date -u -d '1 minute ago' +%Y-%m-%dT%H:%M:%SZ)"
  gh workflow run fallback.yml -R "$SLUG" --ref "$BRANCH"
  RUN=""
  for i in $(seq 1 30); do
    sleep 4
    RUN="$(gh run list -R "$SLUG" --workflow fallback.yml --branch "$BRANCH" --event workflow_dispatch --limit 10 \
      --json databaseId,headSha,createdAt \
      --jq "[.[] | select(.headSha == \"$sha\" and .createdAt >= \"$since\")][0].databaseId // empty")"
    if [ -n "$RUN" ]; then break; fi
  done
  [ -n "$RUN" ] || die "Started the fallback workflow but couldn't find its run."
  RUN_URL="https://github.com/$SLUG/actions/runs/$RUN"
  gh run watch -R "$SLUG" "$RUN" --exit-status --interval 15
}

# Every build, on the PC or in CI, runs the full checks once. When they pass, the commit gets a
# GitHub commit status named full-check, so nothing runs them again on that commit.

# checked <sha>: true if the full checks already passed on <sha>.
checked() {
  [ -n "$(gh api "repos/$SLUG/commits/$1/status" \
    --jq '.statuses[] | select(.context == "full-check" and .state == "success") | .context')" ]
}

# mark_checked <sha> <description>: records that the full checks passed on <sha>.
mark_checked() {
  gh api -X POST "repos/$SLUG/statuses/$1" -f state=success -f context=full-check -f description="$2" >/dev/null
}

# Olex tests changes to the pipeline himself (AGENTS.md, "Scripts and hooks"), so a branch whose
# changes since its last build are only pipeline files or documentation isn't built again.

# pipeline_file <path>: true for a file no build uses: the pipeline (.agents/, .github/workflows/)
# or documentation (*.md files and docs/).
pipeline_file() {
  case "$1" in .agents/* | .github/workflows/* | *.md | docs/*) return 0 ;; *) return 1 ;; esac
}

# unbuilt_changes: prints the files this branch's own commits changed after its newest commit
# whose full checks passed (since main when none has), one per line, without what merges of
# main brought in. Sets LAST_CHECKED to that commit, or empty.
unbuilt_changes() {
  local c base
  LAST_CHECKED=""
  base="$(git merge-base HEAD origin/main)"
  while read -r c; do
    if checked "$c"; then LAST_CHECKED="$c"; break; fi
  done < <(git rev-list --first-parent "$base..HEAD")
  git log --first-parent --no-merges --format= --name-only "${LAST_CHECKED:-$base}..HEAD" | grep -v '^$' | sort -u
}

# only_pipeline_changes: true if the branch changed something since its last build and all of it
# is pipeline files or documentation (unbuilt_changes). Sets CHANGES to that list.
only_pipeline_changes() {
  local f
  CHANGES="$(unbuilt_changes)"
  [ -n "$CHANGES" ] || return 1
  while IFS= read -r f; do
    pipeline_file "$f" || return 1
  done <<<"$CHANGES"
}

# Sets BRANCH to the current branch, or stops if it isn't a feature branch.
require_feature_branch() {
  BRANCH="$(git -C "$WORKTREE" symbolic-ref --quiet --short HEAD)" || die "Not on a branch."
  case "$BRANCH" in
    main) die "On $BRANCH: run this on a feature branch." ;;
    handoff/*) die "On $BRANCH, a handoff branch: it's merged into a feature branch with agent-take, or made one with agent-start." ;;
  esac
}

# stage_all: stages every change, and dies if build/ isn't gitignored.
stage_all() {
  git add -A
  if git diff --cached --name-only | grep -q '^build/'; then
    git reset -q -- build/ # only unstages; the files stay as they are
    die "Files under build/ were about to be committed, so build/ isn't gitignored. Tell Olex."
  fi
}

# conflict_markers <files...>: prints the files that still hold conflict markers.
conflict_markers() {
  local f
  for f in "$@"; do
    if [ -f "$f" ] && grep -qE '^(<<<<<<<|>>>>>>>)( |$)' "$f"; then echo "$f"; fi
  done
}
