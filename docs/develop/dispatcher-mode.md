# Dispatcher Mode

> **Developer / maintainer reference.** This page documents an internal development mechanism and is intentionally excluded from the published documentation site.

## What it is

Dispatcher mode is a built-in seeded mode for `dot-agent-deck` that teaches an agent to decompose a large task into smaller isolated units and dispatch each one via the `dispatch` CLI subcommand. When the agent is running inside dispatcher mode its system prompt is augmented with instructions on how to use the `dispatch` subcommand for worktree-isolated work units.

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

This spawns an isolated sub-agent in a dedicated worktree. The agent is expected to decompose its work into named units and call `dispatch` for each one.

## Worktree isolation

Every `dispatch` call creates its work in a dedicated Git worktree at `../<repo>-dispatch-<slug>`. Each unit is fully isolated from the others — changes to one dispatched worktree never conflict with another or with the main worktree.

## Cleanup

Closing the pane (tab) that holds the dispatcher session automatically removes any worktrees created by that session. No manual cleanup is needed.

## Current limitations

- The return edge (the dispatched unit sending results back to the orchestrator) is not yet implemented — tracked in follow-up PR #174.
