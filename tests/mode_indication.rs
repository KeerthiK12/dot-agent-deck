//! L1 coverage for the persistent current-mode indicators.
//!
//! These tests exercise the production `TerminalWidget` and bottom-bar render
//! paths in process. The frame-level hardware-cursor test uses a dedicated
//! TestBackend seam because `Frame::set_cursor_position` has no buffer cell to
//! inspect.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dot_agent_deck::event::AgentType;
use dot_agent_deck::keybindings::KeybindingConfig;
use dot_agent_deck::state::{SessionState, SessionStatus};
use dot_agent_deck::terminal_widget::TerminalWidget;
use dot_agent_deck::ui::{
    CardDensityKind, UiMode, observe_focused_agent_key_scroll, observe_focused_agent_mouse_scroll,
    render_button_bar_for_mode_to_buffer, render_button_bar_to_buffer,
    render_button_bar_with_bindings_to_buffer, render_card_for_mode_to_buffer,
    render_card_to_buffer, render_focused_pane_cursor_for_mode_to_position,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::widgets::Widget;
use spec::spec;

const COMMAND_CHIP: &str = " COMMAND ";
const TYPING_CHIP: &str = " TYPING ";

fn buffer_to_text(buffer: &Buffer) -> String {
    let area = buffer.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn buffer_to_styled_text(buffer: &Buffer) -> String {
    fn flush(line: &mut String, style: ratatui::style::Style, run: &str) {
        if run.trim().is_empty() && style == ratatui::style::Style::default() {
            return;
        }
        let _ = write!(line, "[{style:?}]{run:?} ");
    }

    let area = buffer.area();
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        let mut run = String::new();
        let mut run_style = buffer[(0, y)].style();
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            if cell.style() != run_style {
                flush(&mut line, run_style, &run);
                run.clear();
                run_style = cell.style();
            }
            run.push_str(cell.symbol());
        }
        flush(&mut line, run_style, &run);
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn selected_card_fixture() -> SessionState {
    let now = chrono::Utc::now();
    SessionState {
        session_id: "mode-card".to_string(),
        agent_type: AgentType::ClaudeCode,
        cwd: Some("/work/mode-card".to_string()),
        status: SessionStatus::Working,
        active_tool: None,
        started_at: now,
        last_activity: now,
        recent_events: VecDeque::new(),
        tool_count: 0,
        last_user_prompt: None,
        first_prompts: Vec::new(),
        pane_id: Some("pane-mode-card".to_string()),
        agent_id: None,
        display_name: None,
    }
}

fn assert_no_rgb(buffer: &Buffer, context: &str) {
    let area = buffer.area();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            assert!(
                !matches!(cell.fg, Color::Rgb(..)) && !matches!(cell.bg, Color::Rgb(..)),
                "{context} cell ({x}, {y}) must not carry an absolute RGB colour, got {:?}",
                cell.style()
            );
        }
    }
}

fn assert_mode_chip_at_origin(buffer: &Buffer, expected: &str, context: &str) {
    let actual = (0..expected.chars().count() as u16)
        .map(|x| buffer[(x, 0)].symbol())
        .collect::<String>();
    assert_eq!(
        actual, expected,
        "{context} must render the current-mode chip at the left edge"
    );

    let required = Modifier::REVERSED | Modifier::BOLD;
    for x in 0..expected.chars().count() as u16 {
        let cell = &buffer[(x, 0)];
        assert!(
            cell.modifier.contains(required),
            "{context} chip cell {x} must be REVERSED|BOLD, got {:?}",
            cell.style()
        );
        assert!(
            !matches!(cell.fg, Color::Rgb(..)) && !matches!(cell.bg, Color::Rgb(..)),
            "{context} chip cell {x} must not carry an absolute RGB colour, got {:?}",
            cell.style()
        );
    }
}

fn render_cursor_widget(input_active: bool) -> Buffer {
    const SCREEN_ROWS: u16 = 3;
    const SCREEN_COLS: u16 = 8;
    const CURSOR_ROW: u16 = 1;
    const CURSOR_COL: u16 = 3;

    let mut parser = vt100::Parser::new(SCREEN_ROWS, SCREEN_COLS, 0);
    parser.process(b"\x1b[2;4H");
    assert_eq!(parser.screen().cursor_position(), (CURSOR_ROW, CURSOR_COL));

    let parser = Arc::new(Mutex::new(parser));
    let widget =
        TerminalWidget::new(parser, "pane".to_string(), true).with_input_active(input_active);
    let area = Rect::new(0, 0, SCREEN_COLS + 2, SCREEN_ROWS + 2);
    let mut buffer = Buffer::empty(area);
    widget.render(area, &mut buffer);
    buffer
}

/// Scenario: Render the same focused terminal screen twice with its vt100 cursor parked at a known cell. PaneInput must retain today's black-on-LightGreen bold block, while command mode must not paint that solid LightGreen cursor highlight.
#[spec("mode/cursor/001")]
#[test]
fn mode_cursor_001_painted_cursor_respects_input_active() {
    let live = render_cursor_widget(true);
    let live_cursor = &live[(4, 2)];
    assert_eq!(live_cursor.fg, Color::Black);
    assert_eq!(live_cursor.bg, Color::LightGreen);
    assert!(live_cursor.modifier.contains(Modifier::BOLD));

    let command = render_cursor_widget(false);
    let command_cursor = &command[(4, 2)];
    assert_ne!(
        command_cursor.bg,
        Color::LightGreen,
        "a focused pane in command mode must not paint the solid LightGreen block cursor; got {:?}",
        command_cursor.style()
    );
}

/// Scenario: Render the same focused pane through a complete TestBackend frame in PaneInput and command mode. The live frame must request a terminal cursor position, while the command-mode frame must leave the terminal cursor hidden.
#[spec("mode/cursor/002")]
#[test]
fn mode_cursor_002_terminal_cursor_hidden_in_command_mode() {
    let typing = render_focused_pane_cursor_for_mode_to_position(UiMode::PaneInput);
    let command = render_focused_pane_cursor_for_mode_to_position(UiMode::Normal);

    assert!(
        typing.is_some(),
        "a focused pane in PaneInput must request the terminal emulator cursor"
    );
    assert_eq!(
        command, None,
        "a focused pane in command mode must not call Frame::set_cursor_position"
    );
}

/// Scenario: Render the production bottom bar once in command mode and once in PaneInput. A left-anchored REVERSED|BOLD chip must name the current mode as COMMAND or TYPING without using an absolute RGB colour, and a snapshot pins both complete bars.
#[spec("mode/chip/001")]
#[test]
fn mode_chip_001_bottom_bar_names_current_mode() {
    let config = KeybindingConfig::default();
    let command = render_button_bar_for_mode_to_buffer(&config, UiMode::Normal, 200, 2);
    let typing = render_button_bar_for_mode_to_buffer(&config, UiMode::PaneInput, 200, 2);

    assert_mode_chip_at_origin(&command, COMMAND_CHIP, "command-mode bottom bar");
    assert_mode_chip_at_origin(&typing, TYPING_CHIP, "PaneInput bottom bar");

    insta::assert_snapshot!(
        "mode_chip_001_current_mode_bars",
        format!(
            "COMMAND MODE\n{}\n\nPANE INPUT\n{}",
            buffer_to_text(&command),
            buffer_to_text(&typing)
        )
    );
}

/// Scenario: Render the global-only Mode-tab bar, the context-rich Dashboard/Orchestration bar, and the PaneInput bar. Every context must keep the chip at the same left edge while retaining the destination-naming Ctrl+D button beside it.
#[spec("mode/chip/002")]
#[test]
fn mode_chip_002_is_universal_and_keeps_destination_button() {
    let config = KeybindingConfig::default();
    let dashboard = render_button_bar_for_mode_to_buffer(&config, UiMode::Normal, 200, 2);
    let mode_tab = render_button_bar_to_buffer(200);
    // Dashboard and Orchestration share the production Cards bottom-bar path;
    // render it independently here so failures name both user-visible contexts.
    let orchestration = render_button_bar_with_bindings_to_buffer(&config, 200, 2);

    for (context, buffer) in [
        ("Dashboard", &dashboard),
        ("Mode tab", &mode_tab),
        ("Orchestration tab", &orchestration),
    ] {
        assert_mode_chip_at_origin(buffer, COMMAND_CHIP, context);
        let text = buffer_to_text(buffer);
        assert!(
            text.contains("[Back to Pane Ctrl+D]"),
            "{context} must retain the destination button alongside the COMMAND chip\n{text}"
        );
    }

    let typing = render_button_bar_for_mode_to_buffer(&config, UiMode::PaneInput, 200, 2);
    assert_mode_chip_at_origin(&typing, TYPING_CHIP, "PaneInput on every tab");
    let typing_text = buffer_to_text(&typing);
    assert!(
        typing_text.contains("[Command Mode Ctrl+D]"),
        "PaneInput must retain the destination button alongside the TYPING chip\n{typing_text}"
    );
}

/// Scenario: Render the same selected Dashboard card in command mode and PaneInput through the production card renderer. Command mode must retain the full Magenta+BOLD accent, while PaneInput keeps the `▸ ` marker but visibly de-emphasises the selection without introducing an absolute RGB colour; a colour-and-modifier-aware snapshot pins both states.
#[spec("mode/deck/001")]
#[test]
fn mode_deck_001_selected_card_accent_tracks_mode() {
    let session = selected_card_fixture();
    let width = 80;
    let density = CardDensityKind::Normal;
    let height = density.rendered_height(true);
    let command = render_card_for_mode_to_buffer(
        &session,
        Some("mode-card"),
        Some(1),
        density,
        0,
        true,
        UiMode::Normal,
        width,
        height,
    );
    let typing = render_card_for_mode_to_buffer(
        &session,
        Some("mode-card"),
        Some(1),
        density,
        0,
        true,
        UiMode::PaneInput,
        width,
        height,
    );
    let legacy = render_card_to_buffer(
        &session,
        Some("mode-card"),
        Some(1),
        density,
        0,
        true,
        width,
        height,
    );

    assert_eq!(
        command, legacy,
        "command-mode selection must stay byte-identical to today's selected-card rendering"
    );

    let border_y = height / 2;
    let command_border = &command[(0, border_y)];
    let typing_border = &typing[(0, border_y)];
    assert_eq!(command_border.fg, Color::Magenta);
    assert!(command_border.modifier.contains(Modifier::BOLD));
    assert!(
        buffer_to_text(&command).contains("▸ "),
        "command mode must retain the selected-card title marker"
    );
    assert!(
        buffer_to_text(&typing).contains("▸ "),
        "PaneInput must retain the selected-card title marker"
    );
    assert_ne!(
        typing_border.style(),
        command_border.style(),
        "PaneInput selection must be visibly de-emphasised relative to command mode"
    );
    assert!(
        !typing_border.modifier.contains(Modifier::BOLD)
            || typing_border.modifier.contains(Modifier::DIM),
        "PaneInput selection must drop BOLD and/or add DIM, got {:?}",
        typing_border.style()
    );
    assert_no_rgb(&command, "command-mode selected card");
    assert_no_rgb(&typing, "PaneInput selected card");

    insta::assert_snapshot!(
        "mode_deck_001_selected_card_styles",
        format!(
            "COMMAND MODE\n{}\nPANE INPUT\n{}",
            buffer_to_styled_text(&command),
            buffer_to_styled_text(&typing)
        )
    );
}

/// Scenario: Send one wheel-up event over the focused agent pane in all four combinations of command/PaneInput mode and child mouse reporting on/off. PaneInput may forward only when reporting is enabled; command mode must always move dot-agent-deck's scrollback and must never emit mouse-protocol bytes to the child.
#[spec("mode/scroll/001")]
#[test]
fn mode_scroll_001_mouse_wheel_routes_by_mode_and_child_mouse_state() {
    for (mode, mouse_mode_enabled, should_forward, context) in [
        (
            UiMode::PaneInput,
            true,
            true,
            "PaneInput with child mouse reporting",
        ),
        (
            UiMode::PaneInput,
            false,
            false,
            "PaneInput without child mouse reporting",
        ),
        (
            UiMode::Normal,
            true,
            false,
            "command mode with child mouse reporting",
        ),
        (
            UiMode::Normal,
            false,
            false,
            "command mode without child mouse reporting",
        ),
    ] {
        let observed = observe_focused_agent_mouse_scroll(mode, mouse_mode_enabled, true, 0, 3, 2);
        if should_forward {
            assert_eq!(
                observed.forwarded_bytes, b"\x1b[<64;4;3M",
                "{context} must forward exactly one wheel-up report to the child"
            );
            assert_eq!(
                observed.scrollback_after, observed.scrollback_before,
                "{context} must leave dot-agent-deck's scrollback at live output"
            );
        } else {
            assert!(
                observed.scrollback_after > observed.scrollback_before,
                "{context} must scroll dot-agent-deck's own scrollback: {observed:?}"
            );
            assert!(
                observed.forwarded_bytes.is_empty(),
                "{context} must not emit mouse-protocol bytes to the child: {observed:?}"
            );
        }
    }
}

/// Scenario: In command mode, press the configured focused-pane scroll-up and scroll-down keys against a pane with synthetic history. The PageUp/PageDown defaults and custom `[dashboard]` remaps must move dot-agent-deck's scrollback in the expected direction without emitting bytes to the child, while each remap disables its former default.
#[spec("mode/scroll/002")]
#[test]
fn mode_scroll_002_keyboard_scroll_is_semantic_and_remappable() {
    let default = KeybindingConfig::default();
    let page_up = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
    let page_down = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);

    let up = observe_focused_agent_key_scroll(&default, UiMode::Normal, page_up, 0);
    assert!(
        up.scrollback_after > up.scrollback_before,
        "default PageUp must scroll back through the focused agent pane: {up:?}"
    );
    assert!(
        up.forwarded_bytes.is_empty(),
        "command-mode PageUp must not emit bytes to the child: {up:?}"
    );

    let down = observe_focused_agent_key_scroll(&default, UiMode::Normal, page_down, 6);
    assert!(
        down.scrollback_after < down.scrollback_before,
        "default PageDown must scroll toward live output in the focused agent pane: {down:?}"
    );
    assert!(
        down.forwarded_bytes.is_empty(),
        "command-mode PageDown must not emit bytes to the child: {down:?}"
    );

    let (remapped, warnings) = KeybindingConfig::from_toml_str(
        "[dashboard]\nscroll_pane_up = \"Alt+u\"\nscroll_pane_down = \"Alt+d\"\n",
    )
    .expect("focused-pane scroll remaps must parse");
    assert!(
        warnings.is_empty(),
        "focused-pane scroll keys must be registered semantic actions: {warnings:?}"
    );

    let old_up = observe_focused_agent_key_scroll(&remapped, UiMode::Normal, page_up, 0);
    assert_eq!(
        old_up.scrollback_after, old_up.scrollback_before,
        "remapping scroll_pane_up must disable its PageUp default"
    );
    let remapped_up = observe_focused_agent_key_scroll(
        &remapped,
        UiMode::Normal,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::ALT),
        0,
    );
    assert!(
        remapped_up.scrollback_after > remapped_up.scrollback_before,
        "the custom scroll_pane_up binding must scroll the focused pane: {remapped_up:?}"
    );

    let old_down = observe_focused_agent_key_scroll(&remapped, UiMode::Normal, page_down, 6);
    assert_eq!(
        old_down.scrollback_after, old_down.scrollback_before,
        "remapping scroll_pane_down must disable its PageDown default"
    );
    let remapped_down = observe_focused_agent_key_scroll(
        &remapped,
        UiMode::Normal,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT),
        6,
    );
    assert!(
        remapped_down.scrollback_after < remapped_down.scrollback_before,
        "the custom scroll_pane_down binding must scroll the focused pane: {remapped_down:?}"
    );
    assert!(
        remapped_up.forwarded_bytes.is_empty() && remapped_down.forwarded_bytes.is_empty(),
        "command-mode keyboard scrolling must never write to the child"
    );
}
