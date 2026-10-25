//! Read-only, bounded return from a full submodule workspace to its parent.
//! Reuses the existing 5-minute navigation cache, never extends its age.
use super::*;

#[derive(Serialize)]
pub struct NavigationStatus {
    fresh: bool,
    head_oid: String,
    branch: String,
}

// Cheap freshness/identity probe only. Never walks files, history, submodules
// or remote refs. The frontend can keep its last snapshot visible while the
// existing refresh_status command does the actual background scan.
fn navigation_status_inner(repository_path: &str) -> Result<NavigationStatus, String> {
    validate_path(repository_path)?;
    let repo = internal_submodule_repository(Path::new(repository_path))?;
    let key = repo_lock_key(repository_path);
    let age = full_status_cache().lock().unwrap().get(&key).map(|(at, _)| at.elapsed());
    let head = repo.head().ok();
    let head_oid = head.as_ref().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    let branch = if repo.head_detached().unwrap_or(false) { String::new() }
        else { head.as_ref().and_then(|head| head.shorthand()).unwrap_or("").to_string() };
    let fresh = age.is_some_and(|age| age < GIT_METADATA_TTL);
    if !fresh {
        perf_log(&perf_logging::cache_lookup_label("navigation", &key, None, age, GIT_METADATA_TTL, "background_refresh"), Duration::ZERO);
    }
    Ok(NavigationStatus { fresh, head_oid, branch })
}

#[tauri::command]
pub async fn navigation_status(repository_path: String) -> Result<NavigationStatus, String> {
    off_main_thread(move || navigation_status_inner(&repository_path)).await
}

fn refresh_navigation_status_inner(repository_path: String, head_oid: String, branch: String, force_fresh: bool) -> Result<Vec<Change>, String> {
    let same_head = |status: &NavigationStatus| status.head_oid == head_oid && status.branch == branch;
    if !same_head(&navigation_status_inner(&repository_path)?) {
        return Err("Repository HEAD/branch changed. Refresh repository before continuing.".into());
    }
    if force_fresh { invalidate_git_metadata(&repository_path); }
    let epoch = status_cache_epoch(&repository_path);
    let changes = refresh_status_inner(repository_path.clone())?;
    if !same_head(&navigation_status_inner(&repository_path)?) {
        return Err("Repository HEAD/branch changed during verification. Refresh repository before continuing.".into());
    }
    // A newer full snapshot also supersedes still-young per-folder snapshots.
    // Keep its index/HEAD-derived caches and the freshly populated root entry.
    let key = repo_lock_key(&repository_path);
    let epochs = status_cache_epochs().lock().unwrap();
    if *epochs.get(&key).unwrap_or(&0) != epoch { return Err(STATUS_SNAPSHOT_SUPERSEDED.into()); }
    let prefix = format!("{key}\u{0}");
    metadata_cache().lock().unwrap().retain(|scope, _| !scope.starts_with(&prefix) || scope == &prefix);
    Ok(changes)
}

#[tauri::command]
pub async fn refresh_navigation_status(repository_path: String, head_oid: String, branch: String, force_fresh: Option<bool>) -> Result<Vec<Change>, String> {
    off_main_thread(move || refresh_navigation_status_inner(repository_path, head_oid, branch, force_fresh.unwrap_or(false))).await
}
use std::hash::{Hash, Hasher};

fn fingerprint(repo: &Repository) -> Result<String, String> {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    repo.head_detached().map_err(|e| e.message().to_string())?.hash(&mut hash);
    let head = repo.head().map_err(|e| e.message().to_string())?;
    head.name().hash(&mut hash); head.target().hash(&mut hash);
    // No working-tree traversal. Detect index/config/ref changes that make
    // the saved parent branch/history/status context unsafe to reuse.
    for path in [repo.path().join("index"), repo.path().join("config"), repo.workdir().ok_or("Missing working tree")?.join(".gitmodules")] {
        let metadata = fs::metadata(path).ok();
        metadata.as_ref().map(|m| m.len()).hash(&mut hash);
        metadata.and_then(|m| m.modified().ok()).hash(&mut hash);
    }
    let mut refs = repo.references().map_err(|e| e.message().to_string())?.map(|reference| {
        let reference = reference.map_err(|e| e.message().to_string())?;
        Ok((reference.name().unwrap_or("").to_string(), reference.target(), reference.symbolic_target().map(str::to_string)))
    }).collect::<Result<Vec<_>, String>>()?;
    refs.sort(); refs.hash(&mut hash);
    Ok(format!("{:016x}", hash.finish()))
}

fn capture_inner(repository_path: &str) -> Result<Option<String>, String> {
    validate_path(repository_path)?;
    let repo = internal_repository(repository_path)?;
    if repo.state() != git2::RepositoryState::Clean {
        perf_log("repository_return: capture refused reason=git-operation-in-progress", Duration::ZERO);
        return Ok(None);
    }
    let key = repo_lock_key(repository_path);
    let age = full_status_cache().lock().unwrap().get(&key).map(|(at, _)| at.elapsed());
    let warm = age.is_some_and(|age| age < GIT_METADATA_TTL);
    perf_log(&perf_logging::cache_lookup_label("parent_return", &key, None, age, GIT_METADATA_TTL,
        if warm { "validate_metadata" } else { "unavailable" }), Duration::ZERO);
    if !warm { return Ok(None); }
    Ok(Some(format!("{}:{}", status_cache_epoch(repository_path), fingerprint(&repo)?)))
}

#[tauri::command]
pub async fn capture_repository_return(repository_path: String) -> Result<Option<String>, String> {
    off_main_thread(move || capture_inner(&repository_path)).await
}

#[tauri::command]
pub async fn resume_repository_return(repository_path: String, submodule_path: String, stamp: String) -> Result<Option<Vec<Change>>, String> {
    off_main_thread(move || resume_inner(&repository_path, &submodule_path, &stamp)).await
}

fn resume_inner(repository_path: &str, submodule_path: &str, stamp: &str) -> Result<Option<Vec<Change>>, String> {
    let started = Instant::now();
    if stamp.len() > 80 || capture_inner(repository_path)?.as_deref() != Some(stamp) {
        perf_log("repository_return: fallback reason=metadata-changed-or-cache-expired", started.elapsed());
        return Ok(None);
    }
    let relative = safe_relative_path(submodule_path)?;
    let relative = normalized(&relative);
    if relative.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return Err("A concrete submodule path is required".into());
    }
    let repo = internal_repository(repository_path)?;
    if !repo.index().map_err(|e| e.message().to_string())?.get_path(Path::new(&relative), 0).is_some_and(|entry| entry.mode == 0o160000) {
        return Err("The saved return path is no longer a submodule".into());
    }
    let epoch = status_cache_epoch(repository_path);
    let before = fingerprint(&repo)?;
    // Reuse the already-tested pathspec-limited cache patch. It checks only
    // this gitlink (including dirty work inside it), not every submodule.
    invalidate_git_metadata_for_submodule_checkout(repository_path, &relative);
    if status_cache_epoch(repository_path) != epoch.wrapping_add(1) || fingerprint(&internal_repository(repository_path)?)? != before {
        perf_log("repository_return: fallback reason=concurrent-change", started.elapsed());
        return Ok(None);
    }
    let key = repo_lock_key(repository_path);
    let snapshot = {
        // The same short epoch -> cache lock order used by status publication.
        // No Git/filesystem work is performed while holding these locks.
        let epochs = status_cache_epochs().lock().unwrap();
        if *epochs.get(&key).unwrap_or(&0) != epoch.wrapping_add(1) { return Ok(None); }
        full_status_cache().lock().unwrap().get(&key).cloned()
    };
    let Some((at, statuses)) = snapshot.filter(|(at, _)| at.elapsed() < GIT_METADATA_TTL) else { return Ok(None); };
    let changes = statuses.into_iter().map(|(path, status, staged)| Change { path, status, staged }).collect();
    perf_log(&format!("repository_return: accepted scope=one-gitlink cache_age_ms={} full_scan=false git_commands=0", at.elapsed().as_millis()), started.elapsed());
    Ok(Some(changes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git").args(["-c", "protocol.file.allow=always", "-c", "user.name=Navigation Test", "-c", "user.email=navigation@example.test"])
            .args(args).current_dir(root).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    struct Fixture { root: PathBuf, parent: PathBuf }
    impl Fixture {
        fn new() -> Self {
            static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!("ddt-return-navigation-{}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)));
            let dependency = root.join("dependency"); let parent = root.join("parent");
            fs::create_dir_all(&dependency).unwrap(); fs::create_dir_all(&parent).unwrap();
            git(&dependency, &["init", "-b", "main"]); fs::write(dependency.join("module.txt"), "initial\n").unwrap();
            git(&dependency, &["add", "."]); git(&dependency, &["commit", "-m", "Initial dependency"]);
            git(&parent, &["init", "-b", "main"]); fs::write(parent.join("parent.txt"), "initial\n").unwrap();
            git(&parent, &["add", "."]); git(&parent, &["commit", "-m", "Initial parent"]);
            git(&parent, &["submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
            git(&parent, &["commit", "-am", "Add dependency"]);
            let fixture = Self { root, parent }; fixture.seed(); fixture
        }
        fn path(&self) -> String { self.parent.to_string_lossy().into_owned() }
        fn seed(&self) { refresh_status_inner(self.path()).unwrap(); }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }
    #[test]
    fn navigation_probe_detects_expiry_without_refreshing_or_touching_git() {
        let f = Fixture::new();
        let index_before = fs::read(f.parent.join(".git/index")).unwrap();
        let fresh = navigation_status_inner(&f.path()).unwrap();
        assert!(fresh.fresh); assert_eq!(fresh.branch, "main"); assert!(!fresh.head_oid.is_empty());
        let key = repo_lock_key(&f.path());
        let old = Instant::now() - GIT_METADATA_TTL - Duration::from_secs(1);
        full_status_cache().lock().unwrap().get_mut(&key).unwrap().0 = old;
        fs::write(f.parent.join("external.txt"), "outside edit").unwrap();
        assert!(!navigation_status_inner(&f.path()).unwrap().fresh);
        assert_eq!(full_status_cache().lock().unwrap().get(&key).unwrap().0, old, "probe must not scan or renew the cache");
        assert_eq!(fs::read(f.parent.join(".git/index")).unwrap(), index_before);
    }
    #[test]
    fn navigation_refresh_detects_external_changes_and_discards_younger_scoped_snapshots() {
        let f = Fixture::new();
        let head = navigation_status_inner(&f.path()).unwrap();
        let old_scope = cached_git_metadata(&f.path(), "parent.txt");
        assert!(old_scope.statuses.is_empty());
        let index_before = fs::read(f.parent.join(".git/index")).unwrap();
        fs::write(f.parent.join("parent.txt"), "external modification\n").unwrap();
        let key = repo_lock_key(&f.path());
        full_status_cache().lock().unwrap().get_mut(&key).unwrap().0 = Instant::now() - GIT_METADATA_TTL - Duration::from_secs(1);
        let changes = refresh_navigation_status_inner(f.path(), head.head_oid.clone(), head.branch, false).unwrap();
        assert!(changes.iter().any(|change| change.path == "parent.txt"));
        assert!(!cached_git_metadata(&f.path(), "parent.txt").statuses.is_empty());
        assert_eq!(navigation_status_inner(&f.path()).unwrap().head_oid, head.head_oid);
        assert_eq!(fs::read(f.parent.join(".git/index")).unwrap(), index_before);
        assert_eq!(fs::read_to_string(f.parent.join("parent.txt")).unwrap(), "external modification\n");
    }
    #[test]
    fn navigation_refresh_refuses_external_checkout_and_missing_child_never_falls_back_to_parent() {
        let f = Fixture::new();
        let before = navigation_status_inner(&f.path()).unwrap();
        git(&f.parent, &["checkout", "-b", "external"]);
        let error = refresh_navigation_status_inner(f.path(), before.head_oid, before.branch, false).err().unwrap();
        assert!(error.contains("HEAD/branch changed"));
        let not_a_repository = f.parent.join("ordinary-folder"); fs::create_dir(&not_a_repository).unwrap();
        assert!(navigation_status_inner(not_a_repository.to_str().unwrap()).is_err());
    }
    #[test]
    fn navigation_refresh_inside_one_submodule_does_not_scan_or_rewrite_parent_cache() {
        let f = Fixture::new();
        let child = f.parent.join("vendor/dep").to_string_lossy().into_owned();
        let head = navigation_status_inner(&child).unwrap();
        let parent_key = repo_lock_key(&f.path());
        let before = full_status_cache().lock().unwrap().get(&parent_key).unwrap().0;
        fs::write(Path::new(&child).join("module.txt"), "child external change").unwrap();
        let changes = refresh_navigation_status_inner(child, head.head_oid, head.branch, true).unwrap();
        assert!(changes.iter().any(|change| change.path == "module.txt"));
        assert_eq!(full_status_cache().lock().unwrap().get(&parent_key).unwrap().0, before);
    }
    #[test]
    fn returning_patches_only_the_child_gitlink_preserving_parent_local_state_and_cache_age() {
        let f = Fixture::new();
        fs::write(f.parent.join("parent.txt"), "staged\n").unwrap(); git(&f.parent, &["add", "parent.txt"]);
        fs::write(f.parent.join("parent.txt"), "staged plus unstaged\n").unwrap(); fs::write(f.parent.join("untracked.txt"), "keep\n").unwrap();
        invalidate_git_metadata(&f.path()); f.seed();
        let stamp = capture_inner(&f.path()).unwrap().unwrap();
        let before_index = fs::read(f.parent.join(".git/index")).unwrap();
        let cached_at = full_status_cache().lock().unwrap().get(&repo_lock_key(&f.path())).unwrap().0;
        let child = f.parent.join("vendor/dep"); fs::write(child.join("module.txt"), "new child version\n").unwrap();
        git(&child, &["commit", "-am", "Child work"]);
        let changes = resume_inner(&f.path(), "vendor/dep", &stamp).unwrap().unwrap();
        assert!(changes.iter().any(|c| c.path == "vendor/dep" && !c.staged));
        assert!(changes.iter().any(|c| c.path == "parent.txt" && c.staged));
        assert!(changes.iter().any(|c| c.path == "untracked.txt"));
        assert_eq!(fs::read(f.parent.join(".git/index")).unwrap(), before_index);
        assert_eq!(fs::read_to_string(f.parent.join("parent.txt")).unwrap(), "staged plus unstaged\n");
        assert_eq!(fs::read_to_string(f.parent.join("untracked.txt")).unwrap(), "keep\n");
        assert_eq!(full_status_cache().lock().unwrap().get(&repo_lock_key(&f.path())).unwrap().0, cached_at, "a scoped refresh cannot extend full-cache freshness");
        assert_eq!(capture_inner(&f.path()).unwrap().unwrap().split(':').next().unwrap().parse::<u64>().unwrap(), stamp.split(':').next().unwrap().parse::<u64>().unwrap() + 1);
    }
    #[test]
    fn changed_parent_index_refs_or_cache_epoch_reject_the_saved_context_without_scanning() {
        for change in ["index", "head", "refs", "epoch"] {
            let f = Fixture::new(); let stamp = capture_inner(&f.path()).unwrap().unwrap();
            match change {
                "index" => { fs::write(f.parent.join("parent.txt"), "changed\n").unwrap(); git(&f.parent, &["add", "parent.txt"]); },
                "head" => { fs::write(f.parent.join("parent.txt"), "changed\n").unwrap(); git(&f.parent, &["commit", "-am", "Parent changed"]); },
                "refs" => git(&f.parent, &["branch", "new-ref"]),
                _ => invalidate_git_metadata(&f.path()),
            }
            let epoch = status_cache_epoch(&f.path());
            assert!(resume_inner(&f.path(), "vendor/dep", &stamp).unwrap().is_none(), "{change}");
            assert_eq!(status_cache_epoch(&f.path()), epoch, "no scoped status patch should run on rejection");
        }
    }
    #[test]
    fn returning_detects_child_working_and_staged_changes_without_touching_child_files_or_index() {
        for kind in ["unstaged", "staged", "untracked"] {
            let f = Fixture::new(); let stamp = capture_inner(&f.path()).unwrap().unwrap();
            let child = f.parent.join("vendor/dep");
            let file = child.join(if kind == "untracked" { "new.txt" } else { "module.txt" });
            fs::write(&file, "keep local child work\n").unwrap();
            if kind == "staged" { git(&child, &["add", "module.txt"]); }
            let index_path = internal_repository(child.to_str().unwrap()).unwrap().path().join("index");
            let index = fs::read(&index_path).unwrap();
            let changes = resume_inner(&f.path(), "vendor/dep", &stamp).unwrap().unwrap();
            assert!(changes.iter().any(|c| c.path == "vendor/dep"), "{kind}");
            assert_eq!(fs::read(index_path).unwrap(), index, "{kind}");
            assert_eq!(fs::read_to_string(file).unwrap(), "keep local child work\n", "{kind}");
        }
    }
    #[test]
    fn missing_expired_or_unsafe_parent_contexts_fall_back_and_arbitrary_paths_cannot_be_scanned() {
        let f = Fixture::new(); let stamp = capture_inner(&f.path()).unwrap().unwrap();
        for bad in ["", ".", "../outside", "parent.txt", "vendor"] { assert!(resume_inner(&f.path(), bad, &stamp).is_err(), "{bad}"); }
        fs::write(f.parent.join(".git/MERGE_HEAD"), "0123456789012345678901234567890123456789\n").unwrap();
        assert!(capture_inner(&f.path()).unwrap().is_none());
        assert!(resume_inner(&f.path(), "vendor/dep", &stamp).unwrap().is_none());
        fs::remove_file(f.parent.join(".git/MERGE_HEAD")).unwrap();
        full_status_cache().lock().unwrap().get_mut(&repo_lock_key(&f.path())).unwrap().0 = Instant::now() - GIT_METADATA_TTL - Duration::from_secs(1);
        assert!(capture_inner(&f.path()).unwrap().is_none());
        assert!(resume_inner(&f.path(), "vendor/dep", &stamp).unwrap().is_none());
        full_status_cache().lock().unwrap().remove(&repo_lock_key(&f.path()));
        assert!(capture_inner(&f.path()).unwrap().is_none());
    }
}
