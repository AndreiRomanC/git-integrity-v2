// Inspect/Compare caches live under the application's OS temp cache directory.
use super::{cache_has_exact_revisions, git_with_timeout, perf_log};
use git2::Repository;
use std::{fs, path::{Path, PathBuf}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

fn open_cache(path: &Path, url: &str) -> Result<Repository, String> {
    let repo = Repository::open_bare(path).map_err(|error| format!("Git metadata is incomplete: {error}"))?;
    if !repo.is_bare() { return Err("Expected a bare repository cache".into()); }
    let origin = repo.config().and_then(|config| config.get_string("remote.origin.url"))
        .map_err(|error| format!("Cannot read cache origin: {error}"))?;
    if origin != url { return Err("Cached origin differs from the selected module".into()); }
    Ok(repo)
}

// The OS releases this lock even if the app exits abruptly. Keep the lock file
// itself so another process cannot lock a different file at the same path.
fn lock_cache(path: &Path) -> Result<fs::File, String> {
    let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(path.with_extension("cache-lock"))
        .map_err(|error| format!("Cannot open the Inspect cache lock: {error}"))?;
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => "This module's Inspect cache is being updated by another Git DrillDown window. Retry when that operation finishes.".to_string(),
        fs::TryLockError::Error(error) => format!("Cannot lock the Inspect cache: {error}"),
    })?;
    Ok(file)
}

fn rebuild_cache(context: &str, path: &Path, url: &str, exists: bool) -> Result<(), String> {
    let started = Instant::now();
    let name = path.file_name().ok_or("Invalid module cache path")?.to_string_lossy();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let preparation = path.with_file_name(format!(".{name}.prepare-{}-{nonce}", std::process::id()));
    fs::create_dir(&preparation).map_err(|error| format!("Cannot prepare the module cache: {error}"))?;
    let staged = preparation.join("repository.git");
    let previous = preparation.join("previous-cache");
    perf_log(&format!("github_module_cache: phase=clone_start recovery={exists} path={}", path.display()), Duration::ZERO);
    let result = (|| {
        // Publish the directory only after Git exits successfully and the
        // result opens. An interrupted clone therefore cannot poison path.
        git_with_timeout(context, &["clone", "--bare", "--", url, &staged.to_string_lossy()],
            Duration::from_secs(600), "10 minutes")
            .map_err(|error| format!("Could not download the module history for Inspect. Retry after resolving the Git error. {error}"))?;
        drop(open_cache(&staged, url).map_err(|error| format!("Downloaded cache validation failed: {error}"))?);
        if exists {
            fs::rename(path, &previous).map_err(|error| format!("Cannot replace the incomplete module cache at {}: {error}", path.display()))?;
        }
        if let Err(error) = fs::rename(&staged, path) {
            if exists {
                fs::rename(&previous, path).map_err(|restore_error| format!(
                    "Could not install the module cache: {error}. The previous cache remains at {}: {restore_error}", previous.display()))?;
            }
            return Err(format!("Could not install the module cache: {error}"));
        }
        Ok(())
    })();
    perf_log(&format!("github_module_cache: phase=rebuild existing={exists} success={} path={}", result.is_ok(), path.display()), started.elapsed());
    // An old invalid directory may contain diagnostic evidence. Retain it
    // outside the project in TEMP, even if the installation/rollback failed.
    if previous.exists() {
        perf_log(&format!("github_module_cache: phase=preserve_invalid path={}", previous.display()), Duration::ZERO);
    } else if let Err(error) = fs::remove_dir_all(&preparation) {
        perf_log(&format!("github_module_cache: phase=cleanup_failed path={} error={error}", preparation.display()), Duration::ZERO);
    }
    result
}

pub(super) fn ensure(context: &str, path: &Path, url: &str, revisions: &[&str]) -> Result<PathBuf, String> {
    let started = Instant::now();
    let parent = path.parent().ok_or("Could not resolve the module cache folder")?;
    fs::create_dir_all(parent).map_err(|error| format!("Could not create module cache folder: {error}"))?;
    let _lock = lock_cache(path).inspect_err(|error| {
        perf_log(&format!("github_module_cache: phase=lock_failed path={} reason={error}", path.display()), started.elapsed());
    })?;
    // Only recover a physical directory owned by the cache. Files and links
    // require inspection; following one could operate on a different repo.
    let exists = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!("Inspect cache location must be a regular directory: {}", path.display()));
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("Cannot inspect the module cache at {}: {error}", path.display())),
    };
    if exists {
        match open_cache(path, url) {
            Ok(repo) => {
                drop(repo);
                if cache_has_exact_revisions(path, revisions) {
                    perf_log(&format!("github_module_cache: phase=reuse revisions={} path={}", revisions.len(), path.display()), started.elapsed());
                    return Ok(path.to_path_buf());
                }
                let fetched = git_with_timeout(&path.to_string_lossy(),
                    &["fetch", "--prune", "origin", "+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"],
                    Duration::from_secs(300), "5 minutes");
                perf_log(&format!("github_module_cache: phase=fetch success={} path={}", fetched.is_ok(), path.display()), started.elapsed());
                fetched.map_err(|error| format!("Could not update the cached module history: {error}"))?;
                return Ok(path.to_path_buf());
            }
            Err(error) => perf_log(&format!(
                "github_module_cache: phase=invalid head={} objects={} config={} path={} reason={error}",
                path.join("HEAD").is_file(), path.join("objects").is_dir(), path.join("config").is_file(), path.display()), started.elapsed()),
        }
    } else {
        perf_log(&format!("github_module_cache: phase=missing path={}", path.display()), started.elapsed());
    }
    rebuild_cache(context, path, url, exists)?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        source: PathBuf,
        context: PathBuf,
        cache: PathBuf,
        tip: String,
        tagged: String,
    }
    impl Fixture {
        fn new() -> Self {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("gdd-module-cache-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let source = root.join("source");
            let context = root.join("project");
            let cache = root.join("cache/module.git");
            fs::create_dir_all(cache.parent().unwrap()).unwrap();
            let repo = Repository::init(&source).unwrap();
            Repository::init(&context).unwrap();
            fs::write(context.join("keep.tdcl"), b"original project input").unwrap();
            let tree = repo.find_tree(repo.treebuilder(None).unwrap().write().unwrap()).unwrap();
            let sig = git2::Signature::now("Cache test", "test@example.invalid").unwrap();
            let base = repo.commit(Some("refs/heads/main"), &sig, &sig, "Base", &tree, &[]).unwrap();
            repo.set_head("refs/heads/main").unwrap();
            let parent = repo.find_commit(base).unwrap();
            let tagged = repo.commit(None, &sig, &sig, "Tag-only history", &tree, &[&parent]).unwrap();
            repo.tag_lightweight("v1", repo.find_commit(tagged).unwrap().as_object(), false).unwrap();
            let tip = repo.commit(Some("refs/heads/main"), &sig, &sig, "Current", &tree, &[&parent]).unwrap();
            repo.reference("refs/heads/feature", tagged, false, "fixture").unwrap();
            Self { root, source, context, cache, tip: tip.to_string(), tagged: tagged.to_string() }
        }
        fn ensure(&self, revisions: &[&str]) -> Result<PathBuf, String> {
            ensure(self.context.to_str().unwrap(), &self.cache, self.source.to_str().unwrap(), revisions)
        }
        fn verify(&self) {
            let repo = open_cache(&self.cache, self.source.to_str().unwrap()).unwrap();
            assert!(repo.is_bare());
            assert!(repo.find_commit(self.tip.parse().unwrap()).is_ok());
            assert_eq!(repo.find_reference("refs/tags/v1").unwrap().target().unwrap().to_string(), self.tagged);
            assert_eq!(repo.find_reference("refs/heads/feature").unwrap().target().unwrap().to_string(), self.tagged);
            let mut walk = repo.revwalk().unwrap();
            walk.push(self.tip.parse().unwrap()).unwrap();
            walk.push(self.tagged.parse().unwrap()).unwrap();
            assert_eq!(walk.collect::<Result<Vec<_>, _>>().unwrap().len(), 3, "both branch and tag-only ancestry must be available");
            assert_eq!(fs::read(self.context.join("keep.tdcl")).unwrap(), b"original project input");
            assert!(!self.context.join(".git/modules").exists());
        }
        fn previous(&self) -> Vec<PathBuf> {
            fs::read_dir(self.cache.parent().unwrap()).unwrap().map(|entry| entry.unwrap().path().join("previous-cache")).filter(|path| path.exists()).collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
    }

    #[test]
    fn missing_cache_clones_all_refs_and_reuses_exact_objects_offline() {
        let f = Fixture::new();
        f.ensure(&[&f.tip, &f.tagged]).unwrap();
        f.verify();
        fs::rename(&f.source, f.root.join("offline")).unwrap();
        f.ensure(&[&f.tip, &f.tagged]).unwrap();
        assert!(f.ensure(&["main"]).unwrap_err().contains("Could not update"));
        assert!(f.previous().is_empty());
    }

    #[test]
    fn incomplete_directories_rebuild_and_preserve_previous_contents() {
        for broken in ["empty", "head-only", "missing-head", "missing-objects", "wrong-origin"] {
            let f = Fixture::new();
            if broken == "empty" || broken == "head-only" {
                fs::create_dir(&f.cache).unwrap();
                if broken == "head-only" { fs::write(f.cache.join("HEAD"), "ref: refs/heads/main\n").unwrap(); }
            } else {
                f.ensure(&[&f.tip]).unwrap();
                match broken {
                    "missing-head" => fs::remove_file(f.cache.join("HEAD")).unwrap(),
                    "missing-objects" => fs::remove_dir_all(f.cache.join("objects")).unwrap(),
                    _ => Repository::open_bare(&f.cache).unwrap().config().unwrap().set_str("remote.origin.url", "other repository").unwrap(),
                }
            }
            fs::write(f.cache.join("diagnostic-marker"), broken).unwrap();
            f.ensure(&[&f.tip, &f.tagged]).unwrap();
            f.verify();
            let previous = f.previous();
            assert_eq!(previous.len(), 1, "{broken}");
            assert_eq!(fs::read_to_string(previous[0].join("diagnostic-marker")).unwrap(), broken);
        }
    }

    #[test]
    fn failed_clone_keeps_existing_cache_and_a_retry_recovers() {
        for existing in [false, true] {
            let f = Fixture::new();
            if existing {
                fs::create_dir(&f.cache).unwrap();
                fs::write(f.cache.join("marker"), b"keep").unwrap();
            }
            fs::rename(&f.source, f.root.join("offline")).unwrap();
            assert!(f.ensure(&[&f.tip]).unwrap_err().contains("Could not download"));
            assert_eq!(f.cache.exists(), existing);
            if existing { assert_eq!(fs::read(f.cache.join("marker")).unwrap(), b"keep"); }
            assert!(fs::read_dir(f.cache.parent().unwrap()).unwrap()
                .all(|entry| !entry.unwrap().file_name().to_string_lossy().contains(".prepare-")));
            fs::rename(f.root.join("offline"), &f.source).unwrap();
            f.ensure(&[&f.tip]).unwrap();
            f.verify();
        }
    }

    #[test]
    fn cache_lock_releases_after_failure_and_reports_another_holder() {
        let f = Fixture::new();
        let held = lock_cache(&f.cache).unwrap();
        assert!(f.ensure(&[&f.tip]).unwrap_err().contains("another Git DrillDown window"));
        assert!(!f.cache.exists());
        drop(held);
        f.ensure(&[&f.tip]).unwrap();
        f.verify();
    }

    #[test]
    fn a_regular_file_at_cache_path_is_preserved() {
        let f = Fixture::new();
        fs::write(&f.cache, b"unrelated file").unwrap();
        assert!(f.ensure(&[&f.tip]).unwrap_err().contains("regular directory"));
        assert_eq!(fs::read(&f.cache).unwrap(), b"unrelated file");
    }

    #[cfg(unix)]
    #[test]
    fn cache_symlinks_are_not_followed() {
        let f = Fixture::new();
        std::os::unix::fs::symlink(&f.source, &f.cache).unwrap();
        assert!(f.ensure(&[&f.tip]).unwrap_err().contains("regular directory"));
        assert!(fs::symlink_metadata(&f.cache).unwrap().file_type().is_symlink());
        assert!(Repository::open(&f.source).is_ok());
    }

    #[test]
    fn movable_refs_are_fetched_instead_of_reusing_old_tips() {
        let f = Fixture::new();
        f.ensure(&[&f.tip]).unwrap();
        let repo = Repository::open(&f.source).unwrap();
        repo.reference("refs/heads/main", f.tagged.parse().unwrap(), true, "move branch").unwrap();
        repo.reference("refs/tags/v1", f.tip.parse().unwrap(), true, "move tag").unwrap();
        drop(repo);
        f.ensure(&["main", "v1"]).unwrap();
        let cache = Repository::open_bare(&f.cache).unwrap();
        assert_eq!(cache.find_reference("refs/heads/main").unwrap().target().unwrap().to_string(), f.tagged);
        assert_eq!(cache.find_reference("refs/tags/v1").unwrap().target().unwrap().to_string(), f.tip);
        assert!(f.previous().is_empty());
    }
}
