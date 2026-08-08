# Dispatcher Mode

> **Developer / maintainer reference.** This page documents an internal development mechanism and is intentionally excluded from the published documentation site.

## What it is

Dispatcher mode is a built-in seeded mode for `dot-agent-deck` that teaches an agent one extra effector: the `dispatch` CLI subcommand, which starts an isolated line of work in its own git worktree. A dispatcher pane is otherwise an **ordinary conversational agent** — it does whatever the user asks — and it reaches for `dispatch` when the user says to *start* something as a separate line of work.

The seed is deliberately scoped to **Agent Deck mechanics, not work methodology**: what the verb is, what it does, and the constraints that follow from process isolation. It holds no opinion on how the user should split up their work, matching the two schedule-authoring seeds. (An earlier version cast the pane as a planner that had to decompose a goal into 2–6 independent units and never do work itself; that was cut — see the Design record in [PRD #220](../../prds/220-dispatcher-mode-worktree-dispatch.md).)

## How to activate it

1. Open a new pane with `Ctrl+n`
2. Cycle through the available modes until `dispatcher` appears in the mode selector
3. Select it and confirm

The dispatcher mode is currently gated behind the `experimental` feature flag — see [experimental-flag.md](experimental-flag.md).

## How the agent uses `dispatch`

Once the agent is running inside dispatcher mode, it can execute:

```
dot-agent-deck dispatch <name> --task "..."
```

This spawns an isolated agent — or a full multi-role orchestration, depending on the dispatched worktree's own `.dot-agent-deck.toml` — in a dedicated worktree. The `--task` text becomes that agent's opening prompt, and because it runs as a fresh process with no access to the dispatcher's conversation, the text has to be self-contained.

Several dispatches from one pane are normal, and are not decomposition: working on three PRDs in parallel is three dispatches of three things the user named.

## Worktree isolation

Every `dispatch` call creates its work in a dedicated Git worktree at `../<repo>-dispatch-<slug>`. Each unit is fully isolated from the others — changes to one dispatched worktree never conflict with another or with the main worktree.

## Cleanup

Cleanup is keyed to the **dispatched unit's own tab**, not the dispatcher's. Closing a unit's tab removes that unit's worktree (the repo itself is always preserved). Closing the dispatcher tab removes nothing — it never owned a worktree.

Removal deliberately **refuses to discard uncommitted work**: if the unit's worktree still has uncommitted changes, it is left on disk and a warning is logged, so you can recover the work. A leaked worktree costs disk; a force-removed one costs work.

The unit's branch (`agent/dispatch-<slug>`) always survives removal, because it may hold the unit's committed work. That means dispatching the **same name again** is refused, naming the leftover branch and telling you how to proceed — delete the branch with `git branch -D agent/dispatch-<slug>` once you are done with it, or dispatch under a different name.

## Current limitations

- The return edge (the dispatched unit sending results back to the dispatcher) is not yet implemented. The dispatcher reports where each unit is running; it is **not** notified when a unit finishes. This is Phase 2 of [PRD #220](https://github.com/vfarcic/dot-agent-deck/issues/220) itself, deferred rather than dropped. (It is *not* tracked by #174 — that is the separate *Cross-project orchestration dispatch* PRD, which **depends on** this one.)
