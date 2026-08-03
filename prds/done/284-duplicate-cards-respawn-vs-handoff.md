# PRD #284: Duplicate cards on one pane — Pi respawn vs scheduler handoff

**Status**: Complete (2026-08-01) — [PR #314](https://github.com/vfarcic/dot-agent-deck/pull/314) merged as `d8a8900c88d83f3ee4ae9389e6b211604094813f` (a merge commit, history preserved); issue #284 closed automatically on merge via its `Closes #284` link. All items in the approved test plan and Definition of Done landed: `status/agent-event/005`/`006` restored, the new `status/supersede/*` sub-area created (`/001`–`/005`, `/007`; `/006` was written then removed as logically unsatisfiable — see below), `scheduler/live/004` and `prompt/close-confirm/005` both green, and the full `cargo test-e2e` tier green (2724 run / 2723 passed, the single failure independently ruled a flake unrelated to this change). Fast tier 1410/1410, linkage-check clean, fmt/clippy clean, and the CLAUDE.md rule 12 cross-version manual test passed (this branch's TUI against a v0.35.3 daemon, both at attach protocol 6). Classified patch/bugfix: no `PROTOCOL_VERSION` bump, no `.breaking.md`.
**Priority**: High
**Created**: 2026-07-30
**GitHub Issue**: [#284](https://github.com/vfarcic/dot-agent-deck/issues/284) — **the spec of record**
**Related**: PRD #110 (session reuse vs `clear=true` respawn — the same seam, one door down), PRD #127 finding #2 (`scheduler/live/004`), PRD #241 (`prompt/close-confirm/005`, the collateral damage)
**Commits that matter**: `78f92b6` (the fix that traded one bug for the other), `8f579dd` (its revert), `242b725` (deleted the two catalog ids the revert orphaned)

## Problem Statement

[Issue #284](https://github.com/vfarcic/dot-agent-deck/issues/284) is the **spec of record** for this work. The bisect evidence, the dead ends already ruled out, the collateral-damage analysis, and the Definition of Done all live there, and are deliberately **not** duplicated into this file — read the issue before touching code. This PRD exists so the knowledge is in-repo (CLAUDE.md rule 13) and so `/prd-done`, `/prd-update-progress` and `/prd-next` have a file to work against.

The trap: two duplicate-card bugs sit at the **same seam** — the retire predicate in `AppState::apply_event` (`src/state.rs`) — and the obvious fix for either one **causes the other**. Both symptoms are indistinguishable on screen (two cards claiming one pane), so it is easy to widen the predicate, watch the symptom disappear, and ship the same defect reached from the other direction. `78f92b6` did exactly that: it fixed case A, broke case B (`scheduler/live/004`) plus an unrelated-looking third test (`prompt/close-confirm/005`), and was reverted by `8f579dd` — which returns case A to being an open bug with its guard tests deleted. **A fix is only correct if it satisfies both cases simultaneously**, which is why the test plan below restores the case-A guards *and* pins case B at state level before the predicate is touched at all.

## The two cases

| | Trigger | Agent type | First frame carrying the new `agent_id` |
|---|---|---|---|
| **A — Pi respawn** | `clear = true` delegate respawns a worker | Pi | `Thinking` / `Idle` (Pi never sends `SessionStart`) |
| **B — scheduler handoff** | synthetic placeholder superseded by the agent's real hook | ClaudeCode / OpenCode | `SessionStart` (`Some(agent_id)` replacing the placeholder's `None`) |

## Approved test plan (user-signed-off)

| # | Catalog ID | Tier | Action |
|---|---|---|---|
| 1 | `status/agent-event/005` | Fast L1 (`AppState::apply_event`) | restore verbatim from `78f92b6` + re-add catalog entry |
| 2 | `status/agent-event/006` | Fast L1 | restore verbatim from `78f92b6` + re-add catalog entry |
| 3 | `status/supersede/001` (NEW sub-area) | Fast L1 | create — case B at state level |
| 4 | `status/supersede/002` (NEW sub-area) | Fast L1 | create — armed-close-target-vanished at state level |
| 5 | `scheduler/live/004` | L2 PTY, existing | modify comments only — correct the stale `RED today:` text |
| 6 | `prompt/close-confirm/005` | L2 PTY, existing | untouched; must stay green |
| 7 | full `cargo test-e2e` | whole L2 tier | must be green in full, not a filtered subset |

Items 3–4 go in a **new `status/supersede/*` sub-area** rather than extending `status/agent-event/*`: case B is a scheduler-placeholder handoff, not an agent-event concern, and a distinct prefix keeps `cargo test-fast status_supersede` targeted.

### Toggle verification: what each test is RED against

Every one of the four fast-tier tests must be shown RED against a tree where it *should* fail. Case B is green today, so a case-B test that passes on first run proves nothing, and toggling in one direction only is precisely how `78f92b6` shipped its defect despite having tests. But the tree each test is RED against is **not** the same for all four, and that per-test matrix — not a blanket "all four RED against the reverted tree" — is the contract:

| Catalog ID | RED against | What it actually guards |
|---|---|---|
| `status/agent-event/005` | the current (reverted) tree | A true case-A reproduction: a Pi respawn whose first frame is `Thinking`, not `SessionStart`, leaves two cards stacked on one pane under the reverted `SessionStart`-only predicate. |
| `status/agent-event/006` | **not** the reverted tree — it is GREEN there. RED only under a predicate widened **without** a monotonicity (`timestamp >= last_activity`) check. | A **sensitivity guard against an unguarded widening**, not a reproduction. Restored verbatim, it asserts only that the LIVE card survives a delayed straggler from the outgoing generation; under the old `SessionStart`-only predicate a delayed `Idle` never retires that live card, so the assertion holds and the test passes. It goes RED the moment the predicate is widened to non-start frames with no ordering guard. |
| `status/supersede/001` | the naive `78f92b6` predicate (temporarily applied, then reverted) | Case B at state level. GREEN on the reverted tree — case B is not broken today. |
| `status/supersede/002` | the naive `78f92b6` predicate (temporarily applied, then reverted) | The armed-close-target-vanished seam at state level. GREEN on the reverted tree, for the same reason. |

**Correction to the signed-off matrix (2026-07-31).** This section previously required all four tests to be RED against the reverted tree. That is **false and not achievable** while *also* honouring test-plan items 1–2, which restore `/005` and `/006` **verbatim** from `78f92b6`. Verbatim `/006` asserts only the survival of the live card (it tolerates a stale sibling, which `tests/CATALOG.md` records explicitly), and that assertion already holds on the reverted tree. The tester measured this and the reviewer independently confirmed it. Two ways out existed: rewrite `/006` so it fails on the reverted tree, or keep it verbatim and accept that it guards a different axis. **Restoring verbatim was chosen** — `/006` is the exact guard `78f92b6` wrote against its own widening, and its value is precisely that it pins the ordering rule any future widening must carry. The claim is corrected in place rather than deleted so the record shows it was caught and resolved; a silently-corrected acceptance criterion is how the next reader loses the thread.

## Definition of done

Copied from the issue so progress is trackable here:

- [x] A fast-tier reproduction of case A that fails without a fix (`status/agent-event/005`)
- [x] Case-A tests restored from `78f92b6` (`status/agent-event/005`, `/006`)
- [x] `scheduler/live/004` still green
- [x] `prompt/close-confirm/005` still green
- [x] Full `cargo test-e2e` green, not a filtered subset (2724 run / 2723 passed; the one failure ruled a flake, not a #284 regression)
- [x] The stale `RED today:` comment on `scheduler/live/004` corrected, so it stops misleading the next reader

## Shipped, but not fixed here — follow-ups filed

A known hazard ships unfixed, on purpose, and is disclosed in the PR body rather than buried: close confirmation arms on the session id alone (`CloseTarget::Session`) and resolves by direct key lookup. Because Pi's producer key `{pane_id}-session` is stable across respawns, an armed target stays resolvable across a generation change, so confirming can act on whichever generation currently occupies the pane. This predates #284 and is neither introduced nor worsened by it — before the fix the key resolved to a stale corpse entry, after it resolves to the live replacement, and in both cases it maps to the pane's current card. The #284 identity refresh is a prerequisite for fixing it properly (arming on generation = session id + agent id), because only now is `agent_id` refreshed in place so a generation change is detectable.

A test, `status/supersede/006`, was written for this during review and then removed as logically unsatisfiable against `status/supersede/005` — it demanded the same `HashMap` key be simultaneously present and absent. Its gap is documented on `/005`'s `tests/CATALOG.md` entry rather than left silent.

Five follow-up issues were filed from the review and the surrounding audit sweep, deliberately scoped out of this PR to keep its blast radius contained:

- [#317](https://github.com/vfarcic/dot-agent-deck/issues/317) — Close confirmation arms on session id alone, so it can act on a replacement generation (the hazard above; highest value).
- [#318](https://github.com/vfarcic/dot-agent-deck/issues/318) — Daemon does not bind hook-event provenance: any same-user process can drive another pane.
- [#319](https://github.com/vfarcic/dot-agent-deck/issues/319) — Hook ingestion has no line-length or connection bound.
- [#320](https://github.com/vfarcic/dot-agent-deck/issues/320) — Retire path needs a per-pane generation discriminator, not a timestamp.
- [#321](https://github.com/vfarcic/dot-agent-deck/issues/321) — Two unverified assumptions in the card supersession path (`started_at` reset on a same-key respawn; site-2 same-producer refresh doesn't verify the pane match).

## Blast radius

The retire predicate reaches **pane-close semantics**, not just cards: close confirmation is keyed on session identity, so whether an armed close target counts as "vanished" depends on exactly when a superseded session is retired. That is how `78f92b6` broke `prompt/close-confirm/005` (`tests/e2e_pane_close.rs`), a test that looks unrelated and was initially misattributed to PRD #241's own feature. Neither guard runs in CI — `cargo test-e2e` is the pre-PR tier per CLAUDE.md rule 5 — which is why a commit could break two e2e tests and still merge. **Any fix here must run the FULL e2e tier, not a filtered subset.**
