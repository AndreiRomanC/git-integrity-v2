use super::*;

pub mod merge;

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

// Computes OID-based divergence independently of the graph renderer.
#[tauri::command]
pub fn graph_branch_divergence(repository_path: String, primary_branch: String) -> Result<Vec<BranchDivergence>, String> {
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

// The graph's explicit "Branch start" marker is the real merge-base between
// the currently checked-out commit (branch or detached HEAD) and the primary
// remote main ref. This intentionally does not depend on visual lane order or
// on the graph's "Primary" picker.
#[tauri::command]
pub fn graph_head_main_merge_base(repository_path: String) -> Result<Option<GraphBranchStart>, String> {
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

    let Some(base_ref) = main_remote_branch else { return Ok(None); };
    let Some(remote_oid) = repo.find_branch(&base_ref, BranchType::Remote).ok().and_then(|branch| branch.get().target()) else {
        return Ok(None);
    };
    let oid = repo.merge_base(head_oid, remote_oid).ok().map(|oid| oid.to_string());
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
    validate_path(&repository_path)?;
    let trimmed = commit_id.trim();
    if trimmed.is_empty() { return Err("Select a commit/checkpoint to restore".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "restore_exact_checkpoint", queue_started.elapsed());

    let repo = internal_repository(&repository_path)?;
    let object = repo.revparse_single(trimmed)
        .or_else(|_| Oid::from_str(trimmed).and_then(|oid| repo.find_object(oid, Some(ObjectType::Commit))))
        .map_err(|error| format!("Cannot resolve commit \"{commit_id}\": {}", error.message()))?;
    let commit = object.peel_to_commit().map_err(|_| format!("\"{commit_id}\" is not a commit"))?;
    let oid = commit.id().to_string();
    drop(commit);
    drop(object);
    drop(repo);

    // This is intentionally separate from ordinary "Checkout this commit":
    // it is the destructive "make my workspace exactly this checkpoint"
    // operation. It discards tracked edits, removes untracked non-ignored
    // leftovers, and forces submodule worktrees to the gitlinks recorded by
    // the selected parent commit. Do not use -x: ignored build artifacts stay
    // ignored instead of being deleted unexpectedly.
    //
    // Important subtlety: jumping between checkpoints with different
    // submodule sets can leave old submodule worktrees behind unless the
    // currently-registered submodules are deinitialized first. A plain
    // checkout + submodule update only moves modules that still exist in the
    // target commit; it does not reliably remove modules that existed only in
    // the previous checkpoint.
    let _ = restore_checkpoint_optional_step(&repository_path, "deinit current submodules", &["submodule", "deinit", "--all", "--force"]);
    restore_checkpoint_step(&repository_path, "pre-clean parent leftovers", &["clean", "-fd"])
        .map_err(|detail| format!("Could not prepare workspace for checkpoint {oid}: {detail}"))?;
    restore_checkpoint_step(&repository_path, "checkout detached checkpoint", &["checkout", "--detach", "--force", &oid])
        .map_err(|detail| format!("Could not restore checkpoint {oid}: {detail}"))?;
    let _ = restore_checkpoint_optional_step(&repository_path, "sync submodule urls", &["submodule", "sync", "--recursive"]);
    restore_checkpoint_step(&repository_path, "post-checkout clean parent leftovers", &["clean", "-fd"])
        .map_err(|detail| format!("Checkpoint restored, but parent clean failed: {detail}"))?;
    restore_checkpoint_step(&repository_path, "init/update submodules", &["submodule", "update", "--init", "--recursive", "--force"])
        .map_err(|detail| format!("Checkpoint restored, but submodule update failed: {detail}"))?;
    let _ = restore_checkpoint_optional_step(&repository_path, "reset submodules hard", &["submodule", "foreach", "--recursive", "git reset --hard"]);
    restore_checkpoint_step(&repository_path, "clean submodule leftovers", &["submodule", "foreach", "--recursive", "git clean -fd"])
        .map_err(|detail| format!("Checkpoint restored, but submodule clean failed: {detail}"))?;
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path);
    Ok(())
}

fn restore_checkpoint_step(repository_path: &str, label: &str, args: &[&str]) -> Result<String, String> {
    let started = Instant::now();
    let result = git(repository_path, args);
    perf_log(&format!("restore_exact_checkpoint: {label} {}", if result.is_ok() { "ok" } else { "ERROR" }), started.elapsed());
    result
}

fn restore_checkpoint_optional_step(repository_path: &str, label: &str, args: &[&str]) -> Result<String, String> {
    let started = Instant::now();
    let result = git(repository_path, args);
    perf_log(&format!("restore_exact_checkpoint: {label} {}", if result.is_ok() { "ok" } else { "ignored ERROR" }), started.elapsed());
    result
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
