//! Optional, read-only metadata for the existing snapshot comparison.
use super::*;
use std::collections::BTreeMap;
pub mod strict;
pub mod introduced;

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
    date: String,
    parent: Option<String>,
    change: String,
    direct: bool,
    kind: &'static str,
    #[serde(flatten)]
    context: EvidenceContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    witness_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct EvidenceContext {
    side: &'static str,
    repository_path: String,
    repository_id: String,
}

impl EvidenceContext {
    fn new(repo: &Repository, side: &'static str) -> Self {
        Self { side, repository_path: normalized(repo.workdir().unwrap_or(repo.path())),
            repository_id: anonymized_repository_id(&repo.path().to_string_lossy()) }
    }
}

fn is_folder_submodule_conversion(before: Option<i32>, after: Option<i32>) -> bool {
    matches!((before, after), (Some(0o160000), Some(0o40000)) | (Some(0o40000), Some(0o160000)))
}

// Reuse the already computed diff. Only changed gitlink roots need an entry
// lookup; never enumerate a submodule or scan each descendant's history again.
fn conversion_roots(diff: &git2::Diff, before: Option<&git2::Tree>, after: &git2::Tree) -> Result<HashSet<String>, String> {
    let mode = |tree: Option<&git2::Tree>, path: &Path| match tree.map(|t| t.get_path(path)) {
        Some(Ok(entry)) => Ok(Some(entry.filemode())),
        None => Ok(None),
        Some(Err(e)) if e.code() == git2::ErrorCode::NotFound => Ok(None),
        Some(Err(e)) => Err(e.to_string()),
    };
    let mut roots = HashSet::new();
    for delta in diff.deltas() {
        if delta.old_file().mode() == git2::FileMode::Commit && delta.new_file().mode() == git2::FileMode::Commit { continue; }
        for file in [delta.old_file(), delta.new_file()] {
            if file.mode() != git2::FileMode::Commit { continue; }
            if let Some(path) = file.path() {
                if is_folder_submodule_conversion(mode(before, path)?, mode(Some(after), path)?) {
                    roots.insert(normalized(path));
                }
            }
        }
    }
    Ok(roots)
}

fn conversion_for_path<'a>(path: &'a str, roots: &HashSet<String>) -> Option<&'a str> {
    if roots.is_empty() { return None; }
    let mut prefix = path;
    loop {
        if roots.contains(prefix) { return Some(prefix); }
        prefix = prefix.rsplit_once('/')?.0;
    }
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
    let scoped = comparison_paths.is_some();
    let relevant: HashSet<String> = if let Some(paths) = comparison_paths {
        // The caller supplies the exact loaded file/gitlink paths, including
        // folder scope. For workspaces do not intersect only HEAD's net diff:
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
    if scoped && relevant.is_empty() {
        perf_log("comparison_authors: scoped_paths=0; authors=0; history walk skipped", started.elapsed());
        return Ok(Vec::new());
    }
    let mut authors: BTreeMap<String, (ComparisonAuthor, HashSet<String>, HashSet<String>)> = BTreeMap::new();
    let mut count = 0;
    let mut direct_count = 0;
    let mut structural_items = 0;
    for (tip, hide, side) in [(left, right, "left"), (right, left, "right")] {
        let context = EvidenceContext::new(repo, side);
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
            if !scoped {
                author.commits += 1;
                author.merge_commits += usize::from(commit.parent_count() > 1);
            }
            // Unscoped legacy callers can retain merge-only identities. Scoped
            // comparisons return only authors with actual matching path touches;
            // never treat integrating other people's commits as their own edit.
            // Resolution-only edits are deliberately excluded: stored parent
            // trees cannot prove which edits were made manually by this author.
            if commit.parent_count() > 1 { continue; }
            let mut touched_scope = false;
            let tree = commit.tree().map_err(|e| e.to_string())?;
            for parent_index in 0..commit.parent_count().max(1) {
                let parent_tree = if commit.parent_count() == 0 { None }
                    else { Some(commit.parent(parent_index).and_then(|p| p.tree()).map_err(|e| e.to_string())?) };
                let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None).map_err(|e| e.to_string())?;
                let conversions = conversion_roots(&diff, parent_tree.as_ref(), &tree)?;
                for delta in diff.deltas() {
                    // Renames retain existing comparison semantics (delete/add).
                    for path in [delta.old_file().path(), delta.new_file().path()].into_iter().flatten() {
                        let path = normalized(path);
                        if relevant.contains(&path) {
                            let conversion = conversion_for_path(&path, &conversions);
                            let structural_child = conversion.is_some_and(|root| root != path);
                            let kind = if conversion.is_some() { "folder-submodule-conversion" }
                                else if delta.old_file().mode() == git2::FileMode::Commit || delta.new_file().mode() == git2::FileMode::Commit { "submodule-update" }
                                else { "file-change" };
                            touched_scope |= !structural_child;
                            // Bounded evidence by kind, side and first-parent
                            // membership. A conversion never suppresses a real
                            // later file edit, or an event on the other side.
                            let evidence = author.evidence.entry(path.clone()).or_default();
                            if !evidence.iter().any(|touch| touch.direct == direct && touch.kind == kind && touch.context.side == side) {
                                evidence.push(ComparisonTouch {
                                    commit: commit.id().to_string(),
                                    date: short_date(commit.time().seconds()),
                                    parent: if commit.parent_count() == 0 { None } else { Some(commit.parent_id(0).map_err(|e| e.to_string())?.to_string()) },
                                    change: format!("{:?}", delta.status()),
                                    direct,
                                    kind, context: context.clone(),
                                    witness_path: conversion.map(str::to_owned),
                                });
                                structural_items += usize::from(structural_child);
                            }
                            // Keep structural evidence for All differences,
                            // not in the file-touch/shared contributor set.
                            if !structural_child {
                                if direct { direct_paths.insert(path.clone()); }
                                paths.insert(path);
                            }
                        }
                    }
                }
            }
            if scoped && touched_scope { author.commits += 1; }
            direct_count += usize::from(direct && (!scoped || touched_scope));
        }
    }
    let result: Vec<_> = authors.into_values().filter(|(author, _, _)| !scoped || !author.evidence.is_empty()).map(|(mut author, paths, direct_paths)| {
        author.paths = paths.into_iter().collect();
        author.paths.sort();
        author.direct_paths = direct_paths.into_iter().collect();
        author.direct_paths.sort();
        author
    }).collect();
    perf_log(&format!("comparison_authors: {count} commits; direct_non_merge={direct_count}; scoped={scoped}; compared_paths={}; authors={}; structural_items={structural_items} (object trees only; non-merge touches; conversion imports separate)", relevant.len(), result.len()), started.elapsed());
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
    pub(super) struct Fixture { pub(super) repo: Repository, pub(super) path: PathBuf }
    impl Fixture {
        pub(super) fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("ddt-author-test-{}-{}-{sequence}", std::process::id(), std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
            Self { repo: Repository::init(&path).unwrap(), path }
        }
        pub(super) fn commit(&self, email: &str, files: &[(&str, &str)], parents: &[Oid]) -> Oid {
            self.entries(email, &files.iter().map(|&(path, content)| (path, content, 0o100644)).collect::<Vec<_>>(), parents)
        }
        pub(super) fn entries(&self, email: &str, files: &[(&str, &str, i32)], parents: &[Oid]) -> Oid {
            fn tree(repo: &Repository, files: &[(&str, &str, i32)]) -> Oid {
                let mut builder = repo.treebuilder(None).unwrap();
                let mut folders: BTreeMap<&str, Vec<(&str, &str, i32)>> = BTreeMap::new();
                for &(path, content, mode) in files {
                    if let Some((folder, rest)) = path.split_once('/') { folders.entry(folder).or_default().push((rest, content, mode)); }
                    else {
                        let id = if mode == 0o160000 { Oid::from_str(content).unwrap() } else { repo.blob(content.as_bytes()).unwrap() };
                        builder.insert(path, id, mode).unwrap();
                    }
                }
                for (folder, children) in folders { builder.insert(folder, tree(repo, &children), 0o40000).unwrap(); }
                builder.write().unwrap()
            }
            let tree = self.repo.find_tree(tree(&self.repo, files)).unwrap();
            let signature = git2::Signature::now("Same display name", email).unwrap();
            let parents: Vec<_> = parents.iter().map(|id| self.repo.find_commit(*id).unwrap()).collect();
            self.repo.commit(None, &signature, &signature, "Checkpoint", &tree, &parents.iter().collect::<Vec<_>>()).unwrap()
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.path); } }
    fn paths<'a>(authors: &'a [ComparisonAuthor], email: &str) -> Vec<&'a str> {
        authors.iter().find(|a| a.email == email).unwrap().paths.iter().map(String::as_str).collect()
    }

    fn compared_paths(f: &Fixture, folder: &str, left: Oid, right: Oid) -> Vec<String> {
        compare_git_tree_file_list(&f.path.to_string_lossy(), folder, &left.to_string(), &right.to_string()).unwrap().rows.into_iter()
            .flat_map(|row| [row.local.map(|e| e.relative_path), row.remote.map(|e| e.relative_path)].into_iter().flatten()).collect()
    }

    fn contributors<'a>(authors: &'a [ComparisonAuthor], path: &str) -> Vec<&'a str> {
        authors.iter().filter(|a| a.paths.iter().any(|p| p == path)).map(|a| a.email.as_str()).collect()
    }

    fn assert_reversed(forward: &[ComparisonAuthor], mut reversed: Vec<ComparisonAuthor>) {
        for author in &mut reversed {
            for event in author.evidence.values_mut().flatten() {
                event.context.side = match event.context.side { "left" => "right", "right" => "left", other => panic!("Unknown side: {other}") };
            }
        }
        assert_eq!(serde_json::to_value(forward).unwrap(), serde_json::to_value(reversed).unwrap());
    }

    #[test]
    fn shared_history_keeps_overwritten_intermediate_touches_but_not_fully_reverted_files() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("folder/shared", "base"), ("folder/reverted", "base"), ("folder/solo", "base"), ("elsewhere/file", "base")], &[]);
        let intermediate = f.commit("alice@test", &[("folder/shared", "alice"), ("folder/reverted", "temporary"), ("folder/solo", "alice"), ("elsewhere/file", "base")], &[base]);
        let repeated = f.commit("alice@test", &[("folder/shared", "alice again"), ("folder/reverted", "temporary"), ("folder/solo", "alice again"), ("elsewhere/file", "base")], &[intermediate]);
        let overwritten = f.commit("bob@test", &[("folder/shared", "bob replaces everything"), ("folder/reverted", "base"), ("folder/solo", "alice again"), ("elsewhere/file", "base")], &[repeated]);
        let tip = f.commit("endpoint@test", &[("folder/shared", "bob replaces everything"), ("folder/reverted", "base"), ("folder/solo", "alice again"), ("elsewhere/file", "endpoint")], &[overwritten]);
        let scope = compared_paths(&f, "folder", base, tip);
        let result = collect_for_paths(&f.repo, base, tip, Some(scope.clone())).unwrap();
        assert_eq!(contributors(&result, "folder/shared"), ["alice@test", "bob@test"]);
        assert_eq!(contributors(&result, "folder/solo"), ["alice@test"], "two commits from one identity do not make a shared file");
        assert!(contributors(&result, "folder/reverted").is_empty(), "identical endpoints exclude even a file touched by two authors");
        assert!(contributors(&result, "elsewhere/file").is_empty());
        assert_eq!(result.len(), 2, "the endpoint author's unrelated changes do not establish authorship");
        assert!(result.iter().all(|a| a.name == "Same display name"), "identities use emails, not display names");
        assert_eq!(result[0].commits, 2);
        let evidence = &result[0].evidence["folder/shared"][0];
        assert_eq!(evidence.commit, repeated.to_string());
        assert_eq!(evidence.parent.as_deref(), Some(intermediate.to_string().as_str()));
        assert!(!evidence.date.is_empty());
        assert_reversed(&result, collect_for_paths(&f.repo, tip, base, Some(scope)).unwrap());
        let full_diff = compare_git_tree_file(&f.path.to_string_lossy(), "folder/shared", &base.to_string(), &tip.to_string()).unwrap();
        assert_eq!(full_diff.local_content, "base");
        assert_eq!(full_diff.remote_content, "bob replaces everything", "history metadata never substitutes an intermediate diff");
    }

    #[test]
    fn shared_history_counts_both_divergent_sides_and_original_authors_not_merge_resolutions() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("folder/shared", "base")], &[]);
        let left = f.commit("alice@test", &[("folder/shared", "left")], &[base]);
        let right = f.commit("bob@test", &[("folder/shared", "right")], &[base]);
        let merged = f.commit("integrator@test", &[("folder/shared", "resolved"), ("folder/resolution_only", "new")], &[left, right]);
        for (start, end) in [(left, right), (right, left), (base, merged), (merged, base)] {
            let result = collect_for_paths(&f.repo, start, end, Some(compared_paths(&f, "folder", start, end))).unwrap();
            assert_eq!(contributors(&result, "folder/shared"), ["alice@test", "bob@test"]);
            assert!(contributors(&result, "folder/resolution_only").is_empty(), "merge-only resolution is not a proven personal edit");
            assert_eq!(result.len(), 2);
            assert!(result.iter().all(|a| a.merge_commits == 0));
            assert!(result.iter().flat_map(|a| a.evidence.values().flatten()).all(|e| e.commit != merged.to_string()));
        }
        let after_left = collect_for_paths(&f.repo, left, merged, Some(compared_paths(&f, "folder", left, merged))).unwrap();
        assert_eq!(contributors(&after_left, "folder/shared"), ["bob@test"], "Alice's common history is outside this interval");
    }

    #[test]
    fn shared_gitlink_history_counts_pointer_authors_without_claiming_internal_file_authorship() {
        let f = Fixture::new();
        let first = "1111111111111111111111111111111111111111";
        let second = "2222222222222222222222222222222222222222";
        let third = "3333333333333333333333333333333333333333";
        let base = f.entries("base@test", &[("modules/unit", first, 0o160000)], &[]);
        let alice = f.entries("alice@test", &[("modules/unit", second, 0o160000)], &[base]);
        let bob = f.entries("bob@test", &[("modules/unit", third, 0o160000)], &[alice]);
        let result = collect_for_paths(&f.repo, base, bob, Some(compared_paths(&f, "modules", base, bob))).unwrap();
        assert_eq!(contributors(&result, "modules/unit"), ["alice@test", "bob@test"]);
        assert!(result.iter().all(|a| a.paths == ["modules/unit"]));
        assert!(contributors(&result, "modules/unit/file.c").is_empty());
        assert!(!f.path.join("modules").exists(), "no recursive checkout or fetch for metadata");
        assert!(f.repo.find_commit(Oid::from_str(third).unwrap()).is_err(), "child objects deliberately absent");
        let result = collect_for_paths(&f.repo, alice, bob, Some(compared_paths(&f, "modules", alice, bob))).unwrap();
        assert_eq!(contributors(&result, "modules/unit"), ["bob@test"]);
    }

    #[test]
    fn folder_authors_only_count_matching_touches_from_mixed_commits() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("folder/file", "old"), ("folder/gone", "delete"), ("folder/stable", "same"), ("folder-other/file", "other"), ("elsewhere/file", "other")], &[]);
        let mixed = f.commit("alice@test", &[("folder/file", "alice"), ("folder/deep/new", "added"), ("folder/stable", "same"), ("folder-other/file", "other"), ("elsewhere/file", "alice")], &[base]);
        let outside = f.commit("alice@test", &[("folder/file", "alice"), ("folder/deep/new", "added"), ("folder/stable", "same"), ("folder-other/file", "other"), ("elsewhere/file", "alice again")], &[mixed]);
        let sibling = f.commit("bob@test", &[("folder/file", "alice"), ("folder/deep/new", "added"), ("folder/stable", "same"), ("folder-other/file", "bob"), ("elsewhere/file", "alice again")], &[outside]);
        let temporary = f.commit("carol@test", &[("folder/file", "alice"), ("folder/deep/new", "added"), ("folder/stable", "temporary"), ("folder-other/file", "bob"), ("elsewhere/file", "alice again")], &[sibling]);
        let tip = f.commit("dave@test", &[("folder/file", "alice"), ("folder/deep/new", "added"), ("folder/stable", "same"), ("folder-other/file", "bob"), ("elsewhere/file", "alice again")], &[temporary]);
        let scope = compared_paths(&f, "folder", base, tip);
        let result = collect_for_paths(&f.repo, base, tip, Some(scope.clone())).unwrap();
        assert_eq!(result.len(), 1, "no sibling-folder authors or touches absent from the endpoint diff");
        assert_eq!(result[0].email, "alice@test");
        assert_eq!(result[0].commits, 1, "the mixed commit counts once; Alice's outside-only commit does not count");
        assert_eq!(result[0].paths, ["folder/deep/new", "folder/file", "folder/gone"]);
        assert_eq!(result[0].direct_paths, result[0].paths);
        assert!(result[0].evidence.values().flatten().all(|e| e.commit == mixed.to_string()));
        assert_eq!(result[0].evidence.len(), 3);
        assert_reversed(&result, collect_for_paths(&f.repo, tip, base, Some(scope)).unwrap());
        let other = collect_for_paths(&f.repo, base, tip, Some(compared_paths(&f, "folder-other", base, tip))).unwrap();
        assert_eq!(other.len(), 1); assert_eq!(other[0].email, "bob@test");
        assert_eq!(other[0].paths, ["folder-other/file"]);
    }

    #[test]
    fn folder_authors_keep_incoming_authors_but_not_integrator_or_other_feature_paths() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[], &[]);
        let left = f.commit("alice@test", &[("other/a", "left")], &[base]);
        let right = f.commit("bob@test", &[("folder/b", "incoming"), ("other/b", "right")], &[base]);
        let merged = f.commit("merger@test", &[("folder/b", "incoming"), ("other/a", "left"), ("other/b", "right")], &[left, right]);
        for (start, end) in [(base, merged), (left, right), (right, left), (left, merged)] {
            let result = collect_for_paths(&f.repo, start, end, Some(compared_paths(&f, "folder", start, end))).unwrap();
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].email, "bob@test"); assert_eq!(result[0].commits, 1);
            assert_eq!(result[0].paths, ["folder/b"]); assert_eq!(result[0].merge_commits, 0);
            assert_eq!(result[0].evidence["folder/b"][0].commit, right.to_string());
        }
    }

    #[test]
    fn folder_authors_include_atomic_gitlinks_without_opening_submodules() {
        let f = Fixture::new();
        f.repo.config().unwrap().set_str("diff.ignoreSubmodules", "all").unwrap();
        let old = "1111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222";
        let base = f.entries("base@test", &[("folder/module", old, 0o160000)], &[]);
        let changed = f.entries("alice@test", &[("folder/module", new, 0o160000)], &[base]);
        let tip = f.entries("bob@test", &[("folder/module", new, 0o160000), ("other/file", "bob", 0o100644)], &[changed]);
        let result = collect_for_paths(&f.repo, base, tip, Some(compared_paths(&f, "folder", base, tip))).unwrap();
        assert_eq!(result.len(), 1); assert_eq!(result[0].email, "alice@test");
        assert_eq!(result[0].paths, ["folder/module"]); assert_eq!(result[0].commits, 1);
        assert_eq!(result[0].evidence["folder/module"][0].commit, changed.to_string());
        assert!(!f.path.join("folder").exists());
        assert!(f.repo.find_commit(Oid::from_str(new).unwrap()).is_err());
    }

    #[test]
    fn empty_folder_comparison_never_falls_back_to_repository_authors() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("folder/file", "same")], &[]);
        let tip = f.commit("alice@test", &[("folder/file", "same"), ("other/file", "new")], &[base]);
        for folder in ["folder", "missing"] {
            let scope = compared_paths(&f, folder, base, tip);
            assert!(scope.is_empty());
            assert!(collect_for_paths(&f.repo, base, tip, Some(scope)).unwrap().is_empty());
        }
        assert_eq!(collect(&f.repo, base, tip).unwrap().len(), 1, "unscoped history is unchanged");
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
        assert_eq!(own.date, short_date(f.repo.find_commit(tip).unwrap().time().seconds()));
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
        assert_reversed(&result, collect(&f.repo, tip, base).unwrap());
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
        assert_reversed(&forward, collect(&f.repo, right, left).unwrap());
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
    fn conversions_keep_structural_evidence_without_crediting_imported_or_removed_files() {
        let f = Fixture::new();
        let gitlink = "1111111111111111111111111111111111111111";
        let packed = f.entries("base@test", &[("module", gitlink, 0o160000)], &[]);
        let unpacked = f.commit("converter@test", &[("module/code", "imported"), ("module/model", "binary\0payload")], &[packed]);
        let repacked = f.entries("packer@test", &[("module", gitlink, 0o160000)], &[unpacked]);
        for (base, tip, email) in [(packed, unpacked, "converter@test"), (unpacked, repacked, "packer@test")] {
            for (left, right, side) in [(base, tip, "right"), (tip, base, "left")] {
                let result = collect_for_paths(&f.repo, left, right, Some(vec!["module".into(), "module/code".into(), "module/model".into()])).unwrap();
                assert_eq!(paths(&result, email), ["module"]);
                assert!(contributors(&result, "module/code").is_empty());
                let author = result.iter().find(|a| a.email == email).unwrap();
                for path in ["module", "module/code", "module/model"] {
                    let event = &author.evidence[path][0];
                    assert_eq!(event.kind, "folder-submodule-conversion");
                    assert_eq!(event.witness_path.as_deref(), Some("module"));
                    assert_eq!(event.context.side, side);
                    assert_eq!(event.commit, tip.to_string());
                }
                let scoped = collect_for_paths(&f.repo, left, right, Some(vec!["module/code".into()])).unwrap();
                assert_eq!(scoped.len(), 1, "structural evidence is retained even in a descendant-only scope");
                assert!(scoped[0].paths.is_empty());
            }
        }
        assert!(f.repo.find_commit(Oid::from_str(gitlink).unwrap()).is_err(), "no child objects needed");
        assert!(!f.path.join("module").exists());
    }

    #[test]
    fn conversion_does_not_mask_independent_later_file_edits_or_create_shared_authors() {
        let f = Fixture::new();
        let base = f.entries("base@test", &[("unit", "1111111111111111111111111111111111111111", 0o160000)], &[]);
        let imported = f.commit("alice@test", &[("unit/own", "copied"), ("unit/other", "copied")], &[base]);
        let edited = f.commit("alice@test", &[("unit/own", "real edit"), ("unit/other", "copied")], &[imported]);
        let tip = f.commit("bob@test", &[("unit/own", "real edit"), ("unit/other", "bob edit")], &[edited]);
        for (left, right) in [(base, tip), (tip, base)] {
            let result = collect_for_paths(&f.repo, left, right, Some(vec!["unit/own".into(), "unit/other".into()])).unwrap();
            assert_eq!(paths(&result, "alice@test"), ["unit/own"]);
            assert_eq!(contributors(&result, "unit/other"), ["bob@test"]);
            let evidence = &result.iter().find(|a| a.email == "alice@test").unwrap().evidence["unit/own"];
            assert_eq!(evidence.len(), 2);
            assert!(evidence.iter().any(|e| e.kind == "file-change" && e.commit == edited.to_string()));
            assert!(evidence.iter().any(|e| e.kind == "folder-submodule-conversion" && e.commit == imported.to_string()));
        }
    }

    #[test]
    fn event_sides_and_repositories_are_preserved_even_for_the_same_author_on_both_sides() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("file", "base")], &[]);
        let left = f.commit("alice@test", &[("file", "left")], &[base]);
        let right = f.commit("alice@test", &[("file", "right")], &[base]);
        for (a, b) in [(left, right), (right, left)] {
            let result = collect(&f.repo, a, b).unwrap();
            let evidence = &result[0].evidence["file"];
            assert_eq!(evidence.len(), 2);
            for (commit, side) in [(a, "left"), (b, "right")] {
                let event = evidence.iter().find(|e| e.commit == commit.to_string()).unwrap();
                assert_eq!(event.context.side, side);
                assert_eq!(event.kind, "file-change");
                assert_eq!(event.context.repository_path, normalized(f.repo.workdir().unwrap()));
                let json = serde_json::to_value(event).unwrap();
                assert_eq!(json["side"], side); assert!(json["repository_id"].is_string());
                assert!(json.get("context").is_none(), "flat IPC evidence contract");
            }
        }
    }

    #[test]
    fn many_conversion_children_reuse_one_root_without_submodule_objects() {
        let f = Fixture::new();
        let base = f.entries("base@test", &[("unit", "1111111111111111111111111111111111111111", 0o160000)], &[]);
        let names: Vec<_> = (0..1500).map(|i| format!("unit/deep/file-{i}")).collect();
        let files: Vec<_> = names.iter().map(|name| (name.as_str(), "imported content")).collect();
        let tip = f.commit("converter@test", &files, &[base]);
        let result = collect_for_paths(&f.repo, base, tip, Some(names)).unwrap();
        assert_eq!(result.len(), 1); assert!(result[0].paths.is_empty());
        assert_eq!(result[0].evidence.len(), 1500);
        assert!(result[0].evidence.values().all(|events| events.len() == 1 && events[0].witness_path.as_deref() == Some("unit")));
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
        collect_for_paths(&f.repo, base, tip, Some(vec!["f".into()])).unwrap();
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
