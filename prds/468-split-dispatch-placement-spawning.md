# PRD #468: Split dispatch into placement and spawning

**Status**: Proposed
**Issue**: [#468](https://github.com/vfarcic/dot-agent-deck/issues/468)
**Builds on**: [#220](https://github.com/vfarcic/dot-agent-deck/issues/220) (the `dispatch` verb + dispatcher mode), [#222](https://github.com/vfarcic/dot-agent-deck/issues/222) (orchestrator context on disk)
**Priority**: Medium

## Problem Statement

`dispatch` does two unrelated jobs in one step: it creates a git worktree, and it spawns an agent or orchestration into that worktree. Coupling them means **placement is implicit in the verb** — every dispatch lands in a new tree, because that is the only thing the verb can do.

That is correct for starting a new line of work, and it is what #220 set out to make easy. It is wrong for **continuing** one.

The case that exposes it, observed repeatedly in real use on 2026-08-09: a single agent analyses an issue, reaches a verdict, and the work that follows is large enough to want an orchestration. Everything the orchestration needs already exists in the single agent's worktree — the branch, the analysis, any probe tests it wrote (one such session had authored `tests/zz_probe_351.rs`). But `dispatch --orchestration` would create a *new* worktree from `main`, stranding all of it. The only workable path today is manual: the user opens an orchestration in the existing worktree themselves. That works, and it is what people actually do — but the orchestrator arrives with no context and re-derives the analysis the single agent had already finished.

So the CLI supports the case it does not need to (placement it invented) and does not support the case it does (placement the user already chose).

A second, smaller observation: the worktree half of `dispatch` is not where the value is. Creating a worktree is `git worktree add ../<name> -b <branch>`. Agents know `git`. The deck's actual contribution is spawning a configured agent or orchestration against a directory and delivering it a task.

## Solution Overview

Split the verb into the two steps it already contains, and expose only the second.

1. **Placement** — the agent creates the worktree or branch itself, with `git`. No CLI involvement.
2. **Spawning** — `dispatch` takes a **target directory** and a task, and starts a single agent or an orchestration there.

Dispatching into the current directory is then in-place continuation, and needs no second verb. The single agent → orchestration handoff becomes: the agent writes a brief into `.dot-agent-deck/`, then either recommends the user start an orchestration there, or (with approval) dispatches one into its own directory.

The rule that replaces the current implicit behaviour:

> Agents start orchestrations. Placement is an explicit argument. Whether it is a new directory or the current one follows from whether the work is a **new line** or a **continuation** — not from which verb was called.

### Why this does not reopen the "after-case"

PRD #220 explicitly and correctly rules out **mid-flight worktree adoption**: a running orchestrator creating a worktree partway through and expecting already-running workers to follow. That cannot work — worker pane cwds are frozen at spawn (`src/agent_pty.rs:736`), the orchestrator's cwd is neither movable nor reported to the daemon, and coordination files are pinned to the pane's recorded cwd via `pane_cwd_map`. #220's conclusion stands: **worktree is always a pre-spawn decision, never a runtime relocation.**

This proposal preserves that invariant exactly. The directory is still chosen *before* the spawn; nothing already running is relocated. What changes is only *who* chooses it and *when* — the agent, with `git`, instead of the verb, implicitly. A continuation spawns a **new** orchestration into an existing directory after the previous agent has finished; it does not move a live one.

## Scope

### In Scope

- `dispatch` accepts an explicit target directory and no longer creates worktrees.
- Removal of worktree/branch creation from the CLI, including the `../<repo>-dispatch-<name>` path derivation and the single-use-name guard that depends on it.
- The dispatcher seed teaches the two-step flow and, critically, **where** worktrees belong, so placement stays consistent once it is no longer enforced by code.
- Target/config resolution for an arbitrary directory (see Risks).
- Guidance for single agents on writing a handoff brief into `.dot-agent-deck/` under a unique name, and referencing it explicitly in the prompt given to the orchestrator.
- Tests and docs per CLAUDE.md rules 4, 5 and 11.

### Out of Scope

- **Mid-flight worktree adoption** — remains unsupported, per #220.
- **Automatic escalation.** A single agent may *recommend* an orchestration; the user decides. This is a deliberate limit: the agent that just spent its context producing an analysis is the worst-positioned party to judge whether that analysis warrants a team, and unguided judgement here reliably degrades into always escalating.
- **Auto-discovery of briefs.** Briefs are referenced explicitly by path, never found implicitly. This removes the stale-brief failure mode by construction rather than mitigating it — nothing auto-reads anything, so nothing stale can be silently applied, and no deletion step is needed. (A consume-on-read design was considered and rejected: it puts correctness on a cleanup path that a SIGKILL skips, which is precisely how #322's temp roots leak.)
- **Same-directory concurrency isolation** — still deferred to #140/#156; see Risks.

## Success Criteria

- An agent can create a worktree with `git` and dispatch a single agent or an orchestration into it, with the same end state `dispatch` produces today.
- An agent can dispatch an orchestration into **its own** directory, and that orchestration operates on the branch and files already present, without re-deriving prior analysis.
- A dispatched orchestration receives its task through the existing `.dot-agent-deck/orchestrator-context.md` path, unchanged from #222.
- Dispatched worktrees continue to land on disk-backed siblings in practice, with the convention documented where the seed can enforce it.
- `cargo test-fast` green per task; `cargo test-e2e` green pre-PR, including a PTY-attached L2 test covering dispatch-into-an-existing-directory.

## Milestones

- [ ] **M1 — `dispatch` takes a target directory.** The verb accepts an explicit directory and spawns there; worktree creation still present but bypassed when a directory is given.
- [ ] **M2 — Target/config resolution settled for an arbitrary directory.** Decide and implement whether the orchestration config comes from the invoking repo or the target directory (see Risks), with the choice recorded in this PRD.
- [ ] **M3 — Worktree creation removed from the CLI.** Path derivation and the name guard come out; the dispatcher seed gains the placement convention and the two-step flow.
- [ ] **M4 — In-place continuation works end to end.** A single agent writes a brief, dispatches an orchestration into its own directory, and the orchestrator works from the existing branch and files.
- [ ] **M5 — Tests.** L2 PTY coverage for dispatch-into-existing-directory and for in-place continuation; existing `orchestration/dispatch/001` and `/002` updated rather than duplicated, with `tests/CATALOG.md` entries revised.
- [ ] **M6 — Docs and cross-version.** User-facing docs for the two-step flow; rule 12 contract check, since this touches the daemon spawn path and orchestration.

## Key Files

- `src/dispatch.rs` — worktree derivation, `SpawnRequest` construction (`working_dir` is the field that carries placement, and the only one that differs between the two cases).
- `src/spawn.rs` — `compose_orchestrator_context` (`spawn.rs:117`, gated at `spawn.rs:587`), and the target-resolution comment at `spawn.rs:88` that M2 must resolve.
- `src/orchestrator_context.rs` — `prepare_orchestrator_prompt(config, cwd, task)`; already parameterised on cwd and task, so it needs no change.
- `src/ui.rs` — `DISPATCHER_SEED_PROMPT` (`ui.rs:531`) and `DISPATCHER_MODE_NAME`.
- `.dot-agent-deck.toml` — the orchestrator `prompt_template`, which already establishes the "write long context to `.dot-agent-deck/<slug>.md` and reference the path" norm this proposal generalises.

## Risks and Mitigations

- **Placement becomes prompt-enforced rather than code-enforced.** Today it is impossible to put a dispatched worktree on a tmpfs; afterwards it is only discouraged. CLAUDE.md rule 14 documents why that matters, and the failure is silent and misattributed — a `cc` linker error or a `SIGKILL` on `rustc`, with nothing pointing at the filesystem. *Mitigation*: state the convention explicitly in the dispatcher seed; consider a warning when the target directory is on a tmpfs, which is cheap and catches the case code no longer prevents.
- **Orphan worktrees become routine.** Worktree-and-spawn is atomic today; split, step 1 can happen without step 2. *Mitigation*: accept it, and keep the `agent/dispatch-*` branch prefix so `git worktree list` still identifies deck-created units mechanically.
- **Config resolution becomes ambiguous.** `spawn.rs:88` deliberately resolves the target from the config the user saw in `--list-targets`, *not* the worktree's. Those cannot diverge today because `dispatch` creates the tree it spawns into. With an arbitrary target directory they can — the directory may sit on a branch whose `.dot-agent-deck.toml` defines different orchestrations. *Mitigation*: M2 decides this explicitly rather than letting it fall out of the implementation.
- **Same-directory coordination-file collision.** Coordination files (`work-done-{role}.md`) are pinned to the pane's recorded cwd, and orchestration identity is keyed by `(name, cwd)`. Dispatching into a directory that already hosted an agent risks collision — the same root cause as #156 and the deferral in #140. *Mitigation*: require the previous agent to have finished before a continuation dispatch; note this PRD as another consumer of #156.

## Open Questions

- Does an orchestration spawned into an existing directory get a pane id that the delegate path can resolve? Dispatched orchestrator panes currently get string ids (`sched-dispatch-<name>-<n>-r0`) and fail with `delegate from unknown pane`, while normally-started panes (numeric ids) work — see PR #466. Whichever fix lands must cover this path too.
- Should the CLI retain a thin convenience wrapper that creates a worktree at the conventional location and dispatches into it, for the common new-line-of-work case? That would preserve today's one-step ergonomics and the rule-14 guarantee, at the cost of the orthogonality this PRD is arguing for.
- Should `dispatch` warn or refuse when the target directory already contains a live agent's coordination files?
