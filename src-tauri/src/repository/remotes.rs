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
                let mut local = repo.find_reference(&format!("refs/heads/{branch}"))
                    .map_err(|error| error.message().to_string())?;
                local.set_target(target, "fast-forward pull").map_err(|error| error.message().to_string())?;
                repo.set_head(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
                let mut checkout = git2::build::CheckoutBuilder::new();
                checkout.safe();
                repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?;
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
