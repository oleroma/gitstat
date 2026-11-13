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
# Set by agent-build pc: the file its build window makes when Olex cancels the build there.
CANCEL_FILE=""
cancelled() { [ -n "$CANCEL_FILE" ] && [ -f "$CANCEL_FILE" ]; }

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
  if cancelled; then
    word=cancelled
    set -- "Olex cancelled the build in its window."
  fi
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
  if [ "$FINISHED" = 0 ] && cancelled; then finish cancelled; fi
  if [ "$FINISHED" = 0 ]; then
    local out
    out="$(printf 'result: failed\nUnexpected error (exit %s); the last command in the log failed.\nlog: %s' "$code" "$LOG_FILE")"
    printf '%s\n' "$out"
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

# run_fallback [checks]: runs fallback.yml on BRANCH and waits for it: the same quick tests and
# build as a PC build, or with "checks" only the full checks (agent-check ci). Sets RUN (the run
# id) and RUN_URL. Shows the run link at once, and a build's release lines (release_lines) as soon
# as every build job has published. Returns 1 if the run failed; stops if it can't be found.
run_fallback() {
  local sha since i checks=false
  if [ "${1:-}" = checks ]; then checks=true; fi
  sha="$(git -C "$WORKTREE" rev-parse HEAD)"
  wait_main_cache
  since="$(date -u -d '1 minute ago' +%Y-%m-%dT%H:%M:%SZ)"
  gh workflow run fallback.yml -R "$SLUG" --ref "$BRANCH" -f checks="$checks"
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
  # Shown right away (not only in the result), so the run can be followed while it builds.
  echo "CI run: $RUN_URL" >&3
  local published=0 state="" n
  while :; do
    sleep 15
    if [ "$published" = 0 ] && [ "$(fallback_published "$RUN")" = true ]; then
      published=1
      n="$(newest_build_tag "$sha")"
      if [ -n "$n" ]; then release_lines "$n" >&3; fi
    fi
    state="$(gh api "repos/$SLUG/actions/runs/$RUN" --jq '"\(.status) \(.conclusion // "")"')"
    case "$state" in completed*) break ;; esac
  done
  [ "$state" = "completed success" ]
}

# fallback_published <run>: prints true once every build job of the run that isn't skipped has
# passed its "Publish" step (and at least one has).
fallback_published() {
  gh api "repos/$SLUG/actions/runs/$1/jobs" --jq '[.jobs[] | select(.conclusion != "skipped")
    | {p: ([.steps[]? | select(.name | startswith("Publish"))][0]), started: ((.steps | length) > 0 or .status == "completed")}]
    | ((map(select(.p != null)) | length > 0 and all(.p.conclusion == "success")) and all(.started))'
}

# newest_build_tag <sha>: the highest n whose build/<n> tag is on <sha>, or nothing.
newest_build_tag() {
  git -C "$WORKTREE" ls-remote --tags origin 'refs/tags/build/*' |
    sed -nE "s#^$1\s+refs/tags/build/([0-9]+)\$#\1#p" | sort -n | tail -n 1
}

# release_lines <n>: the build/<n> pre-release's link and a direct download link for each file.
release_lines() {
  local url="https://github.com/$SLUG/releases/tag/build/$1"
  echo "Published build $(gh api "repos/$SLUG/releases/tags/build/$1" --jq .name): $url"
  gh api "repos/$SLUG/releases/tags/build/$1" --jq '.assets[] | "\(.name): direct download link \(.browser_download_url)"'
}

# The full checks run once per feature, before the merge (agent-check on the PC; a CI build also
# runs them). When they pass, the commit gets a GitHub commit status named full-check, so nothing
# runs them again on that commit.

# checked <sha>: true if the full checks already passed on <sha>.
checked() {
  [ -n "$(gh api "repos/$SLUG/commits/$1/status" \
    --jq '.statuses[] | select(.context == "full-check" and .state == "success") | .context')" ]
}

# mark_checked <sha> <description>: records that the full checks passed on <sha>.
mark_checked() {
  gh api -X POST "repos/$SLUG/statuses/$1" -f state=success -f context=full-check -f description="$2" >/dev/null
}

# report_pr <what> <result text>: posts a PC script's result, whatever it is, as a comment on the
# open PR whose head is this commit ("PC build of <sha> finished: ..."). The thread agent watches
# its PR, so the comment wakes it even when the Remote Control report never reaches it.
report_pr() {
  local sha pr
  sha="${SHA:-$(git -C "$WORKTREE" rev-parse HEAD)}"
  pr="$(gh api "repos/$SLUG/pulls?state=open&per_page=100" --jq ".[] | select(.head.sha == \"$sha\") | .number" | head -n 1)"
  [ -n "$pr" ] || return 0
  gh api -X POST "repos/$SLUG/issues/$pr/comments" -f body="$(printf '%s of %s finished:\n\n```\n%s\n```\n\n_Posted by %s._' "$1" "${sha:0:7}" "$2" "$SCRIPT_NAME pc")" >/dev/null
}

# One PC build or check at a time, across all repos: the lock is a file in D:\dev, next to the
# repos, holding "<PID> <repo> <branch>" of the run that has it. A run waits while another one has
# it, and takes it over when that process is gone.
LOCK="$(dirname "$MAIN_REPO")/.build-lock"

# take_lock <branch>: waits for the lock and takes it. When agent-build pc <branch> or
# agent-check pc <branch> already took it before checking out build/src, the run there takes it over.
take_lock() {
  local owner pid shown=""
  if [ -n "${AGENT_LOCKED:-}" ]; then
    echo "$$ ${MAIN_REPO##*/} $AGENT_LOCKED" >"$LOCK"
    return 0
  fi
  until (set -C; echo "$$ ${MAIN_REPO##*/} $1" >"$LOCK") 2>/dev/null; do
    owner="$(cat "$LOCK" 2>/dev/null || true)"
    pid="${owner%% *}"
    if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then
      rm -f "$LOCK"
      continue
    fi
    if [ -n "$owner" ] && [ "$owner" != "$shown" ]; then
      echo "Waiting: another build is running (${owner#* })." >&3
      shown="$owner"
    fi
    sleep 5
  done
}

# release_lock: removes the lock if this run has it.
release_lock() {
  if [ "$(cut -d' ' -f1 "$LOCK" 2>/dev/null)" = "$$" ]; then rm -f "$LOCK"; fi
}

# checkout_src <branch>: takes the lock, then brings the build/src worktree (made the first time)
# to <branch>. The run in build/src takes the lock over.
checkout_src() {
  local src="$MAIN_REPO/build/src"
  take_lock "$1"
  cleanup() { release_lock; }
  export AGENT_LOCKED="$1"
  if [ -e "$src/.git" ]; then
    git -C "$src" checkout --detach "origin/$1" || die "Couldn't check out $1 in build/src."
  else
    git -C "$MAIN_REPO" worktree add --detach "$src" "origin/$1" || die "Couldn't make build/src."
  fi
}

# Olex tests changes to the pipeline himself (AGENTS.md, "Scripts and hooks"), so a branch that
# changes only pipeline files or documentation since main isn't built.

# pipeline_file <path>: true for a file no build uses: the pipeline (.agents/, .github/workflows/),
# documentation and rules (*.md files and docs/), and git and agent settings.
pipeline_file() {
  case "$1" in
    .agents/* | .github/workflows/* | *.md | docs/* | .gitignore | .gitattributes | .claude/* | LICENSE*) return 0 ;;
    *) return 1 ;;
  esac
}

# branch_changes: sets CHANGES to the files this branch's own commits changed since main, one per
# line, without what merges of main brought in. Call it directly, not in $( ): a subshell would
# lose CHANGES.
branch_changes() {
  local base
  base="$(git merge-base HEAD origin/main)"
  CHANGES="$(git log --first-parent --no-merges --format= --name-only "$base..HEAD" | grep -v '^$' | sort -u || true)"
}

# only_pipeline_changes: true if the branch changed something and all of it, since main, is
# pipeline files or documentation (branch_changes). Sets CHANGES to that list. A branch that also
# changes code is built every time, even when its newest commits touch only pipeline files.
only_pipeline_changes() {
  local f
  branch_changes
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
