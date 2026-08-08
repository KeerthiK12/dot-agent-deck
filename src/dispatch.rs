use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agent_pty::AgentPtyRegistry;
use crate::event::BroadcastMsg;
use crate::issue_dispatch_run::{
    RemovalPolicy, WorktreeCreation, WorktreeRegistry, create_worktree, record_worktree,
    remove_worktree, run_status,
};
use crate::scheduler::StderrNotifier;
use crate::spawn::{SpawnKind, SpawnRequest, SpawnShapeOverride, spawn};

/// PRD #220: the orchestrations a dispatch out of `dir` could start, by resolved
/// name. Empty means only a single agent is available.
///
/// Deliberately a LOCAL config read rather than a daemon round-trip: the
/// dispatched worktree is a copy of this repo, so this repo's
/// `.dot-agent-deck.toml` is the same config the spawn will branch on. Keeping it
/// local means `--list-targets` adds no hook-socket message and no protocol
/// surface at all.
///
/// Roleless `[[orchestrations]]` are filtered out because [`crate::spawn::decide_target`]
/// skips them too — listing one would offer a target that cannot be spawned.
pub fn available_orchestrations(
    config: Option<&crate::project_config::ProjectConfig>,
    dir: &Path,
) -> Vec<(String, usize)> {
    config
        .map(|cfg| {
            cfg.orchestrations
                .iter()
                .filter(|o| !o.roles.is_empty())
                .map(|o| {
                    (
                        crate::project_config::resolve_orchestration_name(&o.name, dir),
                        o.roles.len(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Human-readable `--list-targets` output, read by the dispatcher agent and
/// relayed to the user.
///
/// Schedule/authoring modes are absent by construction: a schedule creates a
/// FUTURE task, so it is not something a dispatch can start, and the dispatcher
/// option itself is not a target either. Only real spawn shapes appear.
pub fn render_available_targets(orchestrations: &[(String, usize)]) -> String {
    let mut out = String::from("Available dispatch targets:\n");
    out.push_str("  single            one agent (--single)\n");
    if orchestrations.is_empty() {
        out.push_str(
            "\nNo orchestrations are defined here, so `single` is the only target.\n\
             Dispatch with `--single`.\n",
        );
        return out;
    }
    for (name, roles) in orchestrations {
        out.push_str(&format!(
            "  orchestration     '{name}' — {roles} roles (--orchestration {name})\n"
        ));
    }
    out.push_str(
        "\nAsk the user which they want before dispatching, then pass the matching flag.\n",
    );
    out
}

fn sanitize_name(name: &str) -> String {
    let slug_chars: String = name
        .replace("..", "_")
        .replace('\0', "")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if slug_chars.is_empty() || slug_chars.chars().all(|c| c == '-') {
        "dispatch".to_string()
    } else {
        slug_chars.trim_matches('-').to_string()
    }
}

struct DispatchPaths {
    worktree_dir: PathBuf,
    branch: String,
}

/// Derive the sibling worktree dir + head branch for one dispatch.
///
/// Sibling layout (`../<repo>-dispatch-<slug>`) rather than nested inside the
/// caller's checkout: a nested tree would be walked by every `rg`, IDE index and
/// file watcher in the parent, and `git clean -xdff` would take it along with any
/// uncommitted agent work. This matches `/worktree-prd`'s `create.sh`.
///
/// `file_name()` is absent for a filesystem root (`/`) and for a path ending in
/// `..`; fall back to a fixed stem rather than panicking, since `working_dir`
/// comes from an agent record and a daemon must not die on a surprising cwd.
fn derive_dispatch_paths(working_dir: &Path, name: &str) -> DispatchPaths {
    let clean_name = sanitize_name(name);
    let slug = format!("dispatch-{clean_name}");
    let stem = working_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let worktree_dir = working_dir
        .parent()
        .unwrap_or(working_dir)
        .join(format!("{stem}-{slug}"));
    let branch = format!("agent/{slug}");
    DispatchPaths {
        worktree_dir,
        branch,
    }
}

pub struct DispatchResult {
    pub worktree_dir: PathBuf,
    pub success: bool,
    pub message: String,
}

pub struct DispatchContext {
    pub working_dir: PathBuf,
    pub registry: Arc<AgentPtyRegistry>,
    pub event_tx: tokio::sync::broadcast::Sender<BroadcastMsg>,
    /// The daemon-wide worktree registry the tab-close handler reads. Uses the
    /// [`WorktreeRegistry`] alias rather than spelling the map out, so the entry
    /// type cannot drift away from the registry it has to interoperate with.
    pub worktrees: WorktreeRegistry,
}

/// Translate the wire choice into the spawn-side override.
///
/// `None` on the wire means "whatever the dispatched worktree's config implies",
/// which is [`SpawnShapeOverride`]-absent — i.e. exactly the pre-selector
/// behaviour, so an older CLI keeps working against a newer daemon.
fn shape_override_of(shape: Option<&crate::event::DispatchShape>) -> Option<SpawnShapeOverride> {
    match shape {
        None => None,
        Some(crate::event::DispatchShape::SingleAgent) => Some(SpawnShapeOverride::SingleAgent),
        Some(crate::event::DispatchShape::Orchestration { name }) => {
            Some(SpawnShapeOverride::Orchestration(name.clone()))
        }
    }
}

pub async fn handle_dispatch(
    ctx: &DispatchContext,
    name: &str,
    task: &str,
    shape: Option<&crate::event::DispatchShape>,
) -> DispatchResult {
    let paths = derive_dispatch_paths(&ctx.working_dir, name);
    let clone_dir = ctx.working_dir.clone();

    match create_worktree(&clone_dir, &paths.worktree_dir, &paths.branch, false).await {
        Ok(WorktreeCreation::Created) => {}
        Ok(WorktreeCreation::AlreadyClaimed) => {
            return DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: false,
                message: format!(
                    "dispatch: worktree {} is already claimed by another dispatch. \
                     Wait for it to finish, or dispatch under a different name.",
                    paths.worktree_dir.display()
                ),
            };
        }
        // The worktree dir is GONE but its branch survived — `git worktree
        // remove` never deletes the branch, so this is the ordinary state after a
        // previous dispatch of the same name was cleaned up. Say so, and name
        // both fixes: the branch is not deleted implicitly because it may hold
        // that dispatch's committed work.
        Ok(WorktreeCreation::BranchExists) => {
            return DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: false,
                message: format!(
                    "dispatch: branch {branch} already exists from an earlier dispatch named \
                     '{name}' (its worktree is already gone). That branch may hold committed \
                     work, so it is left alone. Dispatch under a different name, or run \
                     `git -C {clone} branch -D {branch}` first if you are done with it.",
                    branch = paths.branch,
                    name = name,
                    clone = clone_dir.display(),
                ),
            };
        }
        Err(e) => {
            return DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: false,
                message: format!("dispatch: failed to create worktree: {e}"),
            };
        }
    }

    // `RemovalPolicy::KeepIfDirty`: this worktree is a sibling of the user's own
    // checkout and its name was chosen by an LLM, so closing the tab must not
    // destroy uncommitted work. See [`RemovalPolicy`].
    record_worktree(
        &ctx.worktrees,
        &paths.worktree_dir,
        &clone_dir,
        RemovalPolicy::KeepIfDirty,
    );

    let prompt = task.to_string();

    let req = SpawnRequest {
        task_name: format!("dispatch-{name}"),
        working_dir: paths.worktree_dir.to_string_lossy().into_owned(),
        command: None,
        prompt,
        shape_override: shape_override_of(shape),
    };

    let notifier = StderrNotifier;

    match spawn(req, &ctx.registry, &notifier, Some(&ctx.event_tx), false).await {
        Ok(handle) => DispatchResult {
            worktree_dir: paths.worktree_dir.clone(),
            success: true,
            // Report what was ACTUALLY opened, from the spawn's own verdict.
            // `spawn` → `decide_target` branches on the dispatched worktree's
            // `.dot-agent-deck.toml`: a repo defining `[[orchestrations]]` gets a
            // full multi-role orchestration, anything else a single agent (PRD
            // #220 M1.1). Hardcoding either word makes this message a lie in the
            // other case — and it is written straight into the caller's pane, so
            // the dispatching agent repeats it to the user verbatim.
            message: match &handle.kind {
                SpawnKind::Orchestration { name: orch } => format!(
                    "dispatch: spawned isolated orchestration '{orch}' for '{name}' in {}",
                    paths.worktree_dir.display()
                ),
                SpawnKind::SingleAgent => format!(
                    "dispatch: spawned isolated agent for '{name}' in {}",
                    paths.worktree_dir.display()
                ),
            },
        },
        Err(e) => {
            // `Force` on the rollback path, unlike the tab-close path: we created
            // this worktree seconds ago and the agent never started, so there is
            // no user work to protect — and it MUST actually go, or the leftover
            // dir and branch wedge this name for every later dispatch.
            remove_worktree(&paths.worktree_dir, &clone_dir, RemovalPolicy::Force).await;
            // Also delete the branch: `git worktree remove` never deletes it,
            // but on this rollback path the agent never ran so there is no
            // committed work to protect — leaving the branch would wedge this
            // name for every later dispatch.
            let branch_cleanup_failed = run_status(
                "git",
                &[
                    "-C",
                    &clone_dir.to_string_lossy(),
                    "branch",
                    "-D",
                    &paths.branch,
                ],
            )
            .await
            .is_err();

            if branch_cleanup_failed {
                tracing::warn!(
                    branch = %paths.branch,
                    "spawn rollback: failed to delete branch — name may be wedged for future dispatches"
                );
            }

            {
                let mut wts = ctx.worktrees.lock().unwrap_or_else(|e| e.into_inner());
                wts.remove(&paths.worktree_dir);
            }

            let cleanup_note = if branch_cleanup_failed {
                " (cleanup failed: branch may still exist — name may be wedged)"
            } else {
                ""
            };

            DispatchResult {
                worktree_dir: paths.worktree_dir,
                success: false,
                message: format!("dispatch: spawn failed: {e}{cleanup_note}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_dispatch_run::{new_worktree_registry, take_worktree};

    /// Build a real git repo with one commit, so the `git worktree` primitives
    /// under test operate on a genuine repo rather than a stubbed one.
    fn init_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git available");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        std::fs::create_dir_all(dir).unwrap();
        run(&["init", "-q", "."]);
        run(&["config", "user.email", "t@t.t"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(dir.join("a.txt"), "hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
    }

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        std::process::Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(repo)
            .output()
            .expect("git available")
            .status
            .success()
    }

    // --- slug + path derivation ---

    #[test]
    fn sanitize_name_neutralizes_path_traversal_and_separators() {
        // `..` and `/` must never survive into a path segment.
        assert!(!sanitize_name("../../etc/passwd").contains(".."));
        assert!(!sanitize_name("../../etc/passwd").contains('/'));
        // An all-punctuation name still yields a usable slug.
        assert_eq!(sanitize_name("///"), "dispatch");
        assert_eq!(sanitize_name(""), "dispatch");
        // Ordinary LLM-chosen slugs pass through untouched.
        assert_eq!(sanitize_name("fix-auth-bug"), "fix-auth-bug");
        assert_eq!(sanitize_name("add_rate_limiter"), "add_rate_limiter");
    }

    #[test]
    fn derive_dispatch_paths_places_worktree_as_sibling_not_nested() {
        let paths = derive_dispatch_paths(Path::new("/home/u/myrepo"), "fix-auth");
        assert_eq!(
            paths.worktree_dir,
            PathBuf::from("/home/u/myrepo-dispatch-fix-auth"),
            "the worktree must be a SIBLING of the checkout, never nested inside it"
        );
        assert_eq!(paths.branch, "agent/dispatch-fix-auth");
    }

    #[test]
    fn derive_dispatch_paths_survives_a_root_working_dir() {
        // `/` has no `file_name()`. This must not panic — it runs inside the
        // daemon's hook loop, where a panic kills the connection task.
        let paths = derive_dispatch_paths(Path::new("/"), "x");
        assert_eq!(paths.branch, "agent/dispatch-x");
        assert!(paths.worktree_dir.to_string_lossy().contains("dispatch-x"));
    }

    // --- the leftover-branch refusal (the one-shot-per-name defect) ---

    /// A dispatch name is reusable across cleanup cycles *as a diagnosable
    /// state*: `git worktree remove` PRESERVES the branch, so the second
    /// dispatch of a name must report `BranchExists` — NOT `AlreadyClaimed`,
    /// which would blame a worktree the user can see is already gone.
    #[tokio::test]
    async fn second_dispatch_of_a_name_reports_branch_exists_after_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "fix-auth");

        // First dispatch claims the name.
        assert_eq!(
            create_worktree(&repo, &paths.worktree_dir, &paths.branch, false).await,
            Ok(WorktreeCreation::Created)
        );

        // Tab close: the worktree goes away, the branch does not.
        remove_worktree(&paths.worktree_dir, &repo, RemovalPolicy::KeepIfDirty).await;
        assert!(!paths.worktree_dir.exists(), "worktree dir should be gone");
        assert!(
            branch_exists(&repo, &paths.branch),
            "git worktree remove must not delete the branch — the premise of this test"
        );

        // Second dispatch of the SAME name: refused, but for the real reason.
        assert_eq!(
            create_worktree(&repo, &paths.worktree_dir, &paths.branch, false).await,
            Ok(WorktreeCreation::BranchExists),
            "a leftover branch must be distinguishable from a claimed worktree"
        );
    }

    /// Deleting the leftover branch makes the name usable again — the recovery
    /// path the refusal message tells the user about.
    #[tokio::test]
    async fn deleting_the_leftover_branch_frees_the_name() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "fix-auth");

        create_worktree(&repo, &paths.worktree_dir, &paths.branch, false)
            .await
            .unwrap();
        remove_worktree(&paths.worktree_dir, &repo, RemovalPolicy::KeepIfDirty).await;
        std::process::Command::new("git")
            .args(["branch", "-D", &paths.branch])
            .current_dir(&repo)
            .output()
            .expect("git available");

        assert_eq!(
            create_worktree(&repo, &paths.worktree_dir, &paths.branch, false).await,
            Ok(WorktreeCreation::Created),
            "after deleting the branch the same dispatch name must work again"
        );
    }

    // --- removal policy (the PRD #120 regression) ---

    /// `KeepIfDirty` (PRD #220 dispatch): uncommitted work in the worktree wins
    /// over cleanup — the tree stays so the user can recover it.
    #[tokio::test]
    async fn keep_if_dirty_preserves_a_worktree_with_uncommitted_work() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let paths = derive_dispatch_paths(&repo, "unit");
        create_worktree(&repo, &paths.worktree_dir, &paths.branch, false)
            .await
            .unwrap();
        std::fs::write(paths.worktree_dir.join("uncommitted.txt"), "work").unwrap();

        remove_worktree(&paths.worktree_dir, &repo, RemovalPolicy::KeepIfDirty).await;

        assert!(
            paths.worktree_dir.exists(),
            "a dirty dispatch worktree must survive tab close so work is recoverable"
        );
    }

    /// `Force` (PRD #120 issue-dispatch): the directory MUST go even when dirty,
    /// because `dispatch_decision` reads a surviving worktree as "issue already
    /// claimed" and would skip that issue on every later fire, permanently.
    /// This is the exact regression that dropping `--force` introduced.
    #[tokio::test]
    async fn force_removes_a_dirty_worktree_so_the_slot_is_reclaimable() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let worktree_dir = repo.join(".worktrees").join("issue-7");
        create_worktree(&repo, &worktree_dir, "agent/issue-7", true)
            .await
            .unwrap();
        std::fs::write(worktree_dir.join("uncommitted.txt"), "wip").unwrap();

        remove_worktree(&worktree_dir, &repo, RemovalPolicy::Force).await;

        assert!(
            !worktree_dir.exists(),
            "issue-dispatch must force-remove so the vacated slot is reclaimable"
        );
    }

    // --- the policy survives the registry round-trip the close handler uses ---

    /// The close handler in `daemon_protocol.rs` sees only a path, so the policy
    /// has to come back out of the registry intact — otherwise both producers
    /// silently share whichever policy is hardcoded there.
    #[test]
    fn registry_round_trip_preserves_each_producers_policy() {
        let reg = new_worktree_registry();
        let clone = PathBuf::from("/ws/clone");
        let issue_wt = PathBuf::from("/ws/clone/.worktrees/issue-7");
        let dispatch_wt = PathBuf::from("/ws/clone-dispatch-fix-auth");

        record_worktree(&reg, &issue_wt, &clone, RemovalPolicy::Force);
        record_worktree(&reg, &dispatch_wt, &clone, RemovalPolicy::KeepIfDirty);

        assert_eq!(
            take_worktree(&reg, &issue_wt).map(|e| e.policy),
            Some(RemovalPolicy::Force)
        );
        assert_eq!(
            take_worktree(&reg, &dispatch_wt).map(|e| e.policy),
            Some(RemovalPolicy::KeepIfDirty)
        );
    }

    // --- PRD #220: the target listing + the wire choice ---

    fn cfg(toml: &str) -> crate::project_config::ProjectConfig {
        toml::from_str(toml).expect("parse project config")
    }

    /// The listing offers `single` always, plus every ROLE-BEARING orchestration
    /// by resolved name. Schedule/authoring modes never appear — they create a
    /// future task rather than starting a line of work, so they are not targets.
    #[test]
    fn available_targets_list_single_plus_every_role_bearing_orchestration() {
        let c = cfg("[[modes]]\nname = \"dev\"\n\n\
             [[orchestrations]]\nname = \"digest\"\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n\n\
             [[orchestrations.roles]]\nname = \"worker\"\ncommand = \"sh\"\n\n\
             [[orchestrations]]\nname = \"review\"\n\n\
             [[orchestrations.roles]]\nname = \"lead\"\ncommand = \"cat\"\nstart = true\n");
        let found = available_orchestrations(Some(&c), Path::new("/tmp/repo"));
        assert_eq!(
            found,
            vec![("digest".to_string(), 2), ("review".to_string(), 1)]
        );

        let rendered = render_available_targets(&found);
        assert!(rendered.contains("--single"), "single is always offered");
        assert!(rendered.contains("--orchestration digest"));
        assert!(rendered.contains("--orchestration review"));
        assert!(
            !rendered.contains("schedule") && !rendered.contains("dev"),
            "modes and schedule authoring are not dispatch targets:\n{rendered}"
        );
    }

    /// An unnamed orchestration is listed under the name it will actually spawn
    /// as — the dir basename — so the name the agent passes back matches.
    #[test]
    fn available_targets_resolve_an_unnamed_orchestration_to_the_dir_basename() {
        let c = cfg("[[orchestrations]]\n\n\
             [[orchestrations.roles]]\nname = \"orchestrator\"\ncommand = \"cat\"\nstart = true\n");
        let found = available_orchestrations(Some(&c), Path::new("/home/u/morning-digest"));
        assert_eq!(found, vec![("morning-digest".to_string(), 1)]);
    }

    /// No config at all: only `single`, and the text says so rather than leaving
    /// the agent to infer it from an empty list.
    #[test]
    fn available_targets_without_config_offer_single_only() {
        let found = available_orchestrations(None, Path::new("/tmp/repo"));
        assert!(found.is_empty());
        let rendered = render_available_targets(&found);
        assert!(rendered.contains("--single"));
        assert!(
            rendered.contains("No orchestrations are defined"),
            "the empty case must state the situation:\n{rendered}"
        );
    }

    /// The wire choice maps onto the spawn override, and ABSENT stays absent —
    /// that is what preserves the pre-selector behaviour for an older CLI.
    #[test]
    fn wire_shape_maps_onto_the_spawn_override() {
        use crate::event::DispatchShape;
        assert_eq!(shape_override_of(None), None);
        assert_eq!(
            shape_override_of(Some(&DispatchShape::SingleAgent)),
            Some(SpawnShapeOverride::SingleAgent)
        );
        assert_eq!(
            shape_override_of(Some(&DispatchShape::Orchestration { name: None })),
            Some(SpawnShapeOverride::Orchestration(None))
        );
        assert_eq!(
            shape_override_of(Some(&DispatchShape::Orchestration {
                name: Some("review".into())
            })),
            Some(SpawnShapeOverride::Orchestration(Some("review".into())))
        );
    }

    /// The `shape` field is additive: a payload written by a CLI that predates it
    /// still deserializes, and lands as `None` (= config-derived), so an older
    /// client keeps working against a newer daemon.
    #[test]
    fn dispatch_signal_without_shape_still_deserializes_as_config_derived() {
        let legacy = r#"{"message_type":"dispatch","pane_id":"p1","name":"unit",
                         "task":"do it","timestamp":"2026-08-08T00:00:00Z"}"#;
        let msg: crate::event::DaemonMessage =
            serde_json::from_str(legacy).expect("a pre-selector dispatch payload must still parse");
        match msg {
            crate::event::DaemonMessage::Dispatch(sig) => {
                assert_eq!(sig.name, "unit");
                assert!(
                    sig.shape.is_none(),
                    "an omitted shape must mean config-derived, not a parse failure"
                );
            }
            other => panic!("expected a dispatch message, got {other:?}"),
        }
    }
}
