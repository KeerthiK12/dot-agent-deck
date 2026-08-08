#!/usr/bin/env bash
set -euo pipefail

# Apply (or remove) the `main` branch ruleset that requires changes to land via
# a reviewed pull request.
#
#   scripts/apply-branch-protection.sh status   # show what is configured now
#   scripts/apply-branch-protection.sh apply    # create/update the ruleset
#   scripts/apply-branch-protection.sh delete   # remove it (full override)
#
# READ docs/develop/governance.md BEFORE RUNNING `apply`. In particular:
#
#   1. `main` currently takes two direct pushes from CI — the changelog commit
#      in release.yml and the docs chart bump in docs-publish.yml. Both are
#      rejected with `GH006` under this ruleset unless the RELEASE_TOKEN secret
#      is set to an admin PAT. Applying this without that secret breaks the very
#      next release, exactly as it did for v0.35.6 (CLAUDE.md rule 8).
#   2. REQUIRE_CODE_OWNER_REVIEW must stay `false` until a second maintainer is
#      listed in .github/CODEOWNERS. A code owner cannot approve their own PR,
#      so a single-owner CODEOWNERS blocks every PR that owner opens.

REPO="${REPO:-vfarcic/dot-agent-deck}"
RULESET_NAME="main-protected"

# Require an approving review before merge. Set to 0 to require a PR but no
# approval (useful as a first step while there is only one maintainer).
REQUIRED_APPROVALS="${REQUIRED_APPROVALS:-1}"

# Require review from a .github/CODEOWNERS owner. Keep `false` until CODEOWNERS
# names two maintainers — see note 2 above.
REQUIRE_CODE_OWNER_REVIEW="${REQUIRE_CODE_OWNER_REVIEW:-false}"

# Repository-role bypass. Role id 5 is `admin`.
#
# `always` lets an admin (and any token acting as one, including the
# RELEASE_TOKEN PAT that CI uses) push directly. This is what keeps releases
# working. The cost is honest and worth stating: enforcement against the owner's
# own hands is then a matter of habit, not of mechanism. The stricter
# alternative is to drop this bypass and give CI a GitHub App token as the
# bypass actor instead — note that the default GITHUB_TOKEN *cannot* be a bypass
# actor on a user-owned repo (`422: Actor GitHub Actions integration must be
# part of the ruleset source or owner organization`).
ADMIN_BYPASS_MODE="${ADMIN_BYPASS_MODE:-always}"

usage() { sed -n '3,20p' "$0" >&2; exit 64; }

existing_ruleset_id() {
  gh api "repos/$REPO/rulesets" \
    --jq ".[] | select(.name == \"$RULESET_NAME\") | .id" 2>/dev/null || true
}

payload() {
  cat <<JSON
{
  "name": "$RULESET_NAME",
  "target": "branch",
  "enforcement": "active",
  "bypass_actors": [
    { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "$ADMIN_BYPASS_MODE" }
  ],
  "conditions": {
    "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] }
  },
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" },
    {
      "type": "pull_request",
      "parameters": {
        "required_approving_review_count": $REQUIRED_APPROVALS,
        "require_code_owner_review": $REQUIRE_CODE_OWNER_REVIEW,
        "dismiss_stale_reviews_on_push": true,
        "require_last_push_approval": false,
        "required_review_thread_resolution": true,
        "allowed_merge_methods": ["merge", "squash", "rebase"]
      }
    }
  ]
}
JSON
}

cmd_status() {
  echo "== rulesets on $REPO =="
  local listed
  # `gh api --jq` exits 0 on an empty result set, so `|| echo` never fires;
  # test the captured output instead.
  listed="$(gh api "repos/$REPO/rulesets" \
    --jq '.[] | "\(.id)  \(.name)  [\(.enforcement)]"' 2>/dev/null || true)"
  echo "${listed:-(none)}"
  local id
  id="$(existing_ruleset_id)"
  if [ -n "$id" ]; then
    echo
    echo "== rules in $RULESET_NAME =="
    gh api "repos/$REPO/rulesets/$id" --jq '.rules[] | .type' 2>/dev/null
    echo
    echo "== bypass actors =="
    gh api "repos/$REPO/rulesets/$id" \
      --jq '.bypass_actors[]? | "\(.actor_type) id=\(.actor_id) mode=\(.bypass_mode)"' 2>/dev/null
  fi
  echo
  echo "== RELEASE_TOKEN secret =="
  if gh secret list --repo "$REPO" 2>/dev/null | grep -q '^RELEASE_TOKEN'; then
    echo "set — CI can bypass"
  else
    echo "NOT SET — applying this ruleset will break the next release and /publish-docs"
  fi
}

cmd_apply() {
  if ! gh secret list --repo "$REPO" 2>/dev/null | grep -q '^RELEASE_TOKEN'; then
    echo "refusing to apply: RELEASE_TOKEN is not set on $REPO." >&2
    echo "release.yml and docs-publish.yml push directly to main; without an" >&2
    echo "admin PAT they will fail with GH006. See docs/develop/governance.md." >&2
    exit 1
  fi
  local id
  id="$(existing_ruleset_id)"
  if [ -n "$id" ]; then
    echo "updating ruleset $id ($RULESET_NAME)"
    payload | gh api --method PUT "repos/$REPO/rulesets/$id" --input - >/dev/null
  else
    echo "creating ruleset $RULESET_NAME"
    payload | gh api --method POST "repos/$REPO/rulesets" --input - >/dev/null
  fi
  echo "done."
  cmd_status
}

cmd_delete() {
  local id
  id="$(existing_ruleset_id)"
  if [ -z "$id" ]; then echo "no ruleset named $RULESET_NAME"; return 0; fi
  gh api --method DELETE "repos/$REPO/rulesets/$id"
  echo "deleted ruleset $id ($RULESET_NAME)"
}

case "${1:-}" in
  status) cmd_status ;;
  apply)  cmd_apply ;;
  delete) cmd_delete ;;
  *)      usage ;;
esac
