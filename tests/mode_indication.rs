//! L1 coverage for the persistent current-mode indicators.
//!
//! These tests exercise the production `TerminalWidget` and bottom-bar render
//! paths in process. The frame-level hardware-cursor test uses a dedicated
//! TestBackend seam because `Frame::set_cursor_position` has no buffer cell to
//! inspect.

use std::sync::{Arc, Mutex};

use dot_agent_deck::keybindings::KeybindingConfig;
use dot_agent_deck::terminal_widget::TerminalWidget;
use dot_agent_deck::ui::{
    UiMode, render_button_bar_for_mode_to_buffer, render_button_bar_to_buffer,
    render_button_bar_with_bindings_to_buffer, render_focused_pane_cursor_for_mode_to_position,
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
