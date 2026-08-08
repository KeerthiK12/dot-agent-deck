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

This creates a dedicated worktree and starts a line of work inside it. The `--task` text becomes the opening prompt, and because it runs as a fresh process with no access to the dispatcher's conversation, the text has to be self-contained.

Several dispatches from one pane are normal, and are not decomposition: working on three PRDs in parallel is three dispatches of three things the user named.

## Choosing the shape: one agent, or a team

A unit can start as a single agent or as a full multi-role orchestration. **Which one is the user's call, not the agent's** — and that is why it is asked rather than inferred. The two cases look identical from the request:

- *"work on these three features"* → usually a team per feature
- *"verify these three PRs"* → usually one agent each

So the seed tells the dispatcher to enumerate the shapes this repo offers and ask before the first dispatch:

```
dot-agent-deck dispatch --list-targets
```

which prints `single` plus every role-bearing orchestration by name. The answer then rides on each call:

```
dot-agent-deck dispatch <name> --task "..." --single
dot-agent-deck dispatch <name> --task "..." --orchestration <orchestration-name>
```

`--list-targets` is a **local read** of the repo's own `.dot-agent-deck.toml`, not a daemon round-trip — the dispatched worktree is a copy of this repo, so that config is the one the spawn branches on. It therefore adds no hook-socket message and no protocol surface.

Two details worth knowing. Naming an orchestration the repo does not define is an **error**, not a silent fallback — starting something other than what the user picked is exactly the surprise the selector removes, and the message lists what is available. And schedule/authoring modes never appear in the listing: a schedule creates a *future* task, so it is not something a dispatch can start.

With neither flag, the shape still falls back to whatever the repo's config implies (its first `[[orchestrations]]`, else a single agent), which is the pre-selector behaviour.

## Worktree isolation

Every `dispatch` call creates its work in a dedicated Git worktree at `../<repo>-dispatch-<slug>`. Each unit is fully isolated from the others — changes to one dispatched worktree never conflict with another or with the main worktree.

## Cleanup

Cleanup is keyed to the **dispatched unit's own tab**, not the dispatcher's. Closing a unit's tab removes that unit's worktree (the repo itself is always preserved). Closing the dispatcher tab removes nothing — it never owned a worktree.

Removal deliberately **refuses to discard uncommitted work**: if the unit's worktree still has uncommitted changes, it is left on disk and a warning is logged, so you can recover the work. A leaked worktree costs disk; a force-removed one costs work.

The unit's branch (`agent/dispatch-<slug>`) always survives removal, because it may hold the unit's committed work. That means dispatching the **same name again** is refused, naming the leftover branch and telling you how to proceed — delete the branch with `git branch -D agent/dispatch-<slug>` once you are done with it, or dispatch under a different name.

## Current limitations

- **A dispatched orchestration starts without the delegation protocol, so only its orchestrator acts.** This is the one limitation to know before choosing `--orchestration`. The daemon spawn path never composes the orchestrator context that the interactive `Ctrl+n` path writes: `prepare_orchestrator_prompt` (which writes `.dot-agent-deck/orchestrator-context.md` listing the roles and the delegation protocol) has exactly one caller, `src/ui.rs`, and `src/spawn.rs` never calls it. The orchestrator therefore receives the `--task` text but is never told that it *is* an orchestrator, which roles exist, or how to `delegate` — so it does not delegate, and its worker panes sit idle waiting for work that never arrives. In a repo whose first orchestration has six roles, that is one working agent and five idle ones. Until this is fixed, `--single` is the reliable choice. Tracked on [#222](https://github.com/vfarcic/dot-agent-deck/issues/222), whose "prompt order" parity item is exactly this; scheduled issue-dispatch (#120) has the identical defect because it shares the same spawn path, so one fix covers both.
- The return edge (the dispatched unit sending results back to the dispatcher) is not yet implemented. The dispatcher reports where each unit is running; it is **not** notified when a unit finishes. This is Phase 2 of [PRD #220](https://github.com/vfarcic/dot-agent-deck/issues/220) itself, deferred rather than dropped. (It is *not* tracked by #174 — that is the separate *Cross-project orchestration dispatch* PRD, which **depends on** this one.)
