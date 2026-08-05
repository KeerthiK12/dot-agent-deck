//! PRD #80 M3 — L1 widget test for the tab-strip close affordance.
//!
//! Per PRD #77 Decision 2 this is an in-process test driving the
//! production tab-strip renderer through `render_tab_bar_to_buffer` (a
//! `TestBackend` wrapper, mirroring `render_button_bar_to_buffer`). No
//! subprocess, no PTY. File-layout-mirrors-catalog (Decision 7): catalog
//! ID `mouse/tabstrip/002`'s presence/absence half lands here with a
//! function name `<sub-area>_<NNN>_<short_suffix>` (Decision 17). The
//! click→close behavior half lives in `tests/e2e_mouse_tabstrip.rs`.
//!
//! M3 contract: Mode and Orchestration tabs carry a clickable `[×]` close
//! affordance; the Dashboard tab (always index 0) carries NONE. The
//! `closeable` mask passed to the renderer encodes that — `false` for the
//! Dashboard tab, `true` for Mode/Orchestration tabs.

use dot_agent_deck::state::SessionStatus;
use dot_agent_deck::ui::render_tab_bar_to_buffer;
use ratatui::style::{Color, Modifier};
use spec::spec;

/// Count the `×` close glyphs in the rendered single-row tab strip.
fn close_glyph_count(buffer: &ratatui::buffer::Buffer) -> usize {
    let area = buffer.area();
    (0..area.width)
        .filter(|&x| buffer[(x, 0)].symbol() == "×")
        .count()
}

/// The foreground color of `label`'s first character in the rendered
/// single-row tab strip. Locates the `" {label} "` padded segment
/// `render_tab_strip` writes for every tab and samples the cell right after
/// the leading pad space, so it survives label reordering / width changes as
/// long as the label text itself is unique in the row.
fn tab_label_fg(buffer: &ratatui::buffer::Buffer, label: &str) -> Color {
    let area = buffer.area();
    let row: String = (0..area.width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    let needle = format!(" {label} ");
    let start = row
        .find(&needle)
        .unwrap_or_else(|| panic!("label {label:?} not found in rendered tab strip row: {row:?}"));
    // `start` is the leading pad space; the label's own first character sits
    // one column to the right of it.
    buffer[((start + 1) as u16, 0)].fg
}

/// The `Modifier` flags of `label`'s first character in the rendered
/// single-row tab strip. Mirrors `tab_label_fg`'s cell-location logic so the
/// two stay in lockstep; used to assert `REVERSED`/`BOLD` presence/absence,
/// which a plain `fg` check cannot express.
fn tab_label_modifier(buffer: &ratatui::buffer::Buffer, label: &str) -> Modifier {
    let area = buffer.area();
    let row: String = (0..area.width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    let needle = format!(" {label} ");
    let start = row
        .find(&needle)
        .unwrap_or_else(|| panic!("label {label:?} not found in rendered tab strip row: {row:?}"));
    buffer[((start + 1) as u16, 0)].modifier
}

/// Scenario: Render the tab strip twice. First with only the Dashboard tab
/// (`closeable = [false]`) — the strip must contain NO `×` close glyph,
/// proving the Dashboard tab has no close affordance. Then with Dashboard
/// plus a Mode tab and an Orchestration tab (`closeable = [false, true,
/// true]`) — exactly two `×` glyphs must render, one per closeable tab and
/// none for the Dashboard. RED until M3 renders the `[×]` affordance (today
/// `render_tab_strip` draws no close glyph at all).
#[spec("mouse/tabstrip/002")]
#[test]
fn tabstrip_002_close_glyph_on_mode_orchestration_not_dashboard() {
    // Dashboard alone: never closeable → no close glyph anywhere.
    let dashboard_only = render_tab_bar_to_buffer(&["Dashboard"], &[false], 0, 80, &[None]);
    assert_eq!(
        close_glyph_count(&dashboard_only),
        0,
        "Dashboard tab must render no [×] close affordance, got {:?}",
        dashboard_only_text(&dashboard_only)
    );

    // Dashboard + Mode + Orchestration: only the two non-Dashboard tabs get
    // a close glyph, so exactly two `×` render. A third would mean the
    // Dashboard wrongly gained one; zero means the affordance is missing.
    let three_tabs = render_tab_bar_to_buffer(
        &["Dashboard", "demo", "squad"],
        &[false, true, true],
        0,
        80,
        &[None, None, None],
    );
    assert_eq!(
        close_glyph_count(&three_tabs),
        2,
        "Mode and Orchestration tabs must each render a [×] (and the Dashboard none), got {:?}",
        dashboard_only_text(&three_tabs)
    );
}

/// Stringify the rendered row for assertion messages.
fn dashboard_only_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area();
    (0..area.width)
        .map(|x| buffer[(x, 0)].symbol())
        .collect::<String>()
}

/// Scenario: Render the tab strip with a Dashboard tab plus an orchestration
/// tab whose panes carry a mix of `SessionStatus` values, and assert the
/// orchestration tab's label renders in the `palette::status_color()` of the
/// SINGLE highest-priority status among its panes — fixed order Error(Red) >
/// WaitingForInput(Yellow) > Working(Green) > Thinking/Compacting(Blue) >
/// Idle/Unknown(DarkGray) (PRD #333). Covers: (a) one `Error` among `Idle`
/// panes -> Red; (b) one `WaitingForInput` among `Working`/`Idle` (no Error)
/// -> Yellow; (c) all `Idle` -> DarkGray; (d) `Thinking` + `Working` (no
/// higher-priority state) -> Green, since Working outranks Thinking. Also
/// asserts a non-orchestration tab (`None`) never gets colorized by this
/// feature. RED today: `render_tab_bar_to_buffer` has no parameter through
/// which a tab's pane statuses can reach the renderer at all, so this fails
/// to compile until that seam (and the aggregate-priority resolver behind it)
/// exist.
#[spec("tabs/orchestration/009")]
#[test]
fn orchestration_009_tab_label_colored_by_highest_priority_status() {
    use SessionStatus::*;

    // (a) Error outranks everything, even surrounded by several Idle panes.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Idle, Error, Idle])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::Red,
        "a tab with an Error pane among Idle panes must render its label Red"
    );

    // (b) No Error present; WaitingForInput outranks Working and Idle.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Working, WaitingForInput, Idle])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::Yellow,
        "a tab with a WaitingForInput pane (no Error present) must render its label Yellow"
    );

    // (c) Every pane Idle -> the lowest-priority / no-activity color.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Idle, Idle, Idle])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::DarkGray,
        "an all-Idle tab must render its label DarkGray"
    );

    // (d) Thinking and Working present, nothing higher-priority -> Green,
    // since Working (priority 3) outranks Thinking (priority 4).
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Thinking, Working])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::Green,
        "a tab with Working and Thinking panes (no higher-priority state) must render its \
         label Green — Working outranks Thinking"
    );

    // Non-orchestration tabs are unaffected: a `None` entry must render with
    // the SAME base label color as any other unaffected tab in the same row,
    // never a status color, even though "demo" here also happens to carry no
    // status data.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "demo"],
        &[false, false],
        0,
        80,
        &[None, None],
    );
    assert_eq!(
        tab_label_fg(&buf, "Dashboard"),
        tab_label_fg(&buf, "demo"),
        "a non-orchestration tab (None) must render with the same base label color as any \
         other unaffected tab, not a status color"
    );
}

/// Scenario: Render the tab strip with an orchestration tab made the ACTIVE
/// tab (unlike `orchestration_009`, which always leaves Dashboard active) and
/// give it a single `Error` pane. Assert the label's `fg` is
/// `palette::status_color(Error)` (Red), `Modifier::BOLD` is set, and
/// `Modifier::REVERSED` is NOT set — main's `Modifier::REVERSED` active-tab
/// style would otherwise invert fg/bg at display time, turning a red LABEL
/// into a red BACKGROUND with terminal-default text, which is the PRD #333
/// P1 defect this pins. Also renders an active Dashboard tab and an inactive
/// orchestration tab in the same file to prove the fix is scoped: a
/// non-orchestration active tab keeps `REVERSED` exactly as today, and an
/// inactive orchestration tab keeps rendering its status color with neither
/// `REVERSED` nor `BOLD`. RED today: the active branch applies `.fg(...)` on
/// top of `active_style` (`REVERSED | BOLD`), so `REVERSED` is still set on
/// an active orchestration tab.
#[spec("tabs/orchestration/010")]
#[test]
fn orchestration_010_active_tab_status_color_is_label_text_not_reversed_background() {
    use SessionStatus::*;

    // Active orchestration tab (index 1) with an Error pane: the status
    // color must land on the label as plain `fg` with BOLD, and REVERSED
    // must be absent so a terminal doesn't invert it into a background.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        1,
        80,
        &[None, Some(&[Idle, Error, Idle])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::Red,
        "an ACTIVE orchestration tab with an Error pane must still render its label fg Red"
    );
    let modifier = tab_label_modifier(&buf, "squad");
    assert!(
        modifier.contains(Modifier::BOLD),
        "an active orchestration tab must render its label BOLD, got {modifier:?}"
    );
    assert!(
        !modifier.contains(Modifier::REVERSED),
        "an active orchestration tab must NOT render REVERSED — that would invert the Red fg \
         into a background at display time instead of coloring the label text, got {modifier:?}"
    );

    // Active non-orchestration (Dashboard) tab: unaffected by this feature,
    // must keep main's existing REVERSED active-tab styling exactly as
    // today. Regression guard for the deliberate non-orchestration scoping.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, None],
    );
    let dashboard_modifier = tab_label_modifier(&buf, "Dashboard");
    assert!(
        dashboard_modifier.contains(Modifier::REVERSED),
        "an active non-orchestration tab must keep Modifier::REVERSED unchanged, got {dashboard_modifier:?}"
    );

    // Inactive orchestration tab (Dashboard active, index 0): the existing
    // working path — status color on fg, no REVERSED, no BOLD — must not
    // regress once the active branch is fixed.
    let buf = render_tab_bar_to_buffer(
        &["Dashboard", "squad"],
        &[false, true],
        0,
        80,
        &[None, Some(&[Idle, Error, Idle])],
    );
    assert_eq!(
        tab_label_fg(&buf, "squad"),
        Color::Red,
        "an INACTIVE orchestration tab with an Error pane must still render its label fg Red"
    );
    let inactive_modifier = tab_label_modifier(&buf, "squad");
    assert!(
        !inactive_modifier.contains(Modifier::REVERSED)
            && !inactive_modifier.contains(Modifier::BOLD),
        "an inactive orchestration tab must carry neither REVERSED nor BOLD, got {inactive_modifier:?}"
    );
}
