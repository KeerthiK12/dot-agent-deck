use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::agent_pty::AgentPtyRegistry;
use crate::event::BroadcastMsg;
use crate::issue_dispatch_run::{create_worktree, remove_worktree, WorktreeCreation};
use crate::scheduler::StderrNotifier;
use crate::spawn::{spawn, SpawnRequest};

fn sanitize_name(name: &str) -> String {
    let sanitized = name
        .replace('/', "-")
        .replace('\\', "-")
        .replace("..", "_")
        .replace('\0', "");
    if sanitized.is_empty() || sanitized.chars().all(|c| c == '-') {
        "dispatch".to_string()
    } else {
        sanitized
    }
}

struct DispatchPaths {
    worktree_dir: PathBuf,
    branch: String,
}

fn derive_dispatch_paths(working_dir: &Path, name: &str) -> DispatchPaths {
    let clean_name = sanitize_name(name);
    let slug = format!("dispatch-{clean_name}");
    let worktree_dir = working_dir.join(".worktrees").join(&slug);
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
    pub worktrees: Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    pub callbacks: Arc<Mutex<HashMap<String, String>>>,
}

pub async fn handle_dispatch(
    ctx: &DispatchContext,
    caller_pane_id: &str,
    name: &str,
    task: Option<&str>,
) -> DispatchResult {
    let paths = derive_dispatch_paths(&ctx.working_dir, name);
    let clone_dir = ctx.working_dir.clone();

    match create_worktree(&clone_dir, &paths.worktree_dir, &paths.branch).await {
        Ok(WorktreeCreation::Created) => {}
        Ok(WorktreeCreation::AlreadyClaimed) => {
            return DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: false,
                message: format!(
                    "dispatch: worktree {} is already claimed by another dispatch",
                    paths.worktree_dir.display()
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

    {
        let mut wts = ctx.worktrees.lock().await;
        wts.insert(paths.worktree_dir.clone(), clone_dir.clone());
    }

    let prompt = match task {
        Some(t) => t.to_string(),
        None => format!(
            "Isolated worktree for task: {name}. Read your environment and proceed."
        ),
    };

    let req = SpawnRequest {
        task_name: format!("dispatch-{name}"),
        working_dir: paths.worktree_dir.to_string_lossy().into_owned(),
        command: None,
        prompt,
    };

    let notifier = StderrNotifier;

    match spawn(req, &ctx.registry, &notifier, Some(&ctx.event_tx), false).await {
        Ok(_handle) => {
            let dispatch_id = format!("dispatch-{name}");
            {
                let mut cb = ctx.callbacks.lock().await;
                cb.insert(
                    dispatch_id.clone(),
                    caller_pane_id.to_string(),
                );
            }

            DispatchResult {
                worktree_dir: paths.worktree_dir.clone(),
                success: true,
                message: format!(
                    "dispatch: spawned isolated orchestration for '{}' in {}",
                    name,
                    paths.worktree_dir.display()
                ),
            }
        }
        Err(e) => {
            let _ = remove_worktree(&paths.worktree_dir, &clone_dir).await;

            {
                let mut wts = ctx.worktrees.lock().await;
                wts.remove(&paths.worktree_dir);
            }

            DispatchResult {
                worktree_dir: paths.worktree_dir,
                success: false,
                message: format!("dispatch: spawn failed: {e}"),
            }
        }
    }
}
