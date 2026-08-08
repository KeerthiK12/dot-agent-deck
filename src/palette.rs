//! PRD #155 — centralized color palette (single source of truth).
//!
//! Before this module the TUI's semantic colors were scattered as inline
//! `Color::X` literals across the deck-card and embedded-pane render paths,
//! and the two surfaces drifted apart (a working agent could look different as
//! a deck card vs. as an embedded pane). This palette names the semantic
//! **roles** once and both render paths resolve their colors through it, so a
//! given state renders identically everywhere (PRD #155 Option A).
//!
//! ## Border policy (Option A — identical in both render paths)
//!
//! The card/pane border encodes **STATUS** in both the dashboard deck and the
//! embedded panes. The unified border-resolution precedence is:
//!
//! 1. **focused AND live** (embedded panes only) → [`FOCUSED`] (Cyan).
//! 2. else → the agent's **status** role ([`status_color`]).
//!
//! **Selection is deliberately absent from that list** (issue #442). A deck card
//! signals selection on the *glyph* channel — `BorderType::Thick` plus the `▸ `
//! title marker — and never with a colour of its own, so a selected card keeps
//! reporting the state of its agent. The full ladder, including how the
//! command / `PaneInput` mode distinction rides on emphasis, lives in exactly
//! one place: `ui::card_border_glyph`.
//!
//! The per-card status **badge** shows status too, so the focus override in (1)
//! never loses status information.
//!
//! ### Why (1) requires "live" (issue #88 follow-up)
//!
//! For an **embedded pane**, "focused" and "keystrokes reach it" are different
//! facts: in command mode the focused pane is still the one `Ctrl+D` / `Enter`
//! return you to, but it accepts no keys. The Cyan accent originally rendered
//! in both cases, which made the loudest border signal on screen claim "type
//! here" while the keyboard was driving the deck — the mode was invisible on a
//! full-screen mode tab, where nothing else in the frame changes.
//!
//! So for panes, (1) applies only in `UiMode::PaneInput`
//! (`TerminalWidget::with_input_active`). In command mode the focused pane
//! falls through to (2) and reports its agent's status like any other pane,
//! while **border thickness** (`BorderType::Thick`) carries focus instead.
//! Colour answers "are my keystrokes landing here?", thickness answers "which
//! pane is focused?" — one channel each, no longer competing.
//!
//! ### Why a deck card is mode-aware too (PRD #341 M4, revised by issue #442)
//!
//! A card carries no cursor and takes no keystrokes of its own, so it has no
//! input mode in the sense (1) means. But *the deck* does: in command mode the
//! keyboard drives the cards, and in `UiMode::PaneInput` it drives a pane while
//! the selection merely persists. The selection cue rendered identically in
//! both, which left the Dashboard — where the pane overlay is weakest and the
//! user's eyes are on the deck — looking equally live either way.
//!
//! `UiMode` is therefore threaded into card rendering as well. PRD #341 M4
//! originally spent COLOUR on this — Magenta + BOLD in command mode, Magenta +
//! `Modifier::DIM` in `UiMode::PaneInput` — and issue #442 removed that: DIM
//! Magenta on a dark theme is indistinguishable from [`STATUS_IDLE`], so the
//! selected card read as an idle one. Cards now do what panes already did, one
//! step further: colour is status, thickness is selection, and **emphasis**
//! (BOLD or nothing — never DIM) is the mode. See `ui::card_border_glyph`.
//!
//! Thickness was chosen over a further accent colour because the 16-colour-safe
//! palette is full (green/blue/yellow/red are statuses, cyan is focus) and the
//! remaining candidates are grays — the exact light-background hazard PRD #13
//! exists to prevent. `BorderType` never feeds `Block::inner`, so it costs no
//! layout: the pane's inner area, its PTY size, the card's inner area, and the
//! PRD #84 invariant-3 contract are all unaffected.
//!
//! All roles are **named ANSI** colors only — no absolute `Color::Rgb`, which
//! the theme guards (`theme/contrast/001`) forbid so terminal themes can remap
//! them.

use ratatui::style::Color;

use crate::state::SessionStatus;

// ---------------------------------------------------------------------------
// Status roles
// ---------------------------------------------------------------------------

/// Working — the agent is actively running a tool / producing output.
pub const STATUS_WORKING: Color = Color::Green;
/// Thinking — the agent is reasoning before acting.
pub const STATUS_THINKING: Color = Color::Blue;
/// Waiting — the agent needs user input to proceed.
pub const STATUS_WAITING: Color = Color::Yellow;
/// Error — the agent hit a failure.
pub const STATUS_ERROR: Color = Color::Red;
/// Idle — no current activity (dimmed).
pub const STATUS_IDLE: Color = Color::DarkGray;

// ---------------------------------------------------------------------------
// Accent roles (must be distinct from every status color and from each other)
// ---------------------------------------------------------------------------

/// The focused embedded pane — the one accent role left. Cyan was originally
/// used for focus *and* selection; PRD #155 Option A split them by giving
/// selection a Magenta accent of its own, and issue #442 retired that accent
/// entirely in favour of border thickness. So focus keeps Cyan and nothing else
/// claims a colour.
///
/// There is deliberately **no `SELECTED` colour role**. Selection is a glyph
/// (`BorderType::Thick`) plus the `▸ ` title marker — see `ui::card_border_glyph`.
/// Do not reintroduce a selection colour here: it would put a third meaning back
/// on the border's colour channel, which is what made the selected card
/// indistinguishable from an idle one (issue #442).
pub const FOCUSED: Color = Color::Cyan;

/// Resolve a session status to its centralized border/badge role color. This
/// is the single source of truth shared by the deck-card render path
/// (`src/ui.rs`) and the embedded-pane render path (`src/terminal_widget.rs`),
/// so a given state shows the same border color in both contexts.
pub fn status_color(status: &SessionStatus) -> Color {
    match status {
        SessionStatus::Working => STATUS_WORKING,
        SessionStatus::Thinking => STATUS_THINKING,
        // Compacting is a thinking-adjacent transient state; it shares the
        // thinking role rather than introducing a sixth status color.
        SessionStatus::Compacting => STATUS_THINKING,
        SessionStatus::WaitingForInput => STATUS_WAITING,
        SessionStatus::Error => STATUS_ERROR,
        SessionStatus::Idle => STATUS_IDLE,
        // PRD #162 forward-compat: an unknown wire status renders with the
        // neutral idle color so it never masquerades as an active state.
        SessionStatus::Unknown => STATUS_IDLE,
    }
}

/// This status's rank in the PRD #333 fixed priority order — lower ranks
/// win. Mirrors the aliasing [`status_color`] already applies (Compacting
/// shares Thinking's rank, Unknown shares Idle's) so a status that resolves
/// to the same color also resolves to the same priority.
fn priority_rank(status: &SessionStatus) -> u8 {
    match status {
        SessionStatus::Error => 0,
        SessionStatus::WaitingForInput => 1,
        SessionStatus::Working => 2,
        SessionStatus::Thinking | SessionStatus::Compacting => 3,
        SessionStatus::Idle | SessionStatus::Unknown => 4,
    }
}

/// PRD #333 M1 — resolve the single highest-priority `SessionStatus` among an
/// orchestration tab's pane statuses, per the fixed order Error > NeedsInput >
/// Working > Thinking > Idle (ties within a rank keep whichever status was
/// encountered first). An empty slice (a tab with no panes) falls back to
/// `Idle`, the same neutral "nothing going on" state an all-Idle tab
/// resolves to.
pub fn highest_priority_status(statuses: &[SessionStatus]) -> SessionStatus {
    statuses
        .iter()
        .min_by_key(|status| priority_rank(status))
        .cloned()
        .unwrap_or(SessionStatus::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: feed `highest_priority_status` slices covering every rank in
    /// the PRD #333 table (including the Compacting→Thinking and
    /// Unknown→Idle aliases) and confirm it always returns the single
    /// highest-priority status present, plus the defined no-panes fallback.
    #[test]
    fn highest_priority_status_orders_by_priority() {
        use SessionStatus::*;

        assert_eq!(highest_priority_status(&[Idle, Error, Idle]), Error);
        assert_eq!(
            highest_priority_status(&[Working, WaitingForInput, Idle]),
            WaitingForInput
        );
        assert_eq!(highest_priority_status(&[Idle, Idle, Idle]), Idle);
        assert_eq!(highest_priority_status(&[Thinking, Working]), Working);
        assert_eq!(highest_priority_status(&[Compacting, Idle]), Compacting);
        assert_eq!(highest_priority_status(&[Unknown, Idle]), Unknown);
        assert_eq!(highest_priority_status(&[Error, WaitingForInput]), Error);
        assert_eq!(highest_priority_status(&[]), Idle);
    }
}
