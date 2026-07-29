# Code Review: `feat/prd-220-dispatcher-mode`

**Branch:** `feat/prd-220-dispatcher-mode`  
**Base:** `babc6b6` (after Viktor's Task 1 and 2)  
**Reviewer:** AI Code Review  
**Date:** 2026-07-29  
**Scope:** 13 files, 248+/41-

---

## Build & Checks

| Check | Result |
|-------|--------|
| `cargo build` | ✅ PASS |
| `cargo fmt --check` | ✅ PASS |
| `cargo clippy -- -D warnings` | ✅ PASS |
| `cargo test --lib` | ✅ PASS (841 tests) |

---

## Issues Found

### CRITICAL

*None.*

---

### HIGH

*None.*

---

### MEDIUM

#### 1. Seed prompt tells the agent the wrong worktree path

**Location:** `src/ui.rs:522` vs `src/dispatch.rs:41-45`

**Problem:**  
The dispatcher mode's seed prompt embeds incorrect information about where worktrees are created:

```rust
// src/ui.rs:522
let seed = format!(
    "{seed}\n\nworking_dir: {dir}\n\nThe repo at this path is where worktrees will be created under .worktrees/.",
    seed = DISPATCHER_SEED_PROMPT,
    dir = working_dir.display(),
);
```

However, `derive_dispatch_paths` actually creates worktrees as **sibling directories**:

```rust
// src/dispatch.rs:41-45
let worktree_dir = working_dir.parent().unwrap_or(working_dir).join(format!(
    "{}-{}",
    working_dir.file_name().unwrap().to_string_lossy(),
    slug
));
```

This produces paths like `../<repo>-dispatch-<name>`, not `<repo>/.worktrees/dispatch-<name>`.

The developer documentation (`docs/develop/dispatcher-mode.md`) correctly states:

> Every `dispatch` call creates its work in a dedicated Git worktree at `../<repo>-dispatch-<slug>`.

This confirms the implementation moved but the seed prompt was not updated.

**Impact:**  
The dispatcher agent will report the wrong worktree location to the user when it decomposes work and calls `dispatch`. This is a functional correctness bug that directly affects user experience.

**Fix:**  
Update the seed prompt in `build_dispatcher_mode` to reflect the actual sibling-directory layout:

```rust
let seed = format!(
    "{seed}\n\nworking_dir: {dir}\n\nThe repo at this path is the main worktree. Dispatched worktrees are created as sibling directories at ../<repo>-dispatch-<name>.",
    seed = DISPATCHER_SEED_PROMPT,
    dir = working_dir.display(),
);
```

Also update the unit test `build_dispatcher_mode_produces_correct_config` (line 19808-19811) to assert the correct path pattern instead of `.worktrees/`.

---

#### 2. Regression fix in `e2e_scheduler_manager.rs` is correct but fragile

**Location:** `tests/e2e_scheduler_manager.rs:1289-1290`

**Problem:**  
The test uses a saturate-and-back-off approach to select the `schedule: issues` option:

```rust
deck.send_keys(b"\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C"); // Right ×8 → saturate
deck.send_keys(b"\x1b[D"); // Left ×1 → schedule: issues (before dispatcher)
```

This works today because `dispatcher` is the last cycler slot and `schedule: issues` is second-to-last. However, if any new cycler option is added before or after `dispatcher`, this test will silently land on the wrong option and may fail or pass with false confidence.

**Impact:**  
Test fragility — future additions to the mode cycler will break this test without obvious cause.

**Fix:**  
Consider using a targeted selection mechanism (e.g., typing the mode name if the form supports it) or explicitly asserting the final cycler label before submitting:

```rust
deck.send_keys(b"\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C"); // Right ×8 → saturate
deck.send_keys(b"\x1b[D"); // Left ×1 → schedule: issues (before dispatcher)
deck.wait_for_string("schedule: issues mode"); // Explicit assertion
```

The test already has this assertion at line 1294, which mitigates the risk. However, the comment at line 1289 should note the fragility.

---

### LOW

#### 3. `file_name().unwrap()` in `derive_dispatch_paths` can panic

**Location:** `src/dispatch.rs:43`

**Problem:**  
The code calls `unwrap()` on `working_dir.file_name()`:

```rust
let worktree_dir = working_dir.parent().unwrap_or(working_dir).join(format!(
    "{}-{}",
    working_dir.file_name().unwrap().to_string_lossy(),
    slug
));
```

`file_name()` returns `None` for root paths (`/`, `C:\`) or paths ending in `..`. While unrealistic in practice (since `working_dir` is always a repo clone), this is a crash-prone pattern.

**Impact:**  
Theoretical panic on malformed paths. Low probability in production but violates defensive coding practices.

**Fix:**  
Use `unwrap_or_else` with a fallback:

```rust
let repo_name = working_dir
    .file_name()
    .unwrap_or_else(|| std::ffi::OsStr::new("repo"))
    .to_string_lossy();
let worktree_dir = working_dir.parent().unwrap_or(working_dir).join(format!(
    "{}-{}",
    repo_name,
    slug
));
```

---

#### 4. `unwrap_or_default()` on missing task spawns agent with empty prompt

**Location:** `src/daemon.rs:934`

**Problem:**  
The daemon handler uses `unwrap_or_default()` when the task is missing:

```rust
let task = signal.task.as_deref().unwrap_or_default();
let result = dispatch::handle_dispatch(
    &ctx,
    &signal.name,
    task,
)
```

If `DispatchSignal.task` is `None`, the agent receives an empty prompt. The CLI path prevents this via `resolve_task` (which requires `--task` or `--task-file`), but a direct daemon-socket message could bypass this check.

**Impact:**  
An agent spawned with an empty prompt will likely fail or behave unpredictably. Low probability since the CLI enforces the invariant, but the daemon should be defensive.

**Fix:**  
Return an error result to the caller pane instead of spawning with an empty prompt:

```rust
let task = match signal.task.as_deref() {
    Some(t) if !t.trim().is_empty() => t,
    _ => {
        let result = DispatchResult {
            worktree_dir: PathBuf::new(),
            success: false,
            message: "dispatch: task text is required (use --task or --task-file)".to_string(),
        };
        if let Err(e) = pty_registry
            .write_to_pane_and_submit(&signal.pane_id, &result.message)
            .await
        {
            warn!(
                pane_id = %signal.pane_id,
                error = %e,
                "dispatch: failed to write error into caller pane"
            );
        }
        return;
    }
};
```

---

## Design Decisions Compliance (Viktor's 4)

| Decision | Respected? | Notes |
|----------|-----------|-------|
| Dedicated `show_dispatcher()` wrapper (CLAUDE.md #9) | ✅ Yes | `src/features.rs:106` — one wrapper per feature, grep-findable |
| Worktree isolation via sibling dirs (`../<repo>-dispatch-<slug>`) | ✅ Yes | `src/dispatch.rs:41-45` — matches developer docs |
| Safety-first `remove_worktree` (check dirty before remove, drop `--force`) | ✅ Yes | `src/issue_dispatch_run.rs:135-155` — checks `git status --porcelain`, leaves dirty worktrees in place |
| `reuse_existing_branch` param on `create_worktree` (dispatcher refuses existing branch, issue-dispatch reuses) | ✅ Yes | `src/issue_dispatch_run.rs:631-658` — dispatcher passes `false`, issue-dispatch passes `true` |

---

## Code Quality & Style

### Positive Observations

1. **Excellent documentation**: The developer doc (`docs/develop/dispatcher-mode.md`) is clear, concise, and correctly excludes itself from the published site per CLAUDE.md #11.

2. **Proper experimental flag gating**: The `show_dispatcher()` wrapper follows CLAUDE.md #9 precisely — one wrapper per feature, gates only the presentation seam (the cycler option in `src/ui.rs`), not the behavior.

3. **Safety-first worktree removal**: The `remove_worktree` change is a significant improvement — checking for uncommitted changes before removal prevents data loss. The error handling (leave in place on status check failure) is conservative and correct.

4. **Good test coverage**: The L2 PTY-attached test (`tests/e2e_dispatcher_mode.rs`) exercises the real user-visible path with a genuine Claude agent, meeting CLAUDE.md rule 4's bar for major features.

5. **Clean API simplification**: Removing the unused `_caller_pane_id` and `to_orchestration` fields from `handle_dispatch` and `DispatchSignal` reduces surface area without losing functionality.

6. **Idiomatic Rust**: The code uses standard patterns (`unwrap_or_else`, `match` on `Result`, proper error propagation) and follows the project's conventions.

---

### Minor Style Notes

1. **Comment clarity in `derive_dispatch_paths`** (`src/dispatch.rs:38-51`): The function could benefit from a brief doc comment explaining the sibling-directory layout, since it's non-obvious:

   ```rust
   /// Derive the worktree directory and branch name for a dispatch unit.
   /// Worktrees are created as sibling directories to the main repo at
   /// `../<repo>-dispatch-<name>` to avoid nesting and simplify cleanup.
   fn derive_dispatch_paths(working_dir: &Path, name: &str) -> DispatchPaths {
       // ...
   }
   ```

2. **Test naming**: The test `dispatcher_001_opens_mode_tab_with_real_agent` is clear and follows the project's `<sub-area>_<NNN>_<description>` convention (CLAUDE.md Decision 17).

---

## Regression Analysis

### Files Modified

| File | Change Type | Risk |
|------|-------------|------|
| `src/daemon.rs` | Simplified `handle_dispatch` call | Low — removed unused params |
| `src/dispatch.rs` | Worktree path layout, API simplification | Medium — path change affects agent |
| `src/event.rs` | Removed `to_orchestration` field | Low — field was unused |
| `src/features.rs` | Added `show_dispatcher()` wrapper | None — additive |
| `src/issue_dispatch_run.rs` | Safety-first removal, `reuse_existing_branch` | Medium — behavior change |
| `src/main.rs` | Removed `--to` CLI flag | Low — flag was unused |
| `src/ui.rs` | Fixed flag gate, added constants/tests | Low — presentation only |
| `tests/e2e_dispatcher_mode.rs` | New L2 test | None — additive |
| `tests/e2e_scheduler_manager.rs` | Regression fix for cycler | Low — test only |
| `tests/CATALOG.md` | New test entry | None — documentation |
| `docs/develop/dispatcher-mode.md` | New developer doc | None — documentation |
| `docs/develop/experimental-flag.md` | Updated flag table | None — documentation |
| `changelog.d/pr-232-dispatch-dispatcher-mode.md` | Changelog fragment | None — documentation |

### Potential Regressions

1. **Worktree path change**: The move from `.worktrees/dispatch-<name>` to `../<repo>-dispatch-<name>` is a breaking change for any existing dispatcher sessions. However, since dispatcher mode is gated behind the experimental flag and this is the initial implementation, no users should have persistent state affected by this.

2. **`remove_worktree` behavior change**: Dropping `--force` and adding the dirty-check means worktrees with uncommitted changes are now preserved instead of being forcibly removed. This is a safety improvement but could surprise users who expect automatic cleanup. The warning log mitigates this.

3. **`create_worktree` API change**: Adding the `reuse_existing_branch` parameter is a breaking change for any callers, but the only two callers (`dispatch.rs` and `issue_dispatch_run.rs`) are updated in this PR.

---

## Test Coverage

### Unit Tests

- **`dispatcher_mode_name_and_seed_constants`** (`src/ui.rs:19782-19792`): Validates the mode name and seed prompt contain required keywords. Good.

- **`build_dispatcher_mode_produces_correct_config`** (`src/ui.rs:19795-19816`): Validates the mode config structure and seed prompt format. **Needs update** to reflect the correct worktree path (see Issue #1).

- **`create_worktree` tests** (`src/issue_dispatch_run.rs:944-968`): Updated to pass the new `reuse_existing_branch` parameter. Tests still pass.

### Integration Tests

- **`dispatcher_001_opens_mode_tab_with_real_agent`** (`tests/e2e_dispatcher_mode.rs:31-98`): L2 PTY-attached test that exercises the full user-visible path with a real Claude agent. Meets CLAUDE.md rule 4's bar for major features. Marked `[reel]` for demo-reel eligibility.

- **`form_007_issue_dispatch_option_seeds_issue_dispatch_authoring`** (`tests/e2e_scheduler_manager.rs:1286-1309`): Updated to account for the new `dispatcher` cycler slot. Regression fix is correct.

### Test Gaps

1. **No unit test for `derive_dispatch_paths`**: The function's path derivation logic (sibling directories, slug sanitization) is not directly tested. Consider adding a test that validates the path format for various inputs.

2. **No test for `remove_worktree` dirty-check**: The new safety-first behavior (leaving dirty worktrees in place) is not tested. Consider adding a test that creates a worktree with uncommitted changes and verifies it is not removed.

---

## Documentation

### Developer Docs

- **`docs/develop/dispatcher-mode.md`**: Clear, concise, and correctly gated. Explains activation, usage, worktree isolation, cleanup, and current limitations. Good.

- **`docs/develop/experimental-flag.md`**: Updated to include the `show_dispatcher()` wrapper in the flag table. Good.

### Changelog

- **`changelog.d/pr-232-dispatch-dispatcher-mode.md`**: Correctly formatted and describes the new features. Good.

### Inline Comments

- **Seed prompt** (`src/ui.rs:496-516`): Well-documented with clear rules for the agent. Good.

- **`build_dispatcher_mode`** (`src/ui.rs:518-534`): Brief but clear. Good.

- **`derive_dispatch_paths`** (`src/dispatch.rs:38-51`): No doc comment. Could benefit from explanation of the sibling-directory layout (see Style Notes).

---

## Verdict

### **CHANGES REQUESTED**

**Required before merge:**

1. Fix the seed prompt path mismatch (Issue #1) — this is a functional correctness bug that will cause the dispatcher agent to report wrong worktree locations to the user.

**Recommended (can be follow-ups):**

2. Add a defensive fallback for `file_name().unwrap()` in `derive_dispatch_paths` (Issue #3).
3. Add validation for empty task text in the daemon handler (Issue #4).
4. Add a comment noting the fragility of the cycler saturation approach in `e2e_scheduler_manager.rs` (Issue #2).

**Optional:**

5. Add unit tests for `derive_dispatch_paths` and the dirty-check in `remove_worktree`.
6. Add a doc comment to `derive_dispatch_paths` explaining the sibling-directory layout.

---

## Summary

The PR implements the dispatcher mode feature correctly and follows the project's conventions (CLAUDE.md #9 experimental flag gating, safety-first worktree removal, proper test coverage). The code is idiomatic Rust and passes all build/test/lint checks.

The primary blocker is the seed prompt path mismatch (Issue #1), which will cause the dispatcher agent to report incorrect worktree locations. This is a straightforward fix.

The remaining issues are low-severity hardening and test improvements that can be addressed in follow-up PRs.

**Recommendation:** Fix Issue #1, then merge.
