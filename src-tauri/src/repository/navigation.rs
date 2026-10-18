//! Read-only, bounded return from a full submodule workspace to its parent.
//! Reuses the existing 5-minute navigation cache, never extends its age.
use super::*;
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
    if repo.state() != git2::RepositoryState::Clean { return Ok(None); }
    let warm = full_status_cache().lock().unwrap().get(&repo_lock_key(repository_path))
        .is_some_and(|(at, _)| at.elapsed() < GIT_METADATA_TTL);
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
            let root = std::env::temp_dir().join(format!("ddt-return-navigation-{}", std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
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
