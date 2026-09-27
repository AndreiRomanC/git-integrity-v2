use super::super::*;

#[derive(Serialize, Clone)]
pub struct ConflictedFile {
    pub(in crate::repository) path: String,
    pub(in crate::repository) has_ours: bool,
    pub(in crate::repository) has_theirs: bool,
    pub(in crate::repository) kind: String,
}

#[derive(Serialize)]
pub struct MergeOutcome {
    pub(in crate::repository) status: String,
    pub(in crate::repository) message: String,
    pub(in crate::repository) conflicts: Vec<ConflictedFile>,
}

#[derive(Serialize, Clone)]
pub struct SubmoduleMergeReviewItem {
    pub(in crate::repository) path: String,
    pub(in crate::repository) base: Option<String>,
    pub(in crate::repository) current: Option<String>,
    pub(in crate::repository) incoming: Option<String>,
    pub(in crate::repository) result: Option<String>,
    pub(in crate::repository) local_checkout: Option<String>,
    pub(in crate::repository) status: String,
    pub(in crate::repository) reason: String,
    pub(in crate::repository) suspicious: bool,
}

#[derive(Serialize, Clone)]
pub struct SubmoduleMergeReview {
    pub(in crate::repository) items: Vec<SubmoduleMergeReviewItem>,
    pub(in crate::repository) warnings: usize,
    pub(in crate::repository) summary: String,
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

fn contains_conflict_markers(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let mut saw_start = false;
    let mut saw_separator = false;
    for line in text.lines() {
        if line.starts_with("<<<<<<< ") {
            saw_start = true;
            saw_separator = false;
        } else if saw_start && line.starts_with("=======") {
            saw_separator = true;
        } else if saw_start && saw_separator && line.starts_with(">>>>>>> ") {
            return true;
        }
    }
    false
}

const GIT_FILEMODE_COMMIT: u32 = 0o160000;
const GIT_INDEX_ENTRY_STAGEMASK: u16 = 0x3000;

fn short_oid(oid: Option<git2::Oid>) -> String {
    oid.map(|oid| oid.to_string()[..8].to_string()).unwrap_or_else(|| "none".into())
}

fn oid_string(oid: Option<git2::Oid>) -> Option<String> { oid.map(|oid| oid.to_string()) }

fn tree_gitlink_map(tree: &git2::Tree<'_>) -> HashMap<String, git2::Oid> {
    let mut links = HashMap::new();
    let _ = tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if entry.filemode() == GIT_FILEMODE_COMMIT as i32 {
            if let Some(name) = entry.name() {
                links.insert(format!("{root}{name}"), entry.id());
            }
        }
        git2::TreeWalkResult::Ok
    });
    links
}

fn index_gitlink_map(repo: &Repository) -> Result<HashMap<String, git2::Oid>, String> {
    let index = repo.index().map_err(|error| error.message().to_string())?;
    Ok(index.iter()
        .filter(|entry| entry.mode == GIT_FILEMODE_COMMIT && (entry.flags & GIT_INDEX_ENTRY_STAGEMASK) == 0)
        .map(|entry| (String::from_utf8_lossy(&entry.path).into_owned(), entry.id))
        .collect())
}

fn initialized_submodule_dirty_paths(repository_path: &str, repo: &Repository) -> Vec<(String, usize)> {
    let mut dirty = Vec::new();
    let Ok(submodules) = repo.submodules() else { return dirty; };
    for submodule in submodules {
        let Some(path) = submodule.path().to_str().map(|value| value.replace('\\', "/")) else { continue; };
        let absolute = Path::new(repository_path).join(&path);
        let Ok(sub_repo) = internal_submodule_repository(&absolute) else { continue; };
        let count = internal_statuses(&sub_repo, None).map(|statuses| statuses.len()).unwrap_or(0);
        if count > 0 { dirty.push((path, count)); }
    }
    dirty
}

fn graph_has_commit(repo: &Repository, oid: git2::Oid) -> bool {
    repo.find_commit(oid).is_ok()
}

fn is_submodule_ancestor(repo: &Repository, ancestor: git2::Oid, descendant: git2::Oid) -> Option<bool> {
    if ancestor == descendant { return Some(true); }
    if !graph_has_commit(repo, ancestor) || !graph_has_commit(repo, descendant) { return None; }
    repo.graph_descendant_of(descendant, ancestor).ok()
}

fn classify_submodule_merge_item(
    repository_path: &str,
    path: &str,
    base: Option<git2::Oid>,
    current: Option<git2::Oid>,
    incoming: Option<git2::Oid>,
    result: Option<git2::Oid>,
) -> (String, String, bool, Option<String>) {
    let absolute = Path::new(repository_path).join(path);
    let sub_repo = internal_submodule_repository(&absolute).ok();
    let local_checkout = sub_repo.as_ref()
        .and_then(|repo| repo.head().ok())
        .and_then(|head| head.target())
        .map(|oid| oid.to_string());

    let local_mismatch = match (local_checkout.as_deref(), result) {
        (Some(local), Some(result)) => local != result.to_string(),
        _ => false,
    };
    let incoming_ahead_current = sub_repo.as_ref().and_then(|repo| match (current, incoming) {
        (Some(current), Some(incoming)) => is_submodule_ancestor(repo, current, incoming),
        _ => None,
    }).unwrap_or(false) && current != incoming;
    let current_ahead_incoming = sub_repo.as_ref().and_then(|repo| match (current, incoming) {
        (Some(current), Some(incoming)) => is_submodule_ancestor(repo, incoming, current),
        _ => None,
    }).unwrap_or(false) && current != incoming;
    let result_behind_incoming = sub_repo.as_ref().and_then(|repo| match (result, incoming) {
        (Some(result), Some(incoming)) => is_submodule_ancestor(repo, result, incoming),
        _ => None,
    }).unwrap_or(false) && result != incoming;
    let result_behind_current = sub_repo.as_ref().and_then(|repo| match (result, current) {
        (Some(result), Some(current)) => is_submodule_ancestor(repo, result, current),
        _ => None,
    }).unwrap_or(false) && result != current;
    let current_and_incoming_diverged = sub_repo.as_ref().and_then(|repo| match (current, incoming) {
        (Some(current), Some(incoming)) => {
            let current_contains_incoming = is_submodule_ancestor(repo, incoming, current)?;
            let incoming_contains_current = is_submodule_ancestor(repo, current, incoming)?;
            Some(!current_contains_incoming && !incoming_contains_current)
        }
        _ => None,
    }).unwrap_or(false);

    if result == incoming && result.is_some() {
        let mut reason = format!("Merge result follows incoming/origin version {}.", short_oid(incoming));
        if local_mismatch {
            reason.push_str(&format!(" Local checkout is still at {}, so run submodule update when ready.", local_checkout.as_deref().unwrap_or("unknown")));
        }
        return ("OK".into(), reason, local_mismatch, local_checkout);
    }
    if result == current && result.is_some() {
        if incoming_ahead_current {
            return (
                "Review recommended".into(),
                format!("Incoming/origin is ahead in this submodule history ({} → {}), but Git kept the current branch pointer {}.", short_oid(current), short_oid(incoming), short_oid(result)),
                true,
                local_checkout,
            );
        }
        if current_and_incoming_diverged {
            return (
                "Diverged history".into(),
                format!("Current branch ({}) and incoming/origin ({}) are on diverged submodule histories. Git kept the current branch pointer.", short_oid(current), short_oid(incoming)),
                true,
                local_checkout,
            );
        }
        let mut reason = "Merge result keeps the current branch submodule pointer.".to_string();
        if local_mismatch {
            reason.push_str(&format!(" Local checkout is {}, while the selected pointer is {}.", local_checkout.as_deref().unwrap_or("unknown"), short_oid(result)));
        }
        return ("OK".into(), reason, local_mismatch, local_checkout);
    }
    if result.is_some() && result != current && result != incoming {
        return (
            "Unexpected merge result".into(),
            format!("Merge result {} differs from both current branch ({}) and incoming/origin ({}). Review before committing.", short_oid(result), short_oid(current), short_oid(incoming)),
            true,
            local_checkout,
        );
    }
    if result_behind_incoming || result_behind_current {
        return (
            "Possible rollback".into(),
            format!("Merge result {} appears to be an ancestor of another selected side (current {}, incoming {}).", short_oid(result), short_oid(current), short_oid(incoming)),
            true,
            local_checkout,
        );
    }
    if current_and_incoming_diverged {
        return (
            "Diverged history".into(),
            format!("Current branch ({}) and incoming/origin ({}) diverged in this submodule.", short_oid(current), short_oid(incoming)),
            true,
            local_checkout,
        );
    }
    if current_ahead_incoming {
        return (
            "Current branch ahead".into(),
            format!("Current branch points to a descendant ({}) of incoming/origin ({}).", short_oid(current), short_oid(incoming)),
            false,
            local_checkout,
        );
    }
    (
        "Review recommended".into(),
        format!("Submodule pointer changed across the merge: base {}, current {}, incoming {}, result {}.", short_oid(base), short_oid(current), short_oid(incoming), short_oid(result)),
        true,
        local_checkout,
    )
}

fn submodule_merge_review_inner(repo: &mut Repository, repository_path: &str) -> Result<SubmoduleMergeReview, String> {
    if repo.state() != git2::RepositoryState::Merge {
        return Ok(SubmoduleMergeReview { items: vec![], warnings: 0, summary: "No merge is in progress.".into() });
    }
    let mut merge_heads = Vec::new();
    repo.mergehead_foreach(|oid| { merge_heads.push(*oid); true }).map_err(|error| error.message().to_string())?;
    let current_commit = repo.head().map_err(|error| error.message().to_string())?.peel_to_commit().map_err(|error| error.message().to_string())?;
    let incoming_oid = merge_heads.first().copied().ok_or_else(|| "Merge is in progress, but MERGE_HEAD is missing.".to_string())?;
    let incoming_commit = repo.find_commit(incoming_oid).map_err(|error| error.message().to_string())?;
    let base_oid = repo.merge_base(current_commit.id(), incoming_commit.id()).ok();
    let current_links = tree_gitlink_map(&current_commit.tree().map_err(|error| error.message().to_string())?);
    let incoming_links = tree_gitlink_map(&incoming_commit.tree().map_err(|error| error.message().to_string())?);
    let base_links = base_oid.and_then(|oid| repo.find_commit(oid).ok()).and_then(|commit| commit.tree().ok()).map(|tree| tree_gitlink_map(&tree)).unwrap_or_default();
    let result_links = index_gitlink_map(repo)?;

    let mut paths: Vec<String> = base_links.keys()
        .chain(current_links.keys())
        .chain(incoming_links.keys())
        .chain(result_links.keys())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    paths.sort();

    let mut items = Vec::new();
    for path in paths {
        let base = base_links.get(&path).copied();
        let current = current_links.get(&path).copied();
        let incoming = incoming_links.get(&path).copied();
        let result = result_links.get(&path).copied();
        let unique: HashSet<Option<git2::Oid>> = [base, current, incoming, result].into_iter().collect();
        if unique.len() <= 1 { continue; }
        let (status, reason, suspicious, local_checkout) = classify_submodule_merge_item(repository_path, &path, base, current, incoming, result);
        items.push(SubmoduleMergeReviewItem {
            path,
            base: oid_string(base),
            current: oid_string(current),
            incoming: oid_string(incoming),
            result: oid_string(result),
            local_checkout,
            status,
            reason,
            suspicious,
        });
    }
    let warnings = items.iter().filter(|item| item.suspicious).count();
    let summary = if items.is_empty() {
        "No submodule pointer changes were detected in this merge.".into()
    } else {
        format!("{} submodule pointer{} to review · {} warning{}", items.len(), if items.len() == 1 { "" } else { "s" }, warnings, if warnings == 1 { "" } else { "s" })
    };
    Ok(SubmoduleMergeReview { items, warnings, summary })
}

fn gather_conflicts(repo: &Repository) -> Result<Vec<ConflictedFile>, String> {
    let index = repo.index().map_err(|error| error.message().to_string())?;
    let conflicts = index.conflicts().map_err(|error| error.message().to_string())?;
    let mut files = Vec::new();
    for conflict in conflicts.flatten() {
        let path = conflict.our.as_ref().or(conflict.their.as_ref()).or(conflict.ancestor.as_ref())
            .map(|entry| String::from_utf8_lossy(&entry.path).into_owned());
        if let Some(path) = path {
            let kind = if conflict.our.as_ref().or(conflict.their.as_ref()).or(conflict.ancestor.as_ref()).map(|entry| entry.mode) == Some(GIT_FILEMODE_COMMIT) {
                "submodule"
            } else {
                "file"
            };
            files.push(ConflictedFile { path, has_ours: conflict.our.is_some(), has_theirs: conflict.their.is_some(), kind: kind.into() });
        }
    }
    Ok(files)
}

// Works on either the parent repository or a submodule repository selected
// through target_path. The repository resolution stays shared with all other
// branch actions so commands cannot silently operate on the wrong scope.
#[tauri::command]
pub fn merge_branch(repository_path: String, target_path: String, source_ref: String) -> Result<MergeOutcome, String> {
    let target_is_submodule = !target_path.trim().is_empty();
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
    let dirty_submodules = if target_is_submodule { Vec::new() } else { initialized_submodule_dirty_paths(&repository_path, &repo) };
    if !dirty.is_empty() || !dirty_submodules.is_empty() {
        let mut details = Vec::new();
        if !dirty.is_empty() {
            details.push(format!("Main Repository has {} uncommitted change{}.", dirty.len(), if dirty.len() == 1 { "" } else { "s" }));
        }
        if !dirty_submodules.is_empty() {
            let shown = dirty_submodules.iter().take(8)
                .map(|(path, count)| format!("- {path} ({count} local change{})", if *count == 1 { "" } else { "s" }))
                .collect::<Vec<_>>()
                .join("\n");
            let more = if dirty_submodules.len() > 8 { format!("\n- …and {} more", dirty_submodules.len() - 8) } else { String::new() };
            details.push(format!("Initialized submodules with local changes:\n{shown}{more}"));
        }
        return Err(format!("Cannot start the merge while local changes exist. Commit, stash or discard them first so the merge cannot overwrite local work.\n\n{}", details.join("\n\n")));
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
    if !target_is_submodule {
        let review = submodule_merge_review_inner(&mut repo, &repository_path)?;
        if !review.items.is_empty() {
            return Ok(MergeOutcome {
                status: "submodule_review".into(),
                message: format!("Merge prepared. Review {} submodule pointer{} before creating the merge commit.", review.items.len(), if review.items.len() == 1 { "" } else { "s" }),
                conflicts: vec![],
            });
        }
    }
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
pub fn submodule_merge_review(repository_path: String, target_path: String) -> Result<SubmoduleMergeReview, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let mut repo = internal_repository(&repository_path)?;
    submodule_merge_review_inner(&mut repo, &repository_path)
}

#[tauri::command]
pub fn apply_submodule_merge_revision(repository_path: String, target_path: String, relative_path: String, revision: String) -> Result<(), String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    let revision = revision.trim();
    if revision.is_empty() { return Err("Choose a concrete submodule commit SHA first.".into()); }
    let oid = git2::Oid::from_str(revision).map_err(|_| format!("Invalid submodule revision \"{revision}\""))?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "apply_submodule_merge_revision", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Merge {
        return Err("There is no merge in progress here.".into());
    }
    let submodule_path = Path::new(&repository_path).join(&relative);
    if let Ok(sub_repo) = internal_submodule_repository(&submodule_path) {
        if sub_repo.find_commit(oid).is_err() {
            return Err(format!("{} does not have commit {} locally. Fetch the submodule first or choose a reachable revision.", relative_string, &oid.to_string()[..8]));
        }
    }
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    let mut entry = index.get_path(&relative, 0).unwrap_or_else(|| git2::IndexEntry {
        ctime: git2::IndexTime::new(0, 0),
        mtime: git2::IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: GIT_FILEMODE_COMMIT,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: oid,
        flags: 0,
        flags_extended: 0,
        path: relative_string.as_bytes().to_vec(),
    });
    entry.id = oid;
    entry.mode = GIT_FILEMODE_COMMIT;
    entry.flags &= !GIT_INDEX_ENTRY_STAGEMASK;
    index.add(&entry).map_err(|error| error.message().to_string())?;
    index.write().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
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
pub async fn open_merge_tool(repository_path: String, target_path: String, relative_path: String) -> Result<String, String> {
    off_main_thread(move || open_merge_tool_inner(repository_path, target_path, relative_path)).await
}

fn open_merge_tool_inner(repository_path: String, target_path: String, relative_path: String) -> Result<String, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative = normalized(&relative);
    let configured_tool = Command::new("git")
        .args(["config", "--get", "merge.tool"])
        .current_dir(&repository_path)
        .output()
        .ok()
        .and_then(|output| output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
        .filter(|tool| !tool.is_empty());
    let output = Command::new("git")
        .args(["mergetool", "--no-prompt", "--", &relative])
        .current_dir(&repository_path)
        .output()
        .map_err(|error| format!("Could not start git mergetool: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(if detail.is_empty() {
            "Git mergetool failed. Configure one with `git config merge.tool <tool>` or check `git mergetool --tool-help`.".into()
        } else { detail });
    }
    invalidate_git_metadata(&repository_path);
    Ok(configured_tool.map(|tool| format!("Merge tool \"{tool}\" finished for {relative}."))
        .unwrap_or_else(|| format!("Git mergetool finished for {relative}.")))
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
            let target = normalized(&relative);
            let mut gitlink_entry = None;
            let mut gitlink_oid = None;
            let content = {
                let index = repo.index().map_err(|error| error.message().to_string())?;
                let conflicts = index.conflicts().map_err(|error| error.message().to_string())?;
                let mut content = None;
                for conflict in conflicts.flatten() {
                    let entry = if resolution == "ours" { conflict.our } else { conflict.their };
                    let Some(mut entry) = entry else { continue };
                    if String::from_utf8_lossy(&entry.path) != target { continue; }
                    if entry.mode == GIT_FILEMODE_COMMIT {
                        gitlink_oid = Some(entry.id);
                        entry.flags &= !GIT_INDEX_ENTRY_STAGEMASK;
                        gitlink_entry = Some(entry);
                    } else {
                        content = blob_text(&repo, Some(entry.id));
                    }
                    break;
                }
                content
            };
            if let Some(entry) = gitlink_entry {
                let mut index = repo.index().map_err(|error| error.message().to_string())?;
                index.conflict_remove(&relative).map_err(|error| error.message().to_string())?;
                index.add(&entry).map_err(|error| error.message().to_string())?;
                index.write().map_err(|error| error.message().to_string())?;
                if let Some(oid) = gitlink_oid {
                    let submodule_path = Path::new(&repository_path).join(&relative);
                    if internal_submodule_repository(&submodule_path).is_ok() {
                        let submodule_path_string = submodule_path.to_string_lossy().into_owned();
                        git(&submodule_path_string, &["checkout", "--detach", &oid.to_string()])?;
                        invalidate_git_metadata(&submodule_path_string);
                    }
                }
                invalidate_git_metadata(&repository_path);
                return Ok(());
            }
            let content = content.ok_or_else(|| format!("No {resolution} version exists for {relative_path} (it may have been added only on one side — deleting or keeping the existing file may be more appropriate)."))?;
            fs::write(&absolute, content).map_err(|error| format!("Cannot write {}: {error}", absolute.display()))?;
        }
        "manual" => {
            if !absolute.exists() { return Err(format!("{relative_path} does not exist on disk — nothing to mark resolved.")); }
            let content = fs::read(&absolute).map_err(|error| format!("Cannot read {}: {error}", absolute.display()))?;
            if contains_conflict_markers(&content) {
                return Err(format!("{relative_path} still contains conflict markers (<<<<<<< / ======= / >>>>>>>). Save the resolved content first, then Mark resolved or Recheck conflicts."));
            }
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
