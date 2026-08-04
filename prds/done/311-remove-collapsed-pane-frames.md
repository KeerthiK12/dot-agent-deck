# PRD #311: Stop rendering non-focused agent panes as empty collapsed frames

**Status**: Not started
**Priority**: High
**Created**: 2026-08-01

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

- [ ] **M1 — Collapsed frames no longer render.** The `Stacked` else-arm is gone and the focused pane's rect covers the freed rows.
- [ ] **M2 — PTY sizing settled for undrawn panes.** A non-focused agent has a defined, stable size; focus round-trips do not visibly reflow its content.
- [ ] **M3 — Mode tabs verified unaffected.** Side panes still render simultaneously (or are consciously exempted from this change).
- [ ] **M4 — L1 snapshot coverage.** `insta` render tests pin the new geometry for a multi-role orchestration tab, per CLAUDE.md rule 4.
- [ ] **M5 — L2 PTY coverage.** A vt100 test drives a real multi-role tab, asserts the frames are gone and that a non-focused agent is still live (status still updates).
- [ ] **M6 — Docs and changelog.** `docs/orchestration.md` and `docs/keyboard-shortcuts.md` updated where they describe the stacked view; changelog fragment added.

## Risks

- **The reclaimed space is invisible to the user if the agent does not reflow.** An agent that redraws only on input could leave the extra rows blank until the next keystroke. Verify with a real agent, not a stand-in.
- **Losing the "who else is here" cue for non-orchestration tabs.** In an orchestration the sidebar covers it. A dashboard tab with several panes may have no equivalent roster, in which case removing the frames there removes the only hint that other panes exist. Scope per tab type if so.
- **Focus-switch thrash.** If undrawn panes are resized to zero and back, agents may reflow twice per switch. M2 exists to prevent this.

## Open Questions

1. **What size should an undrawn pane's PTY be?** Options: freeze at its last drawn size; size it as if focused (so it is already correct when focused); or a fixed sane default. "As if focused" costs nothing and makes focus switching instant, but means several agents believe they are full-size simultaneously.
2. **Do mode tabs use the `Stacked` arm today?** `pane_layout` is one global field (`src/ui.rs:1531`) read by all three `render_terminal_panes` call sites, so a mode tab in `Stacked` presumably collapses its side panes — which would defeat their purpose. If so, that is arguably a pre-existing bug this PRD should either fix or explicitly leave to #312.
3. **Dashboard tabs with multiple panes** — same treatment, or keep the frames there as the only roster?

## Work Log

### 2026-08-01 — Created

Split out of [#307](https://github.com/vfarcic/dot-agent-deck/issues/307) as the literal request. Sequenced first: [#312](https://github.com/vfarcic/dot-agent-deck/issues/312) and [#313](https://github.com/vfarcic/dot-agent-deck/issues/313) both touch the same layout seam and are cheaper once this has landed.
