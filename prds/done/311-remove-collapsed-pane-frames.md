# PRD #311: Stop rendering non-focused agent panes as empty collapsed frames

**Status**: Complete — 2026-08-04
**Priority**: High
**Created**: 2026-08-01
**Completed**: 2026-08-04 (PR [#334](https://github.com/vfarcic/dot-agent-deck/pull/334), squashed as `f86c37b`)

## Problem Statement

In `PaneLayout::Stacked` — the default — the focused pane expands and every other pane in the tab collapses to a titled 1-row block with no content (`src/ui.rs:11803-11845`). The result is a border drawn around nothing, once per non-focused pane.

[#307](https://github.com/vfarcic/dot-agent-deck/issues/307) reported it from a 7-role orchestration: six rows of the terminal spent on the frames of `developer`, `tester`, `reviewer`, `releaser`, `researcher` and `documenter`, none of which shows anything. The reporter's framing was "can we get rid of frames in right pane so it gives bigger screen and easy to work with laptop screen?", followed by "Or even a shortcut to get rid of that part on demand will help eyes".

The rows buy nothing that is not already on screen. In an orchestration tab the left sidebar lists every role with live status, tool counts and click-to-focus — `docs/orchestration.md:171` sells exactly that: "The sidebar shows each role's status live (thinking, working, waiting, idle, error) so you can see at a glance who is busy without switching panes." The collapsed frame is a strictly poorer second copy of information the sidebar already carries, charged at one row each.

On a laptop, with 7 roles, this is roughly 13% of the vertical space.

## Solution Overview

Render only the focused pane in the pane column. Non-focused panes are not drawn at all.

**Nothing about agent lifecycle changes.** Every pane's PTY stays open, every agent keeps running, hooks keep arriving, delegation keeps routing, and the sidebar keeps showing live status for all of them. This is a rendering change and nothing more — a pane that is not drawn is still very much alive.

## Scope

### In Scope

- The `Stacked` arm of `render_terminal_panes` (`src/ui.rs:11803`): drop the collapsed-frame `else` branch.
- The layout maths that reserves those rows — `pane_stack_rects` / `stacked_expanded_index` (`src/ui.rs:11138-11203`) — so the focused pane actually receives the reclaimed height rather than leaving a gap.
- `resize_panes_to_layout`, so the focused pane's PTY is resized to the larger area and the agent reflows into it.
- Whatever the non-focused panes should be sized to when they are not on screen (see Open Questions).

### Out of Scope

- The pane column's 66% width split (`ORCHESTRATION_PANES_PERCENT`) and the sidebar — [#313](https://github.com/vfarcic/dot-agent-deck/issues/313) covers reclaiming those on demand.
- Removing the `Ctrl+T` toggle — [#312](https://github.com/vfarcic/dot-agent-deck/issues/312).
- Mode tabs' side panes, which exist precisely to be watched while the agent works (`docs/workspace-modes.md:8`). If mode tabs currently rely on the `Stacked` arm, they must keep simultaneous rendering; see Open Questions.
- The focused pane's own border, which carries the title, focus, status colour (PRD #155 M3) and command-mode state (`9345a74`).

## Technical Approach

The `Stacked` branch has two arms: the expanded pane renders a `TerminalWidget`, and everything else renders a `Block` with `Borders::TOP` and a title. This PRD deletes the second arm and gives its rows to the first.

The subtlety is not the render call, it is the geometry. `pane_stack_rects` currently allocates `title_bar_height = 1` per collapsed pane and the remainder to the expanded one, and `resize_panes_to_layout` sizes each PTY from those rects. A collapsed pane resolves to a zero inner dimension and is already filtered (`src/ui.rs:10952`, `:11203`), so the "not rendered" case is partly modelled — but a pane that is not rendered at all still needs a defined PTY size, or an agent will reflow to something arbitrary the moment it stops being focused and reflow back when it returns.

### Cross-version safety

None. This is TUI-side rendering, no daemon protocol, no hooks, no orchestration routing — CLAUDE.md rule 12's contract question does not arise. Patch-level bump.

## Success Criteria

- In a 7-role orchestration tab, the focused pane occupies the full height of the pane column; no empty titled frames appear.
- Every non-focused role keeps running: its sidebar card still updates status live, and a delegation to it still routes and completes.
- Switching focus between roles shows each agent's content intact — no lost scrollback, no visible re-layout thrash.
- The reclaimed rows are genuinely usable: the agent's own TUI paints into them rather than leaving the area blank.
- No regression in mode tabs' side panes.

## Milestones

- [x] **M1 — Collapsed frames no longer render.** The `Stacked` else-arm is gone and the focused pane's rect covers the freed rows. `pane_stack_rects` gives non-focused panes `Constraint::Length(0)` and the expanded slot `Fill(1)`, so it receives the whole column.
- [x] **M2 — PTY sizing settled for undrawn panes.** `FrameLayout::pane_target_dims` sizes a non-focused `Stacked` pane from `panes_area` — which, once every other slot reserves zero rows, *is* the expanded pane's rect, so the two agree by construction rather than by coincidence. `tabs/orchestration/006` round-trips focus and finds each role's content intact.
- [x] **M3 — Mode tabs verified unaffected.** Confirmed they never read the global: `render_mode_tab` hardcodes `PaneLayout::Tiled` for the side-pane column (`src/ui.rs`). `tabs/mode/001` now pins that, so a future refactor cannot wire the global in.
- [x] **M4 — L1 coverage.** `orchestration/layout/002` drives the real `compute_frame_layout` + `render_frame` through a `TestBackend`. **Deviation from this milestone as written:** it uses explicit assertions, not `insta`. The seam it uses (`EmbeddedPaneController::for_render_only_tests()`) is an intentionally *empty* controller, so a snapshot would pin a blank pane column and prove nothing; asserting the expanded rect's height against the column height is the stronger check. See Follow-up 1.
- [x] **M5 — L2 PTY coverage.** `tabs/orchestration/006` (PTY-attached, `tests/e2e_orchestration_pane_column.rs`): no collapsed frame for either non-focused role, and `beta`'s sidebar status transitions Idle → Working from its own hook events *while never being the focused pane*.
- [x] **M6 — Docs and changelog.** `docs/orchestration.md`, `docs/keyboard-shortcuts.md`, plus `docs/getting-started.md` (which also described the stacked view); `changelog.d/311.feature.md`.

## Risks

- **The reclaimed space is invisible to the user if the agent does not reflow.** An agent that redraws only on input could leave the extra rows blank until the next keystroke. Verify with a real agent, not a stand-in.
- **Losing the "who else is here" cue for non-orchestration tabs.** In an orchestration the sidebar covers it. A dashboard tab with several panes may have no equivalent roster, in which case removing the frames there removes the only hint that other panes exist. Scope per tab type if so.
- **Focus-switch thrash.** If undrawn panes are resized to zero and back, agents may reflow twice per switch. M2 exists to prevent this.

## Open Questions — resolved

1. **What size should an undrawn pane's PTY be?** → **Size it as if focused.** `FrameLayout::pane_target_dims` targets `panes_area` for any zero-height `Stacked` rect. Because the expanded slot fills the whole column once every other slot reserves zero rows, "as if focused" and "the expanded pane's rect" are the same rect, so no drift is possible. The accepted cost is the one this question named: several agents believe they are full-size simultaneously. In exchange, a focus switch needs no resize at all. `Tiled` is deliberately untouched — a zero-height `Tiled` rect is a genuine "no room" case, not an undrawn pane.

2. **Do mode tabs use the `Stacked` arm today?** → **No.** `render_mode_tab` hardcodes `PaneLayout::Stacked` for its single agent pane and `PaneLayout::Tiled` for the side-pane column; neither reads the global `pane_layout`. So mode tabs were never collapsing side panes and there was no pre-existing bug to fix or hand to #312. The risk this question identified was real but latent — a future refactor could have wired the global in, and since the global defaults to `Stacked` that would have collapsed every side pane but one. `tabs/mode/001` now pins the current behaviour so that cannot happen silently.

3. **Dashboard tabs with multiple panes — same treatment, or keep the frames as the only roster?** → **Same treatment.** Dashboard tabs are not roster-less: the left card grid lists every session with its live status, which is the same argument that justifies removing the frames on orchestration tabs. **This answer has a known cost, discovered while verifying #334.** Dashboard cards render `<type> · <name>` and truncate the name to the card width — roughly ten characters at the default split — so with the frame titles gone, a non-focused pane's *full* name is now readable nowhere. This surfaced as a real test regression: `session/restore/001` had been matching a second pane's full name inside a collapsed frame's title, and went red when the frames disappeared (fixed in `514a27e` by staging shorter names). The truncation itself is pre-existing and is **not** fixed here; see Follow-up 2.

## Follow-ups

Filed out of this PRD's verification rather than fixed in it:

1. **`orchestration/layout/002` cannot see pane content.** It drives the empty `for_render_only_tests()` controller, so `get_screen` returns `None` for every id and nothing renders in the pane column — its "no other role id appears in the grid" assertion is now trivially satisfied (it was genuinely RED pre-fix, when the collapsed arm drew a block without needing a screen). Moving it to `for_render_seam_tests` (one focused pane with real bytes) would let it assert that the focused pane's *content* fills the reclaimed rows.
2. **Dashboard card titles truncate pane names to ~10 characters**, which is what makes a non-focused pane's full name unreadable after this change (see Open Question 3). Pre-existing; related to #313.
3. **`tabs/mode/001`'s sentinel is not false-positive-proof by construction.** Probed during verification: with the sentinels present only in the echoed command text the test correctly fails, so it is sound as written. But per #367, whether an echoed token survives intact depends on where the command line wraps — i.e. on the absolute path length of the checkout, which is exactly what made `tabstrip_003` red in a worktree and green in `main`. `tabs/mode/006`'s runtime-composed sentinel (`printf 'WATCH_%s\n' STREAM_SENTINEL`) is the pattern that removes the hazard.
4. **No automated guard for "the reclaimed rows are genuinely usable".** Success criterion 4 was verified manually with a real interactive Haiku agent (see the Work Log), not by a test. Nothing in the suite would catch a regression where the pane grows but the agent never repaints into it.

## Work Log

### 2026-08-04 — Completed and merged

Implemented by [@prageethw](https://github.com/prageethw) in PR [#334](https://github.com/vfarcic/dot-agent-deck/pull/334), squashed to `f86c37b`. All six milestones met; Open Questions 1–3 resolved above.

Verified via `/verify-pr`: full local gate run (fmt, clippy, release build, `test-fast` 1501/1501, linkage-check) plus the e2e tier at 3405/3406 — the single failure being `dispatch_013`, which passes in isolation in 8.8s and is tracked as a load flake under #351. All nine CI checks green, including `build-macos` and `build-windows` (real build + clippy + fast tier per OS) and `security` (`cargo audit`); those runs had been held at `action_required` because GitHub gates Actions on an outside contributor's fork, and were released after a safety review of the diff.

Two blocking defects were found and fixed before merge:

- **`session/restore/001` regressed** — it had been reading a non-focused pane's name out of a collapsed frame title. Green at the merge-base, red on the branch in isolation. Fixed in `514a27e`; see Open Question 3.
- **`tabs/mode/001` could not pass at all** — its fixture omitted `watch`, which defaults to true, and the then-current `run_watch` printed only after the command exited, so a `printf …; sleep 600` sentinel never reached the pane. Root-caused during this verification and fixed properly upstream in #367 by making `run_watch` stream, which also removed a silent-blank-pane trap for any user-configured `tail -f`-style mode pane.

Success criterion 4 ("the reclaimed rows are genuinely usable") and Risk 1 ("verify with a real agent, not a stand-in") were checked by hand, since every e2e test here uses shell stand-ins: a 4-role Stacked orchestration with a real interactive Haiku agent in the focused pane, driven through the release binary under tmux. The focused pane spanned rows 2→44 of a 45-row terminal — the entire column — with the agent's own TUI painting throughout it, its input box at rows 40–42, and it answered a directive prompt with the fixture's sentinel filename. `Ctrl+T` drew all four panes and toggling back restored the single expanded pane. Criteria for "no regression in mode tabs" and "non-focused roles keep running" are covered by tests; *"a delegation to it still routes and completes"* is covered only indirectly, by the pre-existing `orchestration/route/*` suite.

### 2026-08-01 — Created

Split out of [#307](https://github.com/vfarcic/dot-agent-deck/issues/307) as the literal request. Sequenced first: [#312](https://github.com/vfarcic/dot-agent-deck/issues/312) and [#313](https://github.com/vfarcic/dot-agent-deck/issues/313) both touch the same layout seam and are cheaper once this has landed.
