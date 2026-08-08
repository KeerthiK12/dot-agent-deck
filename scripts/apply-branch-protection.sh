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
#   2. REQUIRED_APPROVALS=1 needs a second maintainer to be useful. Nobody can
#      approve their own pull request, so while there is one collaborator every
#      pull request that person opens is unmergeable without a bypass. Onboard
#      the second maintainer (MAINTAINERS.md) before applying with approvals=1,
#      or apply with REQUIRED_APPROVALS=0 to require a PR but not a review.

REPO="${REPO:-vfarcic/dot-agent-deck}"
# Overridable, like every other tunable here. existing_ruleset_id's ambiguity
# error tells the operator to set this to disambiguate duplicate rulesets, so an
# unconditional assignment would make that documented recovery path a dead end.
RULESET_NAME="${RULESET_NAME:-main-protected}"

# Require an approving review before merge. Set to 0 to require a PR but no
# approval (useful as a first step while there is only one maintainer).
#
# GitHub counts an approval only from an account with write or admin permission,
# so "one approving review" already means "a maintainer approved" — the
# collaborator list is the maintainer list (MAINTAINERS.md). That is also why
# there is no CODEOWNERS file: code owners route review to different people for
# different paths, and with one shared maintainer set there is nothing to route.
REQUIRED_APPROVALS="${REQUIRED_APPROVALS:-1}"

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

usage() { sed -n '4,22p' "$0" >&2; exit 64; }

# Echo the id of the `$RULESET_NAME` ruleset, or nothing if it does not exist.
#
# Deliberately no `2>/dev/null || true`: an auth, rate-limit or network failure
# must not be indistinguishable from "no such ruleset". `cmd_delete` treats an
# empty id as "nothing to remove" and reports success — if a failed lookup
# produced that empty string, the emergency override would claim to have lifted
# protection that is in fact still active. Callers assign on their own line
# (`local id` then `id="$(…)"`), so a non-zero return here propagates under
# `set -e` rather than being masked by `local`'s own exit status.
existing_ruleset_id() {
  local out count
  if ! out="$(gh api "repos/$REPO/rulesets" \
      --jq ".[] | select(.name == \"$RULESET_NAME\") | .id")"; then
    echo "error: could not list rulesets on $REPO." >&2
    echo "Refusing to guess whether $RULESET_NAME exists — check auth (gh auth status)," >&2
    echo "rate limits, and network, then retry." >&2
    return 1
  fi
  # GitHub identifies rulesets by numeric id and does not require names to be
  # unique, so this filter can match more than one. Returning them all would
  # interpolate `123\n456` into a URL; refusing is better than picking one,
  # because updating or deleting a single match while a same-named sibling keeps
  # enforcing is the same false-success failure the error handling above exists
  # to prevent.
  count="$(printf '%s' "$out" | grep -c . || true)"
  if [ "$count" -gt 1 ]; then
    echo "error: $count rulesets on $REPO are named '$RULESET_NAME':" >&2
    while IFS= read -r rid; do
      [ -n "$rid" ] && echo "  id=$rid" >&2
    done <<<"$out"
    echo "Refusing to act on an ambiguous target. Remove or rename the duplicates in" >&2
    echo "Settings > Rules, or set RULESET_NAME to address a specific one." >&2
    return 1
  fi
  printf '%s' "$out"
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
        "require_code_owner_review": false,
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

# Echo "set" or "unset" for the RELEASE_TOKEN secret. Returns non-zero if the
# lookup itself failed, so callers can distinguish "the secret is missing" from
# "we could not find out" — the same distinction existing_ruleset_id preserves.
release_token_state() {
  local names
  if ! names="$(gh secret list --repo "$REPO" --json name --jq '.[].name')"; then
    return 1
  fi
  if printf '%s\n' "$names" | grep -qx 'RELEASE_TOKEN'; then
    echo set
  else
    echo unset
  fi
}

cmd_status() {
  echo "== rulesets on $REPO =="
  local listed
  # Two distinct outcomes that must not be conflated: `gh api --jq` exits 0 with
  # empty output when there are genuinely no rulesets, and non-zero when the
  # call failed. Printing "(none)" for the latter would report the branch as
  # unprotected when it may well be protected.
  if ! listed="$(gh api "repos/$REPO/rulesets" \
      --jq '.[] | "\(.id)  \(.name)  [\(.enforcement)]"')"; then
    echo "error: could not list rulesets on $REPO — protection state is UNKNOWN." >&2
    return 1
  fi
  echo "${listed:-(none)}"
  local id
  id="$(existing_ruleset_id)"
  if [ -n "$id" ]; then
    echo
    echo "== rules in $RULESET_NAME =="
    gh api "repos/$REPO/rulesets/$id" --jq '.rules[] | .type'
    echo
    echo "== bypass actors =="
    gh api "repos/$REPO/rulesets/$id" \
      --jq '.bypass_actors[]? | "\(.actor_type) id=\(.actor_id) mode=\(.bypass_mode)"'
  fi
  echo
  echo "== RELEASE_TOKEN secret =="
  local token_state
  if ! token_state="$(release_token_state)"; then
    echo "UNKNOWN — could not list secrets on $REPO. Do not apply until this resolves." >&2
    return 1
  fi
  if [ "$token_state" = set ]; then
    echo "set — CI can bypass"
  else
    echo "NOT SET — applying this ruleset will break the next release and /publish-docs"
  fi
}

cmd_apply() {
  local token_state
  if ! token_state="$(release_token_state)"; then
    echo "refusing to apply: could not determine whether RELEASE_TOKEN is set on" >&2
    echo "$REPO. Check auth and network — applying blind risks locking CI out of" >&2
    echo "main. See docs/develop/governance.md." >&2
    exit 1
  fi
  if [ "$token_state" != set ]; then
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
