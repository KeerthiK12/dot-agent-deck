//! The test harness must detach from any real deck before it spawns anything.
//!
//! Running the suite from inside a deck pane means this process inherits that
//! pane's `DOT_AGENT_DECK_SOCKET` / `_PANE_ID`. Anything spawned inherits them
//! too, and its hooks then post into the developer's LIVE dashboard — a card
//! appears under a fixture pane id and vanishes again. `ff5170d` scrubs these in
//! `agent_pty::spawn`, which is necessary but not sufficient: real `deck.log`
//! evidence shows four of five leaked fixture pane ids arriving from a tree that
//! already had that fix, through other spawn paths. Clearing the vars from the
//! test process covers every spawn path at once, including ones added later.
//!
//! nextest runs each test in its own process, so mutating this process's
//! environment cannot affect another test.

mod common;

/// Scenario: Set all four deck endpoint variables to values that mimic a live
/// deck, call the harness setup hook, and assert every one of them is gone —
/// so no child this process spawns can inherit a route to a real daemon.
#[test]
fn harness_clears_inherited_deck_endpoints() {
    for (var, value) in [
        (
            "DOT_AGENT_DECK_SOCKET",
            "/run/user/1000/dot-agent-deck.sock",
        ),
        (
            "DOT_AGENT_DECK_ATTACH_SOCKET",
            "/run/user/1000/dot-agent-deck-attach.sock",
        ),
        ("DOT_AGENT_DECK_PANE_ID", "8"),
        ("DOT_AGENT_DECK_AGENT_ID", "8"),
    ] {
        // SAFETY: single-threaded test body, before the harness starts anything.
        unsafe { std::env::set_var(var, value) };
    }

    common::init_test_env();

    for var in [
        "DOT_AGENT_DECK_SOCKET",
        "DOT_AGENT_DECK_ATTACH_SOCKET",
        "DOT_AGENT_DECK_PANE_ID",
        "DOT_AGENT_DECK_AGENT_ID",
    ] {
        assert!(
            std::env::var_os(var).is_none(),
            "{var} survived harness setup — a spawned child would inherit it and \
             could post hook events into a live deck"
        );
    }
}
