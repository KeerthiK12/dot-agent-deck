# PRD #249: Delegate prompt lost or unsubmitted on `clear=true` respawn

**Status**: Not started
**Priority**: High
**Created**: 2026-07-28
**GitHub Issue**: [#249](https://github.com/vfarcic/dot-agent-deck/issues/249)
**Consolidates**: [#199](https://github.com/vfarcic/dot-agent-deck/issues/199)
**Feature flag**: **No** `experimental` gate (CLAUDE.md rule 9). A correctness fix on an existing delivery path with no new user-visible surface; gating it would leave the default orchestration configuration broken for anyone who does not opt in.

## Problem Statement

The delegate path writes a task prompt into a freshly respawned worker **with no readiness buffer**, while the structurally identical orchestrator spawn-time path has had a 500 ms one since v0.27.x. The delegate path is missing a guard that this project already built, proved, and regression-tested for its sibling.

### The mechanism is already confirmed in this codebase

From `CHANGELOG.md:358-361`, fixing the *orchestrator spawn-time* prompt:

> The root cause is a timing race: Claude Code's `SessionStart` hook fires early in its boot sequence, before its TUI input is ready to interpret `\r` as submit. The spawn-time path, which sends the role prompt immediately after detecting `SessionStart`, was writing into a pane that had not yet entered submit-aware mode. The fix adds a 500ms readiness buffer (`SPAWN_TIME_READINESS_BUFFER`) … A regression test (`tests/spawn_time_role_prompt_submit_after_session_start.rs`) drives a Python slow-readiness agent stub and toggle-verifies the fix: the test fails at `BUFFER=0` and passes at `BUFFER=500`.

So this is not a hypothesis to be tested — it is a measured, toggle-verified property of Claude Code's startup. @bustapipes independently reverse-engineered the same conclusion in #199 and correctly identified that `dispatch_one_owned` never received the equivalent guard.

### Where the gap is

`dispatch_one_owned` waits for the respawned agent's `SessionStart`, then writes immediately (`src/state.rs:1252-1265`):

```rust
// Legacy PTY injection for every non-pi-native path: claude / opencode
// workers, and `clear = false` pi workers …
if let Err(e) = registry.write_to_pane_and_submit(&pane_id, &one_liner).await
```

Nothing sits between the readiness signal and the write. `write_to_pane_internal` (`src/agent_pty.rs:2989`) then delivers in three steps, holding the per-agent writer mutex across all of them:

1. `encode_pane_payload(text)` — wraps in bracketed paste (`ESC[200~…ESC[201~`) **only if the text contains `\n`**, else raw bytes (`src/pane_input.rs:53-70`)
2. `write_all(payload)` + flush
3. `SubmitMode::Submit` → `sleep(SUBMIT_DELAY)` = **150 ms** → `write_all(b"\r")` + flush (`src/agent_pty.rs:3049-3062`)

`SUBMIT_DELAY` is a *within-write* sequencer that keeps the Enter from fusing to the payload. It is not a readiness gate — the whole write can still arrive before the agent is interactive.

### Two severities, one cause

Where the write lands on the agent's startup timeline determines the symptom, which is why reports look like different bugs:

| Timing | payload | CR (+150 ms) | Observed symptom | Reported by |
|---|---|---|---|---|
| Agent fully ready | lands | submits | works | maintainer, usual case |
| Agent ready, not yet submit-aware | lands | **swallowed** | text sits in input box until a human presses Enter | maintainer, **rare** |
| Agent not yet accepting input | **lost** | lost | nothing appears; worker idle forever, no events | #199 reporters, **consistent** |

The maintainer's rare "written but not submitted" and the reporters' consistent "prompt never lands" are the same missing guard sampled at different points. This also explains the version correlation in #199 (Claude ≥ 2.1.199 widened the unready window on their machines) without requiring a version cliff, and why the failure rate varies by machine speed and load.

### Prior art: the frequency was already reduced once, not eliminated

`CHANGELOG.md:168` records an earlier fix for the *unsubmitted* variant:

> Delegating to a worker role now injects a **single-line** prompt into the worker pane instead of a multi-line block. Previously the multi-line prompt (which carried the `## When done` completion instructions) could land in the worker's input as a compacted bracketed-paste that **sat unsubmitted until an operator pressed Enter** — stalling unattended orchestration.

That change made the delegate prompt single-line (`compose_delegate_prompt` → `one_liner`, `src/state.rs:1099`), which removed bracketed-paste framing from the delegate path entirely — a real improvement, and why the symptom went from frequent to rare. But it addressed the *payload shape*, not the *readiness race*, so a residual failure rate survived. **Bracketed paste is therefore not implicated in current delegate failures**; the remaining cause is timing alone.

### Three of four agents were fixed individually; the seam was never fixed

| Agent | `clear = true` delegate path | Status |
|---|---|---|
| **Pi** | native — daemon stashes a seed, pi's extension pulls it via `get-seed` → `sendUserMessage`, no PTY injection (`src/state.rs:1112-1166`) | fixed, PRD #201 |
| **Codex** | wrapper readiness + stable launch shape | fixed, PR #237 (merged 2026-07-28) |
| **Claude** | legacy PTY injection, unguarded | **broken** |
| **OpenCode** | same legacy path, exposure unverified | **presumed broken** |

The `is_pi_native` special-case at `src/state.rs:1112` is the tell: readiness keeps getting solved per-agent at the call site, so any newly added agent inherits the unguarded path by default. Fixing the shared seam is what stops this recurring, and it makes the fix agent-agnostic — which matters because the maintainer's own rare reproduction may not even have been Claude.

### The failure is silent

`write_to_pane_and_submit` returning `Ok` means bytes reached a PTY, not that an agent consumed them. Combined with a respawn that legitimately kills the old child, the user sees a healthy card on an idle agent with no way to tell "thinking" from "never got the task". Four reporters described the same confusion; none could have guessed `clear = false` before @xclydes diagnosed it.

### Reporters

| Reporter | dot-agent-deck | Claude Code | Contribution |
|---|---|---|---|
| @bustapipes | any | 2.1.199 / .200 / .201 | original report + correct root cause |
| @peterjcarroll | first-time user | — | confirmation |
| @AkosHosszu | 0.32.1 | 2.1.207 | confirmation |
| @xclydes | — | — | **narrowed it to `clear = true`; found the `clear = false` workaround** |
| @tomikonio | 0.33.0 | 2.1.218 | confirmed workaround across all agents |

## Solution Overview

1. **Gate the legacy injection** on a post-`SessionStart` readiness buffer at the shared seam (`src/state.rs:1255`), mirroring `SPAWN_TIME_READINESS_BUFFER`.
2. **Make the failure visible** so a future regression surfaces instead of manifesting as an idle worker.
3. **Verify OpenCode's exposure** rather than assuming it.

### Mechanism — an awaited delay, not the TUI's polled gate

`SPAWN_TIME_READINESS_BUFFER` is **not** a sleep: `should_inject_spawn_time_prompt` (`src/ui.rs:1329`) compares timestamps and returns a bool that the TUI render loop polls each frame. That does not transplant — `dispatch_one_owned` is async daemon code with no render loop — so an awaited delay is the right mechanism here.

This is unambiguously idiomatic on this path: `src/agent_pty.rs:3051` already awaits `sleep(SUBMIT_DELAY)` inside the very function being called. The forbidden-sleep lint (Decision 21, `xtask/linkage-check/src/main.rs:189`) sits inside an `if is_e2e` branch (`:179`) and applies to e2e test bodies only.

Make the buffer env-overridable, mirroring `DOT_AGENT_DECK_SESSION_START_WAIT_MS` for `SESSION_START_WAIT_TIMEOUT` (`src/state.rs:58-62`), so the e2e harness never pays it.

### The durable answer is #243

A fixed delay is a heuristic tuned to one agent's current startup timing, and it will drift — @bustapipes said so themselves: *"Ideally, a more durable solution can be found to replace the following hack."* #243 (wrapper-side "TUI ready" signal, retiring the load-bearing 30 s wait) and #234 (screen-state observation for hookless agents) are the real fix. This PRD ships the stopgap and files its own retirement.

## Scope

### In Scope

- Post-`SessionStart` readiness gate before the legacy PTY injection at `src/state.rs:1255`, on both the observed and timeout branches, env-overridable for tests.
- Surfacing an undelivered prompt instead of failing silently — at minimum a `warn!`, ideally a notice in the orchestrator pane (`write_to_pane_notice`, precedent at `src/state.rs:1213`).
- Empirical check of whether OpenCode `clear = true` delegation is affected, and coverage either way.
- Fast-tier coverage extending `tests/delegate_prompt_injection.rs` / `tests/orchestration_delegate.rs`.
- A slow-readiness regression test on the `tests/spawn_time_role_prompt_submit_after_session_start.rs` model (see M4).
- Docs for `clear`'s delivery semantics and the interim workaround; changelog fragment; rule 12 answer.

### Out of Scope

- **Replacing the buffer with a real readiness signal** — #243 / #234. This PRD is explicitly the stopgap.
- **Changing `clear` semantics.** Respawn-kills-the-child is intended (`src/state.rs:1117-1126`); the bug is delivery after respawn.
- **Revisiting Pi's native path or Codex's wrapper fix** — both work.
- **Unifying all four agents onto one delivery mechanism.** Probably right eventually; far larger than this, and not needed to unblock the reporters.
- **Re-litigating the payload shape.** The single-line change (`CHANGELOG.md:168`) already removed bracketed paste from this path.

## Technical Approach

### M1 — readiness gate

Add the buffer in the `Ok(new_agent_id)` arm after `wait_for_session_start`, on the non-pi-native path. Two details:

- Apply it on **both** branches — `observed == true` and the timeout fallback (`!observed`, `src/state.rs:1189`). A timeout means readiness was never confirmed, which is *more* reason to wait. @bustapipes' patch sketch put it only in the `observed` branch.
- Pick the value from measurement, not symmetry. The spawn path uses 500 ms for a *warm* case; @bustapipes proposed 1000 ms for respawn, which is a cold agent start and plausibly needs more. The slow-readiness harness in M4 can find the real threshold instead of guessing.

### M2 — OpenCode exposure

OpenCode shares the legacy path so it is presumed affected, but no reporter confirmed it and its startup timing is its own. Verify with a real OpenCode worker under `clear = true`. Note that OpenCode status reporting itself was only fixed in 0.34.0, so any earlier attempt to observe this would have been confounded by #204's bug.

### M3 — make failure visible

The silent-success property is what turned a timing bug into four confused users. Consumption can't be proven from the write side, but the *symptom* is detectable: a worker that received a delegate and emitted no event within a window. Cheapest honest version is a `warn!`; a notice in the orchestrator pane is better and the primitive already exists. Land this **with** M1 rather than deferring — without it, a regression is invisible again.

### M4 — tests

The key insight for testability: **the fix does not require reproducing the failure on a fast machine.** `tests/spawn_time_role_prompt_submit_after_session_start.rs` drives a Python stub that is *deliberately slow to become ready*, then toggle-verifies `BUFFER=0` fails and `BUFFER=500` passes. A real agent cannot be made reliably slow; a stub can. Transplant that harness to the delegate path — this is the one place a stand-in is *more* rigorous than a real agent, because it makes an intermittent race deterministic.

- **Fast tier**: extend `tests/delegate_prompt_injection.rs` and `tests/orchestration_delegate.rs` to assert the gate applies on both branches.
- **Slow-readiness regression**: the toggle test above. This is the primary proof.
- **Real-agent e2e** (rule 4): a real Claude worker on Haiku, respawned via `clear = true`, asserted to act on the task. `scheduler/dispatch/013` is the reference harness; `tests/e2e_delegate_work_done_chain.rs` and `tests/e2e_codex_delegate.rs` are the nearest shapes. Uniquely-named sentinel file so the assertion survives phrasing variance. Note this proves the happy path, not the race — the stub test is what pins the bug.
- Bug fix, so **no ` [reel]` marker**.

### Cross-version contract (CLAUDE.md rule 12)

Touches the daemon and the delegate path, so the answer is owed. Expected: **no** `PROTOCOL_VERSION` bump — daemon-internal timing before an existing PTY write, no wire shape or field meaning moved. Run the previous-release-daemon manual check regardless (branch TUI against older daemon; confirm a delegate still routes and work-done/status hooks arrive), since delegate routing is exactly what this touches.

## Diagnostics to request from #199 reporters

The maintainer cannot reproduce the severe variant, so the reporters are the instrument. Ask for:

1. **Does the prompt text appear in the worker's input box?** The single most valuable question — it splits "payload lost" from "CR swallowed" and tells us which severity they have.
2. **Does pressing Enter manually submit it?** If yes, the payload is intact and only the CR was lost.
3. **`RUST_LOG=pane_write=trace` daemon logs.** This instrumentation was added by the *same* spawn-path fix (`CHANGELOG.md:361`) precisely for this class of bug: it logs the payload bytes and the terminator as separate events, each carrying `pane_id` and `agent_id`, so an operator can see whether the framing is bracketed-paste and whether the terminator was `\r` or `\n` (`src/agent_pty.rs:3036-3062`).
4. **Which agent** the failing worker runs, and its version.
5. **Whether `clear = false` fixes it** — already confirmed by two reporters; useful as a control for any new reporter.

## Success Criteria

- A `clear = true` Claude worker receives **and submits** its delegated task, verified against a current Claude Code version.
- Holds on the timeout fallback path, not only when `SessionStart` is observed.
- The slow-readiness toggle test fails with the buffer at 0 and passes at the chosen value.
- The maintainer's rare "written but not submitted" recurrence disappears.
- `clear = false` continues to work unchanged.
- OpenCode's exposure is established by observation and works either way.
- Pi's native path and Codex's wrapper path untouched and still passing.
- An undelivered prompt produces a visible signal instead of silence.
- The e2e harness does not pay the new buffer.
- All four reporters' configurations work without `clear = false`.

## Milestones

- [ ] **M1**: Readiness gate applied before the legacy injection on both branches, env-overridable
- [ ] **M2**: OpenCode `clear = true` exposure established empirically and covered
- [ ] **M3**: Undelivered-prompt failure surfaced rather than silent
- [ ] **M4**: Slow-readiness toggle test (fails at 0, passes at chosen buffer) plus fast-tier assertions and a real-agent e2e
- [ ] **M5**: Docs describe `clear`'s delivery semantics; changelog fragment; rule 12 answer recorded
- [ ] **M6**: #199 closed with the fix referenced, reporters asked to confirm; retirement dependency filed against #243

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| The buffer is tuned to today's Claude timing and drifts | Explicitly a stopgap; M6 files retirement against #243. Measure the real window via the M4 harness and record it so the next drift is diagnosable. |
| A too-large buffer makes delegation feel sluggish | Once per respawn, only on `clear = true` — not per message. Use the smallest value the harness shows reliable. |
| OpenCode turns out unaffected and the gate is wasted there | Harmless: a short wait before a write that would have succeeded. M2 settles the fact. |
| Reporters never supply diagnostics, so the severe variant stays unconfirmed | The fix is justified by the spawn-path precedent alone. Ship on that basis; treat reporter confirmation as validation, not a gate. |
| The fix silently regresses because the failure mode is invisible | Exactly what M3 addresses — land it with M1. |
| The real-agent e2e is flaky and erodes trust in the suite | Pre-PR tier only, never CI (rule 5). The stub test carries the actual regression signal. |

## Open Questions

1. **What is the real readiness window on a respawned agent?** 500 ms (spawn path) and 1000 ms (@bustapipes' proposal) are both guesses. The M4 harness can measure it.
2. **Should the gate live in `dispatch_one_owned` or inside `write_to_pane_and_submit`?** Pushing it down covers every caller, not just delegate — broader protection, but it would also delay writes that are not post-respawn and don't need it.
3. **Do agents need different windows?** If Claude and OpenCode differ materially, the buffer belongs in the agent registry (`src/agent_registry.rs`) rather than as one constant.
4. **Should the CR get its own readiness re-check?** The maintainer's variant is specifically a lost CR *after* a landed payload, which a single pre-write gate may not fully close if the agent becomes input-ready but not submit-aware mid-write. A second short gate before the terminator, or a verify-and-retry, may be needed.
5. **Is `SUBMIT_DELAY` = 150 ms adequate on a cold agent?** It was chosen for warm panes. The cold-start case may want a larger value, which would be a narrower fix than the pre-write gate and might address the maintainer's variant on its own.

## Verification Notes (from triage)

Recorded so the implementer does not re-derive them:

- `src/state.rs:1252-1265` — the unguarded write, comment-labelled "Legacy PTY injection for every non-pi-native path: claude / opencode workers".
- `src/state.rs:1099` — `compose_delegate_prompt` produces the single-line `one_liner`, so `encode_pane_payload` does **not** apply bracketed-paste framing on this path.
- `src/state.rs:1112` — `is_pi_native` gate; `:1146-1166` — the native seed path that `return`s before injection.
- `src/state.rs:61` — `SESSION_START_WAIT_TIMEOUT` = 30 s, overridable via `DOT_AGENT_DECK_SESSION_START_WAIT_MS`.
- `src/ui.rs:1313` — `SPAWN_TIME_READINESS_BUFFER` = 500 ms; `:1329` — `should_inject_spawn_time_prompt`, a polled predicate, not a sleep.
- `src/agent_pty.rs:2989` — `write_to_pane_internal`; `:3051` — the existing production `sleep(SUBMIT_DELAY)`; `:3036-3062` — the `pane_write` trace events.
- `src/pane_input.rs:53-70` — `encode_pane_payload`, bracketed paste only when the text contains `\n`; `:82` — `SUBMIT_DELAY` = 150 ms.
- `xtask/linkage-check/src/main.rs:179` — forbidden-sleep check is inside `if is_e2e`; production sleeps are not linted. #195 reports CI does not run linkage-check at all.
- `CHANGELOG.md:358-361` — the spawn-path fix: same mechanism, 500 ms buffer, toggle-verified test.
- `CHANGELOG.md:168` — the single-line delegate change that reduced this symptom's frequency without removing its cause.
- PR #237 (Codex wrapper fix) merged 2026-07-28T18:35Z; wrapper-specific, does not cover Claude's NativeHooks path.
- `clear = false` confirmed working by @xclydes and @tomikonio on different Claude versions.
