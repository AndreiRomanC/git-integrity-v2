//! Optional, read-only metadata for the existing snapshot comparison.
use super::*;
use std::collections::BTreeMap;
pub mod strict;

#[derive(Debug, Serialize)]
pub struct ComparisonAuthor {
    key: String,
    name: String,
    email: String,
    commits: usize,
    merge_commits: usize,
    paths: Vec<String>,
    direct_paths: Vec<String>,
    evidence: BTreeMap<String, Vec<ComparisonTouch>>,
}

#[derive(Debug, Serialize)]
pub struct ComparisonTouch {
    commit: String,
    parent: Option<String>,
    change: String,
    direct: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    witness_path: Option<String>,
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
    let mut authors: BTreeMap<String, (ComparisonAuthor, HashSet<String>, HashSet<String>)> = BTreeMap::new();
    let mut count = 0;
    let mut direct_count = 0;
    for (tip, hide) in [(left, right), (right, left)] {
        let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
        // Children before parents lets us follow each endpoint's first-parent
        // chain during the existing walk, with no second walk or tree diff.
        walk.set_sorting(git2::Sort::TOPOLOGICAL).map_err(|e| e.to_string())?;
        walk.push(tip).map_err(|e| e.to_string())?;
        walk.hide(hide).map_err(|e| e.to_string())?;
        let mut direct_next = Some(tip);
        for oid in walk {
            count += 1;
            if count > 100_000 || started.elapsed() > Duration::from_secs(60) {
                return Err("Author history is too large for this request. Choose closer revisions. No partial author results were applied.".into());
            }
            let commit = repo.find_commit(oid.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let direct = direct_next == Some(commit.id());
            if direct {
                direct_next = if commit.parent_count() == 0 { None }
                    else { Some(commit.parent_id(0).map_err(|e| e.to_string())?) };
            }
            let signature = commit.author();
            let name = signature.name().unwrap_or("Unknown author").to_string();
            let email = signature.email().unwrap_or("").trim().to_string();
            // Preserve email case: do not silently merge distinct identities.
            let key = if email.is_empty() { format!("name:{name}") } else { format!("email:{email}") };
            let (author, paths, direct_paths) = authors.entry(key.clone()).or_insert_with(|| (ComparisonAuthor {
                key, name, email, commits: 0, merge_commits: 0, paths: Vec::new(), direct_paths: Vec::new(), evidence: BTreeMap::new(),
            }, HashSet::new(), HashSet::new()));
            author.commits += 1;
            author.merge_commits += usize::from(commit.parent_count() > 1);
            // Keep merge-only authors selectable (zero matching files), but
            // never treat integrating other people's commits as their own edit.
            // Resolution-only edits are deliberately excluded: stored parent
            // trees cannot prove which edits were made manually by this author.
            if commit.parent_count() > 1 { continue; }
            direct_count += usize::from(direct);
            let tree = commit.tree().map_err(|e| e.to_string())?;
            for parent_index in 0..commit.parent_count().max(1) {
                let parent_tree = if commit.parent_count() == 0 { None }
                    else { Some(commit.parent(parent_index).and_then(|p| p.tree()).map_err(|e| e.to_string())?) };
                let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None).map_err(|e| e.to_string())?;
                for delta in diff.deltas() {
                    // Renames retain existing comparison semantics (delete/add).
                    for path in [delta.old_file().path(), delta.new_file().path()].into_iter().flatten() {
                        let path = normalized(path);
                        if relevant.contains(&path) {
                            // At most one direct and one imported witness per
                            // path. Reuse this diff: no extra history/worktree
                            // scan, and no unbounded list of all file touches.
                            let evidence = author.evidence.entry(path.clone()).or_default();
                            if !evidence.iter().any(|touch| touch.direct == direct) {
                                evidence.push(ComparisonTouch {
                                    commit: commit.id().to_string(),
                                    parent: if commit.parent_count() == 0 { None } else { Some(commit.parent_id(0).map_err(|e| e.to_string())?.to_string()) },
                                    change: format!("{:?}", delta.status()),
                                    direct,
                                    witness_path: None,
                                });
                            }
                            if direct { direct_paths.insert(path.clone()); }
                            paths.insert(path);
                        }
                    }
                }
            }
        }
    }
    let result = authors.into_values().map(|(mut author, paths, direct_paths)| {
        author.paths = paths.into_iter().collect();
        author.paths.sort();
        author.direct_paths = direct_paths.into_iter().collect();
        author.direct_paths.sort();
        author
    }).collect();
    perf_log(&format!("comparison_authors: {count} commits; direct_non_merge={direct_count} (object trees only; non-merge touches; first-parent metadata)"), started.elapsed());
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
    fn flat_list_preserves_gitlink_only_changes_without_submodule_objects() {
        let f = Fixture::new();
        let signature = git2::Signature::now("Author", "author@test").unwrap();
        let make = |sha: &str, parent: Option<Oid>| {
            let mut builder = f.repo.treebuilder(None).unwrap();
            builder.insert("module", Oid::from_str(sha).unwrap(), 0o160000).unwrap();
            let tree = f.repo.find_tree(builder.write().unwrap()).unwrap();
            let parents: Vec<_> = parent.into_iter().map(|id| f.repo.find_commit(id).unwrap()).collect();
            f.repo.commit(None, &signature, &signature, "pointer", &tree, &parents.iter().collect::<Vec<_>>()).unwrap()
        };
        let base = make("1111111111111111111111111111111111111111", None);
        let tip = make("2222222222222222222222222222222222222222", Some(base));
        let rows = compare_git_tree_file_list(&f.path.to_string_lossy(), "", &base.to_string(), &tip.to_string()).unwrap().rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "modified");
        assert_eq!(rows[0].local.as_ref().unwrap().kind, "submodule");
        assert_eq!(rows[0].remote.as_ref().unwrap().kind, "submodule");
        assert_eq!(rows[0].local.as_ref().unwrap().revision.as_deref(), Some("1111111111111111111111111111111111111111"));
        let comparison = compare_git_tree_file(&f.path.to_string_lossy(), "module", &base.to_string(), &tip.to_string()).unwrap();
        assert_eq!(comparison.local_content, "Subproject commit 1111111111111111111111111111111111111111\n");
        assert_eq!(comparison.remote_content, "Subproject commit 2222222222222222222222222222222222222222\n");
        assert!(tree_file_content(&f.path.to_string_lossy(), &base.to_string(), "module", false).is_err());
    }

    #[test]
    fn evidence_reuses_real_parent_diffs_and_is_bounded_per_path() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("shared", "base"), ("gone", "old")], &[]);
        let direct = f.commit("alice@test", &[("shared", "direct")], &[base]);
        let side = f.commit("alice@test", &[("shared", "side"), ("gone", "old"), ("imported", "side")], &[base]);
        let merged = f.commit("alice@test", &[("shared", "combined"), ("imported", "side")], &[direct, side]);
        let tip = f.commit("alice@test", &[("shared", "later"), ("imported", "side")], &[merged]);
        let result = collect(&f.repo, base, tip).unwrap();
        let author = result.iter().find(|a| a.email == "alice@test").unwrap();
        let evidence = &author.evidence["shared"];
        assert_eq!(evidence.len(), 2);
        let own = evidence.iter().find(|e| e.direct).unwrap();
        assert_eq!(own.commit, tip.to_string());
        assert_eq!(own.parent, Some(merged.to_string()));
        let imported = evidence.iter().find(|e| !e.direct).unwrap();
        assert_eq!(imported.commit, side.to_string());
        assert_eq!(imported.parent, Some(base.to_string()));
        assert_eq!(author.evidence["gone"][0].change, "Deleted");
        assert_eq!(author.evidence["imported"][0].change, "Added");
        assert!(author.evidence.values().flatten().all(|e| e.commit != merged.to_string()));
        let diff = compare_git_tree_file(&f.path.to_string_lossy(), "shared", own.parent.as_ref().unwrap(), &own.commit).unwrap();
        assert_eq!(diff.local_content, "combined");
        assert_eq!(diff.remote_content, "later");
    }

    #[test]
    fn direct_history_excludes_same_author_imports_but_keeps_later_direct_touches() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("shared", "base")], &[]);
        let own = f.commit("alice@test", &[("shared", "own")], &[base]);
        let side = f.commit("alice@test", &[("shared", "base"), ("imported", "side"), ("later", "side")], &[base]);
        let merged = f.commit("alice@test", &[("shared", "own"), ("imported", "side"), ("later", "side")], &[own, side]);
        let tip = f.commit("alice@test", &[("shared", "own"), ("imported", "side"), ("later", "direct edit")], &[merged]);
        let result = collect(&f.repo, base, tip).unwrap();
        let alice = result.iter().find(|a| a.email == "alice@test").unwrap();
        assert_eq!(alice.paths, ["imported", "later", "shared"]);
        assert_eq!(alice.direct_paths, ["later", "shared"]);
        assert_eq!(serde_json::to_value(&result).unwrap(), serde_json::to_value(collect(&f.repo, tip, base).unwrap()).unwrap());
        let workspace = collect_for_paths(&f.repo, base, tip, Some(vec!["imported".into(), "later".into(), "untracked".into()])).unwrap();
        assert_eq!(workspace.iter().find(|a| a.email == "alice@test").unwrap().direct_paths, ["later"]);
    }

    #[test]
    fn direct_history_keeps_both_diverged_endpoints_and_excludes_merge_only_edits() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[], &[]);
        let left = f.commit("alice@test", &[("left", "x")], &[base]);
        let right = f.commit("alice@test", &[("right", "x")], &[base]);
        let result = collect(&f.repo, left, right).unwrap();
        assert_eq!(result[0].direct_paths, ["left", "right"]);
        let merged = f.commit("merger@test", &[("left", "x"), ("right", "x"), ("resolution", "x")], &[left, right]);
        let result = collect(&f.repo, base, merged).unwrap();
        assert_eq!(result.iter().find(|a| a.email == "alice@test").unwrap().direct_paths, ["left"]);
        assert!(result.iter().find(|a| a.email == "merger@test").unwrap().direct_paths.is_empty());
        assert!(collect(&f.repo, merged, merged).unwrap().is_empty());
    }

    #[test]
    fn direct_history_does_not_guess_squash_or_fast_forward_provenance() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[], &[]);
        // A single-parent commit cannot tell us whether its files were typed,
        // copied, squash-imported, or brought in via a fast-forward.
        let tip = f.commit("alice@test", &[("import", "x")], &[base]);
        let result = collect(&f.repo, base, tip).unwrap();
        assert_eq!(result[0].direct_paths, result[0].paths);
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
