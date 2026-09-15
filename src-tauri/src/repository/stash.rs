use super::*;

#[tauri::command]
pub fn stash_changes(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "stash_changes", queue_started.elapsed());
    let mut repo = internal_repository(&repository_path)?;
    let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?;
    repo.stash_save2(&signature, None, Some(git2::StashFlags::INCLUDE_UNTRACKED)).map_err(|error| format!("Cannot stash changes: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

fn stash_scope_inner(repository_path: String, is_submodule: bool, relative_path: Option<String>) -> Result<StashScope, String> {
    let mut repo = internal_repository(&repository_path)?;
    let repository_name = Path::new(&repository_path).file_name().and_then(|name| name.to_str()).unwrap_or("repository").to_string();
    let current_branch = if repo.head_detached().unwrap_or(false) { String::new() } else { repo.head().ok().and_then(|head| head.shorthand().map(String::from)).unwrap_or_default() };
    let mut raw = Vec::new();
    repo.stash_foreach(|index, message, oid| { raw.push((index, message.to_string(), *oid)); true }).map_err(|error| error.message().to_string())?;
    let stashes = raw.into_iter().map(|(index, message, oid)| {
        let base_commit = repo.find_commit(oid).ok().and_then(|commit| commit.parent_id(0).ok()).map(|id| id.to_string()).unwrap_or_default();
        StashEntry { index, message, base_commit }
    }).collect();
    Ok(StashScope { repository_path, repository_name, current_branch, is_submodule, relative_path, stashes })
}

// Lightweight stash-only reads: opening the stash dialog must not pay for a
// full status scan and 500-commit history walk merely to enumerate refs/stash.
#[tauri::command]
pub fn list_stashes(repository_path: String) -> Result<StashScope, String> {
    validate_path(&repository_path)?;
    stash_scope_inner(repository_path, false, None)
}

#[tauri::command]
pub fn list_submodule_stashes(repository_path: String, relative_path: String) -> Result<StashScope, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    stash_scope_inner(absolute.to_string_lossy().into_owned(), true, Some(normalized(&safe_relative_path(&relative_path)?)))
}

// Stashes a single file/folder instead of the whole working tree. libgit2's
// stash API has no pathspec filter (it always stashes everything), so this
// shells out to real `git stash push -- <path>` — exactly what the CLI does
// under the hood for a scoped stash — through the same guarded `git()`
// helper used by the raw-console escape hatch (no shell, no credential
// prompt hang).
#[tauri::command]
pub fn stash_file(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if let Some((sub_path, inner_relative)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        if !inner_relative.is_empty() { return stash_file(sub_path, inner_relative); }
    }
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    if relative_string.is_empty() { return Err("Select a specific file or folder to stash".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "stash_file", queue_started.elapsed());
    git(&repository_path, &["stash", "push", "--include-untracked", "--", &relative_string])?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn pop_stash(repository_path: String, stash_index: usize) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "pop_stash", queue_started.elapsed());
    let mut repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Clean || repo.index().map(|index| index.has_conflicts()).unwrap_or(true) {
        return Err("Cannot restore a stash while another Git operation or conflict resolution is in progress. Finish or abort it first.".into());
    }
    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() {
        let files: Vec<String> = dirty.iter().take(5).map(|(path, status, _)| format!("{status} {path}")).collect();
        let more = if dirty.len() > 5 { format!(" (+{} more)", dirty.len() - 5) } else { String::new() };
        return Err(format!("Cannot restore the complete stash because this repository already has uncommitted work:\n{}{more}\n\nCommit or stash the current work first, or restore only selected clean files. Nothing was changed.", files.join("\n")));
    }
    let mut options = git2::StashApplyOptions::new();
    repo.stash_apply(stash_index, Some(&mut options)).map_err(|error| format!("Cannot restore stashed work: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    let has_conflicts = repo.index().map(|index| index.has_conflicts()).unwrap_or(false);
    if !has_conflicts { repo.stash_drop(stash_index).map_err(|error| format!("Restored, but could not drop the stash entry: {}", error.message()))?; }
    Ok(())
}

#[tauri::command]
pub fn drop_stash(repository_path: String, stash_index: usize) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "drop_stash", queue_started.elapsed());
    let mut repo = internal_repository(&repository_path)?;
    repo.stash_drop(stash_index).map_err(|error| format!("Cannot drop this stash: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn restore_stash_paths(repository_path: String, stash_index: usize, paths: Vec<String>) -> Result<(), String> {
    validate_path(&repository_path)?;
    if paths.is_empty() { return Err("Select at least one file to restore".into()); }
    let selected: HashSet<String> = paths.iter().map(|path| safe_relative_path(path).map(|p| normalized(&p))).collect::<Result<_, _>>()?;
    let all_files: HashSet<String> = stash_entry_files(repository_path.clone(), stash_index)?.into_iter().collect();
    let missing: Vec<&String> = selected.iter().filter(|path| !all_files.contains(*path)).collect();
    if !missing.is_empty() {
        return Err(format!("The selected path is not present in this stash: {}", missing.iter().map(|path| path.as_str()).collect::<Vec<_>>().join(", ")));
    }

    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "restore_stash_paths", queue_started.elapsed());
    let mut repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Clean || repo.index().map(|index| index.has_conflicts()).unwrap_or(true) {
        return Err("Cannot restore a stash file while another Git operation or conflict resolution is in progress.".into());
    }
    let dirty_paths: HashSet<String> = internal_statuses(&repo, None)?.into_iter().map(|(path, _, _)| path).collect();
    let dirty_selected: Vec<&String> = selected.iter().filter(|path| dirty_paths.contains(*path)).collect();
    if !dirty_selected.is_empty() {
        return Err(format!("Cannot restore because the selected path already has uncommitted work: {}. Commit, stash, or discard that current work first; it will not be overwritten.", dirty_selected.iter().map(|path| path.as_str()).collect::<Vec<_>>().join(", ")));
    }

    let mut stash_oid = None;
    repo.stash_foreach(|index, _, found| { if index == stash_index { stash_oid = Some(*found); } true }).map_err(|error| error.message().to_string())?;
    let stash_oid = stash_oid.ok_or("That stash entry no longer exists")?;
    let stash_commit = repo.find_commit(stash_oid).map_err(|error| error.message().to_string())?;
    let untracked_commit = stash_commit.parent(2).ok();
    let untracked_tree = untracked_commit.as_ref().and_then(|commit| commit.tree().ok());

    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    for path in &selected {
        if untracked_tree.as_ref().is_some_and(|tree| tree.get_path(Path::new(path)).is_ok()) {
            if Path::new(&repository_path).join(path).exists() {
                return Err(format!("Cannot restore '{path}' because a file already exists there. The stash was kept unchanged."));
            }
            untracked.push(path.as_str());
        } else {
            tracked.push(path.as_str());
        }
    }
    drop(untracked_tree);
    drop(untracked_commit);
    drop(stash_commit);
    drop(repo);

    if !tracked.is_empty() {
        let source = format!("--source={stash_oid}");
        let mut args = vec!["restore", source.as_str(), "--worktree", "--"];
        args.extend(tracked);
        git(&repository_path, &args).map_err(|error| format!("Cannot restore the selected tracked file(s): {error}"))?;
    }
    if !untracked.is_empty() {
        let untracked_oid = internal_repository(&repository_path)?.find_commit(stash_oid).map_err(|error| error.message().to_string())?.parent_id(2).map_err(|_| "This stash has no saved untracked-file snapshot".to_string())?;
        let source = format!("--source={untracked_oid}");
        let mut args = vec!["restore", source.as_str(), "--worktree", "--"];
        args.extend(untracked);
        git(&repository_path, &args).map_err(|error| format!("Cannot restore the selected untracked file(s): {error}"))?;
    }
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn abort_stash_conflict(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "abort_stash_conflict", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let head_commit = repo.head().map_err(|error| error.message().to_string())?.peel_to_commit().map_err(|error| error.message().to_string())?;
    let mut checkout = git2::build::CheckoutBuilder::new(); checkout.force();
    repo.reset(head_commit.as_object(), git2::ResetType::Hard, Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn stash_entry_files(repository_path: String, stash_index: usize) -> Result<Vec<String>, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let mut target_oid = None;
    let mut repo_for_walk = internal_repository(&repository_path)?;
    repo_for_walk.stash_foreach(|index, _, oid| { if index == stash_index { target_oid = Some(*oid); } true }).map_err(|error| error.message().to_string())?;
    let stash_oid = target_oid.ok_or("That stash entry no longer exists")?;
    let stash_commit = repo.find_commit(stash_oid).map_err(|error| error.message().to_string())?;
    let stash_tree = stash_commit.tree().map_err(|error| error.message().to_string())?;
    let base_tree = stash_commit.parent(0).and_then(|commit| commit.tree()).ok();
    let mut paths = std::collections::BTreeSet::new();
    let diff = repo.diff_tree_to_tree(base_tree.as_ref(), Some(&stash_tree), None).map_err(|error| error.message().to_string())?;
    for delta in diff.deltas() { if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) { paths.insert(normalized(path)); } }
    if let Some(untracked_tree) = stash_commit.parent(2).ok().and_then(|commit| commit.tree().ok()) {
        let untracked_diff = repo.diff_tree_to_tree(None, Some(&untracked_tree), None).map_err(|error| error.message().to_string())?;
        for delta in untracked_diff.deltas() { if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) { paths.insert(normalized(path)); } }
    }
    Ok(paths.into_iter().collect())
}
