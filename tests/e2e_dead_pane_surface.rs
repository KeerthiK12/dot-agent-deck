#![cfg(feature = "e2e")]

//! PTY-attached coverage for a pane whose agent goes away underneath it.
//!
//! When the attach I/O task exhausts its reconnect budget the pane can never
//! accept input again, but it keeps rendering its last frame — so without a
//! marker it is indistinguishable from an agent that is merely quiet, and every
//! keystroke is silently dropped. A maintainer hit exactly this on v0.35.0 and
//! read it as the deck freezing; the only feedback was a transient
//! `PTY write failed: Pane <id> stream I/O task ended`, naming an internal task.
//!
//! Driven through the real binary on a PTY because the whole defect is about
//! what a user SEES: the rendered title and the message a keystroke produces.
//! Neither is observable from the state layer, and the unit tests covering
//! `PaneLostReason`'s strings cannot reach this path at all.

mod common;

use std::time::Duration;

use common::TuiDeck;

/// Room for the give-up to happen and repaint: the reattach lookup runs for
/// `REATTACH_LOOKUP_TOTAL_BUDGET` (2 × `RESPAWN_SLOT_HANDOVER_WORST_CASE`, so
/// 10s) before the task concludes no live agent will claim the pane. The default
/// `WAIT_TIMEOUT` is exactly 10s, which would race the thing under test.
const GIVE_UP_BUDGET: Duration = Duration::from_secs(25);

/// Scenario: Launch the deck with one pane backed by `cat`, then stop that agent
/// from the daemon side — the agent vanishes underneath a pane the TUI still
/// holds, with no close initiated in the TUI. The attach task retries, finds no
/// live agent for the pane, and gives up. Assert the pane's title then reports
/// `disconnected` rather than continuing to look healthy, and that typing into it
/// explains the agent is gone instead of naming an internal I/O task.
#[test]
fn dead_pane_reports_itself_as_disconnected_and_says_why() {
    let deck = TuiDeck::builder()
        .with_continue_session("orphan-target", "cat")
        .launch_with_fixture("minimal");

    // The pane view is up and healthy: its title carries the session name and
    // NOT the marker. Asserting absence first makes the later wait a genuine
    // absent→present transition rather than a vacuous match.
    deck.wait_for_string("orphan-target");
    let healthy = deck.snapshot_grid();
    assert!(
        !healthy.contains("disconnected"),
        "a live pane must not be labelled disconnected.\nGrid:\n{healthy}"
    );

    // Take the agent away from underneath the pane. `StopAgent` over the attach
    // socket is the daemon-side death the TUI did not ask for — the shape of a
    // crash, an external kill, or a daemon that restarted — as opposed to a
    // Ctrl+W close, which tears the pane down deliberately and must NOT be
    // reported as a failure.
    let agent_id = common::agent_records_on(deck.attach_socket_path())
        .into_iter()
        .find(|r| r.display_name.as_deref() == Some("orphan-target"))
        .map(|r| r.id)
        .expect("the launched pane must be registered daemon-side");
    common::attach_request_on(
        deck.attach_socket_path(),
        &dot_agent_deck::daemon_protocol::AttachRequest::StopAgent {
            id: agent_id.clone(),
        },
    )
    .unwrap_or_else(|e| panic!("StopAgent for {agent_id} failed: {e}"));

    // The give-up must become visible on its own, with no keystroke to provoke
    // it — that is the whole point. Before this fix the pane sat there looking
    // live until the user typed and got an internal error.
    assert!(
        deck.wait_for_grid_string_within("disconnected", GIVE_UP_BUDGET),
        "after its agent went away the pane must label itself disconnected \
         without being prodded.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // The session name survives alongside the marker: the frozen output is kept
    // for inspection, so the pane must remain identifiable rather than being
    // replaced by a bare error.
    let disconnected = deck.snapshot_grid();
    assert!(
        disconnected.contains("orphan-target"),
        "a disconnected pane must stay identifiable — its output is preserved \
         precisely so it can still be read.\nGrid:\n{disconnected}"
    );

    // Typing must explain the state in the user's terms. `AgentGone` is the
    // expected reason: no live agent ever claimed the pane within the retry
    // window, which is what stopping it daemon-side produces.
    deck.send_keys(b"x");
    assert!(
        deck.wait_for_grid_string_within("no longer running", GIVE_UP_BUDGET),
        "typing into a disconnected pane must say the agent is gone.\nGrid:\n{}",
        deck.snapshot_grid()
    );

    // The internal phrasing must not come back. This is the exact string the
    // maintainer saw and could not act on.
    let after_typing = deck.snapshot_grid();
    assert!(
        !after_typing.contains("stream I/O task ended"),
        "the internal I/O-task message must not reach the user.\nGrid:\n{after_typing}"
    );
}
