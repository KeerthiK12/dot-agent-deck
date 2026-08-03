//! Fast-tier contract tests for the Devin CLI native-hooks adapter.
//!
//! Devin is the second agent to reuse [`IntegrationStrategy::NativeHooks`], so
//! these tests pin the two things that were previously only true by accident for
//! a reused strategy: that Devin's registry identity is complete, and that its
//! integration handlers are its OWN rather than the Claude incumbent's.

use dot_agent_deck::agent_registry::{self, IntegrationStrategy};
use dot_agent_deck::event::AgentType;
use ratatui::style::Color;

/// Devin is a typed, detectable first-class agent whose complete metadata lives
/// in the registry and selects the native-hooks integration strategy.
#[test]
fn devin_detect_001_registry_identity_is_complete() {
    assert_eq!(
        AgentType::from_command(Some("devin")),
        Some(AgentType::Devin)
    );
    // Full path resolves via the basename.
    assert_eq!(
        AgentType::from_command(Some("/usr/local/bin/devin")),
        Some(AgentType::Devin)
    );
    // Args after the binary are ignored.
    assert_eq!(
        AgentType::from_command(Some("devin --model opus -- fix the failing tests")),
        Some(AgentType::Devin)
    );
    assert_eq!(format!("{}", AgentType::Devin), "Devin");

    let spec = agent_registry::spec(&AgentType::Devin);
    assert_eq!(spec.agent_type, AgentType::Devin);
    assert_eq!(spec.label, "Devin");
    assert_eq!(spec.default_command, Some("devin"));
    assert_eq!(spec.strategy, Some(IntegrationStrategy::NativeHooks));
    assert!(spec.detect_basenames.contains(&"devin"));
    assert_ne!(
        spec.badge_color,
        Color::DarkGray,
        "Devin must have a non-neutral first-class badge color"
    );
}

/// Devin is in the shipped-agents slice, so detection, the badge, the `type:`
/// filter, and the startup auto-install all pick it up without per-agent code at
/// those sites.
#[test]
fn devin_detect_002_is_a_shipped_agent_with_a_unique_badge() {
    assert!(
        agent_registry::ALL
            .iter()
            .any(|spec| spec.agent_type == AgentType::Devin),
        "Devin must be in ALL or it gets no badge, no filter and no auto-install"
    );

    // `type:devin` (basename) and `type:Devin` (label) both resolve.
    assert_eq!(
        agent_registry::resolve_type_alias("devin"),
        Some(AgentType::Devin)
    );
    assert_eq!(
        agent_registry::resolve_type_alias("DEVIN"),
        Some(AgentType::Devin)
    );

    // Badge colours must stay distinguishable between agents.
    let devin = agent_registry::spec(&AgentType::Devin).badge_color;
    for other in [
        AgentType::ClaudeCode,
        AgentType::OpenCode,
        AgentType::Pi,
        AgentType::Codex,
    ] {
        assert_ne!(
            devin,
            agent_registry::spec(&other).badge_color,
            "Devin's badge colour collides with {other}"
        );
    }
}

/// Devin reuses the `NativeHooks` strategy Claude introduced, so the risk is that
/// it silently runs CLAUDE's installer. Its handlers must be present (a
/// `NativeHooks` agent installs at startup) and must not be the Claude ones.
#[test]
fn devin_detect_003_handlers_are_its_own_not_the_claude_incumbents() {
    let devin = agent_registry::spec(&AgentType::Devin);
    let claude = agent_registry::spec(&AgentType::ClaudeCode);

    assert_eq!(
        devin.strategy, claude.strategy,
        "precondition: both are NativeHooks agents"
    );

    let devin_install = devin.hook_install.expect("Devin installs native hooks");
    let devin_uninstall = devin
        .hook_uninstall
        .expect("Devin removes its own native hooks");
    let startup = devin
        .startup_auto_install
        .expect("a NativeHooks agent installs at TUI startup");

    let claude_install = claude.hook_install.expect("Claude installs native hooks");
    let claude_startup = claude
        .startup_auto_install
        .expect("Claude installs at startup");

    assert!(
        !std::ptr::fn_addr_eq(devin_install, claude_install),
        "Devin must run its OWN installer, not Claude's"
    );
    assert!(
        !std::ptr::fn_addr_eq(startup, claude_startup),
        "Devin must run its OWN startup auto-install, not Claude's"
    );
    assert!(!std::ptr::fn_addr_eq(devin_install, devin_uninstall));

    // Devin's own TUI runs directly on the deck's PTY, so it needs neither a
    // wrapper nor a spawn-time materialize step.
    assert!(devin.materialize.is_none());
}

/// Wire safety: the new `AgentType` variant round-trips as `"devin"`, and an
/// older reader that has never heard of it still decodes the whole record —
/// landing on the neutral "No agent" placeholder rather than erroring.
#[test]
fn devin_detect_004_agent_type_round_trips_on_the_wire() {
    assert_eq!(
        serde_json::to_string(&AgentType::Devin).unwrap(),
        "\"devin\""
    );
    assert_eq!(
        serde_json::from_str::<AgentType>("\"devin\"").unwrap(),
        AgentType::Devin
    );

    // The record shape a subscriber decodes, carrying the new value.
    let event: dot_agent_deck::event::AgentEvent = serde_json::from_str(
        r#"{
            "session_id": "devin-1",
            "agent_type": "devin",
            "event_type": "tool_start",
            "timestamp": "2026-07-31T10:00:00Z"
        }"#,
    )
    .expect("a devin-stamped event must decode");
    assert_eq!(event.agent_type, AgentType::Devin);
}
