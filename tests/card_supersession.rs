use chrono::{Duration, Utc};
use dot_agent_deck::event::{AgentEvent, AgentType, DISPLAY_NAME_METADATA_KEY, EventType};
use dot_agent_deck::state::AppState;

use spec::spec;

const PANE_ID: &str = "scheduler-handoff-pane";
const TASK_NAME: &str = "morning-digest";

fn event(
    session_id: &str,
    agent_type: AgentType,
    event_type: EventType,
    agent_id: Option<&str>,
    timestamp: chrono::DateTime<Utc>,
) -> AgentEvent {
    AgentEvent {
        session_id: session_id.to_string(),
        agent_type,
        event_type,
        tool_name: None,
        tool_detail: None,
        cwd: Some("/tmp/runbox".to_string()),
        timestamp,
        user_prompt: None,
        metadata: Default::default(),
        pane_id: Some(PANE_ID.to_string()),
        agent_id: agent_id.map(str::to_string),
        agent_version: None,
        schema_version: None,
        live_target: None,
    }
}

/// Scenario: A scheduler first surfaces a friendly `No agent` placeholder, then the real agent reports `SessionStart` on the same pane without display-name metadata. The handoff must leave one live card carrying both the real agent id and the scheduler's friendly task name.
#[spec("status/supersede/001")]
#[test]
fn status_supersede_001_real_session_replaces_placeholder_and_keeps_friendly_name() {
    let placeholder_timestamp = Utc::now();
    let mut placeholder = event(
        "scheduler-placeholder",
        AgentType::None,
        EventType::SessionStart,
        None,
        placeholder_timestamp,
    );
    placeholder
        .metadata
        .insert(DISPLAY_NAME_METADATA_KEY.to_string(), TASK_NAME.to_string());

    let mut state = AppState::default();
    state.register_pane(PANE_ID.to_string());
    state.apply_event(placeholder);

    let placeholder_card = state
        .sessions
        .get("scheduler-placeholder")
        .expect("precondition: scheduler placeholder is visible");
    assert_eq!(placeholder_card.agent_id, None);
    assert_eq!(placeholder_card.display_name.as_deref(), Some(TASK_NAME));

    // A real hook may have been emitted before the scheduler's synthetic
    // surface frame was applied, so its event timestamp can be older even
    // though its Some(agent_id) identity authoritatively supersedes the
    // placeholder's None.
    let incoming_timestamp = placeholder_timestamp - Duration::seconds(1);
    let incoming = event(
        "real-agent-session",
        AgentType::ClaudeCode,
        EventType::SessionStart,
        Some("real-agent-id"),
        incoming_timestamp,
    );

    assert!(
        state.sessions["scheduler-placeholder"].pane_id == incoming.pane_id,
        "precondition: the replacement addresses the placeholder's pane"
    );
    assert_ne!(
        state.sessions["scheduler-placeholder"].agent_id, incoming.agent_id,
        "precondition: None placeholder identity differs from Some(real agent)"
    );
    assert!(
        incoming.timestamp < state.sessions["scheduler-placeholder"].last_activity,
        "precondition: only a timestamp guard can reject this authoritative handoff"
    );

    state.apply_event(incoming);

    assert_eq!(
        state.sessions.len(),
        1,
        "the real SessionStart must supersede the No-agent placeholder instead of stacking a second card on its pane"
    );
    let live = state
        .sessions
        .get("real-agent-session")
        .expect("the one surviving card must use the real session identity");
    assert_eq!(live.agent_id.as_deref(), Some("real-agent-id"));
    assert_eq!(
        live.display_name.as_deref(),
        Some(TASK_NAME),
        "the replacement must inherit the scheduler's friendly display name"
    );
}

/// Scenario: A close confirmation is armed against a session id, then a different agent generation reports `SessionStart` on the same pane. The armed identity must disappear from state so confirmation resolves it as vanished and cannot close the replacement.
#[spec("status/supersede/002")]
#[test]
fn status_supersede_002_replaced_armed_session_identity_resolves_as_vanished() {
    let original_timestamp = Utc::now();
    let original = event(
        "armed-session",
        AgentType::ClaudeCode,
        EventType::SessionStart,
        Some("outgoing-agent-id"),
        original_timestamp,
    );

    let mut state = AppState::default();
    state.register_pane(PANE_ID.to_string());
    state.apply_event(original);

    // CloseTarget::Session stores this stable session identity at arm time.
    let armed_session_id = "armed-session".to_string();
    assert!(state.sessions.contains_key(&armed_session_id));

    // Delivery order, not the producer timestamp, determines that this real
    // SessionStart is the replacement. The older stamp exercises the exact
    // case where the naive monotonicity guard retains the armed target.
    let replacement = event(
        "replacement-session",
        AgentType::ClaudeCode,
        EventType::SessionStart,
        Some("incoming-agent-id"),
        original_timestamp - Duration::seconds(1),
    );
    state.apply_event(replacement);

    assert!(
        !state.sessions.contains_key(&armed_session_id),
        "the session identity captured by close confirmation must vanish when another generation takes over the pane"
    );
    assert!(
        state.sessions.contains_key("replacement-session"),
        "the incoming generation must remain visible but must not inherit the armed session id"
    );
    assert_eq!(
        state.sessions.len(),
        1,
        "session replacement must not leave the armed generation beside the live one"
    );
}
