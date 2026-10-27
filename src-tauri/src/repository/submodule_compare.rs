//! On-demand, read-only metadata for a gitlink comparison. Never fetches,
//! checks out a version, discovers an enclosing repository, or scans status.
use super::*;

#[derive(Default, Serialize)]
pub struct Side {
    source: String,
    kind: String,
    revision: Option<String>,
    configured_url: Option<String>,
    resolved_url: Option<String>,
    configured_branch: Option<String>,
    message: Option<String>,
    author: Option<String>,
    authored_at: Option<i64>,
    committed_at: Option<i64>,
    tags: Vec<String>,
    branches: Vec<String>,
    notes: Vec<String>,
}

#[derive(Serialize)]
pub struct Details {
    repository_path: String,
    relative_path: String,
    local_path: Option<String>,
    local_remote_url: Option<String>,
    left: Side,
    right: Side,
    compare_repository_path: Option<String>,
    compare_note: String,
}

// Display remote locations without leaking embedded HTTP credentials or query tokens.
fn display_url(value: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(value) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    } else { value.split(['?', '#']).next().unwrap_or(value).to_owned() }
}

fn registration(repo: &Repository, root: &str, tree: Option<&git2::Tree>, path: &str, side: &mut Side) -> Result<(), String> {
    let source = if let Some(tree) = tree {
        let Some(entry) = super::comparison::snapshot_path_entry(repo, tree, Path::new(".gitmodules")).map_err(|e| e.message().to_owned())? else { return Ok(()); };
        if !matches!(entry.filemode(), 0o100644 | 0o100755) { return Err("Snapshot .gitmodules is not a regular file.".into()); }
        vec!["--blob".to_owned(), entry.id().to_string()]
    } else {
        let file = Path::new(root).join(".gitmodules");
        if !file.exists() { return Ok(()); }
        if fs::symlink_metadata(&file).map_err(|e| e.to_string())?.file_type().is_symlink() {
            return Err("Working-tree .gitmodules is a symbolic link; configuration was not read.".into());
        }
        vec!["--file".to_owned(), file.to_string_lossy().into_owned()]
    };
    // Git parses quoted names, escapes and values. Includes are explicitly off.
    let mut args = vec!["config", "--no-includes", "--null"];
    args.extend(source.iter().map(String::as_str));
    args.extend(["--get-regexp", "^submodule\\..*\\.(path|url|branch)$"]);
    let raw = match git_with_timeout(root, &args, Duration::from_secs(10), "10 seconds") {
        Ok(raw) => raw,
        Err(e) if e.is_empty() => String::new(), // config returns 1 for no matches
        Err(_) => return Err("Cannot read this version's .gitmodules configuration.".into()),
    };
    let records: Vec<_> = raw.split('\0').filter_map(|r| r.split_once('\n')).collect();
    let names: HashSet<_> = records.iter().filter_map(|(key, value)| {
        (value.replace('\\', "/") == path).then(|| key.strip_suffix(".path")).flatten()
    }).collect();
    if names.len() > 1 { return Err("Multiple .gitmodules registrations use this path; repository identity is ambiguous.".into()); }
    if let Some(name) = names.into_iter().next() {
        let url_key = format!("{name}.url");
        if let Some((_, url)) = records.iter().rev().find(|(key, _)| *key == url_key) {
            side.configured_url = Some(display_url(url));
            match resolved_submodule_io_url(repo, url) {
                Ok(resolved) => side.resolved_url = Some(display_url(&resolved)),
                Err(_) => side.notes.push("Relative URL could not be resolved with the current parent remote.".into()),
            }
        }
        let branch_key = format!("{name}.branch");
        side.configured_branch = records.iter().rev().find(|(key, _)| *key == branch_key).map(|(_, v)| (*v).to_owned());
    }
    Ok(())
}

fn read_side(repo: &Repository, root: &str, path: &Path, revision: Option<&str>, workspace_revision: Option<&str>) -> Result<Side, String> {
    let mut side = Side::default();
    let tree = if let Some(revision) = revision {
        let commit = repo.revparse_single(revision).and_then(|o| o.peel_to_commit()).map_err(|e| e.message().to_owned())?;
        side.source = commit.id().to_string();
        Some(commit.tree().map_err(|e| e.message().to_owned())?)
    } else {
        side.source = "Working tree at comparison time".into();
        None
    };
    let entry = if let Some(tree) = tree.as_ref() {
        super::comparison::snapshot_path_entry(repo, tree, path)
            .map_err(|e| super::comparison::snapshot_error(root, &normalized(path), "selected", &side.source, "submodule-path", e))?
            .map(|e| (e.filemode() as u32, e.id()))
    } else if let Some(oid) = workspace_revision {
        Some((0o160000, Oid::from_str(oid).map_err(|e| e.message().to_owned())?))
    } else {
        repo.index().map_err(|e| e.message().to_owned())?.get_path(path, 0).map(|e| (e.mode, e.id))
    };
    side.kind = match entry {
        None => "absent",
        Some((0o160000, oid)) => { side.revision = Some(oid.to_string()); "submodule" },
        Some(_) => "not-submodule",
    }.into();
    if side.revision.is_some() {
        if let Err(note) = registration(repo, root, tree.as_ref(), &normalized(path), &mut side) { side.notes.push(note); }
        if side.configured_url.is_none() { side.notes.push("No usable URL registration in this version's .gitmodules. The gitlink still exists.".into()); }
    }
    Ok(side)
}

fn enrich(repo: &Repository, side: &mut Side) {
    let Some(oid) = side.revision.as_ref().and_then(|id| Oid::from_str(id).ok()) else { return; };
    match repo.find_commit(oid) {
        Ok(commit) => {
            let author = commit.author();
            side.author = Some(format!("{} <{}>", author.name().unwrap_or("Unknown"), author.email().unwrap_or("")));
            side.authored_at = Some(author.when().seconds());
            side.committed_at = Some(commit.time().seconds());
            side.message = Some(commit.message().unwrap_or("No readable commit message").to_owned());
        }
        Err(_) => side.notes.push("Commit object is not available in this local submodule. No fetch was performed.".into()),
    }
}

fn details(root: String, relative_path: String, left_ref: Option<String>, right_ref: String, workspace_revision: Option<String>) -> Result<Details, String> {
    let started = Instant::now();
    let path = safe_relative_path(&relative_path)?;
    if path.as_os_str().is_empty() { return Err("Select a submodule path.".into()); }
    let repo = Repository::open(&root).map_err(|e| e.message().to_owned())?;
    let mut left = read_side(&repo, &root, &path, left_ref.as_deref(), workspace_revision.as_deref())?;
    let mut right = read_side(&repo, &root, &path, Some(&right_ref), None)?;
    let local_path = repo.workdir().map(|workdir| workdir.join(&path));
    let sub = local_path.as_ref().and_then(|p| internal_submodule_repository(p).ok());
    let mut compare_repository_path = None;
    let mut local_remote_url = None;
    if let Some(sub) = sub {
        local_remote_url = first_remote_url(&sub).map(|url| display_url(&url));
        enrich(&sub, &mut left); enrich(&sub, &mut right);
        // Enumerate only local refs once for both OIDs; tags may be annotated.
        if let Ok(refs) = sub.references() {
            for reference in refs.flatten() {
                if !reference.is_tag() && !reference.is_branch() && !reference.is_remote() { continue; }
                let Ok(commit) = reference.peel_to_commit() else { continue; };
                let oid = commit.id().to_string();
                let label = reference.shorthand().unwrap_or("Unnamed ref").to_owned();
                for side in [&mut left, &mut right] {
                    if side.revision.as_ref() == Some(&oid) {
                        if reference.is_tag() { side.tags.push(label.clone()); } else { side.branches.push(label.clone()); }
                    }
                }
            }
        }
        if left.message.is_some() && right.message.is_some() {
            compare_repository_path = local_path.as_ref().map(|p| p.to_string_lossy().into_owned());
        }
    } else {
        for side in [&mut left, &mut right] {
            if side.revision.is_some() { side.notes.push("Submodule repository is not initialized at this local path. Commit metadata is unavailable; no download was attempted.".into()); }
        }
    }
    let compare_note = if compare_repository_path.is_some() {
        "Compare these exact committed snapshots. Local uncommitted files are not included; no checkout or fetch is performed."
    } else if left.kind != "submodule" || right.kind != "submodule" {
        "Content comparison needs a submodule commit on both sides. This path was added, removed, or changed type."
    } else { "Content comparison needs both commit objects in the initialized local submodule repository." }.into();
    perf_log(&format!("submodule_compare_details: left_available={} right_available={} can_compare={}", left.message.is_some(), right.message.is_some(), compare_repository_path.is_some()), started.elapsed());
    Ok(Details { repository_path: root, relative_path: normalized(&path), local_path: local_path.map(|p| p.to_string_lossy().into_owned()), local_remote_url, left, right, compare_repository_path, compare_note })
}

#[tauri::command]
pub async fn submodule_comparison_details(repository_path: String, relative_path: String, left_ref: Option<String>, right_ref: String, workspace_revision: Option<String>) -> Result<Details, String> {
    off_main_thread(move || details(repository_path, relative_path, left_ref, right_ref, workspace_revision)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("ddt-submodule-details-{}-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir_all(&path).unwrap(); Self(path)
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    fn commit(repo: &Repository, message: &str, entries: &[(&str, Oid, i32)]) -> Oid {
        let mut builder = repo.treebuilder(None).unwrap();
        for (path, id, mode) in entries { builder.insert(*path, *id, *mode).unwrap(); }
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let signature = git2::Signature::new("Developer", "dev@example.test", &git2::Time::new(1760000000, 120)).unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &parent.iter().collect::<Vec<_>>()).unwrap()
    }
    fn config(repo: &Repository, url: &str) -> Oid {
        repo.blob(format!("[submodule \"different name\"]\npath = dep\nurl = {url}\nbranch = releases\n").as_bytes()).unwrap()
    }
    fn inspect(root: &Path, left: Option<Oid>, right: Oid, workspace: Option<Oid>) -> Details {
        details(root.to_string_lossy().into_owned(), "dep".into(), left.map(|id| id.to_string()), right.to_string(), workspace.map(|id| id.to_string())).unwrap()
    }

    #[test]
    fn exact_snapshot_metadata_tags_urls_and_read_only_comparison() {
        let f = Fixture::new(); let parent = Repository::init(&f.0).unwrap();
        parent.remote("origin", "https://example.test/team/parent.git").unwrap();
        let sub = Repository::init(f.0.join("dep")).unwrap();
        sub.remote("origin", "https://user:secret@example.test/team/dep.git?token=secret").unwrap();
        let a = commit(&sub, "First\n\nFull old message", &[]);
        let blob = sub.blob(b"new content").unwrap();
        let b = commit(&sub, "Second\n\nFull new message", &[("file.txt", blob, 0o100644)]);
        let obj = sub.find_object(b, None).unwrap();
        sub.tag("annotated-v2", &obj, &git2::Signature::now("Tagger", "t@example.test").unwrap(), "Release", false).unwrap();
        sub.tag_lightweight("v2", &obj, false).unwrap();
        sub.branch("release", &sub.find_commit(b).unwrap(), false).unwrap();
        let old_config = config(&parent, "../old.git");
        let new_config = config(&parent, "../dep.git");
        let left = commit(&parent, "before", &[("dep", a, 0o160000), (".gitmodules", old_config, 0o100644)]);
        let right = commit(&parent, "after", &[("dep", b, 0o160000), (".gitmodules", new_config, 0o100644)]);
        fs::write(f.0.join("dep/file.txt"), "uncommitted work").unwrap();
        let index_before = fs::read(parent.path().join("index")).ok();
        let d = inspect(&f.0, Some(left), right, None);
        assert_eq!(d.left.message.as_deref(), Some("First\n\nFull old message"));
        assert_eq!(d.right.message.as_deref(), Some("Second\n\nFull new message"));
        assert_eq!(d.right.author.as_deref(), Some("Developer <dev@example.test>"));
        assert_eq!(d.left.resolved_url.as_deref(), Some("https://example.test/team/old.git"));
        assert_eq!(d.right.resolved_url.as_deref(), Some("https://example.test/team/dep.git"));
        assert_eq!(d.local_remote_url.as_deref(), Some("https://example.test/team/dep.git"));
        assert_eq!(d.right.configured_branch.as_deref(), Some("releases"));
        assert!(d.right.tags.contains(&"v2".into()) && d.right.tags.contains(&"annotated-v2".into()));
        assert!(d.right.branches.contains(&"release".into()));
        let comparison = compare_git_tree_file_list(d.compare_repository_path.as_ref().unwrap(), "", &a.to_string(), &b.to_string()).unwrap();
        assert_eq!(comparison.rows.len(), 1);
        let reversed = inspect(&f.0, Some(right), left, None);
        assert_eq!(reversed.left.revision, d.right.revision);
        assert_eq!(reversed.right.revision, d.left.revision);
        assert_eq!(fs::read(parent.path().join("index")).ok(), index_before);
        assert_eq!(parent.head().unwrap().target(), Some(right));
        assert_eq!(sub.head().unwrap().target(), Some(b));
        assert_eq!(fs::read_to_string(f.0.join("dep/file.txt")).unwrap(), "uncommitted work");
    }

    #[test]
    fn added_deleted_orphan_and_missing_objects_do_not_fall_back_to_parent() {
        let f = Fixture::new(); let parent = Repository::init(&f.0).unwrap();
        let missing = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
        let empty = commit(&parent, "empty", &[]);
        let orphan = commit(&parent, "orphan", &[("dep", missing, 0o160000)]);
        fs::create_dir(f.0.join("dep")).unwrap();
        let added = inspect(&f.0, Some(empty), orphan, None);
        assert_eq!(added.left.kind, "absent");
        assert_eq!(added.right.revision.as_deref(), Some(&missing.to_string()[..]));
        assert!(added.right.message.is_none());
        assert!(added.right.notes.iter().any(|note| note.contains("not initialized")));
        assert!(added.right.notes.iter().any(|note| note.contains("No usable URL")));
        assert!(added.compare_repository_path.is_none());
        let removed = inspect(&f.0, Some(orphan), empty, None);
        assert_eq!(removed.right.kind, "absent");
        Repository::init(f.0.join("dep")).unwrap();
        let unavailable = inspect(&f.0, Some(orphan), orphan, None);
        assert!(unavailable.left.notes.iter().any(|note| note.contains("Commit object is not available")));
        assert!(unavailable.compare_repository_path.is_none());
        let regular = parent.blob(b"regular file").unwrap();
        let changed_type = commit(&parent, "typechange", &[("dep", regular, 0o100644)]);
        assert_eq!(inspect(&f.0, Some(orphan), changed_type, None).right.kind, "not-submodule");
    }

    #[test]
    fn workspace_uses_displayed_pointer_not_new_checkout_and_validates_path() {
        let f = Fixture::new(); let parent = Repository::init(&f.0).unwrap();
        let sub = Repository::init(f.0.join("dep")).unwrap();
        let a = commit(&sub, "displayed", &[]);
        let b = commit(&sub, "now checked out", &[]);
        let source = commit(&parent, "source", &[("dep", b, 0o160000)]);
        fs::write(f.0.join(".gitmodules"), "[submodule \"local\"]\npath=dep\nurl=https://example.test/local.git\n").unwrap();
        let d = inspect(&f.0, None, source, Some(a));
        assert_eq!(d.left.message.as_deref(), Some("displayed"));
        assert_eq!(d.left.configured_url.as_deref(), Some("https://example.test/local.git"));
        assert!(d.compare_repository_path.is_some());
        assert_eq!(sub.head().unwrap().target(), Some(b));
        assert!(details(f.0.to_string_lossy().into_owned(), "../escape".into(), None, source.to_string(), None).is_err());
    }
}
