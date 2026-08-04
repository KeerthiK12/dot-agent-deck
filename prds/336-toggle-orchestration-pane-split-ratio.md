# PRD #336: Toggle orchestration pane-column split ratio

**Status**: Complete
**Priority**: Medium
**Created**: 2026-08-03

## Problem Statement

In orchestration tabs, the sidebar (role list, left) and the pane column (agent terminals, right) split at a fixed ratio: `ORCHESTRATION_LEFT_PERCENT = 34` / `ORCHESTRATION_PANES_PERCENT = 66` (`src/ui.rs`). On a laptop screen this leaves the working pane noticeably narrower than it could be, and there is no quick way to reclaim that width — only a config edit and restart.

This is a companion to [#311](https://github.com/vfarcic/dot-agent-deck/issues/311) (removed the collapsed non-focused frames) and [#313](https://github.com/vfarcic/dot-agent-deck/issues/313) (a toggleable full-width zoom). Neither addresses the everyday case: keep the sidebar visible, just narrower.

## Solution Overview

Add a keybinding action, default `Ctrl+l` (remappable via the existing `keybindings.rs` `Action`/`ActionSpec` system), that toggles the orchestration tab's split between the default 34/66 and a narrower-sidebar 25/75 (1/4 sidebar, 3/4 panes) — and back again on a second press. Scoped to orchestration tabs **in command mode**; everywhere else — in a pane, or on any other tab — the chord is never claimed, so it reaches the focused pane as ordinary input.

## Scope

### In Scope

- A new `Action::ToggleOrchestrationSplit` registered in `src/keybindings.rs`'s `ACTIONS` table, default chord `Ctrl+l`, section `Global`.
- Per-tab (not global) state tracking which ratio an orchestration tab is currently using, defaulting to 34/66, so toggling one tab does not affect others.
- The layout call sites that read the `ORCHESTRATION_LEFT_PERCENT` / `ORCHESTRATION_PANES_PERCENT` constants directly resolve the active ratio for that tab instead of the fixed constants.
- L1 coverage pinning both ratio states' geometry, the per-tab isolation, and the tab/mode-scoping guard.
- A PTY-attached L2 test driving the real chord and asserting the visible column boundary moves and round-trips.
- `docs/keyboard-shortcuts.md` updated with the new binding; changelog fragment added.

### Out of Scope

- [#312](https://github.com/vfarcic/dot-agent-deck/issues/312) (retiring the global stacked/tiled toggle) and [#313](https://github.com/vfarcic/dot-agent-deck/issues/313) (zoom) — unrelated toggles on the same layout seam.
- Dashboard or mode tabs — this ratio is specific to the orchestration tab's sidebar/pane-column split. Extending the toggle to the Dashboard, and to a third "hidden sidebar" stage, is [#361](https://github.com/vfarcic/dot-agent-deck/issues/361).
- More than two ratio states (no cycling through arbitrary splits) — this is a two-state toggle. See #361.
- Persisting the toggled state across restarts — resets to the 34/66 default on next launch.

## Technical Approach

`ORCHESTRATION_LEFT_PERCENT` / `ORCHESTRATION_PANES_PERCENT` remain as the default-state values. A resolver, `orchestration_split_percents(narrow) -> (u16, u16)`, returns `(25, 75)` or `(34, 66)` and is the single source of truth every call site goes through.

The state lives on the tab: `Tab::Orchestration::split_narrow: bool`. `dispatch_action` flips it and lets the next frame reflow, exactly as `ToggleLayout` does — no resize is pushed from the handler.

**The flag is threaded as data, not held in shared state.** `ActiveTabView::Orchestration` — already the per-tab render snapshot that `compute_frame_layout` receives — carries `split_narrow`, so the layout pass resolves the split of the tab actually being rendered. `orchestration_role_pane_dims` takes `narrow` as an explicit parameter; its only caller is the spawn path, which always passes `false` because a newly opened or restored tab starts at the default regardless of what another tab is toggled to. An earlier revision used a thread-local mirror of the active tab's flag; that was replaced because it made correctness depend on unenforced call ordering, and because #361 (same toggle on Dashboard tabs) would have had to add a second sync site to the same global.

### Tab and mode scoping

`global_action` stays a pure chord→action table with no tab awareness. A separate pure function, `scope_orchestration_split(action, is_orchestration_tab, mode)`, un-resolves `ToggleOrchestrationSplit` to `None` unless the active tab is an orchestration tab **and** the deck is in command mode. `handle_key_event` applies it at the one point in the funnel that has tab context. Keeping it a standalone function makes it unit-testable without a PTY (`orchestration/layout/005`); an inline `if` would only be reachable through the full event loop.

Both halves of the narrowing exist for the same reason: the default `Ctrl+l` is readline's `clear-screen`, so anything running in a pane has a legitimate claim on it. Off an orchestration tab the action can do nothing, so claiming the chord is pure loss. In a pane on an orchestration tab it would be worse — that is the most likely place to want a screen clear, and the user would get a sidebar resize instead.

**Command-mode scoping mirrors `close_pane`** (PRD #241 M1), which is command-mode only precisely so `Ctrl+w` still reaches the PTY as word-delete while the user is typing. The cost is one extra keystroke (`Ctrl+d` first); the alternative is silently eating a chord people press reflexively — and #361 would widen that swallow to every pane on the deck. Consequently the help overlay lists the binding under "Dashboard (command mode)" rather than "Global (works from any pane)", exactly as PRD #241 review F6 did for `close_pane`.

### Cross-version safety

None. This is TUI-side rendering state: no daemon protocol, no hooks, no orchestration routing, and `Tab` derives no `Serialize`/`Deserialize`, so the new field cannot affect any persisted format. CLAUDE.md rule 12's contract question does not arise. Patch-level bump.

### Experimental flag (rule 9)

**Decision: ships visible by default — no `experimental` gate.** The surface is a single additional keybinding on an existing pane layout, off by default in the sense that nothing changes until the user presses it, fully reversible with a second press, and not persisted. There is no new pane, field, tab, or footer to stage behind a flag, and no partially-built surface a user could stumble into. Accordingly there is no `src/features.rs` wrapper, no note in `docs/develop/experimental-flag.md`, and no `graduate-` follow-up issue. Recorded here so the rule 9 question reads as answered rather than skipped.

## Success Criteria

- In an orchestration tab, pressing the toggle chord once changes the sidebar/pane-column split from 34/66 to 25/75; the sidebar visibly narrows and the pane column visibly widens. ✅ `tabs/orchestration/007`
- Pressing it again returns to the 34/66 default. ✅ `tabs/orchestration/007`, `orchestration/layout/004`
- The toggle is scoped per orchestration tab — toggling one tab's ratio does not change another open orchestration tab's ratio. ✅ `orchestration/layout/004`
- No regression to non-orchestration tabs (dashboard, mode tabs) — the toggle has no effect there, and the chord still reaches the focused pane's PTY. ✅ `orchestration/layout/005`, `tabs/orchestration/008`
- In a pane (`PaneInput`) the chord is NOT claimed even on an orchestration tab, so a role agent still receives it. ✅ `orchestration/layout/005`, `tabs/orchestration/007`
- The chord is remappable through the same config mechanism as every other keybinding. ✅ `ACTIONS` entry `toggle_orchestration_split`

## Milestones

- [x] **M1 — Per-tab split-ratio state added.** `Tab::Orchestration::split_narrow`, defaulting to the 34/66 ratio at every construction site.
- [x] **M2 — Toggle action wired.** `Action::ToggleOrchestrationSplit` registered with default chord `Ctrl+l`; pressing it in an orchestration tab flips the tab's ratio state.
- [x] **M3 — Layout call sites resolve the active ratio.** `compute_frame_layout` reads the split off the render snapshot; `orchestration_role_pane_dims` takes it as a parameter.
- [x] **M4 — L1 coverage.** `orchestration/layout/003` (geometry + chord resolution), `/004` (per-tab isolation + round trip), `/005` (the scoping guard).
- [x] **M5 — L2 coverage.** `tabs/orchestration/007` drives a real orchestration tab through the PTY and asserts the visible boundary moves and round-trips; `tabs/orchestration/008` proves the chord still reaches a pane's PTY off an orchestration tab.
- [x] **M6 — Docs and changelog.** `docs/keyboard-shortcuts.md` updated (both the quick table and the remappable-actions table); `changelog.d/336.feature.md` added.

## Risks

- **Chord conflicts.** `Ctrl+l` is free in the default `ACTIONS` table, verified against `main`. Note it is *not* free inside a pane — plenty of programs bind it (readline's clear-screen) — which is exactly why the tab-scoping guard and `tabs/orchestration/008` exist.
- **Follow-on rework.** #361 proposes turning this two-state toggle into a three-stage cycle covering Dashboard tabs too. Threading the flag as data rather than shared state is what keeps that a additive change.

## Open Questions

1. ~~Does per-tab UI state already have a natural home?~~ Resolved: `Tab::Orchestration` for the state, `ActiveTabView::Orchestration` for the render snapshot.
2. ~~Should the toggle state survive tab restore?~~ Resolved: no — a restored tab starts at the default; persistence stays out of scope.

## Work Log

### 2026-08-03 — Created

Split out of the "1/3 vs 1/4 sidebar width" ask as a quick, scoped toggle. Distinct from #312 (retiring the global layout toggle) and #313 (full zoom) — this is a narrower, additive keybinding on the same layout seam.

### 2026-08-03 — M1-M6 complete

Per-tab split state, the `toggle_orchestration_split` action (default `Ctrl+l`), and the layout call sites all landed with L1 and L2 coverage green. Docs and the changelog fragment close out M6.

### 2026-08-04 — Rebased onto `main`; thread-local replaced; scoping guard extracted

PRD #311 landed separately (#334), so its changes were dropped from this branch and the spec ids renumbered around main's (`orchestration/layout/003-005`, `tabs/orchestration/007-008`). Three review findings addressed while rebasing:

- The thread-local mirror of the active tab's flag was replaced by threading `split_narrow` through `ActiveTabView::Orchestration` and an explicit `orchestration_role_pane_dims` parameter.
- The tab-scoping check was extracted from an inline `if` in the event loop into the pure `scope_orchestration_split`, and given its own L1 test — the guard is what keeps `Ctrl+l` from being swallowed on non-orchestration tabs (Greptile P1 on PR #342).
- `Ctrl+l` was missing from the in-app help overlay (`?`) even though the docs advertise that overlay as the full list; it is now listed, qualified as orchestration-tab-only.

`tabs/orchestration/008` was also rewritten: it previously asserted that readline's `clear-screen` wiped a sentinel, which depends on the host's terminal setup and failed on a machine where the forwarding was in fact correct. It now runs `cat -v` and asserts the pane echoes `^L`, observing the forwarded byte directly.
