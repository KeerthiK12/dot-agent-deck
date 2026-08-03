# PRD #341: Make Command vs PaneInput mode unmistakable

**Status**: Complete (2026-08-03) — [PR #353](https://github.com/vfarcic/dot-agent-deck/pull/353) merged as `258a4d4` (a merge commit, history preserved), issue #341 closed. M1–M7 all landed: both cursor paths gated on `input_active`, a persistent mode chip, the dim + decaying banner with its narrow-pane fallback ladder, mode-aware deck selection, command-mode pane scroll, and full test coverage (L1 snapshots, a PTY-attached L2 test, and a real-agent Haiku scenario) plus docs and changelog. `cargo test-fast` 1442/1442; the full `cargo test-e2e` gate 2803/2803, no retries. No `graduate-` follow-up — rule 9 was asked at PRD creation and answered no, so this ships visible with no `experimental` gate. Five follow-up issues filed for deferred or newly-found items: #362, #363, #364, #365, #366 (see Work Log).
**Priority**: High
**Created**: 2026-08-02

## Problem Statement

The TUI has two modes that determine where your keystrokes land. In `UiMode::PaneInput` they reach the focused pane — you are typing to an agent. In `UiMode::Normal` ("command mode") they drive the deck: select a card, jump to pane N, close a pane, quit. A previous release made the focused pane's border mode-aware (`src/palette.rs:26-46`, `src/terminal_widget.rs:165-183`) — cyan when input is live, the agent's status colour plus `BorderType::Thick` when it is not. That helped, but users still routinely act on the belief that they are in one mode while actually in the other.

Three things are working against the border.

**1. The cursor contradicts it.** Both cursor renderers key off `focused` alone and ignore `input_active`:

- `src/terminal_widget.rs:297-318` repaints the vt100 cursor cell black-on-`LightGreen`.
- `src/ui.rs:11991-12008` calls `frame.set_cursor_position`, which is what makes the terminal emulator draw its own — usually blinking — cursor. In ratatui, if nothing calls this during a frame, `Terminal::draw` hides the cursor entirely, so this call is the sole reason a real cursor is visible anywhere in the TUI.

Focus deliberately survives the mode switch: `Action::DetachToNormal` (`src/ui.rs:6655-6669`) sets `ui.mode = UiMode::Normal` without clearing the controller's focused pane, because `resume_pane_input_target` (`src/ui.rs:6630`) needs it to know where `Ctrl+D` sends you back to. The consequence is that in command mode the focused pane still shows a cursor sitting in it. A blinking cursor is the most universal "typing goes here" affordance a terminal has, and when it disagrees with a border colour it wins. The border policy's own doc comment states the intent — *"Colour answers 'are my keystrokes landing here?'"* (`src/palette.rs:36-38`) — and the cursor code simply was never updated to match.

**2. The border encodes a moving target.** In command mode the focused pane falls through to the agent's *status* colour (`src/terminal_widget.rs:167`), which changes on its own as the agent works. So reading the mode means identifying which of five colours is currently showing, rather than noticing that something changed. Change detection is cheap; colour identification is not.

**3. The only text label names the destination, not the state.** `ModeGlobals::for_mode` (`src/ui.rs:12292-12311`) renders a `[Command Mode]` button *while you are in PaneInput*, and `[Back to Pane]` while you are in command mode. Having the words "Command Mode" on screen precisely when you are not in command mode is worse than having no label at all.

There is a layout subtlety that shapes the solution. Which surface is "live" moves depending on the tab (`compute_frame_layout`, `src/ui.rs:11158-11227`):

| Tab | Left | Right |
|---|---|---|
| Dashboard | deck / cards, 33% | panes, 67% |
| Orchestration | role cards, 34% | panes, 66% |
| Mode | **agent pane, 50%** | side panes, 50% |

A pane overlay is the dominant thing on screen on a Mode tab, but on the Dashboard it sits off to the right while your keyboard is actually driving the deck on the left. No single indicator covers every tab well, which is why this PRD ships several complementary ones rather than one.

Finally, read-only-ness is inconsistent today. Agent-pane scroll is gated to PaneInput (`src/ui.rs:10382`, `:10396`), but mode-tab side panes scroll in any mode (`src/ui.rs:10308-10330`, whose comment reads "works in any UI mode by hit-testing rects"). So command mode blocks scrolling back through the one pane you most want to read, and the only way to review agent output is to be in the mode where a stray keypress goes into the agent.

## Solution Overview

Signal the mode on several redundant channels — motion, contrast, position, and text — so no single missed cue leaves the user guessing:

- **Remove the false signal.** Gate both cursors on `input_active`.
- **Add a true signal that is always in the same place.** A persistent chip in the bottom bar naming the *current* mode.
- **Make the transition impossible to miss.** Dim the focused pane's content and overlay a large `COMMAND MODE` banner that decays to the chip once you have demonstrably oriented yourself.
- **Cover the Dashboard tab.** Make the selected-card highlight mode-aware, so the signal is where your eyes already are on the tab where the pane overlay is weakest.
- **Make command mode genuinely useful as a read-only mode.** Enable agent-pane scroll there, matching side panes.

Together these mean that in command mode the pane visibly stops looking interactive (no cursor, dimmed) while the deck visibly starts looking live, and a text label states which mode you are in without you having to infer it.

## Scope

### In Scope

**M1 — Cursors respect mode.** Thread the existing `input_active` flag into both cursor paths. `src/terminal_widget.rs:300` gains an `&& self.input_active` (or renders a dim hollow outline instead of the solid green block, so the pane's cursor position stays visible without reading as interactive). `src/ui.rs:11991` skips `set_cursor_position` entirely unless `input_active`, which requires threading the flag down to that point — it currently only reaches the two `TerminalWidget` construction sites (`:11925`, `:11957`). Optionally set `SetCursorStyle::BlinkingBar` on entering PaneInput as a second channel.

**M2 — Persistent mode chip.** A left-anchored chip in the bottom bar naming the current mode, e.g. ` COMMAND ` / ` TYPING `, rendered `Modifier::REVERSED | BOLD` — the same terminal-relative trick the active tab uses (`render_tab_strip`, `src/ui.rs:10970`), so it needs no absolute colour. Built in `dashboard_hints_string` / `render_button_bar` (`src/ui.rs:13184`, `:12460`) and reflected in `bottom_bar_rows` (`:12518`) if it changes the width budget. The existing destination-naming button stays beside it; the chip states where you *are*, the button states where the chord *goes*.

**M3 — Dim + decaying banner.** While in command mode, apply `Modifier::DIM` across the focused pane's inner area and draw a centred block-letter `COMMAND MODE · Ctrl+D to type` banner over it. Requires a small block font — no text renderer exists in the repo today (`src/ascii_art.rs` is LLM-generated idle art, not a font), but a hand-rolled 5-row `const` table covering the ~10 distinct letters needed is small. The dimming persists for the whole time you are in command mode; only the banner's size is transient (see Technical Approach for the decay rule).

**M4 — Mode-aware deck selection.** The selected card renders Magenta + BOLD + a `▸ ` marker (`src/ui.rs:14510-14516`, `src/palette.rs:78-82`) identically in both modes, because selection persists across the switch — so on the Dashboard the deck looks equally live whether or not the keyboard is driving it. Dim or otherwise de-emphasise the selection accent in PaneInput so the deck is visibly inert exactly when the pane is visibly live.

**M5 — Agent-pane scroll in command mode.** Drop the `if ui.mode == UiMode::PaneInput` guards at `src/ui.rs:10382` and `:10396` so the wheel scrolls the focused agent pane in command mode, matching side-pane behaviour. Keep forwarding to the child app (`forward_mouse_scroll`) only in PaneInput — in command mode the wheel should always drive our scrollback, never the agent's mouse protocol. Consider binding PgUp/PgDn for the keyboard equivalent.

**M6 — Test coverage.** L1 `TestBackend` + `insta` snapshots for the cursor gating, the chip in both modes, the banner at big and collapsed sizes, and the narrow-pane fallback ladder. Per CLAUDE.md rule 4 this is a major user-facing feature, so it also needs at least one PTY-attached L2 test (`e2e_*.rs`, `#[cfg(feature = "e2e")]`) driving the real binary and asserting via vt100 that the banner appears on `Ctrl+D` and collapses on the next binding — plus a real-agent scenario on a cheap model (Haiku) validating the flow as a user actually experiences it, following the `scheduler/dispatch/013` pattern for interactive headless agents. Every `#[spec]` test carries a `/// Scenario:` comment (rule 7) and a `tests/CATALOG.md` entry.

**M7 — Docs and changelog.** Update `docs/keyboard-shortcuts.md` and any mode-related page for the new scroll behaviour and the chip vocabulary; changelog fragment via `dot-ai-changelog-fragment`.

### Out of Scope

- **Renaming the `UiMode` variants.** `Normal` vs `PaneInput` are internal names; the user-facing vocabulary settled in M2 need not match them, and renaming the enum is churn this PRD does not need.
- **A full modal redesign.** No new modes, no vim-style operator grammar, no changes to which keys are bound in which mode.
- **Theming or configurability of the indicators.** Ship one good default. If users want to disable the banner, that is a follow-up.
- **Changing mouse-mode forwarding semantics** beyond the single command-mode scroll case in M5.
- **Blanking pane content outright.** Considered and rejected — see Technical Approach.

## Technical Approach

### Why dim rather than blank

The original proposal was to hide the pane's content entirely behind a full-pane banner, on the reasoning that content you cannot interact with has no value. That conflates *interacting* with *reading*. Command mode is where you decide what to do next, and those decisions are driven by what the panes show: whether the agent you are about to close is mid-work, which pane needs attention, whether a run finished. It is also the **safe resting state** — the mode where stray keystrokes cannot reach an agent. Blinding it would punish the safe behaviour and push users into PaneInput just to watch, which is the mode where a mistyped key goes into the agent. Dimming keeps the content legible while making inertness unmistakable, and a whole-region contrast drop is a far louder peripheral signal than a border colour anyway.

### Banner decay rule

The banner announces the *transition*; once you have shown you know where you are, holding it up is just occlusion. State lives on `UiState`, following the existing `Option<(String, Instant)>` pattern used by `status_message` (`src/ui.rs:1509`, expiry at `:9427` against `STATUS_MESSAGE_TTL`):

- On entering command mode: record the entry `Instant` and clear a `banner_collapsed` latch.
- Render **big** while not collapsed and the entry instant is younger than a TTL (start at ~1s and tune).
- **Collapse** on: the TTL elapsing, *or* a key that resolves to a command-mode `Action`, *or* a bottom-bar button click. All three prove orientation.
- **Do not collapse** on a key that resolves to no action. An unbound printable keystroke is the actual failure this PRD exists to fix — the user thinks they are talking to the agent — so removing the signal at that moment is exactly backwards. Unbound printable keys should hold the banner up, or re-assert it if already collapsed.
- Re-arm on each fresh entry into command mode. Clear on leaving.
- The dimming is unaffected by all of the above and persists for the whole time in command mode.

Some bindings sidestep the question anyway: `Ctrl+n` opens the new-pane form, a full modal that covers everything; `1`-`9` moves focus, so the banner follows to the newly focused pane already collapsed.

The main loop polls with a 16ms timeout (`src/ui.rs:9836`), so it redraws continuously regardless of input. The timed decay needs no new tick machinery.

### Narrow-pane fallback

Five-row block letters need roughly 60 columns and 7 rows of inner area. Panes are frequently smaller — a Dashboard pane column at 67% of an 80-column terminal, split across two tiled panes, is well under that. Define an explicit degradation ladder rather than letting the banner clip or overrun the border, encoded as one pure function with unit tests over a width/height sweep so the choice is testable independently of rendering:

1. Block-letter `COMMAND MODE` + the `Ctrl+D to type` subtitle.
2. Block-letter `COMMAND` alone.
3. A single reversed line: ` COMMAND MODE — Ctrl+D to type `.
4. A single reversed word: ` COMMAND `.
5. Omitted — dimming plus the M2 chip carry the signal.

### Colour constraints

Per PRD #13 and the `src/palette.rs` header, no absolute `Color::Rgb` — named ANSI only, so terminal themes can remap. The banner and chip should lean on `Modifier::REVERSED` / `BOLD` / `DIM`, which are terminal-relative and safe on light and dark backgrounds. If a new semantic role is genuinely needed, add it to `palette.rs` rather than inlining a literal.

### Cross-version safety

None required. This is pure TUI rendering and local input handling — no daemon, no TUI↔daemon protocol, no hooks, no orchestration routing. CLAUDE.md rule 12's contract question does not arise and `PROTOCOL_VERSION` is untouched. Patch-level bump.

### Experimental flag

No. CLAUDE.md rule 9 requires the question be asked for a new user-visible surface; the answer here is no, decided with the user at PRD creation. The core of this work is correcting a signal that is currently *wrong* (M1) and clarifying signals that are currently ambiguous — putting that behind an opt-in flag would hide the fix from exactly the users hitting the confusion. L1 snapshots plus the L2 PTY test give enough safety without a flag.

## Success Criteria

- In command mode, no cursor of any kind renders in the focused pane — neither the painted block nor the terminal's own.
- In PaneInput, the cursor renders exactly as it does today.
- The current mode is stated in words, in the same screen position, on every tab, whenever the bottom bar is showing its buttons. The two inline input modes — `Filter` and `Rename` — are an explicit exception: their bar row *is* the input field, so the `/ ` or `Rename: ` prompt and its cursor own that position instead. Neither `COMMAND` nor `TYPING` would be accurate there, and the visible prompt plus the user's own echoed text already makes the destination of keystrokes unambiguous. Amended 2026-08-03 after code review (finding 4) established that the original "at all times" wording and the shipped implementation could not both be read literally; the exception is deliberate, not a gap.
- Entering command mode produces a change large enough to notice without looking for it, on every tab type — verified on Dashboard, Orchestration, and Mode tabs.
- Pane content remains readable in command mode; you can still tell what each agent is doing and which one you are about to close.
- The banner collapses immediately when a command-mode binding is executed, and does *not* collapse on unbound printable keys.
- The mouse wheel scrolls the focused agent pane in command mode, and never reaches the agent's mouse protocol there.
- No absolute RGB colours are introduced; the indicators are legible on both light and dark terminal backgrounds.

## Milestones

- [x] **M1 — Cursors respect mode.** Both cursor paths gated on `input_active`; the contradiction with the border policy is gone.
- [x] **M2 — Persistent mode chip.** Current mode named in a fixed position in the bottom bar, on every tab.
- [x] **M3 — Dim + decaying banner.** Focused pane dims in command mode; banner renders, decays per the rule above, and degrades correctly at narrow sizes.
- [x] **M4 — Mode-aware deck selection.** Selection accent de-emphasised in PaneInput so the Dashboard tab gets a signal where the user's attention is.
- [x] **M5 — Agent-pane scroll in command mode.** Command mode is a real read-only inspect mode, consistent with side panes.
- [x] **M6 — Test coverage.** L1 snapshots for every new surface; at least one PTY-attached L2 test plus a real-agent (Haiku) scenario per rule 4; `/// Scenario:` comments and `tests/CATALOG.md` entries.
- [x] **M7 — Docs and changelog.** `docs/keyboard-shortcuts.md` updated for the scroll change and mode vocabulary; changelog fragment added.

## Risks

- **`Modifier::DIM` is not universally honoured.** Some terminals ignore it, which would silently remove the steady-state signal and leave only the chip. Verify across the terminals we care about early in M3; if unreliable, the banner and chip must be sufficient on their own, and an alternative de-emphasis (dropping bold, collapsing to a single foreground) should be evaluated.
- **Over-signalling.** Five indicators can add up to visual noise, particularly the banner over a pane the user is trying to read. The decay rule is the main mitigation; be willing to shorten the TTL or shrink the banner after living with it. Ship M1 and M2 first and judge how much of M3 is still needed.
- **M1 alone may resolve most of the problem.** Removing a contradictory signal is often worth more than adding correct ones. That is an argument for sequencing, not for cutting scope — but if M1+M2 prove sufficient in practice, M3's banner is worth re-evaluating before investing in the block font.
- **Snapshot churn.** M2 changes the bottom bar on every tab, so most existing full-frame snapshots will move. Review the regenerated diffs by eye rather than accepting wholesale.
- **Scroll-forwarding regression (M5).** Removing the mode guard must not let the wheel reach an agent's mouse protocol in command mode. `mouse_mode_enabled` currently branches inside the PaneInput-gated arms; restructure carefully and cover both branches with tests.

## Open Questions

1. Chip vocabulary: `COMMAND` / `TYPING`? `COMMAND` / `INSERT` (vim-familiar)? `DECK` / `AGENT` (domain-specific)? The internal names `Normal` / `PaneInput` are poor user-facing labels.
2. Banner TTL — 1s is the starting guess and wants real use to settle.
3. Should a *burst* of unbound printable keys in command mode trigger a louder callout ("You're in COMMAND mode — Ctrl+D to type into the pane") beyond merely holding the banner up? Captured here rather than scoped in; cheap to add later once the base signal exists.
4. Should the banner appear over *every* pane in command mode, or only the focused one? Only-focused is the assumption; every-pane is louder but noisier and makes multi-pane frames unreadable.
5. Does M4's de-emphasis risk making the Dashboard feel dead in PaneInput, where the user may still want to track which card is selected?

## Work Log

### 2026-08-03 (later) — Merged and archived

PR #353 merged into `main` as `258a4d4` (a merge commit, history preserved); issue #341 closed automatically via the PR body's closing keyword. The merge itself was run by the user directly (`gh pr merge 353 --merge`) after Claude Code's auto-mode permission classifier denied two automated attempts in the release session (`gh pr merge 353 --merge` and the equivalent raw `pulls/353/merge` API call) — the same classifier that denied `gh issue close` during PRD #249's close-out (`prds/done/249-delegate-respawn-readiness.md`). Left for a human rather than worked around, consistent with that precedent.

Five follow-up issues were filed. Two resolve items this PRD's Work Log already flagged as deliberately out of scope: **#362** (the wheel's missing pointer hit-test — forwards clamped, fictional coordinates to the child) and **#363** (`wire_stream_pane`'s unvalidated vt100 dimensions on the shipped spawn path — contained today by `guarded_parser_feed`, not eliminated). Three more were found during the merge-gate review rather than in M1–M7's own scope, and are process/tooling gaps rather than product defects: **#364** (`delegate_011`'s pre-existing wall-clock flake against loaded CI runners — unrelated to this PRD's diff, which touched only a markdown URL when it last failed), **#365** (demo reel clips hold a frozen final frame ~2s too long), and **#366** (CLAUDE.md rule 8 gap — Greptile posts no review object on re-review, so `--json reviews` alone is a dead end for confirming the gate settled).

### 2026-08-03 — All milestones implemented; review and e2e gate green

M1–M7 landed across nine feature/fix commits and six test commits. `cargo test-fast` 1442 passing; the full `DOT_AGENT_DECK_RECORD=1 cargo test-e2e` tier 2803/2803 passing with no retries.

Open questions 1–5 were resolved with the user before implementation: vocabulary `COMMAND` / `TYPING`; banner TTL **2.5s** rather than the PRD's 1s guess (1s was judged too short to reliably register); no burst-of-unbound-keys escalation (out of scope, the hold/re-assert behaviour is in); banner and dimming on the focused pane only; and M4 de-emphasises the accent while keeping the `▸ ` marker so the card stays identifiable.

Three findings came out of implementation that the PRD had not anticipated. **The real-agent e2e test earned rule 4's cost**: it found that scrolling back in command mode and returning with `Ctrl+D` left the pane scrolled away from live output, and because cursor placement requires `scrollback() == 0`, no cursor rendered at all — typing mode, cursorless, stale output. Neither M1 nor M5 was wrong alone; the gap existed only because M5 made command-mode scrolling possible. Fixed by a per-frame reconcile keyed on `(mode, focused pane)` rather than on `Ctrl+D`, since ~50 sites assign `ui.mode`. **Code review found the banner's mode edge was observed only at render time**, so a same-drain round-trip (a `Ctrl+D` key repeat, or a double-click on `[Back to Pane]`) never armed the fresh entry; the edge is now observed inside the input drain and the edge memory was split off `entered_at`. **Review also found `PaneInput` with no focused pane left the UI lying** — chip reading ` TYPING `, a cursor possibly painted, keystrokes silently dropped — now resolved by dropping to command mode, the honest state.

Two accepted trade-offs worth carrying forward: at 80 columns the button bar now takes three rows rather than two (arithmetically forced — ten default buttons need 154 cells across two rows and 80 columns with a chip offers 150; a first-row-only indent did reclaim the row at 40–62 columns), and the chip is omitted in the `Filter` / `Rename` inline input modes, which is why the mode-label success criterion was amended above.

Two follow-ups deliberately left out of scope: the wheel's missing pointer hit-test (pre-existing — the pre-M5 arm was never gated on pointer position, and the deck has no wheel scroll region to shadow — but `pane_relative_coords` saturating-subtracts, so a pointer outside the pane forwards clamped, fictional coordinates to the child), and `wire_stream_pane`'s unvalidated parser dimensions on the shipped spawn path (contained in practice by `guarded_parser_feed`).

Security audit: clean apart from one Low finding (release-exposed seams passed caller-controlled dimensions straight to `vt100::Parser::new`), fixed by routing through the existing `parser_init_dims` and `guarded_parser_feed`. Rule 12 verified — no daemon, protocol, hook or orchestration impact; `PROTOCOL_VERSION` unchanged at 6; patch-level, no `.breaking.md` fragment.

Left to manual validation: whether `Modifier::DIM` renders visibly dimmer on the terminals we care about (headless evidence proves the app emits SGR 2 and tmux preserves it, but not that a given emulator draws it differently), whether the 2.5s TTL feels right in use, and the cross-tab behaviour on Orchestration and Mode tabs — the L2 tests cover the Dashboard layout only.

### 2026-08-02 — Created

Grew out of a review of why the mode-aware border shipped in a recent release did not fully solve the problem. Tracing the render path turned up the concrete contradiction in M1 — both cursors ignore `input_active`, so the loudest "type here" affordance on screen fires in the mode where typing does nothing — which reframed the work from "add a better indicator" to "remove the false one first, then add true ones". The layout survey (`compute_frame_layout`) established that the live surface moves between deck and pane depending on tab, which is why the solution is several complementary indicators rather than one. Full-pane blanking was proposed and rejected in favour of dimming; the banner decay rule, including the deliberate asymmetry between bound and unbound keys, was settled in the same discussion. Experimental flag: asked per rule 9, answered no.
