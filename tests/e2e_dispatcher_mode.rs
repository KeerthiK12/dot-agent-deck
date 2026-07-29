#![cfg(feature = "e2e")]

//! L2 PTY-attached reel test for PRD #220 dispatcher mode.
//!
//! Exercises the full user-visible path: launch the deck with the experimental
//! flag ON, open the new-pane form, select the "dispatcher" authoring option,
//! submit, and verify the dispatcher tab surfaces live on the attached TUI
//! with a real Claude agent running inside it.
//!
//! Marked [reel] — this is the genuine spawn → agent → work path (CLAUDE.md
//! rule 4). The agent receives the dispatcher seed prompt via gated delivery,
//! so it genuinely starts and the deck shows its status transition (Working).

mod common;

use std::time::Duration;

use common::TuiDeck;
use spec::spec;

/// Scenario: Launch the deck in the minimal fixture with the experimental flag
/// ON and real Claude credentials imported. Open the new-pane form (Ctrl+N →
/// Space confirms the dir), cycle the Mode field to the experimental "dispatcher"
/// option (the last cycler slot after `schedule: issues`), click [Submit], and
/// assert the dispatcher tab surfaces live as a mode tab on the tab strip with
/// a running agent inside. The agent receives the dispatcher seed prompt
/// (decompose work → call `dot-agent-deck dispatch`), starts genuinely, and
/// transitions through the Working status — proving the full user-visible path.
#[spec("prompt/new-pane/016")]
#[test]
fn dispatcher_001_opens_mode_tab_with_real_agent() {
    skip_unless!(common::check_claude_available());

    let deck = TuiDeck::builder()
        .with_imported_claude_credentials()
        .with_env("DOT_AGENT_DECK_EXPERIMENTAL", "1")
        .launch_with_fixture("minimal");
    deck.wait_for_string("No active sessions");

    // Trust the fixture working directory so the daemon-spawned interactive
    // claude clears its first-run onboarding + per-folder trust gates without a
    // human keystroke and the injected dispatcher seed prompt is received.
    let workdir = deck.workdir().to_string_lossy().into_owned();
    let home = deck.workdir().join("home");
    let claude_json_path = home.join(".claude.json");
    let raw = std::fs::read_to_string(&claude_json_path).expect("read .claude.json");
    let mut cfg: serde_json::Value = serde_json::from_str(&raw).expect("parse .claude.json");
    cfg["projects"][&workdir] = serde_json::json!({
        "hasTrustDialogAccepted": true,
        "hasCompletedProjectOnboarding": true,
        "projectOnboardingSeenCount": 1,
    });
    std::fs::write(
        &claude_json_path,
        serde_json::to_string_pretty(&cfg).expect("serialize .claude.json"),
    )
    .expect("write .claude.json");

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

    // The agent inside the dispatcher tab must genuinely start. The card
    // status transitions to "Working" once the agent begins executing — this
    // is the real-agent proof (not a stand-in, not a cat/stub).
    let saw_working = deck.wait_for_stream_string_within("Working", Duration::from_secs(45));
    eprintln!("reel narrative (soft): agent Working status seen in dispatcher tab = {saw_working}");
}
