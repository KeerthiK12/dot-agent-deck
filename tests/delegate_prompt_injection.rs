//! Regression guard for #187 / PR #188: a delegated worker pane must
//! receive a SINGLE-LINE prompt, and the `work-done` completion footer
//! must live in the worker task FILE rather than in the injected prompt.
//!
//! Why this matters: `encode_pane_payload` wraps any payload containing a
//! newline in bracketed-paste markers (`ESC[200~ … ESC[201~`). In Claude
//! Code that framing lands as a compacted block the worker never submits
//! without a manual Enter (#187). The fix keeps the injected delegate
//! prompt to one line — the single-line pointer at
//! `.dot-agent-deck/worker-task-<role>.md` — and moves the footer into the
//! task file.
//!
//! Unit tests already cover `compose_delegate_prompt` (single-line) and
//! `encode_pane_payload` (single-line → no wrap) in isolation. This test
//! exercises the REAL daemon dispatch wiring end to end — `handle_delegate`
//! → `dispatch_one_owned` → `compose_delegate_prompt` →
//! `write_to_pane_and_submit` — and asserts the bytes that actually reach a
//! worker pane's PTY plus the contents of the generated task file.
//!
//! No LLM and no real agent: the worker pane is a `cat` stub whose PTY
//! echoes whatever the daemon injects, so the snapshot reflects the
//! delivered bytes. Runs in the fast tier (no `e2e` feature gate).

use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use dot_agent_deck::agent_pty::{AgentPtyRegistry, DOT_AGENT_DECK_PANE_ID, SpawnOptions};
use dot_agent_deck::event::{AgentEvent, AgentType, BroadcastMsg, DelegateSignal, EventType};
use dot_agent_deck::state::{AppState, OrchestrationIdentity};
#[cfg(unix)]
use spec::spec;

mod common;

const ORCH_PANE: &str = "orchestrator-pane";
const WORKER_PANE: &str = "worker-pane";
const WORKER_ROLE: &str = "coder";
const POINTER: &[u8] = b"Read .dot-agent-deck/worker-task-coder.md for your task.";
const SESSION_START_ORIGIN_METADATA_KEY: &str = "session_start_origin";
const WRAPPER_FORK_SESSION_START_ORIGIN: &str = "wrapper_fork";
const DELEGATE_READINESS_BUFFER_ENV: &str = "DOT_AGENT_DECK_DELEGATE_READINESS_BUFFER_MS";
const SESSION_START_WAIT_ENV: &str = "DOT_AGENT_DECK_SESSION_START_WAIT_MS";
const WORKER_RESPONSE_TIMEOUT_ENV: &str = "DOT_AGENT_DECK_WORKER_RESPONSE_TIMEOUT_MS";
const DELEGATE_READINESS_BUFFER_MS: u64 = 1000;
const SLOW_STUB_NOT_READY_MS: u64 = 650;

/// Serializes process-environment changes when this integration-test binary is
/// run through plain `cargo test`; nextest already gives each test a process.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn set(values: &[(&'static str, &str)]) -> Self {
        let mut previous = Vec::with_capacity(values.len());
        for (key, value) in values {
            previous.push((*key, std::env::var_os(key)));
            // SAFETY: every env-mutating test in this integration-test binary
            // holds ENV_LOCK for the guard's full lifetime.
            unsafe { std::env::set_var(key, value) };
        }
        Self { previous }
    }

    fn repoint(&self, key: &'static str, value: &str) {
        assert!(
            self.previous.iter().any(|(saved, _)| *saved == key),
            "cannot repoint an environment key this guard does not own: {key}"
        );
        // SAFETY: the caller still holds ENV_LOCK while this guard is alive.
        unsafe { std::env::set_var(key, value) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, previous) in self.previous.drain(..).rev() {
            // SAFETY: the caller still holds ENV_LOCK while this guard drops.
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

/// Poll the agent's PTY snapshot until `needle` appears or `timeout`
/// elapses, returning the final snapshot either way so the caller can
/// assert (and print it on failure).
async fn wait_for_snapshot_needle(
    registry: &AgentPtyRegistry,
    agent_id: &str,
    needle: &[u8],
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(snap) = registry.snapshot(agent_id)
            && snap.windows(needle.len()).any(|w| w == needle)
        {
            return snap;
        }
        if tokio::time::Instant::now() >= deadline {
            return registry.snapshot(agent_id).unwrap_or_default();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn snapshot_contains(snapshot: &[u8], needle: &[u8]) -> bool {
    snapshot
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).expect("write synthetic agent executable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod synthetic agent executable");
}

#[cfg(unix)]
fn path_with_built_deck(bin_dir: &std::path::Path) -> String {
    let deck_dir = std::path::Path::new(env!("CARGO_BIN_EXE_dot-agent-deck"))
        .parent()
        .expect("built deck binary has a parent directory");
    format!(
        "{}:{}:{}",
        bin_dir.display(),
        deck_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
fn clear_true_config(command: &str) -> String {
    format!(
        "[[orchestrations]]\nname = \"test-orchestration\"\n\n[[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"true\"\nstart = true\n\n[[orchestrations.roles]]\nname = \"coder\"\ncommand = \"{command}\"\nclear = true\n"
    )
}

#[cfg(unix)]
fn write_slow_readiness_stub(path: &std::path::Path) {
    let script = format!(
        r#"#!/usr/bin/env python3
import os
import sys
import termios
import time

fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
new = list(old)
new[0] &= ~(termios.IGNBRK | termios.BRKINT | termios.PARMRK
            | termios.ISTRIP | termios.INLCR | termios.IGNCR
            | termios.ICRNL | termios.IXON)
new[1] &= ~termios.OPOST
new[3] &= ~(termios.ECHO | termios.ECHONL | termios.ICANON
            | termios.ISIG | termios.IEXTEN)
termios.tcsetattr(fd, termios.TCSANOW, new)

os.write(1, b'DELEGATE-STUB-RAW-READY')
os.set_blocking(fd, False)
deadline = time.monotonic() + {seconds}
while time.monotonic() < deadline:
    try:
        os.read(fd, 4096)
    except BlockingIOError:
        pass
    time.sleep(0.005)
os.set_blocking(fd, True)

os.write(1, b'DELEGATE-STUB-CAT-READY')
while True:
    data = os.read(fd, 4096)
    if not data:
        break
    os.write(1, data)
"#,
        seconds = SLOW_STUB_NOT_READY_MS as f64 / 1000.0,
    );
    write_executable(path, &script);
}

#[cfg(unix)]
fn snapshot_has_silence_notice(snapshot: &[u8]) -> bool {
    String::from_utf8_lossy(snapshot)
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .any(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("delegat") && line.contains("coder") && line.contains("event")
        })
}

#[cfg(unix)]
async fn wait_for_silence_notice(
    registry: &AgentPtyRegistry,
    agent_id: &str,
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshot = registry.snapshot(agent_id).unwrap_or_default();
        if snapshot_has_silence_notice(&snapshot) || tokio::time::Instant::now() >= deadline {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
struct SlowReadinessResult {
    snapshot: Vec<u8>,
    measured_readiness_window: Duration,
}

#[cfg(unix)]
async fn run_slow_readiness_delegate(buffer_ms: u64) -> SlowReadinessResult {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let stub = cwd.path().join("slow-readiness-agent.py");
    write_slow_readiness_stub(&stub);
    let command = stub.to_string_lossy().into_owned();
    std::fs::write(
        cwd.path().join(".dot-agent-deck.toml"),
        clear_true_config(&command),
    )
    .expect("write slow-readiness orchestration config");
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let old_agent_id = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some(&command),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn initial slow-readiness stand-in");
    {
        let mut state = daemon.state.write().await;
        register_orchestration(&mut state, &cwd_str);
    }

    let signal = DelegateSignal {
        pane_id: ORCH_PANE.to_string(),
        task: "List the files in the current directory.".to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
    };
    daemon
        .state
        .read()
        .await
        .handle_delegate(signal, &daemon.registry, &daemon.event_tx)
        .await;
    let new_agent_id =
        wait_for_replacement_agent(&daemon.registry, WORKER_PANE, &old_agent_id).await;
    let raw_ready = wait_for_snapshot_needle(
        &daemon.registry,
        &new_agent_id,
        b"DELEGATE-STUB-RAW-READY",
        Duration::from_secs(2),
    )
    .await;
    assert!(
        snapshot_contains(&raw_ready, b"DELEGATE-STUB-RAW-READY"),
        "replacement slow-readiness stub never entered raw discard mode; snapshot = {:?}",
        String::from_utf8_lossy(&raw_ready)
    );

    let session_start_at = Instant::now();
    let event = session_start_event(AgentType::None, WORKER_PANE, &new_agent_id, false);
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&event).expect("serialize slow-stub SessionStart"),
    )
    .expect("write slow-stub SessionStart");
    let cat_ready = wait_for_snapshot_needle(
        &daemon.registry,
        &new_agent_id,
        b"DELEGATE-STUB-CAT-READY",
        Duration::from_secs(2),
    )
    .await;
    let measured_readiness_window = session_start_at.elapsed();
    assert!(
        snapshot_contains(&cat_ready, b"DELEGATE-STUB-CAT-READY"),
        "slow-readiness stub did not become input-aware; snapshot = {:?}",
        String::from_utf8_lossy(&cat_ready)
    );

    let mut submitted_pointer = POINTER.to_vec();
    submitted_pointer.push(b'\r');
    let snapshot = wait_for_snapshot_needle(
        &daemon.registry,
        &new_agent_id,
        &submitted_pointer,
        Duration::from_millis(buffer_ms + 1200),
    )
    .await;
    SlowReadinessResult {
        snapshot,
        measured_readiness_window,
    }
}

#[cfg(unix)]
async fn wait_for_replacement_agent(
    registry: &AgentPtyRegistry,
    pane_id: &str,
    old_agent_id: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(record) = registry.agent_records().into_iter().find(|record| {
            record.pane_id_env.as_deref() == Some(pane_id) && record.id != old_agent_id
        }) {
            return record.id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "delegate never replaced agent {old_agent_id:?} for pane {pane_id:?}; records = {:?}",
            registry.agent_records()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
fn register_orchestration(state: &mut AppState, cwd: &str) {
    let orchestration = OrchestrationIdentity::NameCwd {
        name: "test-orchestration".to_string(),
        cwd: cwd.to_string(),
    };
    state
        .pane_role_map
        .insert(ORCH_PANE.to_string(), "orchestrator".to_string());
    state
        .pane_role_map
        .insert(WORKER_PANE.to_string(), WORKER_ROLE.to_string());
    state.orchestrator_pane_ids.insert(ORCH_PANE.to_string());
    state
        .pane_orchestration_map
        .insert(ORCH_PANE.to_string(), orchestration.clone());
    state
        .pane_orchestration_map
        .insert(WORKER_PANE.to_string(), orchestration);
    state
        .pane_cwd_map
        .insert(WORKER_PANE.to_string(), cwd.to_string());
}

#[cfg(unix)]
fn session_start_event(
    agent_type: AgentType,
    pane_id: &str,
    agent_id: &str,
    wrapper_fork: bool,
) -> AgentEvent {
    let mut metadata = std::collections::HashMap::new();
    if wrapper_fork {
        metadata.insert(
            SESSION_START_ORIGIN_METADATA_KEY.to_string(),
            WRAPPER_FORK_SESSION_START_ORIGIN.to_string(),
        );
    }
    AgentEvent {
        session_id: format!("session-{agent_id}"),
        agent_type,
        event_type: EventType::SessionStart,
        tool_name: None,
        tool_detail: None,
        cwd: None,
        timestamp: chrono::Utc::now(),
        user_prompt: None,
        metadata,
        pane_id: Some(pane_id.to_string()),
        agent_id: Some(agent_id.to_string()),
        agent_version: None,
        schema_version: None,
        live_target: None,
    }
}

/// Scenario: Register a worker pane (a `cat` stub) and an orchestrator
/// pane in the same orchestration directly in `AppState`, exactly as a
/// real orchestration tab would at StartAgent time, then call the daemon's
/// real `handle_delegate` for a `coder` task. Assert the worker pane's PTY
/// received the single-line file pointer and NOT the multi-line
/// `## When done` footer, and that the generated
/// `.dot-agent-deck/worker-task-coder.md` carries the footer plus the task
/// body. This is the wiring guard for #187: the footer lives in the file,
/// the injected pane prompt stays one line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delegate_injects_single_line_pointer_and_keeps_footer_in_task_file() {
    common::init_test_env();

    let cwd = common::race_safe_tempdir();
    let cwd_str = cwd
        .path()
        .to_str()
        .expect("tempdir path is UTF-8")
        .to_string();

    let registry = Arc::new(AgentPtyRegistry::new());

    // Worker pane backed by `cat`: the PTY echoes whatever the daemon
    // injects, so the registry snapshot reflects the delivered bytes.
    let worker_agent_id = registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(cwd_str.as_str()),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn worker stub");

    let (event_tx, _event_rx) = broadcast::channel::<BroadcastMsg>(64);

    // Populate the maps `handle_delegate` reads, mirroring what the
    // StartAgent path records for a live orchestration tab: an
    // orchestrator pane (the only valid delegate source) and a worker
    // pane in the SAME orchestration.
    let orchestration = OrchestrationIdentity::NameCwd {
        name: "test-orchestration".to_string(),
        cwd: cwd_str.clone(),
    };
    let mut state = AppState::default();
    state
        .pane_role_map
        .insert(ORCH_PANE.to_string(), "orchestrator".to_string());
    state
        .pane_role_map
        .insert(WORKER_PANE.to_string(), WORKER_ROLE.to_string());
    state.orchestrator_pane_ids.insert(ORCH_PANE.to_string());
    state
        .pane_orchestration_map
        .insert(ORCH_PANE.to_string(), orchestration.clone());
    state
        .pane_orchestration_map
        .insert(WORKER_PANE.to_string(), orchestration.clone());
    state
        .pane_cwd_map
        .insert(WORKER_PANE.to_string(), cwd_str.clone());

    let task = "List the files in the current directory.";
    let signal = DelegateSignal {
        pane_id: ORCH_PANE.to_string(),
        task: task.to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
    };

    // `handle_delegate` fans the dispatch out onto a `tokio::spawn`d task
    // and returns immediately; we poll its observable effects below.
    state.handle_delegate(signal, &registry, &event_tx).await;

    // 1) The injected pane prompt must be the single-line file pointer.
    let snap =
        wait_for_snapshot_needle(&registry, &worker_agent_id, POINTER, Duration::from_secs(5))
            .await;
    let snap_str = String::from_utf8_lossy(&snap);
    assert!(
        snap.windows(POINTER.len()).any(|w| w == POINTER),
        "worker pane never received the single-line file pointer; snapshot = {snap_str:?}"
    );

    // 2) The footer must NOT have been injected into the pane. Pre-#187
    //    the prompt carried the multi-line `## When done` block, which is
    //    exactly what forced the bracketed-paste path. `## When done` is
    //    plain ASCII, so PTY echo would surface it verbatim if it were
    //    present — its absence is the fix.
    assert!(
        !snap
            .windows(b"## When done".len())
            .any(|w| w == b"## When done"),
        "worker pane prompt still contains the `## When done` footer (#187 regression); \
         the footer belongs in the task file, not the injected prompt. snapshot = {snap_str:?}"
    );

    // 3) The footer (and the task body) must live in the worker task file.
    let task_file = cwd
        .path()
        .join(".dot-agent-deck")
        .join("worker-task-coder.md");
    let mut file_body = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = std::fs::read_to_string(&task_file) {
            file_body = s;
            if file_body.contains("## When done") {
                break;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        file_body.contains("## When done") && file_body.contains("dot-agent-deck work-done --task"),
        "worker task file must carry the work-done footer; got: {file_body:?}"
    );
    assert!(
        file_body.contains(task),
        "worker task file must contain the delegated task body; got: {file_body:?}"
    );

    registry.shutdown_all();
}

/// Scenario: Delegate with `clear = true` to a wrapped Codex stand-in whose wrapper surfaces a fork-time `SessionStart` before the child is genuinely ready. The prompt must remain absent after that card-surfacing event and appear only after a native Codex `SessionStart` for the replacement agent arrives.
#[spec("orchestration/delegate/007")]
#[test]
#[cfg(unix)]
fn delegate_007_wrapper_fork_start_does_not_release_native_hook_agent() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let _env = EnvGuard::set(&[
        (
            DELEGATE_READINESS_BUFFER_ENV,
            &DELEGATE_READINESS_BUFFER_MS.to_string(),
        ),
        (SESSION_START_WAIT_ENV, "5000"),
        (WORKER_RESPONSE_TIMEOUT_ENV, "0"),
    ]);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build wrapper readiness runtime")
        .block_on(delegate_007_wrapper_fork_start_does_not_release_native_hook_agent_inner());
}

#[cfg(unix)]
async fn delegate_007_wrapper_fork_start_does_not_release_native_hook_agent_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    let bin_dir = cwd.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create synthetic Codex bin dir");
    write_executable(&bin_dir.join("codex"), "#!/bin/sh\nexec cat\n");
    std::fs::write(
        cwd.path().join(".dot-agent-deck.toml"),
        "[[orchestrations]]\nname = \"test-orchestration\"\n\n[[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"true\"\nstart = true\n\n[[orchestrations.roles]]\nname = \"coder\"\ncommand = \"codex\"\nclear = true\n",
    )
    .expect("write wrapped worker orchestration config");
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let old_agent_id = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("codex"),
            cwd: Some(&cwd_str),
            env: vec![
                (DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string()),
                (
                    "DOT_AGENT_DECK_SOCKET".to_string(),
                    daemon.hook_path.display().to_string(),
                ),
                ("PATH".to_string(), path_with_built_deck(&bin_dir)),
            ],
            ..SpawnOptions::default()
        })
        .expect("spawn initial wrapped Codex stand-in");
    {
        let mut state = daemon.state.write().await;
        register_orchestration(&mut state, &cwd_str);
    }

    let signal = DelegateSignal {
        pane_id: ORCH_PANE.to_string(),
        task: "List the files in the current directory.".to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
    };
    daemon
        .state
        .read()
        .await
        .handle_delegate(signal, &daemon.registry, &daemon.event_tx)
        .await;
    let new_agent_id =
        wait_for_replacement_agent(&daemon.registry, WORKER_PANE, &old_agent_id).await;

    let before_native = wait_for_snapshot_needle(
        &daemon.registry,
        &new_agent_id,
        POINTER,
        Duration::from_secs(2),
    )
    .await;
    assert!(
        !before_native.windows(POINTER.len()).any(|w| w == POINTER),
        "wrapper fork-time SessionStart released the readiness gate before native Codex was ready; prompt reached replacement PTY early: {:?}",
        String::from_utf8_lossy(&before_native)
    );

    let native = session_start_event(AgentType::Codex, WORKER_PANE, &new_agent_id, false);
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&native).expect("serialize native Codex SessionStart"),
    )
    .expect("write native Codex SessionStart");
    let after_native = wait_for_snapshot_needle(
        &daemon.registry,
        &new_agent_id,
        POINTER,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        after_native.windows(POINTER.len()).any(|w| w == POINTER),
        "prompt was not delivered after native Codex SessionStart; snapshot = {:?}",
        String::from_utf8_lossy(&after_native)
    );
}

/// Scenario: Delegate with `clear = true` to a hookless wrapper-like stand-in and emit its marked fork-time `SessionStart`, the only readiness event it will produce. The replacement PTY must receive the prompt promptly instead of waiting for the timeout fallback.
#[spec("orchestration/delegate/008")]
#[test]
#[cfg(unix)]
fn delegate_008_hookless_wrapper_fork_start_still_releases_prompt() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let _env = EnvGuard::set(&[
        (
            DELEGATE_READINESS_BUFFER_ENV,
            &DELEGATE_READINESS_BUFFER_MS.to_string(),
        ),
        (SESSION_START_WAIT_ENV, "5000"),
        (WORKER_RESPONSE_TIMEOUT_ENV, "0"),
    ]);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build hookless wrapper runtime")
        .block_on(delegate_008_hookless_wrapper_fork_start_still_releases_prompt_inner());
}

#[cfg(unix)]
async fn delegate_008_hookless_wrapper_fork_start_still_releases_prompt_inner() {
    common::init_test_env();
    let cwd = common::race_safe_tempdir();
    std::fs::write(
        cwd.path().join(".dot-agent-deck.toml"),
        "[[orchestrations]]\nname = \"test-orchestration\"\n\n[[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"true\"\nstart = true\n\n[[orchestrations.roles]]\nname = \"coder\"\ncommand = \"cat\"\nclear = true\n",
    )
    .expect("write hookless wrapper-like orchestration config");
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let registry = Arc::new(AgentPtyRegistry::new());
    let old_agent_id = registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn hookless wrapper-like stand-in");
    let (event_tx, _rx) = broadcast::channel::<BroadcastMsg>(64);
    let mut state = AppState::default();
    register_orchestration(&mut state, &cwd_str);
    let signal = DelegateSignal {
        pane_id: ORCH_PANE.to_string(),
        task: "List the files in the current directory.".to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
    };
    state.handle_delegate(signal, &registry, &event_tx).await;
    let new_agent_id = wait_for_replacement_agent(&registry, WORKER_PANE, &old_agent_id).await;

    let released_at = Instant::now();
    event_tx
        .send(BroadcastMsg::Event(session_start_event(
            AgentType::None,
            WORKER_PANE,
            &new_agent_id,
            true,
        )))
        .expect("dispatch task subscribes before respawn");
    let snapshot =
        wait_for_snapshot_needle(&registry, &new_agent_id, POINTER, Duration::from_secs(2)).await;
    assert!(
        snapshot.windows(POINTER.len()).any(|w| w == POINTER),
        "hookless wrapper's sole fork-time SessionStart must release prompt delivery promptly; snapshot = {:?}",
        String::from_utf8_lossy(&snapshot)
    );
    assert!(
        released_at.elapsed() < Duration::from_secs(2),
        "the 1000 ms delegate readiness buffer consumed the full two-second prompt-release budget"
    );
    registry.shutdown_all();
}

/// Scenario: Delegate with `clear = true`, emit the replacement worker's matching `SessionStart`, and force a 1000 ms readiness buffer. The task pointer must remain absent early in that interval and appear after the buffer elapses.
#[spec("orchestration/delegate/010")]
#[test]
#[cfg(unix)]
fn delegate_010_observed_session_start_waits_for_readiness_buffer() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let _env = EnvGuard::set(&[
        (
            DELEGATE_READINESS_BUFFER_ENV,
            &DELEGATE_READINESS_BUFFER_MS.to_string(),
        ),
        (SESSION_START_WAIT_ENV, "2000"),
        (WORKER_RESPONSE_TIMEOUT_ENV, "0"),
    ]);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build observed readiness runtime")
        .block_on(delegate_010_observed_session_start_waits_for_readiness_buffer_inner());
}

#[cfg(unix)]
async fn delegate_010_observed_session_start_waits_for_readiness_buffer_inner() {
    let daemon = common::spawn_inprocess_daemon().await;
    let cwd = common::race_safe_tempdir();
    std::fs::write(
        cwd.path().join(".dot-agent-deck.toml"),
        clear_true_config("cat"),
    )
    .expect("write observed-readiness orchestration config");
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let old_agent_id = daemon
        .registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn initial observed-readiness worker");
    {
        let mut state = daemon.state.write().await;
        register_orchestration(&mut state, &cwd_str);
    }
    let signal = DelegateSignal {
        pane_id: ORCH_PANE.to_string(),
        task: "List the files in the current directory.".to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
    };
    daemon
        .state
        .read()
        .await
        .handle_delegate(signal, &daemon.registry, &daemon.event_tx)
        .await;
    let new_agent_id =
        wait_for_replacement_agent(&daemon.registry, WORKER_PANE, &old_agent_id).await;

    let session_start_at = Instant::now();
    let event = session_start_event(AgentType::None, WORKER_PANE, &new_agent_id, false);
    common::write_hook_line(
        &daemon.hook_path,
        &serde_json::to_string(&event).expect("serialize matching SessionStart"),
    )
    .expect("write matching SessionStart");
    tokio::time::sleep(Duration::from_millis(350)).await;
    let early = daemon.registry.snapshot(&new_agent_id).unwrap_or_default();
    assert!(
        !snapshot_contains(&early, POINTER),
        "matching SessionStart released delegate delivery before the configured 1000 ms readiness buffer elapsed; elapsed = {:?}, snapshot = {:?}",
        session_start_at.elapsed(),
        String::from_utf8_lossy(&early)
    );

    let delivered = wait_for_snapshot_needle(
        &daemon.registry,
        &new_agent_id,
        POINTER,
        Duration::from_secs(2),
    )
    .await;
    assert!(
        snapshot_contains(&delivered, POINTER),
        "delegate pointer was not delivered after the observed-branch readiness buffer elapsed; snapshot = {:?}",
        String::from_utf8_lossy(&delivered)
    );
}

/// Scenario: Delegate with `clear = true` to a worker that never emits `SessionStart`, advance a paused Tokio clock across the fallback timeout, and force a 1000 ms readiness buffer. Delivery must remain absent just short of the buffer and arrive after advancing one millisecond beyond it to tolerate Tokio's deadline rounding.
#[spec("orchestration/delegate/011")]
#[test]
#[cfg(unix)]
fn delegate_011_timeout_fallback_also_waits_for_readiness_buffer() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let _env = EnvGuard::set(&[
        (
            DELEGATE_READINESS_BUFFER_ENV,
            &DELEGATE_READINESS_BUFFER_MS.to_string(),
        ),
        (SESSION_START_WAIT_ENV, "30000"),
        (WORKER_RESPONSE_TIMEOUT_ENV, "0"),
    ]);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build timeout-fallback readiness runtime")
        .block_on(delegate_011_timeout_fallback_also_waits_for_readiness_buffer_inner());
}

#[cfg(unix)]
async fn delegate_011_timeout_fallback_also_waits_for_readiness_buffer_inner() {
    common::init_test_env();
    let cwd = common::race_safe_tempdir();
    std::fs::write(
        cwd.path().join(".dot-agent-deck.toml"),
        clear_true_config("cat"),
    )
    .expect("write timeout-fallback orchestration config");
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let registry = Arc::new(AgentPtyRegistry::new());
    let old_agent_id = registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn initial timeout-fallback worker");
    let (event_tx, _rx) = broadcast::channel::<BroadcastMsg>(64);
    let mut state = AppState::default();
    register_orchestration(&mut state, &cwd_str);
    let signal = DelegateSignal {
        pane_id: ORCH_PANE.to_string(),
        task: "List the files in the current directory.".to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
    };
    state.handle_delegate(signal, &registry, &event_tx).await;
    let new_agent_id = wait_for_replacement_agent(&registry, WORKER_PANE, &old_agent_id).await;

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::task::yield_now().await;
    std::thread::sleep(Duration::from_millis(100));
    let after_timeout = registry.snapshot(&new_agent_id).unwrap_or_default();
    assert!(
        !snapshot_contains(&after_timeout, POINTER),
        "timeout fallback wrote the delegate pointer immediately after its SessionStart wait instead of honoring the additional 1000 ms readiness buffer; snapshot = {:?}",
        String::from_utf8_lossy(&after_timeout)
    );

    tokio::time::advance(Duration::from_millis(DELEGATE_READINESS_BUFFER_MS - 2)).await;
    tokio::task::yield_now().await;
    std::thread::sleep(Duration::from_millis(100));
    let just_before_buffer = registry.snapshot(&new_agent_id).unwrap_or_default();
    assert!(
        !snapshot_contains(&just_before_buffer, POINTER),
        "timeout fallback released delegate delivery just short of the configured 1000 ms readiness buffer; snapshot = {:?}",
        String::from_utf8_lossy(&just_before_buffer)
    );

    tokio::time::advance(Duration::from_millis(3)).await;
    tokio::task::yield_now().await;
    std::thread::sleep(Duration::from_millis(100));
    let delivered = registry.snapshot(&new_agent_id).unwrap_or_default();
    assert!(
        snapshot_contains(&delivered, POINTER),
        "delegate pointer was not delivered after the timeout-fallback readiness buffer elapsed; snapshot = {:?}",
        String::from_utf8_lossy(&delivered)
    );
    registry.shutdown_all();
}

/// Scenario: Toggle only the delegate readiness buffer around a slow raw-mode worker that emits `SessionStart` 650 ms before accepting input. A zero buffer must lose the pointer, while 1000 ms must deliver the pointer and its submit CR after the stub becomes ready.
#[spec("orchestration/delegate/012")]
#[test]
#[cfg(unix)]
fn delegate_012_slow_agent_toggle_proves_delivery_and_submission() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let env = EnvGuard::set(&[
        (DELEGATE_READINESS_BUFFER_ENV, "0"),
        (SESSION_START_WAIT_ENV, "2000"),
        (WORKER_RESPONSE_TIMEOUT_ENV, "0"),
    ]);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build slow-readiness toggle runtime")
        .block_on(async {
            let zero = run_slow_readiness_delegate(0).await;
            assert!(
                !snapshot_contains(&zero.snapshot, POINTER),
                "the zero-buffer control unexpectedly delivered the pointer outside the stub's discard window; snapshot = {:?}",
                String::from_utf8_lossy(&zero.snapshot)
            );

            env.repoint(
                DELEGATE_READINESS_BUFFER_ENV,
                &DELEGATE_READINESS_BUFFER_MS.to_string(),
            );
            let buffered = run_slow_readiness_delegate(DELEGATE_READINESS_BUFFER_MS).await;
            eprintln!(
                "delegate slow-readiness window measured from SessionStart: zero arm {:?}, buffered arm {:?}; configured buffer: {} ms",
                zero.measured_readiness_window,
                buffered.measured_readiness_window,
                DELEGATE_READINESS_BUFFER_MS
            );
            assert!(
                buffered.measured_readiness_window >= Duration::from_millis(500)
                    && buffered.measured_readiness_window <= Duration::from_millis(900),
                "the synthetic readiness window drifted outside its intended measurement band: {:?}",
                buffered.measured_readiness_window
            );
            let mut submitted_pointer = POINTER.to_vec();
            submitted_pointer.push(b'\r');
            assert!(
                snapshot_contains(&buffered.snapshot, POINTER),
                "the 1000 ms readiness buffer did not deliver the delegate pointer after the measured {:?} input-readiness window; snapshot = {:?}",
                buffered.measured_readiness_window,
                String::from_utf8_lossy(&buffered.snapshot)
            );
            assert!(
                snapshot_contains(&buffered.snapshot, &submitted_pointer),
                "the delegate pointer was not followed by its submit CR after the readiness buffer; snapshot = {:?}",
                String::from_utf8_lossy(&buffered.snapshot)
            );
        });
}

/// Scenario: Delegate to a worker that receives the pointer but emits no agent event before the short response window expires. The orchestrator pane must gain an LF-terminated visible notice naming the delegate role and missing event, without submitting that notice as an LLM prompt.
#[spec("orchestration/delegate/013")]
#[test]
#[cfg(unix)]
fn delegate_013_silent_worker_surfaces_notice_in_orchestrator_pane() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let _env = EnvGuard::set(&[
        (DELEGATE_READINESS_BUFFER_ENV, "0"),
        (SESSION_START_WAIT_ENV, "2000"),
        (WORKER_RESPONSE_TIMEOUT_ENV, "600"),
    ]);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build delegate failure-visibility runtime")
        .block_on(delegate_013_silent_worker_surfaces_notice_in_orchestrator_pane_inner());
}

#[cfg(unix)]
async fn delegate_013_silent_worker_surfaces_notice_in_orchestrator_pane_inner() {
    common::init_test_env();
    let cwd = common::race_safe_tempdir();
    let observer = cwd.path().join("orchestrator-observer");
    write_executable(
        &observer,
        "#!/bin/sh\nstty raw -echo\nprintf ORCHESTRATOR-NOTICE-READY\nexec cat -u\n",
    );
    let cwd_str = cwd.path().to_string_lossy().into_owned();
    let registry = Arc::new(AgentPtyRegistry::new());
    let orchestrator_agent_id = registry
        .spawn_agent(SpawnOptions {
            command: Some(&observer.to_string_lossy()),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), ORCH_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn raw orchestrator notice observer");
    let observer_ready = wait_for_snapshot_needle(
        &registry,
        &orchestrator_agent_id,
        b"ORCHESTRATOR-NOTICE-READY",
        Duration::from_secs(2),
    )
    .await;
    assert!(
        snapshot_contains(&observer_ready, b"ORCHESTRATOR-NOTICE-READY"),
        "orchestrator notice observer never entered raw no-echo mode; snapshot = {:?}",
        String::from_utf8_lossy(&observer_ready)
    );
    let worker_agent_id = registry
        .spawn_agent(SpawnOptions {
            command: Some("cat"),
            cwd: Some(&cwd_str),
            env: vec![(DOT_AGENT_DECK_PANE_ID.to_string(), WORKER_PANE.to_string())],
            ..SpawnOptions::default()
        })
        .expect("spawn silent delegated worker");
    let (event_tx, _rx) = broadcast::channel::<BroadcastMsg>(64);
    let mut state = AppState::default();
    register_orchestration(&mut state, &cwd_str);
    state
        .pane_cwd_map
        .insert(ORCH_PANE.to_string(), cwd_str.clone());
    let signal = DelegateSignal {
        pane_id: ORCH_PANE.to_string(),
        task: "List the files in the current directory.".to_string(),
        to: vec![WORKER_ROLE.to_string()],
        timestamp: chrono::Utc::now(),
    };
    state.handle_delegate(signal, &registry, &event_tx).await;

    let delivered =
        wait_for_snapshot_needle(&registry, &worker_agent_id, POINTER, Duration::from_secs(2))
            .await;
    assert!(
        snapshot_contains(&delivered, POINTER),
        "silent-worker visibility control failed: the worker never received the delegate pointer; snapshot = {:?}",
        String::from_utf8_lossy(&delivered)
    );
    let notice =
        wait_for_silence_notice(&registry, &orchestrator_agent_id, Duration::from_secs(3)).await;
    assert!(
        snapshot_has_silence_notice(&notice),
        "a worker that received its delegate pointer and emitted no agent event produced no LF-terminated visible notice naming role 'coder' in the orchestrator pane; snapshot = {:?}",
        String::from_utf8_lossy(&notice)
    );
    registry.shutdown_all();
}
