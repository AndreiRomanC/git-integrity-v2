use super::*;

#[derive(Serialize)]
pub struct RemoteInfo {
    name: String,
    fetch_url: String,
    push_url: String,
}

#[tauri::command]
pub fn list_remotes(repository_path: String) -> Result<Vec<RemoteInfo>, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let names = repo.remotes().map_err(|error| error.message().to_string())?;
    let mut result = Vec::new();
    for name in names.iter().flatten() {
        if let Ok(remote) = repo.find_remote(name) {
            result.push(RemoteInfo {
                name: name.to_string(),
                fetch_url: remote.url().unwrap_or("").to_string(),
                push_url: remote.pushurl().or_else(|| remote.url()).unwrap_or("").to_string(),
            });
        }
    }
    Ok(result)
}

#[tauri::command]
pub fn fetch_remote(repository_path: String, remote: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let remote_name = remote.trim();
    let repo = internal_repository(&repository_path)?;
    repo.find_remote(remote_name).map_err(|error| error.message().to_string())?;
    // Use the system Git credential setup already configured by the user.
    git(&repository_path, &["fetch", remote_name]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub async fn fetch_all_remotes(repository_path: String) -> Result<(), String> {
    off_main_thread(move || fetch_all_remotes_inner(repository_path)).await
}

pub(in crate::repository) fn fetch_all_remotes_inner(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    git(&repository_path, &["fetch", "--all"]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub async fn sync_repository(repository_path: String, action: String) -> Result<(), String> {
    off_main_thread(move || sync_repository_inner(repository_path, action)).await
}

pub(in crate::repository) fn sync_repository_inner(repository_path: String, action: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "sync_repository", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let head = repo.head().map_err(|error| error.message().to_string())?;
    let branch = head.shorthand().ok_or("Detached HEAD cannot be synchronized")?.to_string();
    let upstream = repo.find_branch(&branch, BranchType::Local)
        .and_then(|branch| branch.upstream())
        .map_err(|_| "The current branch has no upstream".to_string())?;
    let upstream_name = upstream.name().ok().flatten().ok_or("Invalid upstream")?.to_string();
    let (remote_name, remote_branch) = upstream_name.split_once('/').ok_or("Invalid upstream branch")?;
    drop(upstream);
    drop(head);

    match action.as_str() {
        "pull" => {
            // Pull is deliberately stricter than the system Git default here.
            // Advancing the branch ref first and only then asking libgit2 to
            // update the index/worktree can leave HEAD at the fetched commit
            // when checkout is refused (for example by a locally edited or
            // locked file).  Every incoming file then looks like a brand-new
            // local change.  Refuse a dirty tree before touching the branch;
            // the explicit merge workflow can preserve/stash local work.
            let dirty = internal_statuses(&repo, None)?;
            if !dirty.is_empty() {
                return Err(format!(
                    "Pull was not started because the working tree has {} pending change{}. Commit, stash, or discard the local work first. The current branch and files were not changed.",
                    dirty.len(),
                    if dirty.len() == 1 { "" } else { "s" }
                ));
            }
            fetch_remote(repository_path.clone(), remote_name.into())?;
            let remote_ref = repo.find_reference(&format!("refs/remotes/{remote_name}/{remote_branch}"))
                .map_err(|error| error.message().to_string())?;
            let target = remote_ref.target().ok_or("Remote branch has no target")?;
            let annotated = repo.find_annotated_commit(target).map_err(|error| error.message().to_string())?;
            let (analysis, _) = repo.merge_analysis(&[&annotated]).map_err(|error| error.message().to_string())?;
            if !analysis.is_fast_forward() && !analysis.is_up_to_date() {
                return Err("Pull requires a merge; only fast-forward pull is allowed".into());
            }
            if analysis.is_fast_forward() {
                // Let Git perform its own transactional fast-forward instead
                // of moving refs manually before checkout.  If checkout cannot
                // be completed, Git leaves the original branch/index/worktree
                // intact rather than exposing the incoming commit as local
                // modifications.
                git(&repository_path, &["merge", "--ff-only", &upstream_name])
                    .map_err(|detail| format!("Pull could not fast-forward the working tree; the current branch was left unchanged: {detail}"))?;
            }
        }
        "push" => {
            repo.find_remote(remote_name).map_err(|error| error.message().to_string())?;
            git(&repository_path, &["push", remote_name, &format!("{branch}:refs/heads/{remote_branch}")])
                .map_err(|detail| format!("Push failed: {detail}"))?;
        }
        _ => return Err("Unsupported synchronization action".into()),
    }
    invalidate_git_metadata(&repository_path);
    Ok(())
}
