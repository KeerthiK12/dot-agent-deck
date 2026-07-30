//! The PTY layer answers the DSR cursor-position query (`ESC[6n`).
//!
//! Windows ConPTY asks for it: `portable-pty` 0.9 creates the pseudoconsole
//! with `PSUEDOCONSOLE_INHERIT_CURSOR`, so conhost emits `ESC[6n` and withholds
//! the child's output until a terminal answers. Under 0.8.1 the flag was absent
//! and nothing ever asked, which is why the gap went unnoticed until the bump —
//! `orchestration_delegate::delegate_005…` and
//! `delegate_prompt_injection::delegate_injects_single_line_pointer…` both
//! started timing out on `build-windows` with a PTY snapshot containing exactly
//! `"\u{1b}[6n"` and nothing else.
//!
//! Those two are the platform regression guards but only bite on Windows. This
//! file pins the detect-and-answer behaviour on EVERY platform by making the
//! query arrive through the ordinary output stream: the `cat` stub echoes
//! whatever is written to its PTY, so writing the query in gets it read back out
//! through `pump_reader` exactly as ConPTY would deliver it.
//!
//! Fast tier (no `e2e` gate) — a `cat` stub, no LLM and no daemon socket, the
//! same shape as `orchestration_delegate.rs`.

use std::sync::Arc;
use std::time::Duration;

use dot_agent_deck::agent_pty::{AgentPtyRegistry, DOT_AGENT_DECK_PANE_ID, SpawnOptions};

mod common;

const PANE: &str = "cursor-report-pane";
/// What a terminal is asked: DSR, "report the cursor position".
const QUERY: &[u8] = b"\x1b[6n";
/// What we answer: CPR for row 1, column 1, as the raw bytes we write.
const REPORT_RAW: &[u8] = b"\x1b[1;1R";
/// The same answer as a Unix tty renders it back.
///
/// The evidence this test looks for is the terminal echoing our reply, which is
/// what proves the bytes reached the PTY master. Under `ECHOCTL` a Unix line
/// discipline echoes the ESC as the two literal characters `^[` rather than as
/// byte 0x1b, so the echo arrives in caret notation. (The `cat` stub cannot be
/// used as the witness instead: canonical mode holds our reply in the input
/// buffer until a newline arrives, and a CPR sequence must not carry one.)
const REPORT_ECHOED: &[u8] = b"^[[1;1R";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// True once the cursor-position report is visible in either encoding.
fn answered(snap: &[u8]) -> bool {
    contains(snap, REPORT_RAW) || contains(snap, REPORT_ECHOED)
}

/// Poll the agent's PTY snapshot until the report appears or `timeout` elapses,
/// returning the final snapshot either way.
async fn wait_for_report(
    registry: &AgentPtyRegistry,
    agent_id: &str,
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(snap) = registry.snapshot(agent_id)
            && answered(&snap)
        {
            return snap;
        }
        if tokio::time::Instant::now() >= deadline {
            return registry.snapshot(agent_id).unwrap_or_default();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Scenario: Spawn a `cat`-backed pane and write the DSR cursor-position query
/// (`ESC[6n`) into it. The stub echoes the query back out, so the reader thread
/// sees it on the output stream just as Windows ConPTY would emit it, and must
/// answer by writing a cursor-position report back to the PTY. Assert the
/// `ESC[1;1R` answer shows up in the pane's snapshot — proof the reply reached
/// the PTY rather than being dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pty_answers_cursor_position_query() {
    common::init_test_env();

    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd
        .path()
        .to_str()
        .expect("tempdir path is UTF-8")
        .to_string();

    let registry = Arc::new(AgentPtyRegistry::new());
    let agent_id = registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(cwd_str.as_str()),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn cat stub");

    // `write_to_pane_notice` writes the bytes with no submission semantics; the
    // PTY echoes them straight back onto the output stream the reader thread is
    // pumping, which is the delivery path we need to exercise.
    registry
        .write_to_pane_notice(PANE, std::str::from_utf8(QUERY).expect("query is UTF-8"))
        .await
        .expect("write cursor-position query");

    let snap = wait_for_report(&registry, &agent_id, Duration::from_secs(5)).await;
    assert!(
        answered(&snap),
        "PTY never answered the ESC[6n cursor-position query; snapshot = {:?}",
        String::from_utf8_lossy(&snap)
    );

    registry.shutdown_all();
}
