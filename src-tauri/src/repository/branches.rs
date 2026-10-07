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

#[derive(Serialize, Debug)]
pub struct RemoteTrackingCheckoutResult {
    pub(super) branch: String,
    pub(super) upstream: String,
    pub(super) revision: String,
    pub(super) created: bool,
}

const RESTORE_SUBMODULE_UPDATE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const RESTORE_SUBMODULE_UPDATE_TIMEOUT_LABEL: &str = "60 minutes";

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
    drop(commit);
    drop(object);
    drop(repo);

    // This is intentionally separate from ordinary "Checkout this commit":
    // it is the destructive "make my workspace exactly this checkpoint"
    // operation. It discards tracked edits, removes untracked non-ignored
    // leftovers, and forces submodule worktrees to the gitlinks recorded by
    // the selected parent commit. Ignored build artifacts stay in place.
    //
    // Never start this with `git clean -fd`. `git clean` is not transactional:
    // it can remove nine files, fail on the tenth (locked/read-only/invalid on
    // Windows), and return an error after the workspace has already been
    // damaged. Move every ordinary untracked file into the repository's Git
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
        // Important subtlety: jumping between checkpoints with different
        // submodule sets can leave old submodule worktrees behind unless the
        // currently-registered submodules are deinitialized first. A plain
        // checkout + submodule update only moves modules that still exist in
        // the target commit; it does not reliably remove modules that existed
        // only in the previous checkpoint.
        restore_checkpoint_step(&repository_path, "deinit current submodules", &["submodule", "deinit", "--all", "--force"])
            .map_err(|detail| format!("Could not deinitialize the current submodules before restoring checkpoint {oid}. HEAD was not changed, but the workspace may need Submodule update --init --recursive. Git said: {detail}"))?;
        if let Some(name) = branch.as_deref() {
            restore_checkpoint_step(&repository_path, "checkout branch checkpoint", &["checkout", "--force", name])
                .map_err(|detail| format!("Could not restore branch {name} at checkpoint {oid}: {detail}"))?;
        } else {
            restore_checkpoint_step(&repository_path, "checkout detached checkpoint", &["checkout", "--detach", "--force", &oid])
                .map_err(|detail| format!("Could not restore checkpoint {oid}: {detail}"))?;
        }
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
            let _ = restore_checkpoint_optional_step(&repository_path, "cleanup after failed submodule update: reset initialized submodules", &["submodule", "foreach", "--recursive", "git reset --hard"]);
            let verification = restore_checkpoint_verify(&repository_path).err().map(|error| format!(" Remaining state: {error}")).unwrap_or_default();
            return Err(format!("Checkpoint {oid} was checked out, but submodule update did not finish after {RESTORE_SUBMODULE_UPDATE_TIMEOUT_LABEL}. The restore is incomplete; retry the exact checkpoint restore or run Submodule update --init --recursive after checking the network/VPN. Git said: {detail}{verification}"));
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

// Temporary, same-repository safety area for non-ignored untracked files.
// It lives under the real Git directory so Git never sees it as another
// untracked item and so ordinary repositories keep renames on one volume.
struct RestoreCheckpointQuarantine {
    root: PathBuf,
    workdir: PathBuf,
    moved: Vec<(PathBuf, PathBuf)>,
}

impl RestoreCheckpointQuarantine {
    fn new(repository_path: &str) -> Result<Self, String> {
        let repo = internal_repository(repository_path)?;
        let workdir = repo.workdir().ok_or("A clean checkpoint requires a working-tree repository")?.to_path_buf();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?.as_nanos();
        let root = repo.path().join("gdd-restore").join(format!("{}-{stamp}", std::process::id()));
        Ok(Self { root, workdir, moved: Vec::new() })
    }

    fn move_untracked(&mut self, repository_path: &str, phase: &str) -> Result<usize, String> {
        let started = Instant::now();
        // `-z` makes spaces/newlines unambiguous. Deliberately omit
        // `--directory`: collapsing a folder can accidentally include ignored
        // build artifacts, while this operation promises to preserve them.
        let output = git(repository_path, &["ls-files", "--others", "--exclude-standard", "-z"])
            .map_err(|detail| format!("Could not inspect untracked files: {detail}"))?;
        let raw_paths = output.split('\0').filter(|path| !path.is_empty()).collect::<Vec<_>>();

        // Git reports an embedded untracked repository as a trailing-slash
        // directory even without --directory. Do not silently move/delete an
        // entire repository; reject before moving anything in this phase.
        if raw_paths.iter().any(|path| path.ends_with('/')) {
            return Err("An untracked nested Git repository is present. Move or remove it explicitly, then retry Clean checkout; no files were deleted by this attempt.".into());
        }

        let mut candidates = Vec::with_capacity(raw_paths.len());
        for raw in raw_paths {
            let relative = safe_relative_path(raw)
                .map_err(|_| "Git reported an unsafe untracked path. Nothing was moved; inspect the repository from a terminal before retrying.".to_string())?;
            candidates.push((self.workdir.join(&relative), self.root.join(phase).join(relative)));
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
                return Err(format!("A local file could not be moved into the temporary safety area (it may be open, locked or read-only): {error}. {rollback}"));
            }
            self.moved.push((source, destination));
        }
        let count = self.moved.len() - phase_start;
        // At this point every non-ignored untracked *file* is recoverable in
        // the quarantine. Let Git remove only the now-empty directory shells.
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
        if child.is_dir() { let _ = remove_empty_restore_directories(&child); }
    }
    if fs::read_dir(path)?.next().is_none() { fs::remove_dir(path)?; }
    Ok(())
}

fn restore_checkpoint_quarantine_submodule_leftovers(repository_path: &str, phase: &str, quarantines: &mut Vec<RestoreCheckpointQuarantine>) -> Result<(), String> {
    let mut paths = Vec::new();
    let mut visited = HashSet::new();
    collect_initialized_submodule_workdirs(Path::new(repository_path), &mut visited, &mut paths)?;
    for path in paths {
        let submodule_path = path.to_string_lossy().into_owned();
        let mut quarantine = RestoreCheckpointQuarantine::new(&submodule_path)?;
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
    if lower.contains("timed out") { "timeout" }
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
    fn exact_checkpoint_quarantine_refuses_nested_repository_before_moving_files() {
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

        let path = repository.to_string_lossy().into_owned();
        let mut quarantine = RestoreCheckpointQuarantine::new(&path).unwrap();
        let error = quarantine.move_untracked(&path, "test").unwrap_err();
        assert!(error.contains("nested Git repository"));
        assert!(repository.join("loose.txt").exists(), "validation must happen before the first file is moved");
        assert!(repository.join("nested/.git").exists());
        fs::remove_dir_all(repository).unwrap();
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
