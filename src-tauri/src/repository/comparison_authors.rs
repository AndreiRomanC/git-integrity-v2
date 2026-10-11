//! Optional, read-only metadata for the existing snapshot comparison.
use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct ComparisonAuthor {
    key: String,
    name: String,
    email: String,
    commits: usize,
    merge_commits: usize,
    paths: Vec<String>,
}

// Symmetric difference: commits reachable from either endpoint but not both.
// Only non-merge commits establish a touch. A merge diff against any parent
// contains imported changes; even the intersection of parent diffs can contain
// automatically merged edits. Do not attribute those to the merge author.
// Incoming commits are still walked and attributed to their recorded authors.
fn collect(repo: &Repository, left: Oid, right: Oid) -> Result<Vec<ComparisonAuthor>, String> {
    collect_for_paths(repo, left, right, None)
}

fn collect_for_paths(repo: &Repository, left: Oid, right: Oid, comparison_paths: Option<Vec<String>>) -> Result<Vec<ComparisonAuthor>, String> {
    if repo.is_shallow() {
        return Err("Author filtering needs complete local history; this repository is shallow. No partial author results were applied.".into());
    }
    let started = Instant::now();
    let relevant: HashSet<String> = if let Some(paths) = comparison_paths {
        // Workspace comparisons already have their exact displayed paths. Do
        // not scan the worktree again or intersect only HEAD's net tree diff:
        // a committed change may have been undone before a local edit.
        paths.into_iter().collect()
    } else {
        let left_tree = repo.find_commit(left).and_then(|c| c.tree()).map_err(|e| e.to_string())?;
        let right_tree = repo.find_commit(right).and_then(|c| c.tree()).map_err(|e| e.to_string())?;
        let comparison = repo.diff_tree_to_tree(Some(&left_tree), Some(&right_tree), None).map_err(|e| e.to_string())?;
        comparison.deltas().flat_map(|d| {
            [d.old_file().path().map(normalized), d.new_file().path().map(normalized)].into_iter().flatten()
        }).collect()
    };
    let mut authors: BTreeMap<String, (ComparisonAuthor, HashSet<String>)> = BTreeMap::new();
    let mut count = 0;
    for (tip, hide) in [(left, right), (right, left)] {
        let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
        walk.push(tip).map_err(|e| e.to_string())?;
        walk.hide(hide).map_err(|e| e.to_string())?;
        for oid in walk {
            count += 1;
            if count > 100_000 || started.elapsed() > Duration::from_secs(60) {
                return Err("Author history is too large for this request. Choose closer revisions. No partial author results were applied.".into());
            }
            let commit = repo.find_commit(oid.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let signature = commit.author();
            let name = signature.name().unwrap_or("Unknown author").to_string();
            let email = signature.email().unwrap_or("").trim().to_string();
            // Preserve email case: do not silently merge distinct identities.
            let key = if email.is_empty() { format!("name:{name}") } else { format!("email:{email}") };
            let (author, paths) = authors.entry(key.clone()).or_insert_with(|| (ComparisonAuthor {
                key, name, email, commits: 0, merge_commits: 0, paths: Vec::new(),
            }, HashSet::new()));
            author.commits += 1;
            author.merge_commits += usize::from(commit.parent_count() > 1);
            // Keep merge-only authors selectable (zero matching files), but
            // never treat integrating other people's commits as their own edit.
            // Resolution-only edits are deliberately excluded: stored parent
            // trees cannot prove which edits were made manually by this author.
            if commit.parent_count() > 1 { continue; }
            let tree = commit.tree().map_err(|e| e.to_string())?;
            for parent_index in 0..commit.parent_count().max(1) {
                let parent_tree = if commit.parent_count() == 0 { None }
                    else { Some(commit.parent(parent_index).and_then(|p| p.tree()).map_err(|e| e.to_string())?) };
                let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None).map_err(|e| e.to_string())?;
                for delta in diff.deltas() {
                    // Renames retain existing comparison semantics (delete/add).
                    for path in [delta.old_file().path(), delta.new_file().path()].into_iter().flatten() {
                        let path = normalized(path);
                        if relevant.contains(&path) { paths.insert(path); }
                    }
                }
            }
        }
    }
    let result = authors.into_values().map(|(mut author, paths)| {
        author.paths = paths.into_iter().collect();
        author.paths.sort();
        author
    }).collect();
    perf_log(&format!("comparison_authors: {count} commits (object trees only; non-merge touches)"), started.elapsed());
    Ok(result)
}

#[tauri::command]
pub async fn comparison_authors(repository_path: String, submodule_path: Option<String>, owner: Option<String>, repository_name: Option<String>, left_revision: String, right_revision: String, comparison_paths: Option<Vec<String>>) -> Result<Vec<ComparisonAuthor>, String> {
    off_main_thread(move || {
        validate_path(&repository_path)?;
        let path = match (owner, repository_name, submodule_path) {
            (Some(owner), Some(name), None) => {
                let (owner, name) = validate_github_module_identity(&owner, &name)?;
                // The comparison has already populated this object cache. Do
                // not fetch or authenticate merely to switch an author filter.
                github_module_cache_path(&owner, &name)
            }
            (None, None, Some(path)) => validate_submodule(&repository_path, &path)?,
            (None, None, None) => PathBuf::from(&repository_path),
            _ => return Err("Invalid comparison repository context".into()),
        };
        let repo = internal_repository(&path.to_string_lossy())?;
        let left = Oid::from_str(&left_revision).map_err(|e| e.to_string())?;
        let right = Oid::from_str(&right_revision).map_err(|e| e.to_string())?;
        if comparison_paths.is_some() { collect_for_paths(&repo, left, right, comparison_paths) }
        else { collect(&repo, left, right) }
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture { repo: Repository, path: PathBuf }
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("ddt-author-test-{}-{}-{sequence}", std::process::id(), std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
            Self { repo: Repository::init(&path).unwrap(), path }
        }
        fn commit(&self, email: &str, files: &[(&str, &str)], parents: &[Oid]) -> Oid {
            let mut builder = self.repo.treebuilder(None).unwrap();
            for (name, content) in files { builder.insert(*name, self.repo.blob(content.as_bytes()).unwrap(), 0o100644).unwrap(); }
            let tree = self.repo.find_tree(builder.write().unwrap()).unwrap();
            let signature = git2::Signature::now("Same display name", email).unwrap();
            let parents: Vec<_> = parents.iter().map(|id| self.repo.find_commit(*id).unwrap()).collect();
            self.repo.commit(None, &signature, &signature, "Checkpoint", &tree, &parents.iter().collect::<Vec<_>>()).unwrap()
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.path); } }
    fn paths<'a>(authors: &'a [ComparisonAuthor], email: &str) -> Vec<&'a str> {
        authors.iter().find(|a| a.email == email).unwrap().paths.iter().map(String::as_str).collect()
    }

    #[test]
    fn comparison_authors_linear_intersects_net_diff_and_keeps_full_contents() {
        let f = Fixture::new();
        let a = f.commit("base@test", &[("shared", "base"), ("gone", "delete")], &[]);
        let b = f.commit("alice@test", &[("shared", "alice"), ("temporary", "one")], &[a]);
        let c = f.commit("bob@test", &[("shared", "alice and bob"), ("new", "added")], &[b]);
        let authors = collect(&f.repo, a, c).unwrap();
        assert_eq!(authors.len(), 2); // identities separated despite same name
        assert_eq!(paths(&authors, "alice@test"), ["gone", "shared"]);
        assert_eq!(paths(&authors, "bob@test"), ["new", "shared"]);
        let compare = compare_git_tree_file(&f.path.to_string_lossy(), "shared", &a.to_string(), &c.to_string()).unwrap();
        assert_eq!(compare.local_content, "base");
        assert_eq!(compare.remote_content, "alice and bob");
        assert!(collect(&f.repo, c, c).unwrap().is_empty());
    }

    #[test]
    fn comparison_authors_merge_does_not_attribute_imports_or_resolutions_to_integrator() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("f", "base")], &[]);
        let local = f.commit("alice@test", &[("f", "ours"), ("ours", "x")], &[base]);
        let incoming = f.commit("bob@test", &[("f", "theirs"), ("import", "x")], &[base]);
        let merged = f.commit("merger@test", &[("f", "ours"), ("ours", "x"), ("import", "x"), ("resolution", "new")], &[local, incoming]);
        let authors = collect(&f.repo, base, merged).unwrap();
        assert!(paths(&authors, "merger@test").is_empty());
        assert_eq!(paths(&authors, "alice@test"), ["f", "ours"]);
        assert_eq!(paths(&authors, "bob@test"), ["f", "import"]);
        assert_eq!(authors.iter().find(|a| a.email == "merger@test").unwrap().merge_commits, 1);
        assert!(paths(&collect(&f.repo, local, merged).unwrap(), "merger@test").is_empty());
        assert_eq!(paths(&collect(&f.repo, local, merged).unwrap(), "bob@test"), ["import"]);
    }

    #[test]
    fn comparison_authors_keeps_personal_touches_after_other_authors_and_repeated_merges() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("shared", "base")], &[]);
        let alice = f.commit("alice@test", &[("shared", "alice")], &[base]);
        let bob = f.commit("bob@test", &[("shared", "bob"), ("import", "bob")], &[base]);
        // Even a file differing from BOTH parents may be an automatic merge,
        // not an edit authored by the person pressing Merge.
        let merged = f.commit("alice@test", &[("shared", "alice and bob"), ("import", "bob")], &[alice, bob]);
        let carol = f.commit("carol@test", &[("shared", "carol later"), ("import", "bob"), ("new", "carol")], &[bob]);
        let final_merge = f.commit("alice@test", &[("shared", "all edits"), ("import", "bob"), ("new", "carol")], &[merged, carol]);
        let authors = collect(&f.repo, base, final_merge).unwrap();
        assert_eq!(paths(&authors, "alice@test"), ["shared"]);
        assert_eq!(paths(&authors, "bob@test"), ["import", "shared"]);
        assert_eq!(paths(&authors, "carol@test"), ["new", "shared"]);
        let compare = compare_git_tree_file(&f.path.to_string_lossy(), "shared", &base.to_string(), &final_merge.to_string()).unwrap();
        assert_eq!(compare.remote_content, "all edits");
        let workspace = collect_for_paths(&f.repo, base, final_merge, Some(vec!["shared".into(), "import".into(), "new".into()])).unwrap();
        assert_eq!(paths(&workspace, "alice@test"), ["shared"]);
    }

    #[test]
    fn comparison_authors_diverged_reversed_and_renamed_paths() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("old", "same")], &[]);
        let left = f.commit("left@test", &[("renamed", "same")], &[base]);
        let right = f.commit("right@test", &[("old", "changed")], &[base]);
        let forward = collect(&f.repo, left, right).unwrap();
        assert_eq!(paths(&forward, "left@test"), ["old", "renamed"]);
        assert_eq!(paths(&forward, "right@test"), ["old"]);
        assert_eq!(serde_json::to_value(&forward).unwrap(), serde_json::to_value(collect(&f.repo, right, left).unwrap()).unwrap());
        assert_eq!(compare_git_tree_file_list(&f.path.to_string_lossy(), "", &base.to_string(), &left.to_string()).unwrap().rows.len(), 2);
    }

    #[test]
    fn comparison_authors_workspace_paths_include_reverted_touches_but_not_unattributed_files() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("f", "base")], &[]);
        let changed = f.commit("alice@test", &[("f", "changed")], &[base]);
        let tip = f.commit("bob@test", &[("f", "base")], &[changed]);
        f.repo.reference("refs/heads/main", tip, true, "test").unwrap();
        f.repo.set_head("refs/heads/main").unwrap();
        fs::write(f.path.join("f"), "local edit after revert").unwrap();
        fs::write(f.path.join("untracked"), "no author").unwrap();
        let mut index = f.repo.index().unwrap();
        index.add_path(Path::new("f")).unwrap(); index.write().unwrap();
        let before = fs::read(f.repo.path().join("index")).unwrap();
        assert!(paths(&collect(&f.repo, base, tip).unwrap(), "alice@test").is_empty());
        let authors = collect_for_paths(&f.repo, base, tip, Some(vec!["f".into(), "untracked".into()])).unwrap();
        assert_eq!(paths(&authors, "alice@test"), ["f"]);
        assert_eq!(paths(&authors, "bob@test"), ["f"]);
        assert!(collect_for_paths(&f.repo, tip, tip, Some(vec!["f".into()])).unwrap().is_empty());
        assert_eq!(fs::read(f.repo.path().join("index")).unwrap(), before);
        assert_eq!(fs::read_to_string(f.path.join("f")).unwrap(), "local edit after revert");
        assert_eq!(fs::read_to_string(f.path.join("untracked")).unwrap(), "no author");
    }

    #[test]
    fn comparison_authors_reads_objects_only_and_rejects_missing_history() {
        let f = Fixture::new();
        let base = f.commit("a@test", &[("f", "old")], &[]);
        let tip = f.commit("a@test", &[("f", "new")], &[base]);
        f.repo.reference("refs/heads/test", tip, true, "test").unwrap();
        f.repo.set_head("refs/heads/test").unwrap();
        fs::write(f.path.join("f"), "local changes").unwrap();
        fs::write(f.path.join("untracked"), "untouched").unwrap();
        let mut index = f.repo.index().unwrap(); index.add_path(Path::new("f")).unwrap(); index.write().unwrap();
        let before = fs::read(f.repo.path().join("index")).unwrap();
        collect(&f.repo, base, tip).unwrap();
        assert_eq!(fs::read(f.repo.path().join("index")).unwrap(), before);
        assert_eq!(fs::read_to_string(f.path.join("f")).unwrap(), "local changes");
        assert_eq!(fs::read_to_string(f.path.join("untracked")).unwrap(), "untouched");
        assert_eq!(f.repo.head().unwrap().target(), Some(tip));
        assert!(collect(&f.repo, Oid::zero(), tip).is_err());
        fs::write(f.repo.path().join("shallow"), format!("{base}\n")).unwrap();
        let reopened = Repository::open(&f.path).unwrap();
        assert!(collect(&reopened, base, tip).unwrap_err().contains("shallow"));
    }
}
