#!/usr/bin/env bash
# Shared helpers for the scripts in .agents/. Sourced by them, never run on its own.
# A script ends by printing "result: <word>" and a short summary. Everything
# else, including each command it runs, goes to its log in build/logs/.

set -euo pipefail
shopt -s inherit_errexit

AGENTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The repo, also when a PC run goes on from its copy of .agents/ in build/run/ (run_from_copy).
WORKTREE="$(git -C "$AGENTS_DIR" rev-parse --show-toplevel)"
MAIN_REPO="$WORKTREE"
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

# status <line>: a status line for Olex, with its emoji and Markdown links (AGENTS.md "Status
# lines"). The agent posts it, without "status: ", as soon as it shows.
status() { echo "status: $*" >&3; echo "status: $*"; }

# what_emoji: what this branch builds, as status lines show it: 🤖 an Android APK, 🪟 Windows
# files, both when it builds both. From the repo's parts hook when it has one, else from gradlew.
what_emoji() {
  local parts="" e=""
  if [ -f "$WORKTREE/.agents/hooks/parts" ]; then parts="$(bash "$WORKTREE/.agents/hooks/parts" 2>/dev/null || true)"; fi
  if [ -z "$parts" ]; then
    if [ -f "$WORKTREE/gradlew" ]; then echo 🤖; else echo 🪟; fi
    return 0
  fi
  if grep -qx apk <<<"$parts"; then e+=🤖; fi
  if grep -qvx apk <<<"$parts"; then e+=🪟; fi
  echo "$e"
}

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
    status "🛑 Build cancelled."
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

# gh_retry <gh args>: runs gh, trying again up to 4 times, 10 s apart, when it fails.
gh_retry() {
  local i
  for i in 1 2 3 4 5; do
    gh "$@" && return 0
    sleep 10
  done
  return 1
}

# wait_main_cache: when cache.yml is still compiling main, waits for it (at most 30 minutes),
# so the fallback run starts from main's warm caches instead of building cold. Never fails.
wait_main_cache() {
  local i
  for i in $(seq 1 60); do
    [ -n "$(gh run list -R "$SLUG" --workflow cache.yml --branch main --limit 5 --json status \
      --jq '[.[] | select(.status != "completed")][0].status // empty' 2>/dev/null)" ] || return 0
    if [ "$i" = 1 ]; then status "⏳ Waiting for main's cache run to finish first (up to 30 min)."; fi
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
  if [ "$checks" = true ]; then
    status "🔎 Running the full checks in CI: [CI run]($RUN_URL)"
  else
    status "🔨☁️$(what_emoji) Building in CI: [CI run]($RUN_URL)"
  fi
  local published=0 state="" n
  while :; do
    sleep 15
    if [ "$published" = 0 ] && [ "$(fallback_published "$RUN")" = true ]; then
      published=1
      n="$(newest_build_tag "$sha")"
      if [ -n "$n" ]; then
        status "📦 Build $n published: [build $n](https://github.com/$SLUG/releases/tag/build/$n)"
        release_lines "$n" >&3
      fi
    fi
    # A failed poll is tried again: one lost answer from GitHub doesn't end the build.
    state="$(gh_retry api "repos/$SLUG/actions/runs/$RUN" --jq '"\(.status) \(.conclusion // "")"')" ||
      die "Lost contact with GitHub while CI was running: $RUN_URL (the run goes on)."
    case "$state" in completed*) break ;; esac
  done
  [ "$state" = "completed success" ]
}

# failed_steps <run>: the run's failed jobs and steps, one "job: step" per line.
failed_steps() {
  gh api "repos/$SLUG/actions/runs/$1/jobs" --jq '.jobs[] | select(.conclusion == "failure")
    | "\(.name): \([.steps[]? | select(.conclusion == "failure") | .name] | join(", "))"' 2>/dev/null || true
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

# published_build <sha>: true if a build of <sha> was published: a build/<n> tag is on it and
# its pre-release has files. The tag alone isn't enough: build-number pushes it before the build
# runs, so a failed build leaves one too.
published_build() {
  local n
  while read -r n; do
    [ -n "$n" ] || continue
    [ "$(gh api "repos/$SLUG/releases/tags/build/$n" --jq '.assets | length' 2>/dev/null || echo 0)" -gt 0 ] && return 0
  done < <(git -C "$WORKTREE" ls-remote --tags origin 'refs/tags/build/*' |
    sed -nE "s#^$1\s+refs/tags/build/([0-9]+)\$#\1#p" | sort -rn)
  return 1
}

# main_build: the commit of the newest published build of main itself (agent-publish builds main
# with every part), or nothing. Those commits are on main's first-parent line; feature builds
# are on their branches' commits.
main_build() {
  local first sha
  first="$(git -C "$WORKTREE" rev-list --first-parent -n 200 origin/main)"
  while read -r sha; do
    grep -qxF "$sha" <<<"$first" || continue
    if published_build "$sha"; then echo "$sha"; return 0; fi
  done < <(git -C "$WORKTREE" ls-remote --tags origin 'refs/tags/build/*' |
    sed -nE 's#^([0-9a-f]+)\s+refs/tags/build/([0-9]+)$#\2 \1#p' | sort -rn | awk '{print $2}' | awk '!seen[$0]++')
}

# release_lines <n>: the build/<n> pre-release's link and a direct download link for each file.
release_lines() {
  local url="https://github.com/$SLUG/releases/tag/build/$1"
  echo "Published build $(gh api "repos/$SLUG/releases/tags/build/$1" --jq .name): $url"
  gh api "repos/$SLUG/releases/tags/build/$1" --jq '.assets[] | "\(.name): direct download link \(.browser_download_url)"'
}

# The full checks run once per feature, before the merge (agent-check, on the PC or in CI). When they pass, the commit gets a GitHub commit status named full-check, so nothing
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
# agent-check pc <branch> already took it before checking out the branch, the run of the branch's
# own scripts takes it over.
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
      # Move the dead owner's lock aside before removing it: only one waiter's mv succeeds, so a
      # waiter that read the same dead owner can't remove a lock another waiter just took.
      if [ "$(cat "$LOCK" 2>/dev/null || true)" = "$owner" ] && mv "$LOCK" "$LOCK.stale.$$" 2>/dev/null; then
        if [ "$(cat "$LOCK.stale.$$" 2>/dev/null || true)" = "$owner" ]; then
          rm -f "$LOCK.stale.$$"
        else
          mv -n "$LOCK.stale.$$" "$LOCK" 2>/dev/null || rm -f "$LOCK.stale.$$"
        fi
      fi
      continue
    fi
    if [ -n "$owner" ] && [ "$owner" != "$shown" ]; then
      status "⏳ Waiting: ${owner#* } is building."
      shown="$owner"
    fi
    sleep 5
  done
}

# release_lock: removes the lock if this run has it.
release_lock() {
  if [ "$(cut -d' ' -f1 "$LOCK" 2>/dev/null)" = "$$" ]; then rm -f "$LOCK"; fi
}

# run_from_copy <script> <args>...: a PC run of <script> (agent-build or agent-check) goes on from
# a fresh copy of .agents/ in build/run/<PID>-<n>, so checking out another commit in the repo never
# changes a script while bash is reading it (Windows can't replace a file bash has open). A copy
# holds a .run-copy file, so a run from it returns at once (comparing paths fails in Git Bash,
# where pwd says /d/dev/... and git says D:/dev/...). AGENTS_DIR="" run_from_copy makes a new copy
# anyway (after the checkout, for the branch's own scripts). Copies of ended runs, and this run's
# earlier copies, are removed. More than two copies in one run is a bug: it stops.
run_from_copy() {
  local script="$1" run d name mine=""
  shift
  if [ -n "$AGENTS_DIR" ] && [ -f "$AGENTS_DIR/.run-copy" ]; then return 0; fi
  AGENT_COPIES=$(( ${AGENT_COPIES:-0} + 1 ))
  export AGENT_COPIES
  [ "$AGENT_COPIES" -le 2 ] || die "run_from_copy made more than two copies of .agents in one run: a bug in the pipeline scripts."
  if [ -n "${AGENT_COPY_DIR:-}" ]; then mine="${AGENT_COPY_DIR##*/}"; fi
  mkdir -p "$BUILD_DIR/run"
  for d in "$BUILD_DIR"/run/*/; do
    [ -d "$d" ] || continue
    d="${d%/}"
    name="${d##*/}"
    if [ "$name" = "$mine" ]; then continue; fi
    if [ "${name%%-*}" = "$$" ] || ! kill -0 "${name%%-*}" 2>/dev/null; then rm -rf "$d" || true; fi
  done
  run="$(mktemp -d "$BUILD_DIR/run/$$-XXXXXX")" || die "Couldn't make a folder in $BUILD_DIR/run."
  cp -r "$WORKTREE/.agents" "$run/" || die "Couldn't copy .agents to $run."
  : >"$run/.agents/.run-copy"
  export AGENT_COPY_DIR="$run"
  exec bash "$run/.agents/$script" "$@" >&3 2>&3 3>&-
}

# checkout_branch <branch>: takes the lock, then checks out <branch> (fetched) in the repo, which
# nobody edits on the PC: edits happen in the cloud. Stops when tracked files have changes, so
# nothing is lost. Also fast-forwards the local main to origin/main, so the repo stays current
# after merges. The branch's own scripts then run from a fresh copy (run_from_copy) and take the
# lock over.
checkout_branch() {
  take_lock "$1"
  cleanup() { release_lock; }
  export AGENT_LOCKED="$1"
  local dirty
  dirty="$(git -C "$MAIN_REPO" status --porcelain --untracked-files=no)"
  [ -z "$dirty" ] || die "$MAIN_REPO has changes, so the branch isn't checked out over them:" "$dirty" \
    "Ask Olex what to do with them."
  # The old build/src worktree, from before builds ran in the repo itself.
  if [ -e "$MAIN_REPO/build/src/.git" ]; then git -C "$MAIN_REPO" worktree remove --force "$MAIN_REPO/build/src" || true; fi
  git -C "$MAIN_REPO" worktree prune
  git -C "$MAIN_REPO" checkout --detach "origin/$1" || die "Couldn't check out $1 in $MAIN_REPO."
  git -C "$MAIN_REPO" fetch -q origin main:main 2>/dev/null || true
}

# Olex tests changes to the pipeline himself (AGENTS.md, "Scripts and hooks"), so a branch that
# changes only pipeline files or documentation since main isn't built.

# pipeline_file <path>: true for a file no build uses: the pipeline (.agents/, .github/workflows/),
# documentation and rules (*.md files and docs/), and git and agent settings.
pipeline_file() {
  case "$1" in
    .agents/* | .github/workflows/* | *.md | docs/* | .gitignore | .gitattributes | .claude/* | .gitstat/* | LICENSE*) return 0 ;;
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

# Merges made with "agent-merge --unbuilt" are listed in .agents/unbuilt, one per line:
# "<commit> <parts> <PR number> <PR title>", where <commit> is the branch's last commit and
# <parts> the parts it changed, comma-separated ("all" in a repo without hooks/parts). Every
# build warns about the lines it doesn't build; a later feature's merge removes the lines its
# build covered.

# build_parts: the parts a build of this branch builds, comma-separated (hooks/parts), or "all".
build_parts() {
  if [ -f "$WORKTREE/.agents/hooks/parts" ]; then
    bash "$WORKTREE/.agents/hooks/parts" | paste -sd, -
  else
    echo all
  fi
}

# covers <commit> <parts> <unbuilt commit> <unbuilt parts>: true if a build of <commit> with
# <parts> built that unbuilt merge: <commit> has its changes, and <parts> has every part it changed.
covers() {
  local p
  git merge-base --is-ancestor "$3" "$1" 2>/dev/null || return 1
  for p in ${4//,/ }; do
    case ",$2," in *",$p,"*) ;; *) return 1 ;; esac
  done
}

# unbuilt_lines <ref>: the lines of .agents/unbuilt at <ref>.
unbuilt_lines() { git show "$1:.agents/unbuilt" 2>/dev/null | sed 's/[[:space:]]*$//' | grep -v '^$' || true; }

# warn_unbuilt <commit>: before a build of <commit>, a ⚠️ status line lists the unbuilt merges on
# main that this build doesn't build, and that no published build of main (agent-publish, every
# part) has built since.
warn_unbuilt() {
  local c p n title parts left="" pub
  git fetch -q origin main || true
  parts="$(build_parts)"
  pub="$(main_build)"
  while read -r c p n title; do
    [ -n "$c" ] || continue
    covers "$1" "$parts" "$c" "$p" && continue
    if [ -n "$pub" ] && git merge-base --is-ancestor "$c" "$pub" 2>/dev/null; then continue; fi
    left+="${left:+, }[PR #$n](https://github.com/$SLUG/pull/$n) $title ($p)"
  done < <(unbuilt_lines origin/main)
  [ -z "$left" ] || status "⚠️ Merged without a build and not in this one: $left"
}
