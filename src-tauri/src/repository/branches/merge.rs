use super::super::*;
use super::native_merge as native;

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
    pub(in crate::repository) base_is_ambiguous: bool,
    pub(in crate::repository) current: Option<String>,
    pub(in crate::repository) incoming: Option<String>,
    pub(in crate::repository) result: Option<String>,
    pub(in crate::repository) local_checkout: Option<String>,
    pub(in crate::repository) status: String,
    pub(in crate::repository) reason: String,
    pub(in crate::repository) suspicious: bool,
    pub(in crate::repository) registration_issue: bool,
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

fn short_oid(oid: Option<git2::Oid>) -> String {
    oid.map(|oid| oid.to_string()[..8].to_string())
        .unwrap_or_else(|| "none".into())
}

fn pointer_label(oid: Option<git2::Oid>) -> String {
    oid.map(|oid| oid.to_string()[..8].to_string())
        .unwrap_or_else(|| "no submodule".into())
}

fn oid_string(oid: Option<git2::Oid>) -> Option<String> {
    oid.map(|oid| oid.to_string())
}

fn validate_merge_gitlinks(repository_path: &str, index: &native::Index) -> Result<(), String> {
    let started = Instant::now();
    let missing = native::unregistered(repository_path, index)?;
    perf_log(
        &format!(
            "merge_integrity: engine=git_cli repo={} gitlinks={} unregistered={} result={}",
            anonymized_repository_id(repository_path),
            index.links().len(),
            missing.len(),
            if missing.is_empty() {
                "accepted"
            } else {
                "blocked"
            }
        ),
        started.elapsed(),
    );
    if missing.is_empty() {
        return Ok(());
    }
    for path in missing.iter().take(12) {
        perf_log(
            &format!(
                "merge_integrity: repo={} unregistered_path={path:?}",
                anonymized_repository_id(repository_path)
            ),
            Duration::ZERO,
        );
    }
    Err(format!("Merge not committed: submodule pointers remain without a matching path in the staged .gitmodules:\n{}\n\nChoose No submodule / Remove gitlink in merge review, or restore the matching registration. Update/init cannot repair this mismatch. No commit or automatic deletion was made.",missing.iter().take(12).map(|p|format!("- {p}")).collect::<Vec<_>>().join("\n")))
}
fn is_submodule_ancestor(root: &str, a: Oid, b: Oid) -> Option<bool> {
    if a == b {
        return Some(true);
    }
    native::ancestor(root, a, b).ok()
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
    let sub_repo = absolute
        .join(".git")
        .exists()
        .then(|| absolute.to_string_lossy().into_owned());
    let local_checkout = sub_repo
        .as_ref()
        .and_then(|root| native::oid(root, "HEAD").ok())
        .map(|oid| oid.to_string());

    let local_mismatch = match (local_checkout.as_deref(), result) {
        (Some(local), Some(result)) => local != result.to_string(),
        _ => false,
    };
    if result == current && base == incoming && current != incoming {
        let mut reason = format!(
            "Incoming/origin did not change this submodule since the merge base. Prepared result chose Current Branch: {}.",
            pointer_label(current),
        );
        if local_mismatch {
            reason.push_str(&format!(
                " Folder on disk is {}, while the selected pointer is {}.",
                local_checkout.as_deref().unwrap_or("unknown"),
                pointer_label(result)
            ));
        }
        if current.is_none() && incoming.is_some() {
            reason.push_str(&format!(
                " Use Incoming/origin only if you want to restore {}.",
                pointer_label(incoming)
            ));
        }
        return (
            "Kept current".into(),
            reason,
            local_mismatch,
            local_checkout,
        );
    }
    if result == incoming && base == current && current != incoming {
        let mut reason = format!(
            "Current Branch did not change this submodule since the merge base. Prepared result chose Incoming/origin: {}.",
            pointer_label(incoming),
        );
        if local_mismatch {
            reason.push_str(&format!(
                " Folder on disk is still at {}, so run submodule update when ready.",
                local_checkout.as_deref().unwrap_or("unknown")
            ));
        }
        if incoming.is_none() && current.is_some() {
            reason.push_str(" The incoming side removes the submodule pointer.");
        }
        return (
            "Incoming applied".into(),
            reason,
            local_mismatch,
            local_checkout,
        );
    }
    if result == incoming {
        let mut reason = format!(
            "Prepared result chose Incoming/origin: {}.",
            pointer_label(incoming)
        );
        if base == incoming && current != incoming {
            reason.push_str(&format!(
                " Current Branch was {}, so review this if you expected to keep your branch state.",
                pointer_label(current)
            ));
            return ("Review recommended".into(), reason, true, local_checkout);
        }
        if local_mismatch {
            reason.push_str(&format!(
                " Folder on disk is still at {}, so run submodule update when ready.",
                local_checkout.as_deref().unwrap_or("unknown")
            ));
        }
        return (
            "Incoming applied".into(),
            reason,
            local_mismatch,
            local_checkout,
        );
    }
    let forward = sub_repo
        .as_ref()
        .and_then(|repo| match (current, incoming) {
            (Some(a), Some(b)) if a != b => is_submodule_ancestor(repo, a, b),
            _ => None,
        });
    let backward = sub_repo
        .as_ref()
        .and_then(|repo| match (current, incoming) {
            (Some(a), Some(b)) if a != b => is_submodule_ancestor(repo, b, a),
            _ => None,
        });
    let incoming_ahead_current = forward == Some(true);
    let current_ahead_incoming = backward == Some(true);
    let current_and_incoming_diverged = forward == Some(false) && backward == Some(false);
    if result == current {
        if incoming_ahead_current {
            return (
                "Review recommended".into(),
                format!("Incoming/origin is ahead in this submodule history ({} → {}), but the prepared result kept the Current Branch pointer {}.", short_oid(current), short_oid(incoming), short_oid(result)),
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
        let mut reason = format!(
            "Prepared result chose Current Branch: {}.",
            pointer_label(current)
        );
        if base == current && incoming != current {
            reason.push_str(&format!(
                " Incoming/origin changed this pointer to {}, so review before committing.",
                pointer_label(incoming)
            ));
            return ("Review recommended".into(), reason, true, local_checkout);
        }
        if local_mismatch {
            reason.push_str(&format!(
                " Folder on disk is {}, while the selected pointer is {}.",
                local_checkout.as_deref().unwrap_or("unknown"),
                short_oid(result)
            ));
        }
        return (
            "Kept current".into(),
            reason,
            local_mismatch,
            local_checkout,
        );
    }
    if result.is_some() && result != current && result != incoming {
        return (
            "Unexpected merge result".into(),
            format!("Prepared result {} differs from both Current Branch ({}) and Incoming/origin ({}). Review before committing.", short_oid(result), short_oid(current), short_oid(incoming)),
            true,
            local_checkout,
        );
    }
    if current_and_incoming_diverged {
        return (
            "Diverged history".into(),
            format!(
                "Current branch ({}) and incoming/origin ({}) diverged in this submodule.",
                short_oid(current),
                short_oid(incoming)
            ),
            true,
            local_checkout,
        );
    }
    if current_ahead_incoming {
        return (
            "Current branch ahead".into(),
            format!(
                "Current branch points to a descendant ({}) of incoming/origin ({}).",
                short_oid(current),
                short_oid(incoming)
            ),
            false,
            local_checkout,
        );
    }
    (
        "Review recommended".into(),
        format!("Submodule pointer changed across the merge: base {}, Current Branch {}, Incoming/origin {}, Prepared result {}.", pointer_label(base), pointer_label(current), pointer_label(incoming), pointer_label(result)),
        true,
        local_checkout,
    )
}

fn submodule_merge_review_inner(repository_path: &str) -> Result<SubmoduleMergeReview, String> {
    if !native::in_progress(repository_path)? {
        return Ok(SubmoduleMergeReview {
            items: vec![],
            warnings: 0,
            summary: "No merge is in progress.".into(),
        });
    }
    let current_oid = native::oid(repository_path, "HEAD")?;
    let incoming_oid = *native::heads(repository_path)?
        .first()
        .ok_or("MERGE_HEAD is missing")?;
    let bases = native::bases(repository_path, current_oid, incoming_oid)?;
    let current_links = native::tree(repository_path, &current_oid.to_string())?.links();
    let incoming_links = native::tree(repository_path, &incoming_oid.to_string())?.links();
    let base_links = if bases.len() == 1 {
        native::tree(repository_path, &bases[0].to_string())?.links()
    } else {
        HashMap::new()
    };
    let index = native::index(repository_path)?;
    let result_links = index.links();
    let unregistered: HashSet<_> = native::unregistered(repository_path, &index)?
        .into_iter()
        .collect();
    let mut paths: Vec<String> = base_links
        .keys()
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
        let unique: HashSet<Option<git2::Oid>> =
            [base, current, incoming, result].into_iter().collect();
        let registration_issue = unregistered.contains(&path);
        if (unique.len() <= 1 || (bases.len() != 1 && current == incoming && incoming == result))
            && !registration_issue
        {
            continue;
        }
        let (mut status, mut reason, mut suspicious, local_checkout) =
            classify_submodule_merge_item(repository_path, &path, base, current, incoming, result);
        if bases.len() != 1 {
            status = "Review recommended".into();
            suspicious = true;
            reason=format!("Multiple common bases; Git computed a virtual base. Prepared pointer: {}. Review the current and incoming versions before committing.",pointer_label(result));
        }
        if registration_issue {
            status = "Missing .gitmodules entry".into();
            reason.push_str(" The prepared gitlink has no matching path in the staged .gitmodules. Choose No submodule / Remove gitlink if removed, or restore its registration if it must stay. Update/init cannot fix this mismatch; completing the merge is blocked until resolved.");
            suspicious = true;
        }
        perf_log(&format!("merge_review: repo={} path={path:?} base={} current={} incoming={} result={} registration_issue={registration_issue}",
            anonymized_repository_id(repository_path), short_oid(base), short_oid(current), short_oid(incoming), short_oid(result)), Duration::ZERO);
        items.push(SubmoduleMergeReviewItem {
            path,
            base: oid_string(base),
            base_is_ambiguous: bases.len() != 1,
            current: oid_string(current),
            incoming: oid_string(incoming),
            result: oid_string(result),
            local_checkout,
            status,
            reason,
            suspicious,
            registration_issue,
        });
    }
    let warnings = items.iter().filter(|item| item.suspicious).count();
    let summary = if items.is_empty() {
        "No submodule pointer changes were detected in this merge.".into()
    } else {
        format!(
            "{} submodule pointer change{} · {} need review",
            items.len(),
            if items.len() == 1 { "" } else { "s" },
            warnings
        )
    };
    Ok(SubmoduleMergeReview {
        items,
        warnings,
        summary,
    })
}

fn gather_conflicts(repository_path: &str) -> Result<Vec<ConflictedFile>, String> {
    let index = native::index(repository_path)?;
    Ok(index
        .conflict_paths()
        .into_iter()
        .map(|path| {
            let kind = if (1..=3).any(|stage| {
                index
                    .get(&path, stage)
                    .is_some_and(|e| e.mode == GIT_FILEMODE_COMMIT)
            }) {
                "submodule"
            } else {
                "file"
            };
            ConflictedFile {
                has_ours: index.get(&path, 2).is_some(),
                has_theirs: index.get(&path, 3).is_some(),
                path,
                kind: kind.into(),
            }
        })
        .collect())
}

// Works on either the parent repository or a submodule repository selected
// through target_path. The repository resolution stays shared with all other
// branch actions so commands cannot silently operate on the wrong scope.
#[tauri::command]
pub async fn merge_branch(
    repository_path: String,
    target_path: String,
    source_ref: String,
) -> Result<MergeOutcome, String> {
    off_main_thread(move || merge_branch_inner(repository_path, target_path, source_ref)).await
}

pub(in crate::repository) fn merge_branch_inner(
    repository_path: String,
    target_path: String,
    source_ref: String,
) -> Result<MergeOutcome, String> {
    let target_is_submodule = !target_path.trim().is_empty();
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "merge_branch", queue_started.elapsed());
    let plan = native::plan(&repository_path, source_ref.trim(), None)?;
    let current_branch = plan
        .branch
        .as_ref()
        .ok_or("This is a detached HEAD; create or switch to a local branch before merging.")?;
    let dirty = native::bytes(
        &repository_path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
    )?;
    if !dirty.is_empty() {
        return Err(format!("Cannot start the merge while local changes exist. Main Repository / Initialized submodules with local changes:\n{}\nCommit, stash or discard them first.",String::from_utf8_lossy(&dirty).replace('\0',"\n")));
    }
    perf_log(
        &format!(
            "merge_start: engine=git_cli repo={} current={} incoming={} bases={:?}",
            anonymized_repository_id(&repository_path),
            plan.current,
            plan.incoming,
            plan.bases
        ),
        Duration::ZERO,
    );
    if plan.status == "up_to_date" {
        return Ok(MergeOutcome {
            status: "up_to_date".into(),
            message: format!("{current_branch} is already up to date with {source_ref}."),
            conflicts: vec![],
        });
    }
    // Pin commits, not a ref that can move during the operation. Fast-forward
    // stays a fast-forward; --no-commit alone cannot pause one for review.
    if plan.status == "fast_forward" {
        validate_merge_gitlinks(&repository_path, &plan.result)?;
    }
    if native::oid(&repository_path, "HEAD")? != plan.current
        || native::branch(&repository_path)? != plan.branch
        || native::oid(&repository_path, source_ref.trim())? != plan.incoming
    {
        return Err("A branch changed before the merge. Refresh and preview again.".into());
    }
    let incoming = plan.incoming.to_string();
    let args = if plan.status == "fast_forward" {
        vec![
            "merge",
            "--ff-only",
            "--no-edit",
            "--no-autostash",
            "--no-overwrite-ignore",
            &incoming,
        ]
    } else {
        vec![
            "merge",
            "--no-ff",
            "--no-commit",
            "--no-edit",
            "--no-autostash",
            "--no-rerere-autoupdate",
            "--no-overwrite-ignore",
            "-s",
            "ort",
            &incoming,
        ]
    };
    let branch_options = format!("branch.{current_branch}.mergeOptions=");
    let mut command = vec!["-c", branch_options.as_str()];
    command.extend(args);
    let executed = native::output(&repository_path, &command);
    invalidate_git_metadata(&repository_path);
    let output = executed.map_err(|e| {
        format!(
            "Merge did not complete. Review the working tree and merge state before retrying. {e}"
        )
    })?;
    let index = native::index(&repository_path)?;
    if !output.status.success()
        && !(output.status.code() == Some(1)
            && index.conflicted()
            && native::in_progress(&repository_path)?)
    {
        return Err(format!(
            "Merge did not complete. Review the working tree and merge state before retrying. {}",
            native::failure(&output)
        ));
    }
    let actual_head = native::oid(&repository_path, "HEAD")?;
    let expected_head = if plan.status == "fast_forward" {
        plan.incoming
    } else {
        plan.current
    };
    if actual_head != expected_head || native::branch(&repository_path)? != plan.branch {
        return Err("The checked-out branch changed while Git prepared the merge. Inspect the repository before continuing; no automatic commit was made.".into());
    }
    if plan.status != "fast_forward" && native::heads(&repository_path)? != vec![plan.incoming] {
        return Err("The pending merge's incoming commit changed. Inspect the merge state before continuing; no automatic commit was made.".into());
    }
    if index.entries != plan.result.entries {
        if plan.status == "fast_forward" {
            return Err("The fast-forward ran, but the index differs from its verified target. Inspect the repository before retrying.".into());
        }
        return Err("Git prepared a different index from the preview (for example an external process or hook changed it). No automatic commit was made. Review the pending merge before continuing.".into());
    }
    perf_log(
        "merge_native: preview_matches_prepared=true",
        Duration::ZERO,
    );
    if plan.status == "fast_forward" {
        return Ok(MergeOutcome {
            status: "fast_forwarded".into(),
            message: format!("Fast-forwarded {current_branch} to {source_ref}."),
            conflicts: vec![],
        });
    }
    if index.conflicted() {
        let conflicts = gather_conflicts(&repository_path)?;
        return Ok(MergeOutcome{status:"conflicts".into(),message:format!("Merging {source_ref} produced {} conflicts. Resolve them, then complete the merge.",conflicts.len()),conflicts});
    }
    if !target_is_submodule {
        let review = submodule_merge_review_inner(&repository_path)?;
        if !review.items.is_empty() {
            return Ok(MergeOutcome{status:"submodule_review".into(),message:format!("Merge prepared. Review {} submodule pointers before creating the merge commit.",review.items.len()),conflicts:vec![]});
        }
    }
    let oid = complete_merge_internal(
        &repository_path,
        &format!("Merge {source_ref} into {current_branch}"),
    )?;
    Ok(MergeOutcome {
        status: "merged".into(),
        message: format!("Merged {source_ref} into {current_branch} ({}).", &oid[..8]),
        conflicts: vec![],
    })
}

#[tauri::command]
pub async fn merge_in_progress(
    repository_path: String,
    target_path: String,
) -> Result<bool, String> {
    off_main_thread(move || merge_in_progress_inner(repository_path, target_path)).await
}
pub(in crate::repository) fn merge_in_progress_inner(
    repository_path: String,
    target_path: String,
) -> Result<bool, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    native::in_progress(&repository_path)
}

#[tauri::command]
pub async fn list_conflicts(
    repository_path: String,
    target_path: String,
) -> Result<Vec<ConflictedFile>, String> {
    off_main_thread(move || list_conflicts_inner(repository_path, target_path)).await
}
pub(in crate::repository) fn list_conflicts_inner(
    repository_path: String,
    target_path: String,
) -> Result<Vec<ConflictedFile>, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    gather_conflicts(&repository_path)
}

#[tauri::command]
pub async fn submodule_merge_review(
    repository_path: String,
    target_path: String,
) -> Result<SubmoduleMergeReview, String> {
    off_main_thread(move || submodule_merge_review_request_inner(repository_path, target_path))
        .await
}

pub(in crate::repository) fn submodule_merge_review_request_inner(
    repository_path: String,
    target_path: String,
) -> Result<SubmoduleMergeReview, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let handle = repo_write_lock(&repository_path);
    let _guard = handle
        .lock()
        .map_err(|_| "Repository operation lock is unavailable.")?;
    submodule_merge_review_inner(&repository_path)
}

#[tauri::command]
pub async fn apply_submodule_merge_revision(
    repository_path: String,
    target_path: String,
    relative_path: String,
    revision: String,
) -> Result<(), String> {
    off_main_thread(move || {
        apply_submodule_merge_revision_inner(repository_path, target_path, relative_path, revision)
    })
    .await
}
pub(in crate::repository) fn apply_submodule_merge_revision_inner(
    repository_path: String,
    target_path: String,
    relative_path: String,
    revision: String,
) -> Result<(), String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    let revision = revision.trim();
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(
        &repository_path,
        "apply_submodule_merge_revision",
        queue_started.elapsed(),
    );
    if !native::in_progress(&repository_path)? {
        return Err("There is no merge in progress here.".into());
    }
    let index = native::index(&repository_path)?;
    let previous = index.get(&relative_string, 0).map(|e| e.id);
    let id = if revision.is_empty() || revision == "__none__" {
        None
    } else {
        Some(Oid::from_str(revision).map_err(|_| "Invalid submodule revision")?)
    };
    if let Some(id) = id {
        let absolute = Path::new(&repository_path).join(&relative);
        if absolute.join(".git").exists() {
            native::oid(&absolute.to_string_lossy(),&id.to_string()).map_err(|_|format!("{relative_string} does not have commit {id} locally. Fetch it or choose another revision."))?;
        }
    }
    // Review only accepts a submodule path from this merge, never an arbitrary
    // regular-file path supplied by an outdated dialog.
    let current = native::tree(&repository_path, "HEAD")?;
    let incoming = native::heads(&repository_path)?
        .first()
        .copied()
        .ok_or("Missing merge head")?;
    let theirs = native::tree(&repository_path, &incoming.to_string())?;
    if ![&index, &current, &theirs].iter().any(|idx| {
        (0..=3).any(|stage| {
            idx.get(&relative_string, stage)
                .is_some_and(|e| e.mode == GIT_FILEMODE_COMMIT)
        })
    }) {
        return Err("This path is not a submodule in the pending merge.".into());
    }
    let applied = native::set_link(&repository_path, &relative_string, id);
    invalidate_git_metadata(&repository_path);
    applied?;
    perf_log(&format!("merge_submodule_choice: engine=git_cli repo={} path={relative_string:?} previous={} selected={}",anonymized_repository_id(&repository_path),short_oid(previous),id.map(|x|x.to_string()).unwrap_or_else(||"none".into())),Duration::ZERO);
    Ok(())
}

#[tauri::command]
pub async fn conflict_sides(
    repository_path: String,
    target_path: String,
    relative_path: String,
) -> Result<ConflictSides, String> {
    off_main_thread(move || conflict_sides_inner(repository_path, target_path, relative_path)).await
}
pub(in crate::repository) fn conflict_sides_inner(
    repository_path: String,
    target_path: String,
    relative_path: String,
) -> Result<ConflictSides, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative = normalized(&relative);
    let index = native::index(&repository_path)?;
    if !(1..=3).any(|stage| index.get(&relative, stage).is_some()) {
        return Err(format!("{relative_path} is not a conflicted file"));
    }
    let read = |stage| -> Option<String> {
        let e = index.get(&relative, stage)?;
        if e.mode == GIT_FILEMODE_COMMIT {
            return None;
        }
        String::from_utf8(native::blob(&repository_path, e.id).ok()?).ok()
    };
    Ok(ConflictSides {
        ancestor: read(1),
        ours: read(2),
        theirs: read(3),
    })
}

#[tauri::command]
pub async fn open_merge_tool(
    repository_path: String,
    target_path: String,
    relative_path: String,
) -> Result<String, String> {
    off_main_thread(move || open_merge_tool_inner(repository_path, target_path, relative_path))
        .await
}

fn open_merge_tool_inner(
    repository_path: String,
    target_path: String,
    relative_path: String,
) -> Result<String, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative = normalized(&relative);
    let configured_tool = Command::new("git")
        .args(["config", "--get", "merge.tool"])
        .current_dir(&repository_path)
        .output()
        .ok()
        .and_then(|output| {
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        })
        .filter(|tool| !tool.is_empty());
    let output = Command::new("git")
        .args(["mergetool", "--no-prompt", "--", &relative])
        .current_dir(&repository_path)
        .output()
        .map_err(|error| format!("Could not start git mergetool: {error}"))?;
    // A tool can save a resolution and still exit unsuccessfully.
    invalidate_git_metadata(&repository_path);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(if detail.is_empty() {
            "Git mergetool failed. Configure one with `git config merge.tool <tool>` or check `git mergetool --tool-help`.".into()
        } else {
            detail
        });
    }
    Ok(configured_tool
        .map(|tool| format!("Merge tool \"{tool}\" finished for {relative}."))
        .unwrap_or_else(|| format!("Git mergetool finished for {relative}.")))
}

#[tauri::command]
pub async fn resolve_conflict(
    repository_path: String,
    target_path: String,
    relative_path: String,
    resolution: String,
) -> Result<(), String> {
    off_main_thread(move || {
        resolve_conflict_inner(repository_path, target_path, relative_path, resolution)
    })
    .await
}
pub(in crate::repository) fn resolve_conflict_inner(
    repository_path: String,
    target_path: String,
    relative_path: String,
    resolution: String,
) -> Result<(), String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(
        &repository_path,
        "resolve_conflict",
        queue_started.elapsed(),
    );
    let path = normalized(&relative);
    let index = native::index(&repository_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    perf_log(
        &format!("merge_resolve: engine=git_cli path={path:?} choice={resolution}"),
        Duration::ZERO,
    );
    let resolved = (|| -> Result<(), String> {
        match resolution.as_str() {
            "ours" | "theirs" => {
                if !(1..=3).any(|stage| index.get(&path, stage).is_some()) {
                    return Err(format!("{path} is not a conflicted file"));
                }
                let entry = index.get(&path, if resolution == "ours" { 2 } else { 3 });
                let link = (1..=3).any(|stage| {
                    index
                        .get(&path, stage)
                        .is_some_and(|e| e.mode == GIT_FILEMODE_COMMIT)
                });
                if link {
                    if entry.is_some_and(|e| e.mode != GIT_FILEMODE_COMMIT) {
                        return Err("Folder/submodule conversion requires resolving the paths explicitly; no automatic overwrite was made.".into());
                    }
                    native::set_link(&repository_path, &path, entry.map(|e| e.id))?;
                    if let Some(e) = entry {
                        if absolute.join(".git").exists() {
                            let child = absolute.to_string_lossy();
                            let result =
                                native::text(&child, &["checkout", "--detach", &e.id.to_string()]);
                            invalidate_git_metadata(&child);
                            result?;
                        }
                    }
                    return Ok(());
                }
                if entry.is_none() {
                    native::text(&repository_path, &["rm", "--", &path])?;
                    return Ok(());
                }
                native::text(
                    &repository_path,
                    &[
                        "checkout",
                        if resolution == "ours" {
                            "--ours"
                        } else {
                            "--theirs"
                        },
                        "--",
                        &path,
                    ],
                )?;
            }
            "manual" => {
                if !absolute.exists() {
                    return Err(format!(
                        "{path} does not exist on disk; resolve its deletion explicitly."
                    ));
                }
                let bytes = fs::read(&absolute).map_err(|e| e.to_string())?;
                if contains_conflict_markers(&bytes) {
                    return Err(format!(
                        "{path} still contains conflict markers. Save the resolved content first."
                    ));
                }
            }
            _ => return Err("Unknown conflict resolution".into()),
        }
        native::text(&repository_path, &["add", "--", &path])?;
        Ok(())
    })();
    invalidate_git_metadata(&repository_path);
    perf_log(
        &format!(
            "merge_resolve: path={path:?} result={}",
            if resolved.is_ok() {
                "accepted"
            } else {
                "error"
            }
        ),
        Duration::ZERO,
    );
    resolved
}

pub(in crate::repository) fn complete_merge_internal(
    repository_path: &str,
    message: &str,
) -> Result<String, String> {
    if !native::in_progress(repository_path)? {
        return Err("There is no merge in progress here.".into());
    }
    let index = native::index(repository_path)?;
    if index.conflicted() {
        return Err("There are still unresolved conflicts.".into());
    }
    validate_merge_gitlinks(repository_path, &index)?;
    let current = native::oid(repository_path, "HEAD")?;
    let parents = native::heads(repository_path)?;
    let tree = native::text(repository_path, &["write-tree"])?;
    let committed_tree = native::text(repository_path, &["rev-parse", "HEAD^{tree}"])?;
    let recorded_parents = native::text(
        repository_path,
        &["rev-list", "--parents", "-n", "1", "HEAD"],
    )?;
    let recorded = recorded_parents
        .split_whitespace()
        .skip(2)
        .collect::<Vec<_>>();
    if committed_tree == tree
        && !parents.is_empty()
        && parents
            .iter()
            .all(|p| recorded.contains(&p.to_string().as_str()))
    {
        // The previous commit succeeded but cleanup was interrupted. Git's
        // --quit clears only merge state and leaves HEAD/index/files intact.
        let cleaned = native::text(repository_path, &["merge", "--quit"]);
        invalidate_git_metadata(repository_path);
        cleaned.map_err(|e|format!("Merge commit {current} already exists, but cleanup failed. No new commit was created. {e}"))?;
        return Ok(current.to_string());
    }
    perf_log(
        &format!(
            "merge_commit: engine=git_cli repo={} tree={tree} current={current} incoming={:?}",
            anonymized_repository_id(repository_path),
            parents
        ),
        Duration::ZERO,
    );
    // Argument passing preserves the message; no shell/editor interpolation.
    let committed = native::output(repository_path, &["commit", "--no-edit", "-m", message]);
    invalidate_git_metadata(repository_path);
    let out = committed?;
    let oid = native::oid(repository_path, "HEAD")?;
    if !out.status.success() {
        return Err(if oid != current {
            format!("Merge commit {oid} was created, but Git reported an error afterward. Inspect the repository before retrying. {}",native::failure(&out))
        } else {
            format!("Git did not complete the merge commit. Review the pending state before retrying. {}",native::failure(&out))
        });
    }
    if native::in_progress(repository_path)?
        || native::git_path(repository_path, "MERGE_MSG")?.exists()
    {
        return Err(format!("Merge commit {oid} was created, but merge state remains. Inspect it before retrying; no automatic retry was made."));
    }
    let actual = native::text(
        repository_path,
        &["rev-list", "--parents", "-n", "1", "HEAD"],
    )?;
    let expected = std::iter::once(current.to_string())
        .chain(parents.iter().map(ToString::to_string))
        .collect::<Vec<_>>();
    if actual.split_whitespace().skip(1).collect::<Vec<_>>() != expected {
        return Err(format!("Commit {oid} was created with unexpected parents. Inspect it before any further action; no retry was made."));
    }
    Ok(oid.to_string())
}

#[tauri::command]
pub async fn complete_merge(
    repository_path: String,
    target_path: String,
    message: String,
) -> Result<String, String> {
    off_main_thread(move || complete_merge_request_inner(repository_path, target_path, message))
        .await
}

pub(in crate::repository) fn complete_merge_request_inner(
    repository_path: String,
    target_path: String,
    message: String,
) -> Result<String, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    if message.trim().is_empty() {
        return Err("Merge commit message cannot be empty".into());
    }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "complete_merge", queue_started.elapsed());
    if !native::in_progress(&repository_path)? {
        return Err("There is no merge in progress here.".into());
    }
    complete_merge_internal(&repository_path, message.trim())
}

#[tauri::command]
pub async fn abort_merge(repository_path: String, target_path: String) -> Result<(), String> {
    off_main_thread(move || abort_merge_inner(repository_path, target_path)).await
}
pub(in crate::repository) fn abort_merge_inner(
    repository_path: String,
    target_path: String,
) -> Result<(), String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "abort_merge", queue_started.elapsed());
    if !native::in_progress(&repository_path)? {
        return Err("There is no merge in progress here.".into());
    }
    let result = native::text(&repository_path, &["merge", "--abort"]);
    invalidate_git_metadata(&repository_path);
    result.map(|_| ()).map_err(|e| {
        format!("Abort did not complete. Review the working tree and merge state. {e}")
    })
}

// Working-tree buttons must not undo a reviewed submodule removal merely
// because its retained local folder appears as untracked. Reversing the choice
// remains available explicitly in the merge review.
pub(in crate::repository) fn stage_merge_paths(
    root: &str,
    paths: &[PathBuf],
    unstage: bool,
) -> Result<StageResult, String> {
    let before = native::index(root)?;
    let head = native::tree(root, "HEAD")?;
    let removed: Vec<_> = head
        .links()
        .into_keys()
        .filter(|p| {
            before.get(p, 0).is_none()
                && !(1..=3).any(|s| before.get(p, s).is_some())
                && !before
                    .entries
                    .values()
                    .any(|e| e.path.starts_with(&format!("{p}/")))
        })
        .collect();
    let names = paths
        .iter()
        .map(|p| {
            normalized(
                &p.components()
                    .filter(|c| !matches!(c, Component::CurDir))
                    .collect::<PathBuf>(),
            )
        })
        .collect::<Vec<_>>();
    for name in &names {
        if let Some(removed) = removed.iter().find(|p| {
            name.is_empty()
                || *p == name
                || p.starts_with(&format!("{name}/"))
                || name.starts_with(&format!("{p}/"))
        }) {
            if unstage || Path::new(root).join(removed).exists() {
                return Err(format!("{removed} was removed by this merge. Its local folder may still exist. Use the submodule merge review to change this choice; Stage/Unstage did not restore the old reference."));
            }
        }
        if !unstage
            && Path::new(root).join(name).is_dir()
            && embedded_git_repo_path(&Path::new(root).join(name))
            && !before
                .get(name, 0)
                .is_some_and(|e| e.mode == GIT_FILEMODE_COMMIT)
        {
            return Err(embedded_git_repo_error(name));
        }
    }
    let mut result = StageResult::default();
    let mut selected = Vec::new();
    for name in &names {
        if !unstage {
            if let Some(e) = before
                .get(name, 0)
                .filter(|e| e.mode == GIT_FILEMODE_COMMIT)
            {
                let child = Path::new(root).join(name);
                if child.join(".git").exists()
                    && native::oid(&child.to_string_lossy(), "HEAD")? == e.id
                {
                    result.skipped_dirty_submodules.push(name.clone());
                    continue;
                }
            }
        }
        selected.push(name.as_str());
    }
    if !selected.is_empty() {
        let mut args = if unstage {
            vec!["reset", "HEAD", "--"]
        } else {
            vec!["add", "--all", "--"]
        };
        args.extend(selected);
        let outcome = native::text(root, &args);
        invalidate_git_metadata(root);
        outcome?;
        result.staged_paths = names
            .into_iter()
            .filter(|p| !result.skipped_dirty_submodules.contains(p))
            .collect();
    }
    Ok(result)
}
