# PRD #468: Split dispatch into placement and spawning

**Status**: Proposed
**Issue**: [#468](https://github.com/vfarcic/dot-agent-deck/issues/468)
**Builds on**: [#220](https://github.com/vfarcic/dot-agent-deck/issues/220) (the `dispatch` verb + dispatcher mode), [#222](https://github.com/vfarcic/dot-agent-deck/issues/222) (orchestrator context on disk)
**Interacts with**: [#425](https://github.com/vfarcic/dot-agent-deck/issues/425) (worktree ownership marker written at creation time), [#466](https://github.com/vfarcic/dot-agent-deck/pull/466) (dispatched-orchestration delegate routing)
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

PRD #220 explicitly and correctly rules out **mid-flight worktree adoption**: a running orchestrator creating a worktree partway through and expecting already-running workers to follow. That cannot work — worker pane cwds are frozen at spawn (`spawn()` copies `SpawnOptions::cwd` onto the `CommandBuilder` at `src/agent_pty.rs:996`, a few lines before `spawn_command` consumes it at `:1071`; the registry then exposes `resize`/`set_agent_label`/`set_agent_type` but no cwd mutator, so nothing can move a live pane afterwards), the orchestrator's cwd is neither movable nor reported to the daemon, and coordination files are pinned to the pane's recorded cwd via `pane_cwd_map`. #220's conclusion stands: **worktree is always a pre-spawn decision, never a runtime relocation.**

This proposal preserves that invariant exactly. The directory is still chosen *before* the spawn; nothing already running is relocated. What changes is only *who* chooses it and *when* — the agent, with `git`, instead of the verb, implicitly. A continuation spawns a **new** orchestration into an existing directory after the previous agent has finished; it does not move a live one.

## Ownership and cleanup

Splitting placement out of `dispatch` splits a **lifecycle**, not just a creation step, and the second half needs saying explicitly or an implementer will inherit today's behaviour by default.

`dispatch` does not merely create a worktree today — it **claims it for deletion**. `handle_dispatch` calls `record_worktree(&ctx.worktrees, &paths.worktree_dir, &clone_dir, RemovalPolicy::KeepIfDirty)` (`src/dispatch.rs:338-343`), registering the tree in the daemon-wide `WorktreeRegistry` that the tab-close handler reads and acts on (`src/daemon_protocol.rs:1520-1539`, which calls `worktree_still_in_use`, then `take_worktree`, then `remove_worktree`). Its unit tests guard this under the heading `// --- removal policy (the PRD #120 regression) ---` (`src/dispatch.rs:584`), with the registry round-trip at `:630`.

Carried into this design unchanged, that is destructive in exactly the case the PRD exists to enable. `dispatch` would record whatever directory it is handed, so **closing the tab would delete a directory the deck did not create** — for a continuation, the *originating agent's* worktree, with the branch, the analysis and the probe tests the Problem Statement exists to preserve. `RemovalPolicy::KeepIfDirty` does not save it: it removes **clean** trees by design (`src/issue_dispatch_run.rs:191-213` probes `git status --porcelain` and returns early only when that probe reports uncommitted work, or fails outright so dirtiness is unknown), and the scenario describes an agent that has committed its work.

There is one accidental guard today, and this design must not rely on it: the close handler skips removal while `worktree_still_in_use(&registry.agent_records(), &worktree)` (`src/issue_dispatch_run.rs:174-178`) finds any live agent record whose `worktree_of_record` is that path. That protection is switched off by exactly the condition a continuation dispatch wants — the previous agent having finished — so under this PRD's own recommended usage the last tab close would delete the directory.

**The rule, therefore:** `dispatch` records for removal only a directory **it created**. After M3 it creates none, so `record_worktree` comes out of the dispatch path entirely and cleanup becomes the agent's responsibility alongside creation. In-place continuation carries the explicit guarantee that **`dispatch` never removes the target directory**. Removing worktrees the deck did not create is the job of `worktree reclaim` (#422/#425), which asks before touching a tree it cannot prove it made — the correct model for directories chosen by someone else.

This is the same decision as the orphan-worktree risk below, not a separate one: the deck stops owning dispatched trees at both ends of their life.

## Scope

### In Scope

- `dispatch` accepts an explicit target directory and no longer creates worktrees. Proposed signature: `dot-agent-deck dispatch <name> --dir <path> [--task <text>|--task-file <path>] (--single | --orchestration [<name>])`, with `--dir` defaulting to the caller's cwd so in-place continuation is the zero-extra-argument case.
- `<name>` is **retained**, and keeps two of the three roles it carries today: the tab/notifier name (`task_name: format!("dispatch-{name}")`, `src/dispatch.rs:348`) and the pane-id component (`sched-dispatch-<name>-<n>-r0`). Only the worktree-slug role goes. The `--help` text at `src/main.rs:114` currently advertises exactly the role being removed (*"used for worktree naming"*), as does the subcommand's own doc comment at `:111-112` (*"Create a git worktree and start an isolated line of work inside it"*); both are corrected here.
- Removal of worktree/branch creation from the CLI, including the `../<repo>-dispatch-<name>` path derivation and the single-use-name guard that depends on it. `derive_dispatch_paths` has a single production caller — `handle_dispatch` (`src/dispatch.rs:262`), reached only from the daemon's dispatch-signal arm (`DaemonMessage::Dispatch`, `src/daemon.rs:1759`, which calls `handle_dispatch` at `:1818`) — so the blast radius is confined to this verb. Issue-dispatch (#120) has its own derivation (`derive_issue_paths`, `src/issue_dispatch_run.rs`) and is untouched.
- Removal of `record_worktree` from the dispatch path, per **Ownership and cleanup** above, with the `RemovalPolicy::KeepIfDirty` tests at `src/dispatch.rs:584`/`:630` and the `dispatch/close/001` e2e entry revised to match rather than silently dropped.
- **The replacement interlock**: `dispatch` refuses when the target directory already holds a live agent's coordination files. This is what takes over from the single-use-name guard for the continuation case — see Decisions.
- The dispatcher seed teaches the two-step flow and, critically, **where** worktrees belong, so placement stays consistent once it is no longer enforced by code.
- Target/config resolution for an arbitrary directory (see Risks).
- Guidance for single agents on writing a handoff brief into `.dot-agent-deck/` under a unique name, and referencing it explicitly in the prompt given to the orchestrator.
- Tests and docs per CLAUDE.md rules 4, 5 and 11.

### Out of Scope

- **Mid-flight worktree adoption** — remains unsupported, per #220.
- **Automatic escalation.** A single agent may *recommend* an orchestration; the user decides. This is a deliberate limit: the agent that just spent its context producing an analysis is the worst-positioned party to judge whether that analysis warrants a team, and unguided judgement here reliably degrades into always escalating.
- **Auto-discovery of briefs.** Briefs are referenced explicitly by path, never found implicitly. This removes the stale-brief failure mode by construction rather than mitigating it — nothing auto-reads anything, so nothing stale can be silently applied, and no deletion step is needed. (A consume-on-read design was considered and rejected: it puts correctness on a cleanup path that a SIGKILL skips, which is precisely how #322's temp roots leak.)
- **Same-directory concurrency isolation** — still deferred to #140/#156; the interlock above refuses the collision rather than isolating it.
- **Retro-cleanup of worktrees created by the old one-step `dispatch`.** They keep their registry entries for as long as the daemon lives and close as they do today; nothing is migrated.

## Decisions

- **Experimental flag → no.** Decided during PRD creation, per CLAUDE.md rule 9. This changes the shape of an existing, already-graduated surface rather than adding a new one: #220 shipped the dispatcher behind `features::show_dispatcher()` and graduated it in the same PR, and its Decisions record that *"the `dispatch` verb and its daemon handler were always ungated"*. Gating the directory argument would put a flag on one argument of an ungated verb, which is not a presentation switch and would leave the two halves of the same command on opposite sides of a feature flag. The risk the flag usually manages — unfinished behaviour touching real state — is addressed directly by the ownership rule and the interlock above, both of which make the verb *less* able to destroy state than it is today.
- **CLI signature → `--dir`, defaulting to cwd.** A flag rather than a second positional, so `<name>` stays where it is and every existing invocation in the dispatcher seed and `docs/dispatcher-mode.md` keeps its shape. Defaulting to the caller's cwd makes in-place continuation the zero-extra-argument case, which is the flow being added; requiring an explicit `.` would tax the common case to serve the rarer one.
- **Ownership → `dispatch` never removes a directory it did not create.** Full reasoning in **Ownership and cleanup** above. After M3 it creates none, so it removes none.
- **Interlock → refuse, not warn.** `dispatch` refuses when the target directory holds a live agent's coordination files. Warning and proceeding would leave the `(name, cwd)` orchestration-identity collision — #156's root cause — reachable through a *supported* path rather than only by user error, in a design whose whole purpose is to make dispatching into an already-populated directory normal. Note that collisions on a **new** directory stay interlocked by git itself: `git worktree add ../x -b agent/foo` refuses when the path or branch exists, so today's `AlreadyClaimed` (`src/dispatch.rs:300`) and `BranchExists` (`:316`) messages become git's. What is genuinely unguarded after the split is continuation into an existing directory, which is precisely where this check goes.
- **The deck's own context file → fixed name, overwrite stands.** `prepare_orchestrator_prompt` keeps writing `.dot-agent-deck/orchestrator-context.md` under that exact name, and keeps overwriting it unconditionally. This is deliberately *not* the treatment agent-written briefs get, and the asymmetry is the point: a brief is **authored** content whose loss is unrecoverable, so it gets a unique name; the context file is **derived** content — regenerated from the orchestration config plus the caller's task on every dispatch — so a continuation superseding its predecessor's copy destroys nothing that cannot be rebuilt by dispatching again. A unique per-dispatch name was considered and rejected: the path is a hard-coded string everywhere that names it, and they would all have to move together — the pointer line the orchestrator is actually sent (`src/orchestrator_context.rs:206`, `:211`), the developer-facing contract in `docs/develop/dispatcher-mode.md:39`, this PRD's own SC3 *"unchanged from #222"*, and the assertions in `src/orchestrator_context.rs:300`/`:317`/`:337`, `src/dispatch.rs:884`, `tests/e2e_dispatcher_mode.rs:729` and `tests/e2e_pane_send_result.rs:164`/`:229` — and paying that to version a derived file buys an audit trail nobody asked for. The concurrent case — two orchestrations sharing one cwd and one context file, where the second write is invisible to the first — is excluded by the interlock above rather than by the filename, which is the correct place to exclude it.
- **CLAUDE.md rule 12 → additive wire change, no `PROTOCOL_VERSION` bump, one `.breaking.md` fragment.** The target directory is a new field on `DispatchSignal` (`src/event.rs:940`), the hook-socket wire struct — not on `SpawnRequest` (`src/spawn.rs:87`), which is `#[derive(Debug, Clone)]` (`:86`) with no serde derives and never crosses a version boundary. The precedent sits beside it: `shape`'s doc comment (`src/event.rs:945-953`) records that `#[serde(default)]` keeps such a field additive and does not move `PROTOCOL_VERSION`, which therefore stays at its current `7` (`src/daemon_protocol.rs:206`). What **does** need versioning is the reverse direction. Serde ignores unknown fields, so a **new CLI against an old daemon** has its directory silently dropped, falls through to `derive_dispatch_paths(&ctx.working_dir, name)` and spawns into a fresh `../<repo>-dispatch-<name>` — the agent asks for in-place continuation and gets a new tree off the caller's cwd, with no error. Same wire, different meaning: that is rule 12's semantic break, so it ships with a `changelog.d/468.breaking.md` fragment and a **minor** bump while `0.x`. **M7** confirms this by running the cross-version test rather than deciding it.

## Success Criteria

- An agent can create a worktree with `git` and dispatch a single agent or an orchestration into it, with the dispatched unit running in that directory and its task delivered — the outcome `dispatch` achieves today, reached in two steps instead of one.
- An agent can dispatch an orchestration into **its own** directory, and that orchestrator works from the existing branch and files rather than a fresh checkout.
- A dispatched orchestration receives its task through the existing `.dot-agent-deck/orchestrator-context.md` path, unchanged from #222.
- Dispatching into a directory that holds a live agent's coordination files is refused, with a message naming the directory and what was found.
- Closing a dispatched tab leaves the target directory on disk.
- Dispatched worktrees continue to land on disk-backed siblings in practice, with the convention documented where the seed can enforce it.
- `cargo test-fast` green per task; `cargo test-e2e` green pre-PR, including a PTY-attached L2 test covering dispatch-into-an-existing-directory.

## Milestones

- [ ] **M1 — `dispatch` takes a target directory.** The verb accepts `--dir` (defaulting to cwd) and spawns there; worktree creation still present but bypassed when a directory is given.
- [ ] **M2 — Target/config resolution settled for an arbitrary directory.** Decide and implement whether the orchestration config comes from the invoking repo or the target directory (see Risks), with the choice recorded in this PRD.
- [ ] **M3 — Worktree creation removed from the CLI.** Path derivation and the name guard come out; the coordination-file interlock goes in as their replacement; the dispatcher seed gains the placement convention and the two-step flow.
- [ ] **M4 — Ownership and cleanup.** `record_worktree` comes out of the dispatch path; the removal-policy tests at `src/dispatch.rs:584`/`:630` and the `dispatch/close/001` entry are revised to assert the new contract (the target directory survives tab close) rather than deleted; `src/main.rs:111-115`'s `--help` text is corrected.
- [ ] **M5 — In-place continuation works end to end.** A single agent writes a brief, dispatches an orchestration into its own directory, and the orchestrator works from the existing branch and files.
- [ ] **M6 — Tests.** L2 PTY coverage for dispatch-into-existing-directory and for in-place continuation; existing `orchestration/dispatch/001` and `/002` updated rather than duplicated, with `tests/CATALOG.md` entries revised.
- [ ] **M7 — Docs and cross-version.** Update the published `docs/dispatcher-mode.md` (listed in `site/sidebars.js:15`) for the two-step flow, and the developer-facing `docs/develop/dispatcher-mode.md` alongside it so the two do not drift; run the rule 12 cross-version test to confirm the Decisions entry above, and land the `changelog.d/468.breaking.md` fragment.

## Key Files

- `src/dispatch.rs` — worktree derivation (`:194-210`), the name guards (`:300`, `:316`), `record_worktree` (`:338-343`), and `SpawnRequest` construction (`:347`; `working_dir` is the field that carries placement, and the only one that differs between the two cases).
- `src/event.rs` — `DispatchSignal` (`:940`), the hook-socket wire struct the target directory is added to; `shape`'s doc comment at `:945-953` is the additive-field precedent.
- `src/daemon_protocol.rs:1520-1539` — the tab-close cleanup path that M4 stops feeding; `PROTOCOL_VERSION` (`:206`) is the value the rule 12 decision above says does not move.
- `src/issue_dispatch_run.rs` — `record_worktree` (`:127`) / `take_worktree` (`:161`) / `remove_worktree` (`:191`) and `worktree_still_in_use` (`:174`); issue-dispatch keeps all of them, dispatch stops using them.
- `src/spawn.rs` — `compose_orchestrator_context` (`spawn.rs:130`, gated at `spawn.rs:672`), and the target-resolution comment at `spawn.rs:97-107` that M2 must resolve.
- `src/orchestrator_context.rs` — `prepare_orchestrator_prompt(config, cwd, task)` (`:186-190`); already parameterised on cwd and task, so it needs no signature change, and its fixed filename is settled in Decisions above.
- `src/main.rs:111-115` — the `Dispatch` subcommand doc comment and `<name>`'s `--help` text, both of which describe the worktree role being removed.
- `src/ui.rs` — `DISPATCHER_SEED_PROMPT` (`ui.rs:536`) and `DISPATCHER_MODE_NAME` (`ui.rs:523`).
- `.dot-agent-deck.toml:156` — the orchestrator `prompt_template`'s *"if context is long, write it to `.dot-agent-deck/<task-slug>.md` and reference that path in `--task`"* line, the norm this proposal generalises.

## Risks and Mitigations

- **Placement becomes prompt-enforced rather than code-enforced.** Today it is impossible to put a dispatched worktree on a tmpfs; afterwards it is only discouraged. CLAUDE.md rule 14 documents why that matters, and the failure is silent and misattributed — a `cc` linker error or a `SIGKILL` on `rustc`, with nothing pointing at the filesystem. *Mitigation*: state the convention explicitly in the dispatcher seed; consider a warning when the target directory is on a tmpfs, which is cheap and catches the case code no longer prevents.
- **Orphan worktrees become routine, and deck-created units stop being mechanically detectable.** Worktree-and-spawn is atomic today; split, step 1 can happen without step 2. The obvious mitigation — keep the `agent/dispatch-*` prefix so `git worktree list` still identifies deck-created units — **does not survive M3**: the only thing that produces that prefix is the code M3 deletes (`slug = format!("dispatch-{clean_name}")` at `src/dispatch.rs:196`, `branch = format!("agent/{slug}")` at `:205`). Afterwards the prefix can only be *requested* by the seed, never produced, so orphan detection built on it is best-effort and unenforceable. *Mitigation*: state the prefix as a seed-enforced convention and accept that posture explicitly; where a mechanical answer is needed, the code-level mechanism is **#425**'s ownership marker in the worktree's git metadata dir, not a branch-name heuristic. Note the interaction runs both ways and neither document currently records it: #425 says *"anywhere else the deck creates a worktree should do the same"*, while after M3 the deck creates none on this path — so either #425 is moot for dispatch, or its marker has to be written by whatever now does the creating. That is worth settling in #425 with this PRD in hand.
- **Config resolution becomes ambiguous.** `spawn.rs:97-107` deliberately resolves the target from the config the user saw in `--list-targets`, *not* the worktree's. Those cannot diverge today because `dispatch` creates the tree it spawns into. With an arbitrary target directory they can — the directory may sit on a branch whose `.dot-agent-deck.toml` defines different orchestrations. *Mitigation*: M2 decides this explicitly rather than letting it fall out of the implementation.
- **Same-directory coordination-file collision.** Coordination files (`work-done-{role}.md`) are pinned to the pane's recorded cwd, and orchestration identity is keyed by `(name, cwd)`. Dispatching into a directory that already hosted an agent risks collision — the same root cause as #156 and the deferral in #140. *Mitigation*: the coordination-file interlock in In Scope is the enforcer — `dispatch` refuses rather than relying on the caller to have waited. Note this PRD as another consumer of #156.
- **The deck's own context file has a fixed name and is written unconditionally.** In Scope is careful that *agent*-written briefs get unique names; `prepare_orchestrator_prompt` has no such protection — `src/orchestrator_context.rs:191-201` writes a fixed `orchestrator-context.md` into `<target>/.dot-agent-deck/` with no existence check. Once the target is an arbitrary directory that may already have hosted an orchestration, a continuation silently overwrites its predecessor's context file. *Mitigation*: **decided, not deferred** — the overwrite stands, because the file is derived rather than authored content; see the context-file entry in Decisions for the full reasoning and for why the concurrent case is the interlock's job rather than the filename's. M5 asserts the resulting behaviour (a continuation's orchestrator reads its own task, not its predecessor's) instead of re-opening the choice.

## Open Questions

- ~~Does an orchestration spawned into an existing directory get a pane id that the delegate path can resolve?~~ **Answered and now confirmed: PR #466 merged on 2026-08-14, and this branch carries it.** The original framing assumed the string id shape (`sched-dispatch-<name>-<n>-r0`) was causal; #466 establishes it was incidental — nothing ever *recorded* the dispatched pane id, so any shape would have been equally unknown, and `handle_delegate` has always been shape-agnostic (a `HashMap<String>` lookup). Verified against the merged tree: `AppState::register_orchestration_role` (`src/state.rs:3329`) is called for **every** role from inside `spawn::spawn` (`src/spawn.rs:401`, registering at `:580`), the same function this PRD's dispatch keeps calling, so registration is independent of both id shape and target directory. The sequencing note this entry used to carry is discharged — #466's rewrite of `src/dispatch.rs` and `src/spawn.rs` is already in the base M1–M3 will build on, so there is nothing left to race.
- Should the CLI retain a thin convenience wrapper that creates a worktree at the conventional location and dispatches into it, for the common new-line-of-work case? That would preserve today's one-step ergonomics and the rule-14 guarantee, at the cost of the orthogonality this PRD is arguing for.
- ~~Should `dispatch` warn or refuse when the target directory already contains a live agent's coordination files?~~ **Promoted to a Decision** — it refuses, and it lands with M3 as the replacement for the single-use-name guard rather than after it.
