use super::super::*;

#[derive(Serialize, Clone)]
pub struct ConflictedFile {
    pub(in crate::repository) path: String,
    pub(in crate::repository) has_ours: bool,
    pub(in crate::repository) has_theirs: bool,
}

#[derive(Serialize)]
pub struct MergeOutcome {
    pub(in crate::repository) status: String,
    pub(in crate::repository) message: String,
    pub(in crate::repository) conflicts: Vec<ConflictedFile>,
}

#[derive(Serialize)]
pub struct ConflictSides {
    pub(in crate::repository) ancestor: Option<String>,
    pub(in crate::repository) ours: Option<String>,
    pub(in crate::repository) theirs: Option<String>,
}

fn blob_text(repo: &Repository, id: Option<git2::Oid>) -> Option<String> {
    let id = id?;
    let blob = repo.find_blob(id).ok()?;
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

fn gather_conflicts(repo: &Repository) -> Result<Vec<ConflictedFile>, String> {
    let index = repo.index().map_err(|error| error.message().to_string())?;
    let conflicts = index.conflicts().map_err(|error| error.message().to_string())?;
    let mut files = Vec::new();
    for conflict in conflicts.flatten() {
        let path = conflict.our.as_ref().or(conflict.their.as_ref()).or(conflict.ancestor.as_ref())
            .map(|entry| String::from_utf8_lossy(&entry.path).into_owned());
        if let Some(path) = path {
            files.push(ConflictedFile { path, has_ours: conflict.our.is_some(), has_theirs: conflict.their.is_some() });
        }
    }
    Ok(files)
}

// Works on either the parent repository or a submodule repository selected
// through target_path. The repository resolution stays shared with all other
// branch actions so commands cannot silently operate on the wrong scope.
#[tauri::command]
pub fn merge_branch(repository_path: String, target_path: String, source_ref: String) -> Result<MergeOutcome, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "merge_branch", queue_started.elapsed());
    let mut repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Clean {
        return Err("A merge (or other operation) is already in progress here. Resolve or abort it first.".into());
    }
    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() {
        return Err("There are uncommitted changes here. Commit or stash them first, so a merge can't mix them up with incoming changes.".into());
    }
    if repo.head_detached().unwrap_or(true) {
        return Err("This is a detached HEAD (not on a branch), so there is nothing to merge into.".into());
    }
    let current_branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).ok_or("Could not determine the current branch")?;
    let reference = repo.resolve_reference_from_short_name(source_ref.trim()).map_err(|_| format!("Could not find branch \"{source_ref}\" — fetch first if it's a remote branch."))?;
    let annotated = repo.reference_to_annotated_commit(&reference).map_err(|error| error.message().to_string())?;
    let (analysis, _) = repo.merge_analysis(&[&annotated]).map_err(|error| error.message().to_string())?;

    if analysis.is_up_to_date() {
        return Ok(MergeOutcome { status: "up_to_date".into(), message: format!("{current_branch} is already up to date with {source_ref}."), conflicts: vec![] });
    }
    if analysis.is_fast_forward() {
        let target = annotated.id();
        let mut local = repo.find_reference(&format!("refs/heads/{current_branch}")).map_err(|error| error.message().to_string())?;
        local.set_target(target, "fast-forward merge").map_err(|error| error.message().to_string())?;
        repo.set_head(&format!("refs/heads/{current_branch}")).map_err(|error| error.message().to_string())?;
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?;
        invalidate_git_metadata(&repository_path);
        return Ok(MergeOutcome { status: "fast_forwarded".into(), message: format!("Fast-forwarded {current_branch} to {source_ref}."), conflicts: vec![] });
    }

    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.allow_conflicts(true).conflict_style_merge(true).force();
    repo.merge(&[&annotated], None, Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    drop(annotated);
    drop(reference);

    let index = repo.index().map_err(|error| error.message().to_string())?;
    if index.has_conflicts() {
        let conflicts = gather_conflicts(&repo)?;
        let count = conflicts.len();
        return Ok(MergeOutcome { status: "conflicts".into(), message: format!("Merging {source_ref} produced {count} conflict{}. Resolve them, then complete the merge.", if count == 1 { "" } else { "s" }), conflicts });
    }

    drop(index);
    let oid = complete_merge_internal(&mut repo, &repository_path, &format!("Merge {source_ref} into {current_branch}"))?;
    Ok(MergeOutcome { status: "merged".into(), message: format!("Merged {source_ref} into {current_branch} ({}).", &oid[..8.min(oid.len())]), conflicts: vec![] })
}

#[tauri::command]
pub fn merge_in_progress(repository_path: String, target_path: String) -> Result<bool, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    Ok(repo.state() == git2::RepositoryState::Merge)
}

#[tauri::command]
pub fn list_conflicts(repository_path: String, target_path: String) -> Result<Vec<ConflictedFile>, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    gather_conflicts(&repo)
}

#[tauri::command]
pub fn conflict_sides(repository_path: String, target_path: String, relative_path: String) -> Result<ConflictSides, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative = normalized(&relative);
    let repo = internal_repository(&repository_path)?;
    let index = repo.index().map_err(|error| error.message().to_string())?;
    let conflicts = index.conflicts().map_err(|error| error.message().to_string())?;
    for conflict in conflicts.flatten() {
        let path = conflict.our.as_ref().or(conflict.their.as_ref()).or(conflict.ancestor.as_ref()).map(|entry| String::from_utf8_lossy(&entry.path).into_owned());
        if path.as_deref() != Some(relative.as_str()) { continue; }
        return Ok(ConflictSides {
            ancestor: blob_text(&repo, conflict.ancestor.as_ref().map(|entry| entry.id)),
            ours: blob_text(&repo, conflict.our.as_ref().map(|entry| entry.id)),
            theirs: blob_text(&repo, conflict.their.as_ref().map(|entry| entry.id)),
        });
    }
    Err(format!("{relative_path} is not a conflicted file"))
}

#[tauri::command]
pub fn resolve_conflict(repository_path: String, target_path: String, relative_path: String, resolution: String) -> Result<(), String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "resolve_conflict", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    match resolution.as_str() {
        "ours" | "theirs" => {
            let index = repo.index().map_err(|error| error.message().to_string())?;
            let conflicts = index.conflicts().map_err(|error| error.message().to_string())?;
            let target = normalized(&relative);
            let mut content = None;
            for conflict in conflicts.flatten() {
                let entry = if resolution == "ours" { conflict.our } else { conflict.their };
                let Some(entry) = entry else { continue };
                if String::from_utf8_lossy(&entry.path) != target { continue; }
                content = blob_text(&repo, Some(entry.id));
                break;
            }
            let content = content.ok_or_else(|| format!("No {resolution} version exists for {relative_path} (it may have been added only on one side — deleting or keeping the existing file may be more appropriate)."))?;
            fs::write(&absolute, content).map_err(|error| format!("Cannot write {}: {error}", absolute.display()))?;
        }
        "manual" => {
            if !absolute.exists() { return Err(format!("{relative_path} does not exist on disk — nothing to mark resolved.")); }
        }
        other => return Err(format!("Unknown resolution kind \"{other}\"")),
    }
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    index.add_path(&relative).map_err(|error| error.message().to_string())?;
    index.write().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

fn complete_merge_internal(repo: &mut Repository, repository_path: &str, message: &str) -> Result<String, String> {
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    if index.has_conflicts() { return Err("There are still unresolved conflicts.".into()); }
    let mut merge_heads = Vec::new();
    repo.mergehead_foreach(|oid| { merge_heads.push(*oid); true }).map_err(|error| error.message().to_string())?;
    let head_commit = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let mut parents = Vec::new();
    if let Some(commit) = head_commit.as_ref() { parents.push(commit.clone()); }
    for oid in &merge_heads { if let Ok(commit) = repo.find_commit(*oid) { parents.push(commit); } }
    let tree_id = index.write_tree_to(repo).map_err(|error| error.message().to_string())?;
    let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?;
    let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?;
    let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
    let oid = repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &parent_refs).map_err(|error| error.message().to_string())?;
    repo.cleanup_state().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(repository_path);
    Ok(oid.to_string())
}

#[tauri::command]
pub fn complete_merge(repository_path: String, target_path: String, message: String) -> Result<String, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Merge commit message cannot be empty".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "complete_merge", queue_started.elapsed());
    let mut repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Merge {
        return Err("There is no merge in progress here.".into());
    }
    complete_merge_internal(&mut repo, &repository_path, message.trim())
}

#[tauri::command]
pub fn abort_merge(repository_path: String, target_path: String) -> Result<(), String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "abort_merge", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Merge {
        return Err("There is no merge in progress here.".into());
    }
    let head_commit = repo.head().map_err(|error| error.message().to_string())?.peel_to_commit().map_err(|error| error.message().to_string())?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.reset(head_commit.as_object(), git2::ResetType::Hard, Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    repo.cleanup_state().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}
