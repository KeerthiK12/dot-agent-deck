//! Issue #424: the shared policy for **provisional** spawn-time prompt delivery.
//!
//! A write into a PTY is not a delivery. `SendResult::Applied` / `Queued` mean
//! only that the bytes reached the pane's writer (`src/pane.rs`); whether the
//! agent's TUI was in submit-CR-aware mode when the CR landed is unknowable from
//! the write's own return value. Against a Claude Code pane that is still
//! starting MCP servers the CR is swallowed and the pane comes up healthy, Idle,
//! and prompt-less — indistinguishable, to every pre-#424 delivery path, from a
//! prompt that was delivered and acted upon.
//!
//! The only honest evidence that a prompt was actually submitted is the agent
//! saying so: every supported agent maps "a user prompt was submitted" onto an
//! [`EventType::Thinking`](crate::event::EventType::Thinking) event carrying
//! `user_prompt` (Claude/Codex `UserPromptSubmit`, OpenCode `session.prompt`).
//! So a write is **provisional** until such an event comes back for the same
//! pane carrying the same prompt text; until then the delivery keeps its prompt,
//! its identity and its retry armed, and re-submits under a bounded backoff.
//!
//! Three independent delivery implementations consume this module, which is why
//! the policy lives here rather than in any one of them:
//!
//! 1. [`crate::ui::deliver_orchestrator_prompt`] — an orchestration tab's
//!    spawn-time role prompt (TUI-owned).
//! 2. `crate::ui::process_pending_seed_prompts` — a `[[modes]]` `seed_prompt`
//!    (TUI-owned, PRD #127).
//! 3. [`crate::spawn::spawn`]'s delivery — `dispatch`, the scheduler and
//!    issue-dispatch (daemon-owned).
//!
//! Deliberately NOT merged into one implementation: that seam is already losing
//! prompts and a rewrite of it is a much larger, riskier change than the fix the
//! issue needs. See the coder's report on #424 for the follow-up proposal.

use chrono::{DateTime, TimeDelta, Utc};

/// The byte length `crate::hook` truncates a reported `user_prompt` to before it
/// ever reaches [`crate::event::AgentEvent`].
///
/// **This is the silent-failure trap of the whole confirmation design.** A seed
/// prompt longer than this is reported back in truncated form, so comparing a
/// locally-known prompt to a reported one with plain `==` matches for short
/// prompts, never matches for long ones, and fails by *retrying until the
/// deadline abandons the prompt* — i.e. it looks exactly like the bug being
/// fixed. Always compare through [`prompt_submission_matches`], never directly.
pub const USER_PROMPT_MAX_LEN: usize = 200;

/// Hard cap on how long an automatic prompt (an orchestrator role prompt, a mode
/// seed, or a spawn-time dispatch prompt) is retried before it is abandoned with
/// visible feedback. Shared by all three delivery paths so "how long do we keep
/// trying" cannot drift between them.
pub const AUTOMATIC_PROMPT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Clock-skew tolerance applied when deciding whether a reported submission is
/// NEWER than our own write.
///
/// The write is timestamped by whoever performed it (the TUI process) while the
/// event is timestamped by the agent's hook (the daemon host), so the two clocks
/// are not the same clock. The tolerance only has to be larger than realistic
/// NTP skew and far smaller than the age of any stale prompt history it exists
/// to reject — a session that submitted this exact text before we ever wrote it.
const CONFIRMATION_CLOCK_SKEW_SECS: i64 = 5;

/// Truncate `s` to at most `max` BYTES, appending `…` when anything was cut.
///
/// Slices on a char boundary at or below `max`: the naive `&s[..max]` panics
/// whenever the cut lands inside a multi-byte character, which in the hook
/// binary means a prompt containing any non-ASCII text past the limit kills the
/// hook process and emits no event at all.
pub fn truncate_on_char_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Whether a hook-reported `user_prompt` confirms submission of `expected` —
/// the prompt text we wrote into the pane.
///
/// Accepts either the full text or the [`USER_PROMPT_MAX_LEN`]-truncated form
/// the hook layer produces, because which one arrives depends only on the
/// prompt's length. Both sides are trimmed: `encode_pane_payload` strips
/// trailing whitespace before the bytes ever reach the PTY, so the agent
/// submits — and reports — the trimmed text.
pub fn prompt_submission_matches(expected: &str, reported: &str) -> bool {
    let expected = expected.trim();
    let reported = reported.trim();
    reported == expected || reported == truncate_on_char_boundary(expected, USER_PROMPT_MAX_LEN)
}

/// The earliest event timestamp that may count as confirmation of a write
/// performed at `written_at`. Anything older is pre-existing prompt history for
/// the pane, not evidence about this delivery.
pub fn confirmation_floor(written_at: DateTime<Utc>) -> DateTime<Utc> {
    written_at - TimeDelta::seconds(CONFIRMATION_CLOCK_SKEW_SECS)
}

/// Backoff before re-submitting a written-but-UNCONFIRMED prompt: 0.5 s, 1 s,
/// 2 s, 4 s, 8 s, then capped at 15 s. `attempts` is the number of submissions
/// made so far (≥ 1).
///
/// Deliberately NOT `crate::ui::send_retry_delay`'s 2 s-capped schedule, which
/// exists for a target that is *refusing* delivery and may become live at any
/// moment, so it keeps catch-up latency small at the cost of frequent retries.
/// An unconfirmed write is the opposite case: the agent accepted the bytes and
/// is booting, the wait can legitimately run to tens of seconds (5-6 MCP servers
/// on the reported failure), and every retry is a *second copy of the prompt*
/// typed into whatever the pane is showing. Escalating to a 15 s cap keeps the
/// whole [`AUTOMATIC_PROMPT_DEADLINE`] window covered in single-digit attempts
/// instead of ~30.
pub fn unconfirmed_retry_delay(attempts: u32) -> std::time::Duration {
    const BASE_MS: u64 = 500;
    const CAP: std::time::Duration = std::time::Duration::from_secs(15);
    let shift = attempts.saturating_sub(1).min(8);
    std::time::Duration::from_millis(BASE_MS.saturating_mul(1u64 << shift)).min(CAP)
}

/// Mint a GLOBALLY-UNIQUE logical delivery id for an automatic prompt.
///
/// Combines a per-PROCESS nonce (two processes — and a pid reused across
/// restarts — never collide) with a global monotonic counter (two ids within one
/// process never collide), keyed by pane for log readability. The per-process
/// nonce matters because the daemon's dedup ledger outlives any one TUI: a
/// restarted TUI reusing a plain `seed-<pane>-<n>` could otherwise have a
/// genuinely-new prompt silently suppressed as a replay (PRD #20 finding #3).
pub fn mint_delivery_id(pane_id: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nonce = *NONCE.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::process::id().hash(&mut h);
        // Nanos since the epoch disambiguate a pid reused across restarts.
        if let Ok(dur) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            dur.as_nanos().hash(&mut h);
        }
        h.finish()
    });
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("send-{nonce:016x}-{pane_id}-{seq}")
}

/// The idempotency key put ON THE WIRE for attempt `attempt` (1-based) of the
/// logical delivery `delivery_id`.
///
/// Issue #424: the daemon's delivery ledger CACHES an `Applied`/`Queued`/
/// `Ambiguous` outcome per `delivery_id` and replays it for any later request
/// carrying that id — *without writing to the PTY again*
/// (`AgentPtyRegistry::admit_delivery`). That is exactly right for its original
/// purpose (a retry after a lost response must not double-submit) and exactly
/// wrong for an unconfirmed prompt, whose retry exists precisely to produce a
/// second physical submission: it would replay `Applied` forever and never touch
/// the pane again.
///
/// So the logical delivery keeps ONE stable identity — it is what the local
/// prompt/confirmation/retry state is keyed on, and what the logs correlate on —
/// while each ATTEMPT goes on the wire under its own derived id. The ledger
/// therefore sees a genuinely new delivery per attempt and admits it
/// (`Proceed`), while concurrent duplicates of the *same* attempt still collapse
/// onto one submission through the ledger's single-flight guard, and the
/// fingerprint check still refuses a reused id carrying a different payload.
///
/// The wire field is an opaque string with no charset or length validation
/// (`daemon_protocol::WriteAndSubmitExtras`), so this needs no protocol change.
pub fn attempt_delivery_id(delivery_id: &str, attempt: u32) -> String {
    format!("{delivery_id}#a{attempt}")
}

/// Info-level record that a prompt's bytes were written into `pane_id`.
///
/// Issue #424 §4: before this, the entire delivery path was silent, so a lost
/// seed left no trace anywhere and the failure could only be reconstructed from
/// process tables and file mtimes. "Unconditional" here means unconditional
/// *within* the delivery path — it is not gated behind extra verbosity — not
/// that it materializes a subscriber: `init_logging_from_env` installs one only
/// when `DOT_AGENT_DECK_LOG` is set, so with no log configured these calls are
/// no-ops rather than terminal noise.
pub fn log_prompt_written(path: &str, pane_id: &str, delivery_id: &str, attempt: u32) {
    tracing::info!(
        path,
        pane_id,
        delivery_id,
        attempt,
        "prompt written to pane; provisional until UserPromptSubmit confirms it"
    );
}

/// Info-level record that a written prompt is still UNCONFIRMED and is being
/// re-submitted.
pub fn log_prompt_unconfirmed(path: &str, pane_id: &str, delivery_id: &str, attempt: u32) {
    tracing::info!(
        path,
        pane_id,
        delivery_id,
        attempt,
        "prompt delivery unconfirmed; re-submitting"
    );
}

/// Info-level record that the agent reported submitting the prompt we wrote —
/// the only evidence that turns a provisional write into a delivery.
pub fn log_prompt_confirmed(path: &str, pane_id: &str, delivery_id: &str, attempt: u32) {
    tracing::info!(
        path,
        pane_id,
        delivery_id,
        attempt,
        "prompt delivery confirmed by the agent's submitted prompt"
    );
}

/// Info-level record that this delivery has no confirmation channel at all, so
/// the write is final and no retry will be attempted. Emitted for a target that
/// never signalled readiness (a bare shell, `cat`, a hook-less launcher): such
/// an agent reports no submitted prompts either, so retrying would only type the
/// prompt into it repeatedly and then abandon it.
pub fn log_prompt_unconfirmable(path: &str, pane_id: &str, delivery_id: &str, reason: &str) {
    tracing::info!(
        path,
        pane_id,
        delivery_id,
        reason,
        "prompt written to pane; delivery cannot be confirmed by this agent, not retrying"
    );
}

/// Warn-level record that a prompt was never confirmed within
/// [`AUTOMATIC_PROMPT_DEADLINE`] and has been abandoned.
pub fn log_prompt_abandoned(path: &str, pane_id: &str, delivery_id: &str, attempts: u32) {
    tracing::warn!(
        path,
        pane_id,
        delivery_id,
        attempts,
        "prompt delivery unconfirmed at the deadline; abandoning"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_prompt_matches_verbatim() {
        assert!(prompt_submission_matches("hello there", "hello there"));
        assert!(!prompt_submission_matches("hello there", "hello world"));
    }

    #[test]
    fn trailing_whitespace_is_not_a_mismatch() {
        // `encode_pane_payload` strips trailing whitespace before the bytes
        // reach the PTY, so the agent reports the trimmed form.
        assert!(prompt_submission_matches("do the thing\n", "do the thing"));
    }

    /// The trap from the tester's finding #2: a prompt longer than the hook's
    /// truncation limit is reported back truncated, so exact equality never
    /// matches and the delivery would retry until the deadline abandoned it.
    #[test]
    fn long_prompt_matches_its_truncated_report() {
        let long = "x".repeat(USER_PROMPT_MAX_LEN + 40);
        let reported = truncate_on_char_boundary(&long, USER_PROMPT_MAX_LEN);
        assert_ne!(reported, long, "the fixture must actually be truncated");
        assert!(prompt_submission_matches(&long, &reported));
        assert!(!prompt_submission_matches(&long, &"y".repeat(50)));
    }

    /// Truncation must never split a multi-byte character — the naive
    /// `&s[..max]` panics there, which in the hook binary means no event at all.
    #[test]
    fn truncation_never_splits_a_multibyte_char() {
        // 'é' is 2 bytes, so byte 200 lands mid-character.
        let s = format!("{}é{}", "a".repeat(199), "b".repeat(60));
        let cut = truncate_on_char_boundary(&s, USER_PROMPT_MAX_LEN);
        assert_eq!(cut, format!("{}…", "a".repeat(199)));
        assert!(prompt_submission_matches(&s, &cut));
    }

    #[test]
    fn unconfirmed_backoff_escalates_then_caps() {
        assert_eq!(
            unconfirmed_retry_delay(1),
            std::time::Duration::from_millis(500)
        );
        assert_eq!(
            unconfirmed_retry_delay(2),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            unconfirmed_retry_delay(3),
            std::time::Duration::from_secs(2)
        );
        assert_eq!(
            unconfirmed_retry_delay(5),
            std::time::Duration::from_secs(8)
        );
        assert_eq!(
            unconfirmed_retry_delay(9),
            std::time::Duration::from_secs(15)
        );
        // The whole deadline window is covered in single-digit attempts.
        let mut total = std::time::Duration::ZERO;
        let mut attempts = 0u32;
        while total < AUTOMATIC_PROMPT_DEADLINE {
            attempts += 1;
            total += unconfirmed_retry_delay(attempts);
        }
        assert!(
            attempts <= 9,
            "{attempts} submissions to cover the deadline"
        );
    }

    #[test]
    fn attempt_ids_are_distinct_per_attempt_and_share_their_logical_id() {
        let logical = mint_delivery_id("pane-7");
        let first = attempt_delivery_id(&logical, 1);
        let second = attempt_delivery_id(&logical, 2);
        assert_ne!(first, second, "each attempt must be a new ledger identity");
        assert!(first.starts_with(&logical) && second.starts_with(&logical));
    }

    #[test]
    fn minted_delivery_ids_are_unique() {
        let a = mint_delivery_id("pane-1");
        let b = mint_delivery_id("pane-1");
        assert_ne!(a, b);
    }

    #[test]
    fn confirmation_floor_precedes_the_write() {
        let now = Utc::now();
        assert!(confirmation_floor(now) < now);
    }
}
