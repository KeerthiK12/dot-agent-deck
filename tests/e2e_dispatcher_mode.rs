#![cfg(feature = "e2e")]

//! L2 PTY-attached reel test for PRD #220 dispatcher mode.
//!
//! Exercises the full user-visible path: launch the deck with the experimental
//! flag ON, open the new-pane form, select the "dispatcher" authoring option,
//! submit, give the seeded agent a goal, and verify it really dispatches — the
//! daemon creates the sibling git worktree the feature promises.
//!
//! Marked [reel] — this is the genuine spawn → agent → work path (CLAUDE.md
//! rule 4). The agent receives the dispatcher seed prompt via gated delivery,
//! decomposes the goal, and invokes `dot-agent-deck dispatch` itself; the
//! assertion is on the resulting worktree, so nothing here is a stand-in.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Removes a dispatch worktree on drop, including on panic.
///
/// Dispatch worktrees are SIBLINGS of the fixture dir, so they land outside the
/// harness tempdir and its `TempDir` drop never touches them — without this every
/// run of this test leaves a `/tmp/.tmpXXXX-dispatch-probe-unit` behind forever.
struct SiblingWorktreeGuard(PathBuf);

impl Drop for SiblingWorktreeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// PATH for the spawned deck (→ daemon → agents) with the freshly-built
/// `dot-agent-deck` binary's dir prepended to the host PATH.
///
/// Without this the seeded dispatcher agent runs whatever `dot-agent-deck`
/// happens to be installed on the host, which predates the `dispatch` verb — the
/// agent then reports "dispatch doesn't exist as a subcommand" and the test
/// silently proves nothing about the feature. The rest of the host PATH is kept
/// so `git` and `claude` still resolve. Mirrors
/// `e2e_issue_dispatch_real.rs::path_with_binary_dir`.
fn path_with_binary_dir() -> String {
    let bin = env!("CARGO_BIN_EXE_dot-agent-deck");
    let bindir = Path::new(bin).parent().expect("binary path has a parent");
    format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Give the fixture repo an initial commit.
///
/// The harness `git init`s the copied fixture but never commits, leaving an
/// unborn HEAD — and `git worktree add` cannot create a worktree from that. A
/// dispatch in such a repo fails on worktree creation, so without this the
/// dispatch path is unreachable no matter what the agent does.
fn commit_fixture_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git available");
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    };
    run(&["config", "user.email", "deck-test@example.com"]);
    run(&["config", "user.name", "Deck Test"]);
    run(&["add", "-A"]);
    run(&["commit", "-qm", "fixture baseline"]);
}

/// Scenario: Launch the deck in the minimal fixture with the experimental flag
/// ON and real Claude credentials imported. Open the new-pane form (Ctrl+N →
/// Space confirms the dir), cycle the Mode field to the experimental "dispatcher"
/// option (the last cycler slot after `schedule: issues`), and click [Submit] —
/// the dispatcher tab must surface live on the tab strip. Then type a goal asking
/// for one unit named `probe-unit`; the seeded agent runs
/// `dot-agent-deck dispatch probe-unit` itself and the daemon creates the sibling
/// worktree `../<repo>-dispatch-probe-unit`, which the test waits for on disk.
#[spec("prompt/new-pane/016")]
#[test]
fn dispatcher_001_opens_mode_tab_with_real_agent() {
    skip_unless!(common::check_claude_available());

    let deck = TuiDeck::builder()
        .with_imported_claude_credentials()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        // The branch build must win over any host-installed `dot-agent-deck`, or
        // the agent cannot see the `dispatch` verb at all.
        .with_env("PATH", path_with_binary_dir())
        .launch_with_fixture("minimal");
    deck.wait_for_string("No active sessions");

    // `git worktree add` needs a real commit to branch from.
    commit_fixture_repo(deck.workdir());

    // Trust the fixture working directory so the daemon-spawned interactive
    // claude clears its first-run onboarding + per-folder trust gates without a
    // human keystroke and the injected dispatcher seed prompt is received.
    //
    // Seeded via the harness helper rather than hand-editing `.claude.json`:
    // `with_imported_claude_credentials` imports CREDENTIALS only, so there is no
    // `~/.claude.json` in the per-test HOME to read — the helper is what creates
    // it (starting from the host's, to preserve `hasCompletedOnboarding`) and then
    // marks each path trusted. Trust both the raw and canonicalized forms, since
    // the agent's own cwd may arrive either way (on macOS the tempdir is a
    // `/var` → `/private/var` symlink).
    let mut trust_paths = vec![deck.workdir().to_string_lossy().into_owned()];
    if let Ok(canonical) = deck.workdir().canonicalize() {
        let canonical = canonical.to_string_lossy().into_owned();
        if !trust_paths.contains(&canonical) {
            trust_paths.push(canonical);
        }
    }
    common::seed_claude_trust_in_home(deck.home_dir(), &trust_paths)
        .expect("seed Claude onboarding and project trust");

    // Open the new-pane form: Ctrl+N → directory picker → Space confirms.
    deck.send_keys(b"\x0e"); // Ctrl+n → directory picker
    deck.send_keys(b" "); // Space → confirm current dir → new-pane form
    deck.wait_for_string("No mode");

    // Cycle to the dispatcher option — it is the LAST slot (after No mode,
    // schedule, schedule: issues). Saturate with enough Rights to reach the
    // end (the cycler caps), so this is robust against future additions.
    deck.send_keys(b"\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C"); // Right ×8
    deck.wait_for_string("dispatcher mode");

    // Submit via the [Submit] button — deterministic, no fragile Enter count.
    let (scol, srow) = deck
        .find_in_grid("[Submit]")
        .expect("the new-pane form should render a [Submit] button");
    deck.click(scol, srow);

    // Submitting closes the form and spawns the dispatcher mode tab.
    deck.wait_for_absence("[Submit]");

    // The dispatcher tab must surface live on the tab strip. The tab strip
    // appears only when 2+ tabs exist (Dashboard + dispatcher mode tab), so
    // seeing "dispatcher" on the grid means a real mode tab was created live
    // and the strip is painting.
    let saw_tab = deck.wait_for_stream_string_within("dispatcher", Duration::from_secs(60));
    assert!(
        saw_tab,
        "the dispatcher tab never surfaced a LIVE tab labelled \"dispatcher\" \
         within 60s — expected the dispatcher mode tab to appear in the attached \
         TUI's tab strip without a reconnect.\n\
         Final grid:\n{}",
        deck.snapshot_grid()
    );

    // Give the seeded agent an actual goal. Without one it correctly stalls
    // asking for a task, which is why an earlier version of this test observed
    // no work at all. The instruction is deliberately directive and names the
    // unit, so the assertion below survives LLM phrasing and tool variance.
    deck.wait_for_string("dispatcher");
    deck.send_keys(
        b"Dispatch exactly one unit named probe-unit, with the task \"list the files here\". \
          Call the dispatch command now. Do not ask me anything first.\r",
    );

    // The real-agent proof, end to end: the agent decomposed the goal, invoked
    // `dot-agent-deck dispatch`, the daemon created the git worktree, and it
    // landed at the SIBLING path the feature promises (`../<repo>-dispatch-<slug>`,
    // never nested inside the checkout). Asserting on the worktree rather than on
    // a status word avoids depending on claude's randomized spinner gerunds
    // ("Undulating…", "Thinking…"), which is what made the previous check vacuous.
    let expected_worktree = deck
        .workdir()
        .parent()
        .expect("fixture tempdir has a parent")
        .join(format!(
            "{}-dispatch-probe-unit",
            deck.workdir()
                .file_name()
                .expect("fixture dir has a name")
                .to_string_lossy()
        ));
    // Armed BEFORE the wait, so the worktree is reclaimed even if the assertion
    // below fails or the agent creates it late.
    let _worktree_guard = SiblingWorktreeGuard(expected_worktree.clone());

    // A real agent sometimes answers instead of acting — it acknowledges the
    // instruction, or asks a clarifying question, and then sits idle. Re-nudge on
    // a fixed cadence rather than waiting out one long silence, so a single
    // conversational detour doesn't fail the run. Bounded: NUDGES × NUDGE_EVERY
    // is the whole budget, and it stays inside this test's nextest kill window
    // (see `.config/nextest.toml`) so the assertion below — and its grid dump —
    // actually runs instead of the process being killed mid-wait.
    const NUDGE_EVERY: Duration = Duration::from_secs(70);
    const NUDGES: u32 = 3;
    let mut dispatched = false;
    'outer: for round in 0..NUDGES {
        if round > 0 {
            deck.send_keys(
                b"You have not called the dispatch command yet. \
                  Run `dot-agent-deck dispatch probe-unit --task \"list the files here\"` now, \
                  with no further questions.\r",
            );
        }
        let until = std::time::Instant::now() + NUDGE_EVERY;
        while std::time::Instant::now() < until {
            if expected_worktree.is_dir() {
                dispatched = true;
                break 'outer;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    assert!(
        dispatched,
        "the dispatcher agent never produced a dispatch worktree at {} after {} nudges \
         over {}s — expected it to call `dot-agent-deck dispatch probe-unit` and the \
         daemon to create the sibling worktree.\n\
         Final grid:\n{}",
        expected_worktree.display(),
        NUDGES,
        NUDGE_EVERY.as_secs() * NUDGES as u64,
        deck.snapshot_grid()
    );
}
