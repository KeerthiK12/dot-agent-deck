# Governance: maintainers and the protected `main`

This page describes how changes reach `main`, who may approve them, and — because the two are inseparable here — why turning the gate on requires a CI change first. It is maintainer-facing and deliberately unpublished (CLAUDE.md rule 11).

## The model

`main` is protected by a repository ruleset named `main-protected`. Every change lands through a pull request with at least one approving review. A maintainer's own pull request is reviewed by another maintainer. The repository owner retains ownership and can override anything, either by holding the `admin` bypass or by disabling the ruleset outright.

Two properties of that arrangement are worth stating plainly rather than discovering later.

**The admin bypass is what keeps releases alive, and it also softens the rule for the owner.** CI pushes two commits directly to `main`, so *something* has to be allowed past the gate. Granting the bypass to the `admin` repository role covers CI's PAT and, unavoidably, covers the owner's own hands at the same time. Enforcement against the owner is therefore a matter of habit, not of mechanism. The stricter arrangement — no admin bypass, with a GitHub App token as the sole bypass actor — is available and is described under [Making the gate bind the owner too](#making-the-gate-bind-the-owner-too).

**A gate needs two maintainers before it means anything.** With a single maintainer, "requires one approving review" means every pull request that maintainer opens is blocked with nobody able to approve it, so every merge becomes a bypass and the rule decays into ceremony within a week. The rollout below is sequenced around that fact.

## Why CI has to change first

Two workflows push straight to `main`:

- `.github/workflows/release.yml` — the changelog commit, in the `prepare` job
- `.github/workflows/docs-publish.yml` — the docs chart bump, in the `publish` job

Neither push carries check runs, and the default `GITHUB_TOKEN` is not an admin. Under a protected `main` both are rejected with `GH006: Protected branch update failed`, which kills the tag in `prepare` and breaks standalone `/publish-docs` runs. This is not hypothetical: it is precisely what happened to v0.35.6 when required status checks were briefly enabled, and it is why `main` carries no protection at all today (CLAUDE.md rule 8).

The fix is a `RELEASE_TOKEN` secret holding a fine-grained PAT with **Contents: read and write** on this repository, owned by an account with admin access. Both workflows now pass it to `actions/checkout`:

```yaml
token: ${{ secrets.RELEASE_TOKEN || github.token }}
```

The `|| github.token` fallback means the workflows behave exactly as they do today while the secret is unset. That is deliberate — it makes the workflow change safe to merge before any protection exists, so the two steps can be sequenced independently.

Note that `GITHUB_TOKEN` **cannot** be named as a ruleset bypass actor on a user-owned repository; the API rejects it with `422: Actor GitHub Actions integration must be part of the ruleset source or owner organization`. A PAT or a GitHub App is the only route.

## Rollout

The order matters. Each step is safe to stop at.

**1. Merge the plumbing.** The `token:` change, `.github/CODEOWNERS`, `scripts/apply-branch-protection.sh`, and this page. Nothing is enforced yet and nothing changes behaviour.

**2. Create the PAT and set the secret.** A fine-grained PAT scoped to this repository with Contents: read and write, stored as `RELEASE_TOKEN`. Verify with `scripts/apply-branch-protection.sh status`, which reports whether the secret is visible.

**3. Cut one release with the secret in place but no ruleset.** This proves the PAT path works while the safety net is still down. If the tag completes and the chart bump lands, the token is correct.

**4. Onboard the second maintainer.** Grant `write` (or `maintain`) access, then add them to every line of `.github/CODEOWNERS`. Do not skip ahead to step 5 with one name in that file.

**5. Apply the ruleset.**

```bash
scripts/apply-branch-protection.sh apply
```

The script refuses to run if `RELEASE_TOKEN` is unset. It defaults to one approving review and to `require_code_owner_review: false`; flip the latter on with `REQUIRE_CODE_OWNER_REVIEW=true` once CODEOWNERS names two people.

**6. Reconsider required status checks.** They were removed for the same `GH006` reason and can now come back, since the PAT bypasses them too. Add them to the ruleset's `rules` array as a `required_status_checks` entry once step 3 has proven the token.

## What is gated, and what is not

`.github/CODEOWNERS` scopes required review to the surfaces where a mistake breaks interoperability between an older and a newer build: the daemon, the TUI↔daemon protocol, orchestration, hooks, plus the release workflows and `scripts/`. That is the same surface CLAUDE.md rule 12 already singles out for the cross-version contract check.

Docs, tests, changelog fragments and chores are deliberately outside it. The gate exists to catch protocol breaks that ship silently behind a stable wire, not to add a round trip to a typo fix.

## Making the gate bind the owner too

If the honour-system caveat above is unacceptable, replace the admin bypass with a GitHub App:

1. Create a GitHub App owned by the repository owner, with **Contents: read and write**, and install it on this repository.
2. Have CI mint an installation token (for example with `actions/create-github-app-token`) and pass that to `actions/checkout` instead of `RELEASE_TOKEN`.
3. Re-run the script with `ADMIN_BYPASS_MODE` removed from the payload and the App added as the sole `bypass_actors` entry (`actor_type: "Integration"`).

The owner can still override at any time by editing or deleting the ruleset — `scripts/apply-branch-protection.sh delete` — but the override becomes a deliberate, audit-logged act rather than an invisible one. That friction is the entire point.

## Emergency override

```bash
scripts/apply-branch-protection.sh delete   # remove the gate
scripts/apply-branch-protection.sh apply    # put it back
```

Both are recorded in the repository audit log.
