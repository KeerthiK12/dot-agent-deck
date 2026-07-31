# PRD #284: Duplicate cards on one pane — Pi respawn vs scheduler handoff

**Status**: In progress (started 2026-07-30)
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

Every one of the four fast-tier tests must be shown RED against a tree where it *should* fail — items 1–2 against the current (reverted) tree, items 3–4 against a tree with the naive predicate temporarily applied (then reverted). Case B is green today, so a case-B test that passes on first run proves nothing. Toggling in one direction only is precisely how `78f92b6` shipped its defect despite having tests.

## Definition of done

Copied from the issue so progress is trackable here:

- [ ] A fast-tier reproduction of case A that fails without a fix
- [ ] Case-A tests restored from `78f92b6`
- [ ] `scheduler/live/004` still green
- [ ] `prompt/close-confirm/005` still green
- [ ] Full `cargo test-e2e` green, not a filtered subset
- [ ] The stale `RED today:` comment on `scheduler/live/004` corrected, so it stops misleading the next reader

## Blast radius

The retire predicate reaches **pane-close semantics**, not just cards: close confirmation is keyed on session identity, so whether an armed close target counts as "vanished" depends on exactly when a superseded session is retired. That is how `78f92b6` broke `prompt/close-confirm/005` (`tests/e2e_pane_close.rs`), a test that looks unrelated and was initially misattributed to PRD #241's own feature. Neither guard runs in CI — `cargo test-e2e` is the pre-PR tier per CLAUDE.md rule 5 — which is why a commit could break two e2e tests and still merge. **Any fix here must run the FULL e2e tier, not a filtered subset.**
