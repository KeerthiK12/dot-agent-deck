#!/usr/bin/env bash
#
# Phase 1 of /verify-pr: check a contributor's PR out into its own worktree so
# the full check suite can run without colliding with other agents.
#
# Usage: setup.sh <pr-number> [--force]
#        setup.sh <pr-number> --baseline
#
# Deliberately a SIBLING of `.claude/skills/dot-ai-worktree-prd/create.sh`
# rather than a caller of it: that script exists to start NEW work, so it
# derives the branch name from `prds/<n>-*.md` and always branches from `main`.
# Reviewing a PR needs the opposite — a branch pinned to the contributor's head
# commit, fork included. The conventions are kept identical on purpose: same
# `../<repo>-<suffix>` path scheme, same validate-then-create ordering, same
# `KEY=value` / `ERROR=true` output contract, so both read the same way.
#
# `--baseline` creates a second, detached worktree at the merge-base. Use it to
# answer "does this check already fail without the PR?" before blaming the PR
# for a red result.
#
# Logs and metadata go under `<worktree>/target/verify-pr/`, which is gitignored
# (`/target`), so the worktree stays clean and `git worktree remove` never needs
# `--force` at teardown.

set -uo pipefail

if [ $# -lt 1 ]; then
  echo "ERROR=true"
  echo "MESSAGE=Usage: setup.sh <pr-number> [--force|--baseline]"
  exit 0
fi

pr="${1#\#}"
pr="${pr##*/}"
shift

force=false
baseline=false
for arg in "$@"; do
  case "$arg" in
    --force) force=true ;;
    --baseline) baseline=true ;;
    *)
      echo "ERROR=true"
      echo "MESSAGE=Unknown argument '$arg'"
      exit 0
      ;;
  esac
done

if ! [[ "$pr" =~ ^[0-9]+$ ]]; then
  echo "ERROR=true"
  echo "MESSAGE=Could not parse a PR number from '$1'"
  exit 0
fi

for tool in git gh; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "ERROR=true"
    echo "MESSAGE=${tool} is required"
    exit 0
  fi
done

if ! repo_root=$(git rev-parse --show-toplevel 2>&1); then
  echo "ERROR=true"
  echo "MESSAGE=Not in a git repository: ${repo_root}"
  exit 0
fi
repo_name=$(basename "$repo_root")

default_branch="main"
if ref=$(git symbolic-ref --quiet refs/remotes/origin/HEAD 2>/dev/null); then
  default_branch="${ref#refs/remotes/origin/}"
fi

# Refresh the default branch's remote-tracking ref with an explicit refspec so
# the later merge and merge-base are computed against current origin.
git fetch origin "+refs/heads/${default_branch}:refs/remotes/origin/${default_branch}" --quiet 2>/dev/null || true

# --- PR metadata -----------------------------------------------------------

# `authorAssociation` is REST-only, not a `gh pr view --json` field.
if ! pr_meta=$(gh pr view "$pr" --json headRefName,headRefOid,baseRefName,author,mergeable,mergeStateStatus,title --jq '
  "\(.headRefName)\t\(.headRefOid)\t\(.baseRefName)\t\(.author.login)\t\(.mergeable)\t\(.mergeStateStatus)\t\(.title)"
' 2>&1); then
  echo "ERROR=true"
  echo "MESSAGE=gh pr view $pr failed: ${pr_meta}"
  exit 0
fi

IFS=$'\t' read -r head_branch head_sha base_branch author mergeable merge_state title <<<"$pr_meta"
association=$(gh api "repos/{owner}/{repo}/pulls/${pr}" --jq '.author_association' 2>/dev/null || echo UNKNOWN)

# --- Baseline mode ---------------------------------------------------------

if [ "$baseline" = true ]; then
  base_path="../${repo_name}-pr-${pr}-base"
  # Fetched into FETCH_HEAD rather than a named remote-tracking ref on purpose:
  # a named ref would outlive the baseline worktree and quietly accumulate one
  # stale `origin/pr-<n>-head` per PR ever reviewed, which the teardown in
  # SKILL.md does not clean up. FETCH_HEAD is transient.
  if ! git fetch origin "refs/pull/${pr}/head" --quiet 2>&1; then
    echo "ERROR=true"
    echo "MESSAGE=Could not fetch refs/pull/${pr}/head"
    exit 0
  fi
  merge_base=$(git merge-base "origin/${default_branch}" FETCH_HEAD 2>/dev/null)
  if [ -z "$merge_base" ]; then
    echo "ERROR=true"
    echo "MESSAGE=Could not compute a merge-base for PR ${pr}"
    exit 0
  fi
  if [ -d "$base_path" ]; then
    echo "ERROR=true"
    echo "MESSAGE=Baseline worktree '${base_path}' already exists"
    exit 0
  fi
  if ! out=$(git worktree add --detach "$base_path" "$merge_base" 2>&1); then
    echo "ERROR=true"
    echo "MESSAGE=${out}"
    exit 0
  fi
  mkdir -p "${base_path}/target/verify-pr"
  echo "SUCCESS=true"
  echo "MODE=baseline"
  echo "BASELINE_PATH=${base_path}"
  echo "MERGE_BASE=${merge_base}"
  echo "NOTE=Run the same failing check here; if it fails at the merge-base too, the PR did not cause it."
  exit 0
fi

# --- Branch and path ------------------------------------------------------

worktree_path="../${repo_name}-pr-${pr}"

# Prefer the PR's own head-branch name: `/tag-release`'s cleanup detection
# matches merged PRs by `headRefName`, so a worktree named this way is found
# automatically after the PR merges — including after a squash merge, where the
# commits never land verbatim on main.
#
# Two hard exclusions, because the fetch below is a FORCED update of whatever
# branch is named here:
#
#   1. The default branch. A fork PR raised from the contributor's own `main`
#      reports `headRefName=main` (PR #360 is exactly this), and reusing that
#      name would overwrite the reviewer's local `main` with the PR head.
#   2. Any name that already exists locally. It belongs to somebody else's work.
#
# Either case falls back to `pr-<n>-verify`, which this skill owns by
# convention, so force-updating it is safe.
branch_name="$head_branch"
if [ "$branch_name" = "$default_branch" ] ||
  git show-ref --verify --quiet "refs/heads/${branch_name}" 2>/dev/null; then
  branch_name="pr-${pr}-verify"
fi

# Only the skill-owned fallback name may be recycled, and only on --force.
if git show-ref --verify --quiet "refs/heads/${branch_name}" 2>/dev/null && [ "$force" != true ]; then
  echo "ERROR=true"
  echo "BRANCH_NAME=${branch_name}"
  echo "MESSAGE=Branch '${branch_name}' already exists. Pass --force to reset it to PR ${pr}'s current head."
  exit 0
fi

# --- Validate (mirrors create.sh) ----------------------------------------

errors=()

if [ -d "$worktree_path" ]; then
  if [ "$force" = true ]; then
    if ! out=$(git worktree remove "$worktree_path" 2>&1); then
      errors+=("Could not remove existing worktree '${worktree_path}': ${out} (it has local changes — remove it by hand if they are disposable)")
    fi
  else
    errors+=("Worktree path '${worktree_path}' already exists (pass --force to recreate it, or review the existing one)")
  fi
fi

if git worktree list --porcelain 2>/dev/null | grep -q "^branch refs/heads/${branch_name}$"; then
  errors+=("Branch '${branch_name}' is checked out in another worktree")
fi

if [ ${#errors[@]} -gt 0 ]; then
  echo "ERROR=true"
  echo "BRANCH_NAME=${branch_name}"
  echo "WORKTREE_PATH=${worktree_path}"
  echo "ERRORS:"
  for err in "${errors[@]}"; do echo "  ${err}"; done
  exit 0
fi

# --- Fetch the PR head ----------------------------------------------------

# `refs/pull/<n>/head` is served by the BASE repo, so this works for forks with
# no extra remote — and it pins the review to the exact commit the PR proposes.
if ! out=$(git fetch origin "+refs/pull/${pr}/head:refs/heads/${branch_name}" 2>&1); then
  echo "ERROR=true"
  echo "MESSAGE=Could not fetch refs/pull/${pr}/head into ${branch_name}: ${out}"
  exit 0
fi

fetched_sha=$(git rev-parse "$branch_name" 2>/dev/null)
if [ "$fetched_sha" != "$head_sha" ]; then
  echo "WARNING=Fetched ${fetched_sha} but the API reports head ${head_sha}; the PR may have been pushed to mid-fetch. Re-run with --force."
fi

if ! out=$(git worktree add "$worktree_path" "$branch_name" 2>&1); then
  echo "ERROR=true"
  echo "MESSAGE=${out}"
  exit 0
fi

# Per `/worktree-prd` Step 3: local settings are untracked, so a fresh worktree
# starts without them. Skip silently when absent.
if [ -f "${repo_root}/.claude/settings.local.json" ]; then
  cp "${repo_root}/.claude/settings.local.json" "${worktree_path}/.claude/settings.local.json" 2>/dev/null || true
fi

out_dir="${worktree_path}/target/verify-pr"
mkdir -p "${out_dir}/logs"

merge_base=$(git merge-base "origin/${default_branch}" "$branch_name" 2>/dev/null)

# --- Verify the MERGE RESULT, not the bare head ---------------------------

# CI tests the merge commit, so a PR that is green in isolation can still break
# main. Merging here reproduces what will actually land.
merge_result="clean"
merge_output=$(git -C "$worktree_path" -c user.name="verify-pr" -c user.email="verify-pr@local" \
  merge --no-edit "origin/${default_branch}" 2>&1)
merge_status=$?
if [ $merge_status -ne 0 ]; then
  merge_result="conflict"
  git -C "$worktree_path" merge --abort 2>/dev/null || true
fi

behind=$(git rev-list --count "${branch_name}..origin/${default_branch}" 2>/dev/null || echo unknown)

{
  echo "PR_NUMBER=${pr}"
  echo "PR_TITLE=${title}"
  echo "PR_AUTHOR=${author}"
  echo "PR_AUTHOR_ASSOCIATION=${association}"
  echo "PR_HEAD_SHA=${head_sha}"
  echo "PR_BASE_BRANCH=${base_branch}"
  echo "PR_HEAD_BRANCH=${head_branch}"
  echo "BRANCH_NAME=${branch_name}"
  echo "WORKTREE_PATH=${worktree_path}"
  echo "DEFAULT_BRANCH=${default_branch}"
  echo "MERGE_BASE=${merge_base}"
  echo "MERGE_RESULT=${merge_result}"
  echo "COMMITS_BEHIND_MAIN=${behind}"
  echo "GH_MERGEABLE=${mergeable}"
  echo "GH_MERGE_STATE=${merge_state}"
} | tee "${out_dir}/meta.env"

echo "OUT_DIR=${out_dir}"
if [ "$merge_result" = "conflict" ]; then
  echo "MERGE_CONFLICT_OUTPUT<<EOF"
  echo "$merge_output"
  echo "EOF"
  echo "NOTE=origin/${default_branch} does NOT merge cleanly; the merge was aborted and the worktree sits at the PR head. Checks below therefore describe the head, not what would land."
fi
echo "SUCCESS=true"
