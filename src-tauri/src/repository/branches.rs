use super::*;

pub mod merge;
pub mod merge_preview;

#[derive(Serialize)]
pub struct BranchCreationContext {
    pub(super) current_branch: String,
    pub(super) current_commit: String,
    pub(super) main_remote_branch: Option<String>,
    pub(super) ahead: usize,
    pub(super) behind: usize,
}

#[derive(Serialize)]
pub struct BranchDivergence {
    pub(super) name: String,
    pub(super) tip: String,
    // Real DAG divergence relative to the selected primary branch. These
    // values must never be inferred from the visual lane or row position.
    pub(super) ahead: usize,
    pub(super) behind: usize,
    // The actual common ancestor, or None for disconnected histories.
    pub(super) merge_base: Option<String>,
}

#[derive(Serialize)]
pub struct GraphBranchStart {
    pub(super) base_ref: String,
    pub(super) oid: String,
}

#[derive(Serialize, Debug)]
pub struct RemoteTrackingCheckoutResult {
    pub(super) branch: String,
    pub(super) upstream: String,
    pub(super) revision: String,
    pub(super) created: bool,
}

const RESTORE_SUBMODULE_UPDATE_TIMEOUT: Duration = SUBMODULE_UPDATE_TIMEOUT;
const RESTORE_SUBMODULE_UPDATE_TIMEOUT_LABEL: &str = SUBMODULE_UPDATE_TIMEOUT_LABEL;
const RESTORE_INDEX_LOCK_WAIT: Duration = Duration::from_secs(2);

// Computes OID-based divergence independently of the graph renderer.
#[tauri::command]
pub async fn graph_branch_divergence(repository_path: String, primary_branch: String) -> Result<Vec<BranchDivergence>, String> {
    let queued = Instant::now();
    off_main_thread(move || {
        perf_log("graph_branch_divergence: worker_queue", queued.elapsed());
        let started = Instant::now();
        let result = graph_branch_divergence_inner(repository_path, primary_branch);
        perf_log("graph_branch_divergence: local_history", started.elapsed());
        result
    }).await
}

fn graph_branch_divergence_inner(repository_path: String, primary_branch: String) -> Result<Vec<BranchDivergence>, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let Some(primary_tip) = repo.find_branch(&primary_branch, BranchType::Local).ok().and_then(|b| b.get().target()) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for branch_type in [BranchType::Local, BranchType::Remote] {
        if let Ok(iterator) = repo.branches(Some(branch_type)) {
            for item in iterator.flatten() {
                let name = match item.0.name().ok().flatten() { Some(name) => name.to_string(), None => continue };
                let Some(tip) = item.0.get().target() else { continue };
                let (ahead, behind) = repo.graph_ahead_behind(tip, primary_tip).unwrap_or((0, 0));
                let merge_base = repo.merge_base(tip, primary_tip).ok().map(|oid| oid.to_string());
                result.push(BranchDivergence { name, tip: tip.to_string(), ahead, behind, merge_base });
            }
        }
    }
    Ok(result)
}

// The graph's "Shared base" annotation is the real merge-base between
// the currently checked-out commit (branch or detached HEAD) and the primary
// remote main ref. This intentionally does not depend on visual lane order or
// on the graph's "Primary" picker.
#[tauri::command]
pub async fn graph_head_main_merge_base(repository_path: String) -> Result<Option<GraphBranchStart>, String> {
    let queued = Instant::now();
    off_main_thread(move || {
        perf_log("graph_head_main_merge_base: worker_queue", queued.elapsed());
        graph_head_main_merge_base_inner(repository_path)
    }).await
}

fn graph_head_main_merge_base_inner(repository_path: String) -> Result<Option<GraphBranchStart>, String> {
    let started = Instant::now();
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let head = repo.head().map_err(|error| error.message().to_string())?;
    let head_oid = head.peel_to_commit().map_err(|error| error.message().to_string())?.id();

    let mut main_remote_branch = None;
    for candidate in ["origin/main", "origin/master"] {
        if repo.find_branch(candidate, BranchType::Remote).is_ok() {
            main_remote_branch = Some(candidate.to_string());
            break;
        }
    }
    if main_remote_branch.is_none() { main_remote_branch = default_remote_ref(&repository_path); }

    let Some(base_ref) = main_remote_branch else {
        perf_log(&format!("graph_start_base: repo={} source=current_head_merge_base head={head_oid} target=none reason=no_base_ref primary_picker_used=false",
            anonymized_repository_id(&repository_path)), started.elapsed());
        return Ok(None);
    };
    let Some(remote_oid) = repo.find_branch(&base_ref, BranchType::Remote).ok().and_then(|branch| branch.get().target()) else {
        perf_log(&format!("graph_start_base: repo={} source=current_head_merge_base head={head_oid} base_ref={base_ref} target=none reason=base_tip_unavailable primary_picker_used=false",
            anonymized_repository_id(&repository_path)), started.elapsed());
        return Ok(None);
    };
    let oid = repo.merge_base(head_oid, remote_oid).ok().map(|oid| oid.to_string());
    perf_log(&format!("graph_start_base: repo={} source=current_head_merge_base head={head_oid} base_ref={base_ref} base_tip={remote_oid} target={} reason={} primary_picker_used=false no_extra_queries=true",
        anonymized_repository_id(&repository_path), oid.as_deref().unwrap_or("none"), if oid.is_some() { "computed" } else { "no_common_base" }), started.elapsed());
    Ok(oid.map(|oid| GraphBranchStart { base_ref, oid }))
}

// Describes exactly where a new branch would start and how that position
// relates to the main remote branch.
#[tauri::command]
pub fn branch_creation_context(repository_path: String, target_path: String) -> Result<BranchCreationContext, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let head = repo.head().map_err(|error| error.message().to_string())?;
    let current_branch = head.shorthand().unwrap_or("HEAD").to_string();
    let current_commit = head.peel_to_commit().map_err(|error| error.message().to_string())?.id().to_string();
    let local_oid = head.target();

    let mut main_remote_branch = None;
    for candidate in ["origin/main", "origin/master"] {
        if repo.find_branch(candidate, BranchType::Remote).is_ok() { main_remote_branch = Some(candidate.to_string()); break; }
    }
    if main_remote_branch.is_none() { main_remote_branch = default_remote_ref(&repository_path); }

    let (ahead, behind) = local_oid.zip(main_remote_branch.as_deref())
        .and_then(|(local, remote_name)| repo.find_branch(remote_name, BranchType::Remote).ok()?.get().target().map(|remote_oid| (local, remote_oid)))
        .and_then(|(local, remote_oid)| repo.graph_ahead_behind(local, remote_oid).ok())
        .unwrap_or((0, 0));

    Ok(BranchCreationContext { current_branch, current_commit: current_commit[..8.min(current_commit.len())].to_string(), main_remote_branch, ahead, behind })
}

#[tauri::command]
pub fn create_branch(path: String, branch: String) -> Result<(), String> {
    if branch.trim().is_empty() { return Err("Branch name cannot be empty".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&path, "create_branch", queue_started.elapsed());
    let repo = internal_repository(&path)?;
    let head = repo.head().and_then(|head| head.peel_to_commit()).map_err(|error| error.message().to_string())?;
    repo.branch(branch.trim(), &head, false).map_err(|error| error.message().to_string())?;
    drop(head);
    drop(_lock);
    // switch_branch acquires the same lock; release ours before calling it.
    switch_branch(path, branch)
}

#[tauri::command]
pub fn create_branch_at_commit(repository_path: String, branch: String, commit_id: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let branch = branch.trim();
    if branch.is_empty() { return Err("Branch name cannot be empty".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "create_branch_at_commit", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let object = repo.revparse_single(commit_id.trim())
        .or_else(|_| Oid::from_str(commit_id.trim()).and_then(|oid| repo.find_object(oid, Some(ObjectType::Commit))))
        .map_err(|error| format!("Cannot resolve commit \"{commit_id}\": {}", error.message()))?;
    let commit = object.peel_to_commit().map_err(|_| format!("\"{commit_id}\" is not a commit"))?;
    repo.branch(branch, &commit, false).map_err(|error| error.message().to_string())?;
    let target = commit.as_object();
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    repo.checkout_tree(target, Some(&mut checkout)).map_err(|error| format!("Branch created, but checkout refused: {}", error.message()))?;
    repo.set_head(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn checkout_commit(repository_path: String, commit_id: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "checkout_commit", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let object = repo.revparse_single(commit_id.trim())
        .or_else(|_| Oid::from_str(commit_id.trim()).and_then(|oid| repo.find_object(oid, Some(ObjectType::Commit))))
        .map_err(|error| format!("Cannot resolve commit \"{commit_id}\": {}", error.message()))?;
    let commit = object.peel_to_commit().map_err(|_| format!("\"{commit_id}\" is not a commit"))?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    repo.checkout_tree(commit.as_object(), Some(&mut checkout)).map_err(|error| format!("Cannot checkout commit: {}", error.message()))?;
    repo.set_head_detached(commit.id()).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

// Several sequential `git submodule ...` runs (deinit, clean, update --init,
// foreach) that can take minutes on a real project — must not run on the
// webview UI thread.
#[tauri::command]
pub async fn restore_exact_checkpoint(repository_path: String, commit_id: String) -> Result<(), String> {
    off_main_thread(move || restore_exact_checkpoint_inner(repository_path, commit_id)).await
}

pub(super) fn restore_exact_checkpoint_inner(repository_path: String, commit_id: String) -> Result<(), String> {
    restore_exact_checkpoint_inner_with_branch(repository_path, commit_id, None)
}

#[tauri::command]
pub async fn restore_exact_checkpoint_branch(repository_path: String, commit_id: String, branch: String) -> Result<(), String> {
    off_main_thread(move || restore_exact_checkpoint_inner_with_branch(repository_path, commit_id, Some(branch))).await
}

pub(super) fn restore_exact_checkpoint_inner_with_branch(repository_path: String, commit_id: String, branch: Option<String>) -> Result<(), String> {
    validate_path(&repository_path)?;
    let trimmed = commit_id.trim();
    if trimmed.is_empty() { return Err("Select a commit/checkpoint to restore".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "restore_exact_checkpoint", queue_started.elapsed());

    restore_exact_checkpoint_locked(&repository_path, trimmed, branch)
}

// Caller holds repo_write_lock for the whole destructive workflow.
fn restore_exact_checkpoint_locked(repository_path: &str, commit_id: &str, branch: Option<String>) -> Result<(), String> {
    let trimmed = commit_id.trim();

    let repo = internal_repository(&repository_path)?;
    let object = repo.revparse_single(trimmed)
        .or_else(|_| Oid::from_str(trimmed).and_then(|oid| repo.find_object(oid, Some(ObjectType::Commit))))
        .map_err(|error| format!("Cannot resolve commit \"{commit_id}\": {}", error.message()))?;
    let commit = object.peel_to_commit().map_err(|_| format!("\"{commit_id}\" is not a commit"))?;
    let oid = commit.id().to_string();
    let branch = branch.map(|name| name.trim().to_string());
    if let Some(name) = branch.as_deref() {
        if name.is_empty() { return Err("Choose a local branch".into()); }
        let branch_oid = repo.find_branch(name, BranchType::Local)
            .map_err(|_| format!("Local branch {name} no longer exists"))?
            .get().target().ok_or("The selected branch has no commit")?;
        if branch_oid != commit.id() { return Err(format!("Branch {name} no longer points to checkpoint {oid}. Refresh the graph before restoring.")); }
        let current_worktree = repo.workdir().and_then(|path| fs::canonicalize(path).ok());
        let occupied_elsewhere = git(&repository_path, &["worktree", "list", "--porcelain"])?
            .split("\n\n")
            .any(|record| {
                let worktree = record.lines().find_map(|line| line.strip_prefix("worktree "));
                let checked_out = record.lines().any(|line| line == format!("branch refs/heads/{name}"));
                checked_out && worktree.is_some_and(|path| fs::canonicalize(path).ok() != current_worktree)
            });
        if occupied_elsewhere { return Err(format!("Branch {name} is checked out in another worktree. No files were cleaned.")); }
    }
    restore_checkpoint_wait_for_index(&repo.path().join("index.lock"), "before-cleanup", RESTORE_INDEX_LOCK_WAIT)
        .map_err(|detail| format!("No cleanup or submodule deinitialization was started. {detail}"))?;
    drop(commit);
    drop(object);
    drop(repo);

    // This is intentionally separate from ordinary "Checkout this commit":
    // it is the destructive "make my workspace exactly this checkpoint"
    // operation. It discards tracked edits, removes untracked non-ignored
    // leftovers (including whole embedded repositories), and forces submodule
    // worktrees to the gitlinks recorded by the selected parent commit.
    // Ignored artifacts outside removed embedded repositories stay in place.
    //
    // Never start this with `git clean -fd`. `git clean` is not transactional:
    // it can remove nine files, fail on the tenth (locked/read-only/invalid on
    // Windows), and return an error after the workspace has already been
    // damaged. Move every untracked file or embedded repository into the Git
    // directory first. Renames on the same volume are fast and reversible; if
    // any later step fails we put them back, or retain the clearly-reported
    // quarantine rather than silently deleting user data.
    let mut submodule_quarantines = Vec::new();
    restore_checkpoint_quarantine_submodule_leftovers(&repository_path, "before-checkout", &mut submodule_quarantines)
        .map_err(|detail| format!("Could not safely prepare submodules for checkpoint {oid}. No checkout was attempted. {detail}"))?;
    let mut quarantine = match RestoreCheckpointQuarantine::new(&repository_path) {
        Ok(quarantine) => quarantine,
        Err(detail) => {
            let submodule_recovery = rollback_restore_quarantines(&mut submodule_quarantines);
            return Err(format!("Could not create the parent safety area. {detail} {submodule_recovery}"));
        }
    };
    if let Err(detail) = quarantine.move_untracked(&repository_path, "before-checkout") {
        let submodule_recovery = rollback_restore_quarantines(&mut submodule_quarantines);
        return Err(format!("Could not safely prepare workspace for checkpoint {oid}. No checkout was attempted. {detail} {submodule_recovery}"));
    }

    let restore_result = (|| -> Result<(), String> {
        // Quarantining can take time. Recheck all known indexes immediately
        // before deinit; a later external writer is still enforced by Git's
        // own atomic index lock. Never remove someone else's lock to proceed.
        for safety in std::iter::once(&quarantine).chain(submodule_quarantines.iter()) {
            restore_checkpoint_wait_for_index(&safety.index_lock, "before-deinit", RESTORE_INDEX_LOCK_WAIT)
                .map_err(|detail| format!("Stopped before submodule deinitialization or checkout. {detail}"))?;
        }
        // Important subtlety: jumping between checkpoints with different
        // submodule sets can leave old submodule worktrees behind unless the
        // currently-registered submodules are deinitialized first. A plain
        // checkout + submodule update only moves modules that still exist in
        // the target commit; it does not reliably remove modules that existed
        // only in the previous checkpoint.
        restore_checkpoint_step(&repository_path, "deinit current submodules", &["submodule", "deinit", "--all", "--force"])
            .map_err(|detail| format!("Could not deinitialize the current submodules before restoring checkpoint {oid}. HEAD was not changed, but the workspace may need Submodule update --init --recursive. Git said: {detail}"))?;
        restore_checkpoint_checkout(repository_path, &oid, branch.as_deref())?;
        let _ = restore_checkpoint_optional_step(&repository_path, "sync submodule urls", &["submodule", "sync", "--recursive"]);
        // A submodule present only in the old checkpoint becomes an ordinary
        // untracked path only after checkout. Quarantine that second wave too,
        // instead of reintroducing the same partial-delete bug after HEAD moved.
        quarantine.move_untracked(&repository_path, "after-checkout")
            .map_err(|detail| format!("Checkpoint was selected, but obsolete parent files could not all be moved safely. {detail}"))?;
        if let Err(detail) = restore_checkpoint_step_with_timeout(
            &repository_path,
            "init/update submodules",
            &["submodule", "update", "--init", "--recursive", "--force"],
            RESTORE_SUBMODULE_UPDATE_TIMEOUT,
            RESTORE_SUBMODULE_UPDATE_TIMEOUT_LABEL,
        ) {
            // The parent checkpoint is already checked out at this point. A
            // hard reset of initialized submodules is best-effort, but do not
            // run `git clean` here: deleting user files while reporting a
            // failed restore recreates the exact partial-loss bug this flow is
            // designed to prevent.
            let verification = if restore_checkpoint_failure_category(&detail) == "index-locked" {
                // Do not follow a concurrent writer's lock failure with more
                // destructive Git commands in other initialized submodules.
                String::new()
            } else {
                let _ = restore_checkpoint_optional_step(&repository_path, "cleanup after failed submodule update: reset initialized submodules", &["submodule", "foreach", "--recursive", "git reset --hard"]);
                restore_checkpoint_verify(&repository_path).err().map(|error| format!(" Remaining state: {error}")).unwrap_or_default()
            };
            return Err(format!("Checkpoint {oid} was checked out, but submodule update failed. The restore is incomplete (maximum update time: {RESTORE_SUBMODULE_UPDATE_TIMEOUT_LABEL}). Resolve the reported cause before retrying exact checkpoint restore or Submodule update --init --recursive. Git said: {detail}{verification}"));
        }
        let _ = restore_checkpoint_optional_step(&repository_path, "reset submodules hard", &["submodule", "foreach", "--recursive", "git reset --hard"]);
        restore_checkpoint_quarantine_submodule_leftovers(&repository_path, "after-checkout", &mut submodule_quarantines)
            .map_err(|detail| format!("Checkpoint restored, but submodule leftovers could not be removed safely: {detail}"))?;
        restore_checkpoint_verify(&repository_path)
            .map_err(|detail| format!("Checkpoint {oid} restore finished, but verification found the workspace is not exact yet: {detail}. Retry exact checkpoint restore or run Submodule update --init --recursive before trusting this checkout."))?;
        Ok(())
    })();

    if let Err(detail) = restore_result {
        let submodule_recovery = rollback_restore_quarantines(&mut submodule_quarantines);
        let recovery = quarantine.rollback();
        return Err(format!("{detail} {submodule_recovery} {recovery}"));
    }
    let mut cleanup_errors = discard_restore_quarantines(&mut submodule_quarantines);
    if let Err(detail) = quarantine.discard() { cleanup_errors.push(detail); }
    if !cleanup_errors.is_empty() {
        return Err(format!("Checkpoint {oid} was restored successfully, but {} temporary safety backup(s) could not be removed: {}", cleanup_errors.len(), cleanup_errors.join(" ")));
    }
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path);
    Ok(())
}

#[derive(Serialize, Debug)]
pub struct CleanUpstreamResult {
    branch: String,
    upstream: String,
    revision: String,
}

fn clean_upstream_target(repo: &Repository, branch: &str, upstream: &str, expected_head: &str) -> Result<(String, String, String), String> {
    let head = repo.head().map_err(|e| e.message().to_string())?;
    if repo.head_detached().unwrap_or(true) || head.name() != Some(format!("refs/heads/{branch}").as_str())
        || head.target().map(|oid| oid.to_string()).as_deref() != Some(expected_head) {
        return Err("The active branch or its commit changed. Nothing was cleaned; reopen the action on the current local branch.".into());
    }
    if repo.state() != git2::RepositoryState::Clean {
        return Err("Finish or abort the active merge/rebase before replacing this branch. Nothing was cleaned.".into());
    }
    let local = repo.find_branch(branch, BranchType::Local).map_err(|e| e.message().to_string())?;
    let tracking = local.upstream().map_err(|_| "Configure a remote upstream for this branch first")?;
    if tracking.get().shorthand() != Some(upstream) {
        return Err("The configured upstream changed. Nothing was cleaned; reopen the action.".into());
    }
    let config = repo.config().map_err(|e| e.message().to_string())?;
    let remote = config.get_string(&format!("branch.{branch}.remote")).map_err(|_| "Missing remote upstream")?;
    let merge_ref = config.get_string(&format!("branch.{branch}.merge")).map_err(|_| "Missing upstream branch")?;
    let tracking_ref = tracking.get().name().ok_or("Invalid remote-tracking ref")?.to_string();
    if remote == "." || !merge_ref.starts_with("refs/heads/") || !tracking_ref.starts_with("refs/remotes/") {
        return Err("Choose a branch with a real remote branch as its upstream. Nothing was cleaned.".into());
    }
    repo.find_remote(&remote).map_err(|_| "The configured remote is unavailable")?;
    Ok((remote, merge_ref, tracking_ref))
}

#[tauri::command]
pub async fn clean_branch_from_upstream(repository_path: String, branch: String, upstream: String, expected_head: String) -> Result<CleanUpstreamResult, String> {
    off_main_thread(move || clean_branch_from_upstream_inner(repository_path, branch, upstream, expected_head)).await
}

fn clean_branch_from_upstream_inner(repository_path: String, branch: String, upstream: String, expected_head: String) -> Result<CleanUpstreamResult, String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "clean_branch_from_upstream", queue_started.elapsed());
    let result = (|| {
        let repo = internal_repository(&repository_path)?;
        let target = clean_upstream_target(&repo, &branch, &upstream, &expected_head)?;
        restore_checkpoint_wait_for_index(&repo.path().join("index.lock"), "before-upstream-fetch", RESTORE_INDEX_LOCK_WAIT)
            .map_err(|detail| format!("Replace was not started. No fetch, cleanup or submodule deinitialization was performed. {detail}"))?;
        let own = repo.workdir().and_then(|path| fs::canonicalize(path).ok());
        let occupied = git(&repository_path, &["worktree", "list", "--porcelain"])?
            .split("\n\n").any(|record| record.lines().any(|line| line == format!("branch refs/heads/{branch}"))
                && record.lines().find_map(|line| line.strip_prefix("worktree ")).is_some_and(|path| fs::canonicalize(path).ok() != own));
        if occupied { return Err("This branch is also checked out in another worktree. Nothing was cleaned.".into()); }
        drop(repo);
        // Fetch only the configured source branch. Missing/deleted branches
        // fail here, before any cleaning; never use a stale tracking ref.
        let refspec = format!("+{}:{}", target.1, target.2);
        git(&repository_path, &["fetch", "--no-tags", "--no-recurse-submodules", "--", &target.0, &refspec])
            .map_err(|e| format!("Fetch failed; no local files or branch commits were discarded. {e}"))?;
        let repo = internal_repository(&repository_path)?;
        if clean_upstream_target(&repo, &branch, &upstream, &expected_head)? != target {
            return Err("Upstream configuration changed during fetch. Nothing was cleaned.".into());
        }
        let oid = repo.find_reference(&target.2).and_then(|r| r.peel_to_commit()).map_err(|e| e.message().to_string())?.id().to_string();
        drop(repo);
        // Keep the old local branch pointer until the existing recursive clean
        // checkout succeeds. A failed submodule update must not silently report
        // success or move that branch past an incomplete restore.
        restore_exact_checkpoint_locked(&repository_path, &oid, None)
            .map_err(|e| format!("Replacing {branch} from {upstream} did not complete. The local branch pointer was not reset. {e}"))?;
        let repo = internal_repository(&repository_path)?;
        if !repo.head_detached().unwrap_or(false) || repo.head().ok().and_then(|h| h.target()).map(|id| id.to_string()).as_deref() != Some(&oid) {
            return Err("Checkout changed during cleanup. The local branch was not reset; inspect the workspace.".into());
        }
        let branch_ref = format!("refs/heads/{branch}");
        git(&repository_path, &["update-ref", "-m", "Git DrillDown: replace local branch from upstream", &branch_ref, &oid, &expected_head])
            .map_err(|e| format!("Workspace cleaned at {oid} (detached), but {branch} could not be moved. {e}"))?;
        repo.set_head(&branch_ref).map_err(|e| format!("{branch} now points to {oid}, but HEAD could not be attached: {}", e.message()))?;
        Ok(CleanUpstreamResult { branch, upstream, revision: oid })
    })();
    // A failed fetch/restore may still have changed local refs or files.
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path);
    result
}

// Temporary, same-repository safety area for non-ignored untracked paths.
// It lives under the real Git directory so Git never sees it as another
// untracked item and so ordinary repositories keep renames on one volume.
struct RestoreCheckpointQuarantine {
    root: PathBuf,
    workdir: PathBuf,
    index_lock: PathBuf,
    moved: Vec<(PathBuf, PathBuf)>,
}

impl RestoreCheckpointQuarantine {
    fn new(repository_path: &str) -> Result<Self, String> {
        let repo = internal_repository(repository_path)?;
        let workdir = repo.workdir().ok_or("A clean checkpoint requires a working-tree repository")?.to_path_buf();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?.as_nanos();
        let root = repo.path().join("gdd-restore").join(format!("{}-{stamp}", std::process::id()));
        let index_lock = repo.path().join("index.lock");
        Ok(Self { root, workdir, index_lock, moved: Vec::new() })
    }

    fn move_untracked(&mut self, repository_path: &str, phase: &str) -> Result<usize, String> {
        let started = Instant::now();
        // `-z` makes spaces/newlines unambiguous. Deliberately omit
        // `--directory`: collapsing a folder can accidentally include ignored
        // build artifacts, while this operation promises to preserve them.
        let output = git(repository_path, &["ls-files", "--others", "--exclude-standard", "-z"])
            .map_err(|detail| format!("Could not inspect untracked files: {detail}"))?;
        let raw_paths = output.split('\0').filter(|path| !path.is_empty()).collect::<Vec<_>>();

        // Git reports embedded untracked repositories as trailing-slash
        // directories even without --directory. Clean checkout explicitly
        // confirms their removal, including local history. Move each WHOLE
        // directory using the existing quarantine, then discard it on success.
        // Never follow its .git file to delete external/shared Git metadata.
        let embedded_count = raw_paths.iter().filter(|path| path.ends_with('/')).count();
        let mut candidates = Vec::with_capacity(raw_paths.len());
        for raw in raw_paths {
            let relative = safe_relative_path(raw)
                .map_err(|_| "Git reported an unsafe untracked path. Nothing was moved; inspect the repository from a terminal before retrying.".to_string())?;
            if !matches!(relative.components().next(), Some(Component::Normal(_))) || relative.starts_with(".git") {
                return Err("Git reported an unsafe untracked path. Nothing was moved.".into());
            }
            candidates.push((self.workdir.join(&relative), self.root.join(phase).join(relative)));
        }
        if embedded_count > 0 {
            perf_log(&format!("restore_exact_checkpoint: quarantine {phase} embedded_repositories={embedded_count} repo={}", anonymized_repository_id(repository_path)), Duration::ZERO);
        }

        let phase_start = self.moved.len();
        for (source, destination) in candidates {
            // A concurrent external process may remove a path after ls-files.
            // Missing is harmless; every path that still exists must move.
            if fs::symlink_metadata(&source).is_err() { continue; }
            if let Some(parent) = destination.parent() {
                if let Err(error) = fs::create_dir_all(parent) {
                    let rollback = self.rollback_from(phase_start);
                    return Err(format!("Could not create the temporary safety area: {error}. {rollback}"));
                }
            }
            if let Err(error) = fs::rename(&source, &destination) {
                let rollback = self.rollback_from(phase_start);
                let path = source.strip_prefix(&self.workdir).unwrap_or(&source).display();
                return Err(format!("Local path \"{path}\" could not be moved into the temporary safety area (it may be open, locked or read-only): {error}. {rollback}"));
            }
            self.moved.push((source, destination));
        }
        let count = self.moved.len() - phase_start;
        // At this point every listed untracked path is in the quarantine,
        // including embedded repositories. Let Git remove empty directory shells.
        // This preserves the previous exact-checkout behavior (notably old,
        // deinitialized submodule folders) without exposing file contents to
        // git-clean's non-transactional deletion.
        if let Err(detail) = restore_checkpoint_step(repository_path, &format!("remove empty parent directories ({phase})"), &["clean", "-fd"]) {
            let rollback = self.rollback_from(phase_start);
            return Err(format!("Empty leftover directories could not be removed safely: {detail}. {rollback}"));
        }
        perf_log(&format!("restore_exact_checkpoint: quarantine {phase} ok ({count} paths)"), started.elapsed());
        Ok(count)
    }

    fn rollback_from(&mut self, start: usize) -> String {
        let mut restored = 0usize;
        let mut retained = 0usize;
        for index in (start..self.moved.len()).rev() {
            let (source, backup) = &self.moved[index];
            if fs::symlink_metadata(backup).is_err() { continue; }
            if fs::symlink_metadata(source).is_ok() {
                retained += 1;
                continue;
            }
            if let Some(parent) = source.parent() { let _ = fs::create_dir_all(parent); }
            if fs::rename(backup, source).is_ok() { restored += 1; } else { retained += 1; }
        }
        self.moved.truncate(start);
        let _ = remove_empty_restore_directories(&self.root);
        if retained == 0 {
            format!("Restored {restored} temporarily moved local file{}.", if restored == 1 { "" } else { "s" })
        } else {
            format!("Restored {restored} temporarily moved local file(s); {retained} could not be returned and remain safely under {}.", self.root.display())
        }
    }

    fn rollback(&mut self) -> String {
        let message = self.rollback_from(0);
        perf_log("restore_exact_checkpoint: quarantine rollback", Duration::ZERO);
        message
    }

    fn discard(&mut self) -> Result<(), String> {
        if !self.root.exists() { return Ok(()); }
        fs::remove_dir_all(&self.root)
            .map_err(|error| format!("The backup remains at {} ({error}). The restored working tree itself is already at the requested checkpoint.", self.root.display()))?;
        self.moved.clear();
        Ok(())
    }
}

fn remove_empty_restore_directories(path: &Path) -> std::io::Result<()> {
    if !path.exists() { return Ok(()); }
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        // Embedded repositories may contain symlinks/junctions. Cleanup must
        // never traverse them into another checkout or outside this repository.
        if child.symlink_metadata()?.file_type().is_dir() { let _ = remove_empty_restore_directories(&child); }
    }
    if fs::read_dir(path)?.next().is_none() { fs::remove_dir(path)?; }
    Ok(())
}

fn restore_checkpoint_quarantine_submodule_leftovers(repository_path: &str, phase: &str, quarantines: &mut Vec<RestoreCheckpointQuarantine>) -> Result<(), String> {
    let mut paths = Vec::new();
    let mut visited = HashSet::new();
    collect_initialized_submodule_workdirs(Path::new(repository_path), &mut visited, &mut paths)?;
    // Check the complete initialized set BEFORE moving the first file. Reuse
    // the existing traversal; no status scans, extra Git processes or fetches.
    let prepared = paths.iter().map(|path| RestoreCheckpointQuarantine::new(&path.to_string_lossy()))
        .collect::<Result<Vec<_>, _>>()?;
    for safety in &prepared {
        restore_checkpoint_wait_for_index(&safety.index_lock, phase, RESTORE_INDEX_LOCK_WAIT)?;
    }
    for (path, mut quarantine) in paths.iter().zip(prepared) {
        let submodule_path = path.to_string_lossy().into_owned();
        if let Err(error) = quarantine.move_untracked(&submodule_path, phase) {
            let current_recovery = quarantine.rollback();
            let earlier_recovery = rollback_restore_quarantines(quarantines);
            return Err(format!("{error} {current_recovery} {earlier_recovery}"));
        }
        quarantines.push(quarantine);
    }
    Ok(())
}

fn collect_initialized_submodule_workdirs(repository_path: &Path, visited: &mut HashSet<PathBuf>, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    let repo = Repository::open(repository_path).map_err(|error| format!("Cannot inspect initialized submodules: {}", error.message()))?;
    let workdir = repo.workdir().ok_or("Cannot inspect submodules of a bare repository")?.to_path_buf();
    for submodule in repo.submodules().map_err(|error| format!("Cannot list submodules: {}", error.message()))? {
        let path = workdir.join(submodule.path());
        if Repository::open(&path).is_err() { continue; }
        let identity = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !visited.insert(identity) { continue; }
        paths.push(path.clone());
        collect_initialized_submodule_workdirs(&path, visited, paths)?;
    }
    Ok(())
}

fn rollback_restore_quarantines(quarantines: &mut Vec<RestoreCheckpointQuarantine>) -> String {
    if quarantines.is_empty() { return "No submodule local files needed recovery.".into(); }
    let count = quarantines.len();
    let messages = quarantines.iter_mut().rev().map(RestoreCheckpointQuarantine::rollback).collect::<Vec<_>>();
    quarantines.clear();
    format!("Recovered local files for {count} submodule(s): {}", messages.join(" "))
}

fn discard_restore_quarantines(quarantines: &mut Vec<RestoreCheckpointQuarantine>) -> Vec<String> {
    let mut errors = Vec::new();
    for quarantine in quarantines.iter_mut() {
        if let Err(error) = quarantine.discard() { errors.push(error); }
    }
    quarantines.clear();
    errors
}

// The real Git directory handles .git files, linked worktrees and submodules.
// Merely checking .git/index.lock under the visible folder would miss them.
// This is a bounded, read-only preflight, NOT proof of ownership or staleness.
// Git must still acquire its own atomic lock; do not retry a failed checkout
// automatically because hooks or worktree writes may already have occurred.
fn restore_checkpoint_wait_for_index(lock_path: &Path, phase: &str, max_wait: Duration) -> Result<(), String> {
    let started = Instant::now();
    let scope = perf_logging::opaque_id(&lock_path.to_string_lossy());
    let mut waited = false;
    loop {
        match fs::symlink_metadata(lock_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if waited { perf_log(&format!("restore_index_guard: phase={phase} scope={scope} result=released lock_removed=false"), started.elapsed()); }
                return Ok(());
            }
            Err(error) => {
                perf_log(&format!("restore_index_guard: phase={phase} scope={scope} result=inspection-error kind={:?} lock_removed=false", error.kind()), started.elapsed());
                return Err(format!("Cannot inspect Git index lock at {}: {error}. Stopped rather than assuming the index is available.", lock_path.display()));
            }
            Ok(_) => {
                if !waited {
                    perf_log(&format!("restore_index_guard: phase={phase} scope={scope} result=waiting owner=unknown lock_removed=false"), started.elapsed());
                    waited = true;
                }
                if started.elapsed() >= max_wait {
                    perf_log(&format!("restore_index_guard: phase={phase} scope={scope} result=index-locked owner=unknown lock_removed=false"), started.elapsed());
                    return Err(format!("Git index is locked (index.lock): {}. Another Git operation may be active, or an interrupted operation may have left this lock. It was not removed. Wait for other Git operations to finish and retry. If it persists, confirm that no process uses this repository before recovering the lock manually.", lock_path.display()));
                }
                std::thread::sleep(Duration::from_millis(50).min(max_wait.saturating_sub(started.elapsed())));
            }
        }
    }
}

fn restore_checkpoint_checkout(repository_path: &str, oid: &str, branch: Option<&str>) -> Result<(), String> {
    let repo = internal_repository(repository_path)?;
    let ready = restore_checkpoint_wait_for_index(&repo.path().join("index.lock"), "before-checkout", RESTORE_INDEX_LOCK_WAIT);
    drop(repo);
    ready.and_then(|()| {
        if let Some(name) = branch {
            restore_checkpoint_step(repository_path, "checkout branch checkpoint", &["checkout", "--force", name])
        } else {
            restore_checkpoint_step(repository_path, "checkout detached checkpoint", &["checkout", "--detach", "--force", oid])
        }.map(|_| ())
    }).map_err(|detail| format!("Checkout of checkpoint {oid} did not complete. Submodules may already be deinitialized; the workspace is incomplete. Resolve the error before retrying Clean checkout/Replace. Git said: {detail}"))
}

fn restore_checkpoint_step(repository_path: &str, label: &str, args: &[&str]) -> Result<String, String> {
    let started = Instant::now();
    let result = git(repository_path, args).map_err(|detail| restore_checkpoint_failure_detail(&detail));
    restore_checkpoint_perf_log(label, &result, started.elapsed());
    result
}

fn restore_checkpoint_step_with_timeout(repository_path: &str, label: &str, args: &[&str], timeout: Duration, timeout_label: &str) -> Result<String, String> {
    let started = Instant::now();
    let result = git_with_timeout(repository_path, args, timeout, timeout_label).map_err(|detail| restore_checkpoint_failure_detail(&detail));
    restore_checkpoint_perf_log(label, &result, started.elapsed());
    result
}

fn restore_checkpoint_optional_step(repository_path: &str, label: &str, args: &[&str]) -> Result<String, String> {
    let started = Instant::now();
    let result = git(repository_path, args).map_err(|detail| restore_checkpoint_failure_detail(&detail));
    if let Err(detail) = &result {
        perf_log(&format!("restore_exact_checkpoint: {label} ignored ERROR ({})", restore_checkpoint_failure_category(detail)), started.elapsed());
    } else {
        perf_log(&format!("restore_exact_checkpoint: {label} ok"), started.elapsed());
    }
    result
}

fn restore_checkpoint_perf_log(label: &str, result: &Result<String, String>, elapsed: Duration) {
    if let Err(detail) = result {
        // Log a useful category, never the raw output/path list. The UI still
        // receives the concise diagnostic, while shared perf logs avoid
        // leaking repository names or local filenames.
        perf_log(&format!("restore_exact_checkpoint: {label} ERROR ({})", restore_checkpoint_failure_category(detail)), elapsed);
    } else {
        perf_log(&format!("restore_exact_checkpoint: {label} ok"), elapsed);
    }
}

fn restore_checkpoint_failure_category(detail: &str) -> &'static str {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("index is locked") || (lower.contains("index.lock") && (lower.contains("file exists") || lower.contains("already exists"))) { "index-locked" }
    else if lower.contains("timed out") { "timeout" }
    else if lower.contains("filename was too long") || lower.contains("filename too long") { "filename-too-long" }
    else if lower.contains("access is denied") || lower.contains("permission denied") || lower.contains("unable to unlink") || lower.contains("failed to remove") { "locked-or-access-denied" }
    else if lower.contains("nested git repository") { "nested-repository" }
    else { "git-error" }
}

fn restore_checkpoint_verify(repository_path: &str) -> Result<(), String> {
    let submodule_status = restore_checkpoint_step(repository_path, "verify submodule status", &["submodule", "status", "--recursive"])?;
    let bad_submodules = restore_checkpoint_bad_submodule_lines(&submodule_status);
    if !bad_submodules.is_empty() {
        return Err(format!("submodules not at recorded commits: {}", restore_checkpoint_summarize_lines(&bad_submodules)));
    }
    let status = restore_checkpoint_step(repository_path, "verify workspace status", &["status", "--short"])?;
    let remaining = restore_checkpoint_nonempty_lines(&status);
    if !remaining.is_empty() {
        return Err(format!("Git still reports changes: {}", restore_checkpoint_summarize_lines(&remaining)));
    }
    Ok(())
}

fn restore_checkpoint_bad_submodule_lines(output: &str) -> Vec<String> {
    output.lines()
        .filter_map(|line| {
            let line = line.trim_end();
            if line.is_empty() { return None; }
            let marker = line.chars().next().unwrap_or(' ');
            if marker == ' ' { None } else { Some(line.to_string()) }
        })
        .collect()
}

fn restore_checkpoint_nonempty_lines(output: &str) -> Vec<String> {
    output.lines().map(str::trim_end).filter(|line| !line.is_empty()).map(str::to_string).collect()
}

fn restore_checkpoint_failure_detail(detail: &str) -> String {
    let lines = restore_checkpoint_nonempty_lines(detail);
    if restore_checkpoint_failure_category(detail) == "index-locked" {
        // Preserve the actual cause even after many preceding warnings. Raw
        // paths are returned to the local UI, never to the shared perf log.
        let cause = lines.iter().find(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("index is locked") || (lower.contains("index.lock") && (lower.contains("file exists") || lower.contains("already exists")))
        }).map(String::as_str).unwrap_or("Git could not acquire its index lock.");
        return format!("Git index is locked (index.lock). {cause} The lock was not removed. Wait for other Git operations to finish before retrying; a persistent lock requires checking that no process still uses this repository.");
    }
    let long_path_lines = lines.iter().filter(|line| line.to_ascii_lowercase().contains("filename too long")).collect::<Vec<_>>();
    if !long_path_lines.is_empty() {
        let mut examples = long_path_lines.iter().filter_map(|line| {
            line.split_once("failed to remove ").map(|(_, tail)| {
                tail.split_once(": Filename too long").map(|(path, _)| path).unwrap_or(tail).trim_matches(['\'', '"']).to_string()
            })
        }).filter(|path| !path.is_empty()).take(3).collect::<Vec<_>>();
        examples.dedup();
        let example = if examples.is_empty() { String::new() } else { format!(" First affected path{}: {}.", if examples.len() == 1 { "" } else { "s" }, examples.join("; ")) };
        return format!("Windows refused {} path{} because the filename was too long, even though this operation enabled Git long-path support.{} The clean checkout is incomplete.", long_path_lines.len(), if long_path_lines.len() == 1 { "" } else { "s" }, example);
    }
    restore_checkpoint_summarize_lines(&lines)
}

fn restore_checkpoint_summarize_lines(lines: &[String]) -> String {
    const LIMIT: usize = 8;
    let shown = lines.iter().take(LIMIT).cloned().collect::<Vec<_>>().join("; ");
    if lines.len() > LIMIT { format!("{shown}; … and {} more", lines.len() - LIMIT) } else { shown }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UpstreamFixture { base: PathBuf, remote: PathBuf, local: PathBuf }
    fn upstream_git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git").arg("-C").arg(path).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
    impl UpstreamFixture {
        fn new() -> Self {
            let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let base = std::env::temp_dir().join(format!("gdd-upstream-{}-{suffix}", std::process::id()));
            let remote = base.join("remote"); let local = base.join("local");
            fs::create_dir_all(&remote).unwrap();
            upstream_git(&remote, &["init", "-b", "main"]);
            upstream_git(&remote, &["config", "user.name", "Test"]);
            upstream_git(&remote, &["config", "user.email", "test@example.invalid"]);
            fs::write(remote.join("file.txt"), "base\n").unwrap();
            fs::write(remote.join(".gitignore"), "*.ignored\n").unwrap();
            upstream_git(&remote, &["add", "."]); upstream_git(&remote, &["commit", "-m", "base"]);
            upstream_git(&base, &["clone", "-o", "company", remote.to_str().unwrap(), local.to_str().unwrap()]);
            upstream_git(&local, &["branch", "-m", "my-main"]);
            upstream_git(&local, &["config", "user.name", "Test"]);
            upstream_git(&local, &["config", "user.email", "test@example.invalid"]);
            Self { base, remote, local }
        }
        fn clean(&self, head: &str) -> Result<CleanUpstreamResult, String> {
            clean_branch_from_upstream_inner(self.local.to_string_lossy().into(), "my-main".into(), "company/main".into(), head.into())
        }
    }
    impl Drop for UpstreamFixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.base); } }

    #[test]
    fn clean_upstream_index_lock_stops_before_fetch_or_cleanup_and_retry_succeeds() {
        let f = UpstreamFixture::new();
        let before = upstream_git(&f.local, &["rev-parse", "HEAD"]);
        fs::write(f.remote.join("file.txt"), "new server version\n").unwrap();
        upstream_git(&f.remote, &["commit", "-am", "remote advance"]);
        let target = upstream_git(&f.remote, &["rev-parse", "HEAD"]);
        fs::write(f.local.join("file.txt"), "staged edit\n").unwrap();
        upstream_git(&f.local, &["add", "file.txt"]);
        fs::write(f.local.join("file.txt"), "unstaged edit\n").unwrap();
        fs::write(f.local.join("loose.txt"), "keep loose\n").unwrap();
        let index = fs::read(f.local.join(".git/index")).unwrap();
        let config = fs::read(f.local.join(".git/config")).unwrap();
        let lock = f.local.join(".git/index.lock");
        fs::write(&lock, "owned by another operation").unwrap();

        let error = f.clean(&before).unwrap_err();
        assert!(error.contains("No fetch, cleanup or submodule deinitialization"), "{error}");
        assert_eq!(restore_checkpoint_failure_category(&error), "index-locked");
        assert_eq!(fs::read_to_string(&lock).unwrap(), "owned by another operation");
        assert_eq!(fs::read(f.local.join(".git/index")).unwrap(), index);
        assert_eq!(fs::read(f.local.join(".git/config")).unwrap(), config);
        assert_eq!(fs::read_to_string(f.local.join("file.txt")).unwrap(), "unstaged edit\n");
        assert!(f.local.join("loose.txt").exists());
        assert!(!f.local.join(".git/gdd-restore").exists());
        assert_eq!(upstream_git(&f.local, &["rev-parse", "company/main"]), before, "must not fetch with an existing lock");
        assert_eq!(upstream_git(&f.local, &["branch", "--show-current"]), "my-main");
        assert_eq!(upstream_git(&f.local, &["rev-parse", "HEAD"]), before);

        // Only the test fixture owner releases its lock. Production never does.
        fs::remove_file(&lock).unwrap();
        assert_eq!(f.clean(&before).unwrap().revision, target);
        assert_eq!(upstream_git(&f.local, &["branch", "--show-current"]), "my-main");
        assert_eq!(upstream_git(&f.local, &["status", "--porcelain"]), "");
    }

    #[test]
    fn exact_checkpoint_checks_all_submodule_locks_before_moving_any_files() {
        let f = UpstreamFixture::new();
        // A gitfile and a recursive child exercise real Git dirs, not a
        // hard-coded <visible-worktree>/.git/index.lock location.
        upstream_git(&f.local, &["-c", "protocol.file.allow=always", "submodule", "add", f.remote.to_str().unwrap(), "module"]);
        let module = f.local.join("module");
        upstream_git(&module, &["-c", "protocol.file.allow=always", "submodule", "add", f.remote.to_str().unwrap(), "nested"]);
        upstream_git(&module, &["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-am", "nested"]);
        upstream_git(&f.local, &["commit", "-am", "module"]);
        let head = upstream_git(&f.local, &["rev-parse", "HEAD"]);
        let nested = module.join("nested");
        let nested_repo = Repository::open(&nested).unwrap();
        let lock = nested_repo.path().join("index.lock");
        fs::write(&lock, "recursive child lock").unwrap();
        let paths = [&f.local, &module, &nested];
        let mut snapshots = Vec::new();
        for path in paths {
            fs::write(path.join("file.txt"), "local edit\n").unwrap();
            fs::write(path.join("loose.txt"), "untracked\n").unwrap();
            let repo = Repository::open(path).unwrap();
            snapshots.push((repo.path().to_path_buf(), fs::read(repo.path().join("index")).unwrap(), fs::read(repo.path().join("config")).unwrap()));
        }
        let error = restore_exact_checkpoint_inner(f.local.to_string_lossy().into(), head.clone()).unwrap_err();
        assert_eq!(restore_checkpoint_failure_category(&error), "index-locked", "{error}");
        assert!(error.contains("No checkout was attempted"), "{error}");
        assert_eq!(fs::read_to_string(&lock).unwrap(), "recursive child lock");
        for (path, (git_dir, index, config)) in paths.into_iter().zip(snapshots) {
            assert_eq!(fs::read_to_string(path.join("file.txt")).unwrap(), "local edit\n");
            assert!(path.join("loose.txt").exists());
            assert_eq!(fs::read(git_dir.join("index")).unwrap(), index);
            assert_eq!(fs::read(git_dir.join("config")).unwrap(), config, "must not deinit any module");
            assert!(!git_dir.join("gdd-restore").exists(), "must check every child before moving the first file");
        }
        assert_eq!(upstream_git(&f.local, &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn exact_checkpoint_index_guard_uses_linked_worktree_git_dir() {
        let f = UpstreamFixture::new();
        let worktree = f.base.join("linked");
        upstream_git(&f.local, &["worktree", "add", "-b", "linked-branch", worktree.to_str().unwrap()]);
        assert!(worktree.join(".git").is_file());
        let repo = Repository::open(&worktree).unwrap();
        let lock = repo.path().join("index.lock");
        fs::write(&lock, "linked worktree lock").unwrap();
        fs::write(worktree.join("file.txt"), "keep\n").unwrap();
        let error = restore_exact_checkpoint_inner(worktree.to_string_lossy().into(), "HEAD".into()).unwrap_err();
        assert!(error.contains("No cleanup or submodule deinitialization was started"), "{error}");
        assert_eq!(restore_checkpoint_failure_category(&error), "index-locked");
        assert_eq!(fs::read_to_string(&lock).unwrap(), "linked worktree lock");
        assert_eq!(fs::read_to_string(worktree.join("file.txt")).unwrap(), "keep\n");
        assert_eq!(upstream_git(&worktree, &["branch", "--show-current"]), "linked-branch");
    }

    #[test]
    fn exact_checkpoint_index_guard_waits_for_external_release_without_removing_lock() {
        let f = UpstreamFixture::new();
        let lock = f.local.join(".git/index.lock");
        fs::write(&lock, "external lock").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_lock = lock.clone();
        let worker = std::thread::spawn(move || {
            tx.send(restore_checkpoint_wait_for_index(&worker_lock, "test", Duration::from_secs(2))).unwrap();
        });
        assert!(matches!(rx.recv_timeout(Duration::from_millis(100)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(fs::read_to_string(&lock).unwrap(), "external lock");
        fs::remove_file(&lock).unwrap();
        rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        worker.join().unwrap();
        // A directory in the lock's place is also occupied, never deleted.
        fs::create_dir(&lock).unwrap();
        assert!(restore_checkpoint_wait_for_index(&lock, "test", Duration::ZERO).is_err());
        assert!(lock.is_dir());
    }

    #[test]
    fn exact_checkpoint_preserves_index_lock_cause_beyond_warning_summary_limit() {
        let raw = format!("{}fatal: Unable to create 'D:/project/.git/index.lock': File exists.\nAnother git process seems to be running in this repository.\n", "warning: earlier message\n".repeat(20));
        let detail = restore_checkpoint_failure_detail(&raw);
        assert_eq!(restore_checkpoint_failure_category(&detail), "index-locked");
        assert!(detail.contains("D:/project/.git/index.lock"));
        assert!(detail.contains("lock was not removed"));
        assert!(!detail.contains("earlier message"));
        assert_eq!(restore_checkpoint_failure_category("the index is locked; this might be due to a concurrent or crashed process"), "index-locked");
        assert_eq!(restore_checkpoint_failure_category("fatal: Unable to create '/project/.git/index.lock': Permission denied"), "locked-or-access-denied");
        assert_eq!(restore_checkpoint_failure_category("fatal: Unable to create '/project/.git/config.lock': File exists"), "git-error");
    }

    #[test]
    fn exact_checkpoint_late_index_lock_reports_partial_state_without_checkout_or_retry() {
        let f = UpstreamFixture::new();
        let head = upstream_git(&f.local, &["rev-parse", "HEAD"]);
        upstream_git(&f.local, &["-c", "protocol.file.allow=always", "submodule", "add", f.remote.to_str().unwrap(), "module"]);
        upstream_git(&f.local, &["commit", "-am", "module"]);
        let before = upstream_git(&f.local, &["rev-parse", "HEAD"]);
        let path = f.local.to_string_lossy();
        restore_checkpoint_wait_for_index(&f.local.join(".git/index.lock"), "test-preflight", Duration::ZERO).unwrap();
        restore_checkpoint_step(&path, "test-deinit", &["submodule", "deinit", "--all", "--force"]).unwrap();
        // Model another process taking the lock after preflight/deinit.
        let lock = f.local.join(".git/index.lock");
        fs::write(&lock, "late external writer").unwrap();
        let index = fs::read(f.local.join(".git/index")).unwrap();
        let error = restore_checkpoint_checkout(&path, &head, None).unwrap_err();
        assert!(error.contains("Submodules may already be deinitialized"), "{error}");
        assert_eq!(restore_checkpoint_failure_category(&error), "index-locked");
        assert_eq!(fs::read_to_string(&lock).unwrap(), "late external writer");
        assert_eq!(fs::read(f.local.join(".git/index")).unwrap(), index);
        assert_eq!(upstream_git(&f.local, &["rev-parse", "HEAD"]), before);
        assert_eq!(upstream_git(&f.local, &["branch", "--show-current"]), "my-main");
    }

    #[test]
    fn clean_upstream_fetches_then_replaces_differently_named_branch_without_remote_writes() {
        let f = UpstreamFixture::new();
        fs::write(f.remote.join("file.txt"), "new server version\n").unwrap();
        upstream_git(&f.remote, &["commit", "-am", "remote advance"]);
        let target = upstream_git(&f.remote, &["rev-parse", "HEAD"]);
        fs::write(f.local.join("local-only.txt"), "local history").unwrap();
        upstream_git(&f.local, &["add", "."]); upstream_git(&f.local, &["commit", "-m", "local-only"]);
        let before = upstream_git(&f.local, &["rev-parse", "HEAD"]);
        upstream_git(&f.local, &["branch", "untouched"]);
        fs::write(f.local.join("file.txt"), "staged edit").unwrap(); upstream_git(&f.local, &["add", "file.txt"]);
        fs::write(f.local.join("file.txt"), "unstaged edit").unwrap();
        fs::write(f.local.join("loose.txt"), "untracked").unwrap();
        fs::write(f.local.join("cache.ignored"), "keep build artifact").unwrap();
        fs::create_dir(f.local.join("nested")).unwrap(); upstream_git(&f.local.join("nested"), &["init"]);
        fs::write(f.local.join("nested/local.txt"), "remove whole nested repository").unwrap();
        let result = f.clean(&before).unwrap();
        assert_eq!(result.branch, "my-main"); assert_eq!(result.upstream, "company/main"); assert_eq!(result.revision, target);
        assert_eq!(upstream_git(&f.local, &["branch", "--show-current"]), "my-main");
        assert_eq!(upstream_git(&f.local, &["rev-parse", "HEAD"]), target);
        assert_eq!(upstream_git(&f.local, &["rev-parse", "@{upstream}"]), target);
        assert_eq!(upstream_git(&f.local, &["rev-parse", "untouched"]), before);
        assert_eq!(upstream_git(&f.remote, &["rev-parse", "HEAD"]), target);
        assert_eq!(upstream_git(&f.local, &["status", "--porcelain"]), "");
        assert!(!f.local.join("local-only.txt").exists()); assert!(!f.local.join("loose.txt").exists()); assert!(!f.local.join("nested").exists());
        assert!(f.local.join("cache.ignored").exists());
        assert_eq!(fs::read_to_string(f.local.join("file.txt")).unwrap(), "new server version\n");
    }

    #[test]
    fn clean_upstream_preflight_and_fetch_failures_do_not_discard_files() {
        for failure in ["head", "upstream", "detached", "local-upstream", "missing-remote-branch", "remote-unavailable", "merge"] {
            let f = UpstreamFixture::new();
            let head = upstream_git(&f.local, &["rev-parse", "HEAD"]);
            fs::write(f.local.join("file.txt"), "keep my staged change").unwrap();
            upstream_git(&f.local, &["add", "file.txt"]);
            fs::write(f.local.join("loose.txt"), "keep untracked").unwrap();
            match failure {
                "upstream" => { upstream_git(&f.local, &["config", "branch.my-main.merge", "refs/heads/other"]); }
                "detached" => { upstream_git(&f.local, &["checkout", "--detach"]); }
                "local-upstream" => { upstream_git(&f.local, &["config", "branch.my-main.remote", "."]); }
                "missing-remote-branch" => { upstream_git(&f.remote, &["branch", "-m", "renamed"]); }
                "remote-unavailable" => { upstream_git(&f.local, &["remote", "set-url", "company", f.base.join("does-not-exist").to_str().unwrap()]); }
                "merge" => { fs::write(f.local.join(".git/MERGE_HEAD"), &head).unwrap(); }
                _ => {}
            }
            let index = fs::read(f.local.join(".git/index")).unwrap();
            assert!(f.clean(if failure == "head" { "0000000000000000000000000000000000000000" } else { &head }).is_err(), "{failure}");
            assert_eq!(fs::read(f.local.join(".git/index")).unwrap(), index, "{failure}");
            assert_eq!(fs::read_to_string(f.local.join("file.txt")).unwrap(), "keep my staged change", "{failure}");
            assert!(f.local.join("loose.txt").exists(), "{failure}");
            assert_eq!(upstream_git(&f.local, &["rev-parse", "my-main"]), head, "{failure}");
        }
    }

    #[test]
    fn clean_upstream_restores_recorded_submodule_revision_and_cleans_its_worktree() {
        let f = UpstreamFixture::new();
        let dep = f.base.join("dep"); fs::create_dir(&dep).unwrap();
        upstream_git(&dep, &["init", "-b", "main"]);
        upstream_git(&dep, &["config", "user.name", "Test"]); upstream_git(&dep, &["config", "user.email", "test@example.invalid"]);
        fs::write(dep.join("module.txt"), "version 1").unwrap();
        upstream_git(&dep, &["add", "."]); upstream_git(&dep, &["commit", "-m", "v1"]);
        upstream_git(&f.remote, &["-c", "protocol.file.allow=always", "submodule", "add", dep.to_str().unwrap(), "module"]);
        upstream_git(&f.remote, &["commit", "-am", "add module"]);
        upstream_git(&f.local, &["fetch", "company"]); upstream_git(&f.local, &["merge", "--ff-only", "company/main"]);
        upstream_git(&f.local, &["-c", "protocol.file.allow=always", "submodule", "update", "--init"]);
        let before = upstream_git(&f.local, &["rev-parse", "HEAD"]);
        fs::write(dep.join("module.txt"), "version 2").unwrap(); upstream_git(&dep, &["commit", "-am", "v2"]);
        let v2 = upstream_git(&dep, &["rev-parse", "HEAD"]);
        upstream_git(&f.remote.join("module"), &["fetch", "origin"]);
        upstream_git(&f.remote.join("module"), &["checkout", "--detach", &v2]);
        upstream_git(&f.remote, &["commit", "-am", "update module"]);
        // Preload local objects; the actual clean/update must use the recorded
        // commit, not a submodule branch tip or a parent index from before fetch.
        upstream_git(&f.local.join("module"), &["fetch", "origin"]);
        fs::write(f.local.join("module/module.txt"), "discard edited module").unwrap();
        fs::write(f.local.join("module/loose.txt"), "discard loose file").unwrap();
        f.clean(&before).unwrap();
        assert_eq!(upstream_git(&f.local, &["branch", "--show-current"]), "my-main");
        assert_eq!(upstream_git(&f.local.join("module"), &["rev-parse", "HEAD"]), v2);
        assert_eq!(fs::read_to_string(f.local.join("module/module.txt")).unwrap(), "version 2");
        assert!(!f.local.join("module/loose.txt").exists());
        assert_eq!(upstream_git(&f.local, &["status", "--porcelain"]), "");
    }

    #[test]
    fn clean_upstream_failed_submodule_update_does_not_move_local_branch_or_claim_success() {
        let f = UpstreamFixture::new();
        let before = upstream_git(&f.local, &["rev-parse", "HEAD"]);
        fs::write(f.remote.join(".gitmodules"), format!("[submodule \"missing\"]\npath = missing\nurl = {}\n", f.base.join("no-such-remote").display())).unwrap();
        upstream_git(&f.remote, &["add", ".gitmodules"]);
        upstream_git(&f.remote, &["update-index", "--add", "--cacheinfo", &format!("160000,{before},missing")]);
        upstream_git(&f.remote, &["commit", "-m", "unavailable submodule"]);
        let error = f.clean(&before).unwrap_err();
        assert!(error.contains("did not complete"), "{error}");
        assert!(error.contains("local branch pointer was not reset"), "{error}");
        assert_eq!(upstream_git(&f.local, &["rev-parse", "my-main"]), before);
        assert_eq!(upstream_git(&f.local, &["branch", "--show-current"]), "");
    }

    #[test]
    fn clean_upstream_rejects_branch_checked_out_in_another_worktree() {
        let f = UpstreamFixture::new();
        let before = upstream_git(&f.local, &["rev-parse", "HEAD"]);
        upstream_git(&f.local, &["worktree", "add", "--force", f.base.join("other-worktree").to_str().unwrap(), "my-main"]);
        fs::write(f.local.join("file.txt"), "keep").unwrap();
        assert!(f.clean(&before).unwrap_err().contains("another worktree"));
        assert_eq!(fs::read_to_string(f.local.join("file.txt")).unwrap(), "keep");
    }

    #[test]
    fn exact_checkpoint_verification_flags_non_exact_submodule_states() {
        let status = concat!(
            " 1111111111111111111111111111111111111111 modules/clean (heads/main)\n",
            "-2222222222222222222222222222222222222222 modules/missing\n",
            "+3333333333333333333333333333333333333333 modules/moved (heads/dev)\n",
            "U4444444444444444444444444444444444444444 modules/conflict\n",
        );
        let bad = restore_checkpoint_bad_submodule_lines(status);
        assert_eq!(bad.len(), 3);
        assert!(bad[0].starts_with('-'));
        assert!(bad[1].starts_with('+'));
        assert!(bad[2].starts_with('U'));
    }

    #[test]
    fn exact_checkpoint_summary_stays_short_for_large_dirty_sets() {
        let lines = (0..12).map(|index| format!(" M file-{index}.txt")).collect::<Vec<_>>();
        let summary = restore_checkpoint_summarize_lines(&lines);
        assert!(summary.contains("file-0.txt"));
        assert!(summary.contains("and 4 more"));
        assert!(!summary.contains("file-11.txt"));
    }

    #[test]
    fn exact_checkpoint_failure_collapses_repeated_windows_long_path_advice() {
        let detail = concat!(
            "warning: failed to remove 'one/very/deep/file.txt': Filename too long\n",
            "hint: Setting `core.longPaths` may allow the deletion to succeed.\n",
            "warning: failed to remove 'two/very/deep/file.txt': Filename too long\n",
            "hint: Setting `core.longPaths` may allow the deletion to succeed.\n",
        );
        let message = restore_checkpoint_failure_detail(detail);
        assert!(message.contains("Windows refused 2 paths"));
        assert!(message.contains("one/very/deep/file.txt"));
        assert!(message.contains("two/very/deep/file.txt"));
        assert!(!message.contains("hint:"));
    }

    #[test]
    fn exact_checkpoint_quarantine_is_reversible_and_preserves_ignored_files() {
        let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-restore-quarantine-{suffix}"));
        fs::create_dir_all(repository.join("mixed")).unwrap();
        fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
        fs::write(repository.join(".gitignore"), "*.ignored\n").unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("git").arg("-C").arg(&repository).args(args).status().unwrap();
            assert!(status.success(), "git command failed: {args:?}");
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
        run(&["add", ".gitignore", "tracked.txt"]);
        run(&["commit", "-m", "Initial"]);
        fs::write(repository.join("loose.txt"), "loose\n").unwrap();
        fs::write(repository.join("mixed/free.txt"), "free\n").unwrap();
        fs::write(repository.join("mixed/build.ignored"), "keep\n").unwrap();

        let path = repository.to_string_lossy().into_owned();
        let mut quarantine = RestoreCheckpointQuarantine::new(&path).unwrap();
        assert_eq!(quarantine.move_untracked(&path, "test").unwrap(), 2);
        assert!(!repository.join("loose.txt").exists());
        assert!(!repository.join("mixed/free.txt").exists());
        assert!(repository.join("mixed/build.ignored").exists(), "ignored artifacts must never be quarantined or cleaned");

        let recovery = quarantine.rollback();
        assert!(recovery.contains("Restored 2"));
        assert!(repository.join("loose.txt").exists());
        assert!(repository.join("mixed/free.txt").exists());
        assert!(repository.join("mixed/build.ignored").exists());
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn exact_checkpoint_quarantine_removes_embedded_repositories_and_can_roll_back_failure() {
        let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-restore-nested-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
        let run = |path: &Path, args: &[&str]| {
            let status = Command::new("git").arg("-C").arg(path).args(args).status().unwrap();
            assert!(status.success(), "git command failed: {args:?}");
        };
        run(&repository, &["init", "-b", "main"]);
        run(&repository, &["config", "user.email", "test@example.com"]);
        run(&repository, &["config", "user.name", "Test User"]);
        run(&repository, &["add", "tracked.txt"]);
        run(&repository, &["commit", "-m", "Initial"]);
        fs::write(repository.join("loose.txt"), "must survive\n").unwrap();
        fs::create_dir_all(repository.join("nested")).unwrap();
        run(&repository.join("nested"), &["init"]);
        run(&repository.join("nested"), &["config", "user.email", "test@example.com"]);
        run(&repository.join("nested"), &["config", "user.name", "Test User"]);
        fs::write(repository.join("nested/local.txt"), "local-only history\n").unwrap();
        fs::write(repository.join("nested/.gitignore"), "*.ignored\n").unwrap();
        run(&repository.join("nested"), &["add", "."]);
        run(&repository.join("nested"), &["commit", "-m", "Unpublished embedded commit"]);
        fs::write(repository.join("nested/cache.ignored"), "discard with nested repository\n").unwrap();
        fs::write(repository.join("nested/local.txt"), "uncommitted nested edit\n").unwrap();

        let path = repository.to_string_lossy().into_owned();
        let mut quarantine = RestoreCheckpointQuarantine::new(&path).unwrap();
        assert_eq!(quarantine.move_untracked(&path, "test").unwrap(), 2);
        assert!(!repository.join("loose.txt").exists());
        assert!(!repository.join("nested").exists());
        quarantine.rollback();
        assert!(repository.join("loose.txt").exists());
        assert!(repository.join("nested/.git").exists());
        assert_eq!(fs::read_to_string(repository.join("nested/local.txt")).unwrap(), "uncommitted nested edit\n");
        run(&repository.join("nested"), &["cat-file", "-e", "HEAD:local.txt"]);

        assert_eq!(quarantine.move_untracked(&path, "retry").unwrap(), 2);
        quarantine.discard().unwrap();
        assert!(!repository.join("nested").exists(), "confirmed Clean removes the whole embedded repository");
        assert!(!quarantine.root.exists(), "successful Clean leaves no extra safety copy");
        run(&repository, &["cat-file", "-e", "HEAD:tracked.txt"]);
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn exact_checkpoint_quarantine_does_not_delete_gitfile_targets() {
        let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-restore-gitfile-{suffix}"));
        let repository = base.join("parent");
        let external = base.join("external-git");
        let nested = repository.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let run = |args: &[&str]| {
            let output = Command::new("git").arg("-C").arg(&repository).args(args).output().unwrap();
            assert!(output.status.success(), "git failed: {}", String::from_utf8_lossy(&output.stderr));
        };
        run(&["init"]);
        run(&["init", "--separate-git-dir", external.to_str().unwrap(), nested.to_str().unwrap()]);
        fs::write(nested.join("local.txt"), "discard\n").unwrap();
        let gitfile = fs::read(nested.join(".git")).unwrap();
        let external_config = fs::read(external.join("config")).unwrap();
        let path = repository.to_string_lossy().into_owned();
        let mut quarantine = RestoreCheckpointQuarantine::new(&path).unwrap();
        assert_eq!(quarantine.move_untracked(&path, "test").unwrap(), 1);
        quarantine.rollback();
        assert_eq!(fs::read(nested.join(".git")).unwrap(), gitfile);
        assert_eq!(quarantine.move_untracked(&path, "retry").unwrap(), 1);
        quarantine.discard().unwrap();
        assert!(!nested.exists());
        assert_eq!(fs::read(external.join("config")).unwrap(), external_config, "never follow .git to remove external metadata");
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn exact_checkpoint_quarantine_cleanup_does_not_follow_symlinks() {
        let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-restore-symlink-{suffix}"));
        let safety = base.join("safety");
        let outside = base.join("outside");
        fs::create_dir_all(&safety).unwrap();
        fs::create_dir_all(outside.join("empty-child")).unwrap();
        fs::write(outside.join("keep.txt"), "outside checkout\n").unwrap();
        std::os::unix::fs::symlink(&outside, safety.join("link")).unwrap();
        remove_empty_restore_directories(&safety).unwrap();
        assert!(outside.join("empty-child").exists(), "cleanup must not walk into a symlink");
        let mut quarantine = RestoreCheckpointQuarantine { root: safety, workdir: base.clone(), index_lock: base.join(".git/index.lock"), moved: Vec::new() };
        quarantine.discard().unwrap();
        assert!(outside.join("empty-child").exists());
        assert_eq!(fs::read_to_string(outside.join("keep.txt")).unwrap(), "outside checkout\n");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn exact_checkpoint_perf_error_categories_are_actionable_without_paths() {
        assert_eq!(restore_checkpoint_failure_category("fatal: operation timed out"), "timeout");
        assert_eq!(restore_checkpoint_failure_category("warning: failed to remove file"), "locked-or-access-denied");
        assert_eq!(restore_checkpoint_failure_category("Filename too long"), "filename-too-long");
        assert_eq!(restore_checkpoint_failure_category("other failure"), "git-error");
    }
}

// Resolves the target to the submodule's own repository, then refreshes only
// the parent index metadata after the branch was created successfully.
#[tauri::command]
pub fn create_submodule_branch(repository_path: String, relative_path: String, branch: String) -> Result<(), String> {
    if branch.trim().is_empty() { return Err("Branch name cannot be empty".into()); }
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    create_branch(absolute.to_string_lossy().into_owned(), branch)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "create_submodule_branch", queue_started.elapsed());
    let parent = internal_repository(&repository_path)?;
    let mut submodule = parent.find_submodule(&relative_path).map_err(|error| error.message().to_string())?;
    submodule.add_to_index(true).map_err(|error| format!("Branch created, but the parent index could not be updated: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn switch_branch(path: String, branch: String) -> Result<(), String> {
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&path, "switch_branch", queue_started.elapsed());
    let repo = internal_repository(&path)?;
    let branch = branch.trim();
    if branch.is_empty() { return Err("Branch name cannot be empty".into()); }
    let reference = format!("refs/heads/{branch}");
    let target_oid = repo.find_reference(&reference)
        .map_err(|error| error.message().to_string())?
        .peel_to_commit().map_err(|error| error.message().to_string())?.id();
    // Install the target tree before changing HEAD. If safe checkout refuses,
    // HEAD, index and worktree all stay on the original branch.
    let target = repo.find_object(target_oid, Some(ObjectType::Commit)).map_err(|error| error.message().to_string())?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    repo.checkout_tree(&target, Some(&mut checkout)).map_err(|error| format!("Cannot switch to '{branch}': {}", error.message()))?;
    drop(target);
    repo.set_head(&reference).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&path);
    Ok(())
}

// Turns a remote-tracking branch such as origin/feature into a normal local
// branch feature that tracks it, then checks that local branch out. This is
// the safe GUI equivalent of:
//   git switch --track origin/feature
// with deliberate guardrails:
// - an existing same-name local branch is never reset or moved;
// - an existing branch is reused only when it already tracks the selected
//   remote branch;
// - a missing or different upstream is reported instead of silently changing
//   branch configuration.
#[tauri::command]
pub fn checkout_remote_tracking_branch(repository_path: String, remote_branch: String) -> Result<RemoteTrackingCheckoutResult, String> {
    validate_path(&repository_path)?;
    let remote_branch = remote_branch.trim();
    if remote_branch.is_empty() { return Err("Remote branch cannot be empty".into()); }
    if remote_branch.ends_with("/HEAD") { return Err("origin/HEAD is only a pointer, not a branch to check out.".into()); }
    let Some((remote_name, local_name)) = remote_branch.split_once('/') else {
        return Err("Select a remote branch such as origin/feature-name.".into());
    };
    if remote_name.trim().is_empty() || local_name.trim().is_empty() {
        return Err("Select a remote branch such as origin/feature-name.".into());
    }

    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "checkout_remote_tracking_branch", queue_started.elapsed());

    let repo = internal_repository(&repository_path)?;
    let remote_ref = format!("refs/remotes/{remote_branch}");
    let remote_commit = repo.find_reference(&remote_ref)
        .map_err(|_| format!("Remote branch \"{remote_branch}\" is not known locally. Fetch first, then try again."))?
        .peel_to_commit()
        .map_err(|error| format!("Cannot resolve {remote_branch}: {}", error.message()))?;
    let revision = remote_commit.id().to_string();

    let mut created = false;
    match repo.find_branch(local_name, BranchType::Local) {
        Ok(local_branch) => {
            let existing_upstream = local_branch.upstream()
                .ok()
                .and_then(|upstream| upstream.get().shorthand().map(str::to_string));
            match existing_upstream {
                Some(existing) if existing == remote_branch => {}
                Some(existing) => return Err(format!(
                    "Local branch \"{local_name}\" already tracks {existing}. It was not changed. Create a different local branch name if you want to track {remote_branch}."
                )),
                None => return Err(format!(
                    "Local branch \"{local_name}\" already exists but has no upstream. It was not changed. Switch to that local branch directly, or configure its upstream explicitly before using {remote_branch}."
                )),
            }
        }
        Err(_) => {
            let mut branch = repo.branch(local_name, &remote_commit, false)
                .map_err(|error| format!("Could not create local branch \"{local_name}\" from {remote_branch}: {}", error.message()))?;
            branch.set_upstream(Some(remote_branch))
                .map_err(|error| format!("Created {local_name}, but could not set upstream {remote_branch}: {}", error.message()))?;
            created = true;
        }
    }

    let reference = format!("refs/heads/{local_name}");
    let target_oid = repo.find_reference(&reference)
        .map_err(|error| error.message().to_string())?
        .peel_to_commit()
        .map_err(|error| error.message().to_string())?
        .id();
    let target = repo.find_object(target_oid, Some(ObjectType::Commit)).map_err(|error| error.message().to_string())?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    repo.checkout_tree(&target, Some(&mut checkout))
        .map_err(|error| format!("Cannot switch to local branch \"{local_name}\": {}", error.message()))?;
    drop(target);
    repo.set_head(&reference).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(RemoteTrackingCheckoutResult { branch: local_name.to_string(), upstream: remote_branch.to_string(), revision, created })
}

#[tauri::command]
pub fn rename_branch(repository_path: String, old_name: String, new_name: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if new_name.trim().is_empty() { return Err("Branch name cannot be empty".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "rename_branch", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let mut branch = repo.find_branch(old_name.trim(), BranchType::Local).map_err(|error| error.message().to_string())?;
    branch.rename(new_name.trim(), false).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn delete_branch(repository_path: String, branch_name: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "delete_branch", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let current = repo.head().ok().and_then(|head| head.shorthand().map(String::from));
    if current.as_deref() == Some(branch_name.trim()) { return Err("Cannot delete the currently checked out branch. Switch to another branch first".into()); }
    let mut branch = repo.find_branch(branch_name.trim(), BranchType::Local).map_err(|error| error.message().to_string())?;
    branch.delete().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}
