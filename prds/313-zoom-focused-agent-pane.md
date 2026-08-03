# PRD #313: Zoom the focused agent pane

**Status**: Not started
**Priority**: Medium
**Created**: 2026-08-01

## Problem Statement

Even after [#311](https://github.com/vfarcic/dot-agent-deck/issues/311) gives the pane column entirely to the focused agent, a third of the width is still sidebar (`ORCHESTRATION_PANES_PERCENT = 66`, `src/ui.rs:1952`), plus the pane border, the button bar and — with more than one tab open — the tab strip.

That split is right when you are supervising: the sidebar is how you see which of seven agents is working, and it is the reason the orchestration view is worth looking at. It is wrong when you have stopped supervising and started *working in one agent* — reading a long diff, following a plan, going back and forth with the orchestrator on a laptop screen. In that mode the other agents are noise, and there is currently no way to say so.

[#307](https://github.com/vfarcic/dot-agent-deck/issues/307) asked for "bigger screen and easy to work with laptop screen", and followed up with "even a shortcut to get rid of that part on demand will help eyes". #311 answers the literal request; this covers the on-demand part.

## Solution Overview

A reversible zoom: press the key and the focused agent takes effectively the whole terminal; press it again and the normal multi-agent view returns. Two states, one key, no configuration.

This is the `tmux prefix+z` model, and deliberately so — it is the dominant precedent for terminal users (also zellij fullscreen, i3/sway fullscreen, vim `Ctrl+W _`), which means the behaviour needs no explaining and the word "zoom" already means this to the audience.

Zoom is a *view* state, not a mode with its own rules: everything you can do zoomed, you can do unzoomed. It changes what is drawn, never what is running.

## Scope

### In Scope

- A zoom toggle affecting the focused pane in any tab type that has more than one thing on screen.
- Hiding the sidebar and reclaiming the full width for the focused pane.
- A visible indicator that zoom is active.
- PTY resize on zoom and unzoom, so the agent reflows to the larger and smaller area.

### Out of Scope

- Terminal font size. "Zoom" here means "this pane fills the screen", not text scaling.
- Any change to which agents run — zoomed or not, every pane stays alive.
- Per-pane zoom persistence in the saved session (see Open Questions).
- The experimental feature flag. Explicitly decided against for this feature (CLAUDE.md rule 9 asks the question; the answer here is no — it ships visible).

## Technical Approach

Zoom is a third arrangement rather than a flag layered on top of the existing ones: given #311 and #312, the renderer already resolves "what arrangement does this tab use" in one place, and zoom is one more answer to that question. Expressing it in the same type keeps the compiler responsible for finding every geometry site, rather than sprinkling `if zoomed` through the render path.

The state itself is per-tab (or global — see Open Questions), and lives in UI state only; nothing about it reaches the daemon.

### Keybinding

`Ctrl+Z` is free — the `teleport = "Ctrl+z"` line in `src/keybindings.rs:1387` is a fixture inside `unknown_action_ignored_with_warning`, not a real action. But it is not free of consequence: `Ctrl+Z` in a pane is currently encoded to `0x1a` and forwarded to the agent (pinned by `keyevent_ctrl_c_and_ctrl_a`, `src/ui.rs:21419`), so a global binding takes away job control inside every pane.

The alternative is a command-mode letter (`z`), matching the existing command-mode single-key actions (`g`, `r`, `/`) and mapping almost exactly onto `tmux prefix+z` muscle memory — `Ctrl+D` is already this app's prefix. That costs one extra keystroke and preserves `Ctrl+Z` passthrough. Decide before implementing; either way the action is remappable.

### Cross-version safety

None — TUI-only view state. Patch bump.

## Success Criteria

- One key zooms the focused agent to effectively the whole terminal; the same key restores the previous view exactly.
- While zoomed, it is obvious at a glance that you are zoomed — nobody concludes their other agents have disappeared.
- Zooming and unzooming resizes the agent's PTY and the agent reflows correctly both ways, with no lost scrollback.
- Every non-focused agent keeps running while zoomed; delegation and hooks are unaffected.
- Repeated toggling does not degrade the agent's rendering.

## Milestones

- [ ] **M1 — Zoom state and geometry.** The focused pane resolves to a full-terminal rect; sidebar and other panes are not drawn.
- [ ] **M2 — Toggle wired to a binding**, per the decision in Open Question 1, remappable like every other action.
- [ ] **M3 — Zoom indicator.** A visible marker while zoomed (see Open Question 2).
- [ ] **M4 — Focus-change behaviour settled and implemented** (Open Question 3).
- [ ] **M5 — L1 snapshot coverage** for zoomed and unzoomed geometry, per CLAUDE.md rule 4.
- [ ] **M6 — L2 PTY coverage.** A vt100 test zooms a live agent, asserts the sidebar is gone and the agent still paints, unzooms and asserts the view is restored. Per rule 4 this is a user-facing feature, so it needs at least one PTY-attached test — and a real agent if it is to be reel-eligible.
- [ ] **M7 — Docs and changelog.** `docs/keyboard-shortcuts.md` and `docs/orchestration.md` updated; changelog fragment added.

## Risks

- **Forgetting you are zoomed.** The failure mode is a user who thinks their agents vanished, or who watches one agent while another sits blocked. tmux mitigates this with a `Z` marker in the status line; M3 is not optional.
- **Hiding the sidebar hides the only live status of the other agents.** That is the point of the feature, but it means a zoomed user is genuinely less informed. Worth confirming the notification paths (idle-worker detection, work-done lines into the orchestrator pane) still reach them — they should, since those write into the orchestrator's own pane.
- **Resize churn.** Every toggle resizes the PTY; an agent that reflows badly will look worse zoomed than not. This is the thing to verify with a real agent rather than a stand-in.
- **Scope creep into a "focus mode".** Zoom should not acquire its own keybindings, rules or behaviours. If it starts to, it has become a mode and needs a different PRD.

## Open Questions

1. **`Ctrl+Z` globally, or `z` in command mode?** The trade-off is one keystroke against job-control passthrough in every pane. Leaning command-mode `z` for the tmux parallel and because losing `Ctrl+Z` inside an agent's shell is a real cost.
2. **What exactly does zoom hide?** Sidebar certainly. The pane border is the interesting one: it carries the title, focus, status colour (PRD #155 M3) and command-mode state (`9345a74` — a deliberate fix). Dropping it silently undoes that fix unless the button bar's `[Command Mode Ctrl+D]` is judged sufficient. Leaning: keep the border, and let it carry the zoom indicator for M3.
3. **Does zoom follow focus?** If you are zoomed on the orchestrator and jump to a role with `1`–`9`, do you stay zoomed on the new pane or drop back? tmux unzooms on pane switch; here the role-jump keys are a deliberate "go work with that agent" action, so following focus seems more useful. Needs a decision, not a default.
4. **Is zoom per-tab or global?** Per-tab means switching tabs shows each in its own state; global means zoom is a posture you are in. Per-tab is more predictable; global is simpler to explain.
5. **Does zoom survive detach/reattach and session restore?** tmux persists it. Here it is ephemeral UI state and simplest not to persist — but a user who zooms, detaches and returns to an unzoomed deck may find that surprising.

## Work Log

### 2026-08-01 — Created

Split out of the [#307](https://github.com/vfarcic/dot-agent-deck/issues/307) discussion. Sequenced last: [#311](https://github.com/vfarcic/dot-agent-deck/issues/311) and [#312](https://github.com/vfarcic/dot-agent-deck/issues/312) both reshape the layout seam this builds on. Note that #311 alone may satisfy the original request — this is the additional, on-demand step, and is worth reassessing once #311 has shipped and been used.
