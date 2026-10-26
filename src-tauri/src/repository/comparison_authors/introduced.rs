//! Optional integration metadata. Never changes personal-touch/strict results,
//! fetches objects, checks out a revision or reads the working-tree status.
use super::*;

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentComparison {
    repository_path: String,
    relative_path: String,
    left_revision: String,
    right_revision: String,
}

#[derive(Serialize)]
pub struct Introduction {
    commit: String,
    date: String,
    parent: Option<String>,
    kind: String,
    #[serde(flatten)]
    context: EvidenceContext,
    witness_path: String,
    before_revision: Option<String>,
    after_revision: Option<String>,
    // Present only for a parent gitlink witness, never confused with a child
    // file edit or opened using the child's object database.
    parent_repository: Option<String>,
}

#[derive(Serialize)]
pub struct Introducer {
    key: String,
    name: String,
    email: String,
    paths: Vec<String>,
    evidence: BTreeMap<String, Vec<Introduction>>,
}

struct Budget { started: Instant, commits: usize, pointers: usize, evidence: usize }
impl Budget {
    fn new() -> Self { Self { started: Instant::now(), commits: 0, pointers: 0, evidence: 0 } }
    fn check(&self) -> Result<(), String> {
        if self.started.elapsed() > Duration::from_secs(60) || self.commits > 100_000 || self.pointers > 512 || self.evidence > 200_000 {
            Err("Introduction history exceeded its analysis budget. Choose closer revisions. No partial results were applied.".into())
        } else { Ok(()) }
    }
}

type Authors = BTreeMap<String, Introducer>;
fn record(authors: &mut Authors, commit: &git2::Commit, path: &str, witness: Introduction, budget: &mut Budget) -> Result<(), String> {
    let signature = commit.author();
    let name = signature.name().unwrap_or("Unknown author").to_owned();
    let email = signature.email().unwrap_or("").trim().to_owned();
    let key = if email.is_empty() { format!("name:{name}") } else { format!("email:{email}") };
    let author = authors.entry(key.clone()).or_insert_with(|| Introducer { key, name, email, paths: vec![], evidence: BTreeMap::new() });
    let evidence = author.evidence.entry(path.to_owned()).or_default();
    // One witness of each reason and side per item; history membership is not a list
    // of every intermediate commit, nor proof that a specific edit survived.
    if !evidence.iter().any(|e| e.kind == witness.kind && e.parent_repository == witness.parent_repository && e.context.side == witness.context.side) {
        budget.evidence += 1; budget.check()?;
        evidence.push(witness);
    }
    Ok(())
}

fn witness(commit: &git2::Commit, path: &str, kind: &str, before: Option<Oid>, after: Option<Oid>, parent_repository: Option<&str>, context: &EvidenceContext) -> Result<Introduction, String> {
    Ok(Introduction {
        commit: commit.id().to_string(), date: short_date(commit.time().seconds()),
        parent: if commit.parent_count() == 0 { None } else { Some(commit.parent_id(0).map_err(|e| e.to_string())?.to_string()) },
        kind: kind.into(), context: context.clone(), witness_path: path.into(), before_revision: before.map(|id| id.to_string()),
        after_revision: after.map(|id| id.to_string()), parent_repository: parent_repository.map(str::to_owned),
    })
}

fn walk(repo: &Repository, left: Oid, right: Oid, budget: &mut Budget,
    mut visit: impl FnMut(&git2::Commit, &git2::Diff, &HashSet<String>, &EvidenceContext, &mut Budget) -> Result<(), String>) -> Result<(), String> {
    if repo.is_shallow() { return Err("Introduction history is incomplete: this repository is shallow. No partial results were applied.".into()); }
    for (tip, hide, side) in [(left, right, "left"), (right, left, "right")] {
        let context = EvidenceContext::new(repo, side);
        let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL).map_err(|e| e.to_string())?;
        walk.push(tip).map_err(|e| e.to_string())?; walk.hide(hide).map_err(|e| e.to_string())?;
        for oid in walk {
            budget.commits += 1; budget.check()?;
            let commit = repo.find_commit(oid.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let tree = commit.tree().map_err(|e| e.to_string())?;
            // The result versus FIRST parent measures what this commit brought
            // into its receiving branch. Other parents' existing work is not
            // claimed as a personal edit by the merge author.
            let parent = if commit.parent_count() == 0 { None } else { Some(commit.parent(0).and_then(|p| p.tree()).map_err(|e| e.to_string())?) };
            let diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&tree), None).map_err(|e| e.to_string())?;
            let conversions = conversion_roots(&diff, parent.as_ref(), &tree)?;
            visit(&commit, &diff, &conversions, &context, budget)?;
        }
    }
    budget.check()
}

fn gitlink(file: git2::DiffFile) -> Option<Oid> {
    (file.mode() == git2::FileMode::Commit).then(|| file.id())
}

fn pointer_at(repo: &Repository, revision: &str, path: &Path) -> Result<Oid, String> {
    let tree = repo.find_commit(Oid::from_str(revision).map_err(|e| e.to_string())?).and_then(|c| c.tree()).map_err(|e| e.to_string())?;
    let entry = tree.get_path(path).map_err(|_| "The parent snapshot does not contain this submodule.".to_string())?;
    if entry.filemode() != 0o160000 { return Err("The parent snapshot path is not a submodule.".into()); }
    Ok(entry.id())
}

fn parent_repository(child: &Repository, left: Oid, right: Oid, parent: &ParentComparison) -> Result<Repository, String> {
    let path = safe_relative_path(&parent.relative_path)?;
    if path.as_os_str().is_empty() { return Err("A parent submodule path is required.".into()); }
    let repo = Repository::open(&parent.repository_path).map_err(|e| e.to_string())?;
    let expected = internal_submodule_repository(&repo.workdir().ok_or("Parent repository has no working directory.")?.join(&path))?;
    if expected.path().canonicalize().map_err(|e| e.to_string())? != child.path().canonicalize().map_err(|e| e.to_string())? {
        return Err("The parent comparison belongs to a different submodule repository.".into());
    }
    if pointer_at(&repo, &parent.left_revision, &path)? != left || pointer_at(&repo, &parent.right_revision, &path)? != right {
        return Err("The compared child revisions no longer match the parent gitlinks. Reopen the submodule from the parent comparison.".into());
    }
    Ok(repo)
}

fn collect_introductions(repo: &Repository, left: Oid, right: Oid, paths: Option<Vec<String>>, parent: Option<&ParentComparison>) -> Result<Vec<Introducer>, String> {
    let mut budget = Budget::new();
    let left_tree = repo.find_commit(left).and_then(|c| c.tree()).map_err(|e| e.to_string())?;
    let right_tree = repo.find_commit(right).and_then(|c| c.tree()).map_err(|e| e.to_string())?;
    let relevant: HashSet<_> = match paths {
        Some(paths) => paths.into_iter().collect(),
        None => repo.diff_tree_to_tree(Some(&left_tree), Some(&right_tree), None).map_err(|e| e.to_string())?.deltas()
            .flat_map(|d| [d.old_file().path().map(normalized), d.new_file().path().map(normalized)].into_iter().flatten()).collect(),
    };
    let mut authors = Authors::new();
    if relevant.is_empty() { return Ok(vec![]); }
    walk(repo, left, right, &mut budget, |commit, diff, conversions, context, budget| {
        for delta in diff.deltas() {
            budget.check()?;
            let before = gitlink(delta.old_file()); let after = gitlink(delta.new_file());
            let kind = match (commit.parent_count() > 1, before.is_some() || after.is_some()) {
                (true, true) => "merge-submodule", (false, true) => "submodule-update", (true, false) => "merge", (false, false) => "commit",
            };
            for path in [delta.old_file().path(), delta.new_file().path()].into_iter().flatten().map(normalized) {
                if relevant.contains(&path) {
                    let conversion = conversion_for_path(&path, conversions);
                    let kind = if conversion.is_some() {
                        if commit.parent_count() > 1 { "merge-folder-submodule-conversion" } else { "folder-submodule-conversion" }
                    } else { kind };
                    record(&mut authors, commit, &path, witness(commit, conversion.unwrap_or(&path), kind, before, after, None, context)?, budget)?;
                }
            }
        }
        Ok(())
    })?;
    if let Some(parent) = parent {
        let parent_repo = parent_repository(repo, left, right, parent)?;
        let parent_path = normalized(&safe_relative_path(&parent.relative_path)?);
        // Cache only this request's bounded set of pointer pairs; no repository
        // cache or polling. Repeated merges do not repeatedly diff child trees.
        let mut pairs: BTreeMap<(Option<Oid>, Option<Oid>), Vec<String>> = BTreeMap::new();
        walk(&parent_repo, Oid::from_str(&parent.left_revision).map_err(|e| e.to_string())?, Oid::from_str(&parent.right_revision).map_err(|e| e.to_string())?, &mut budget, |commit, diff, _, context, budget| {
            for delta in diff.deltas() {
                budget.check()?;
                if ![delta.old_file().path(), delta.new_file().path()].into_iter().flatten().any(|p| normalized(p) == parent_path) { continue; }
                let before = gitlink(delta.old_file()); let after = gitlink(delta.new_file());
                if before == after { continue; }
                // A tree/gitlink conversion may be emitted by libgit2 as
                // delete/add deltas, with an "absent" old side at the root.
                // Check the actual entries before treating this as init/remove.
                let mode_at = |tree: &git2::Tree| match tree.get_path(Path::new(&parent_path)) {
                    Ok(entry) => Ok(Some(entry.filemode())),
                    Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
                    Err(e) => Err(e.to_string()),
                };
                let after_mode = mode_at(&commit.tree().map_err(|e| e.to_string())?)?;
                let before_mode = if commit.parent_count() == 0 { None }
                    else { mode_at(&commit.parent(0).and_then(|p| p.tree()).map_err(|e| e.to_string())?)? };
                if [before_mode, after_mode].into_iter().flatten().any(|mode| mode != 0o160000) {
                    return Err("Introduction history is incomplete: a folder/submodule conversion crosses this parent interval. Compare that conversion in the parent repository. No partial results were applied.".into());
                }
                let pair = (before, after);
                if !pairs.contains_key(&pair) {
                    budget.pointers += 1; budget.check()?;
                    let tree = |id: Option<Oid>| id.map(|id| repo.find_commit(id).and_then(|c| c.tree())
                        .map_err(|_| format!("Introduction history is incomplete: submodule commit {id} needed by parent commit {} is unavailable locally. No fetch or partial results were applied.", commit.id()))).transpose();
                    let old = tree(before)?; let new = tree(after)?;
                    let changes = repo.diff_tree_to_tree(old.as_ref(), new.as_ref(), None).map_err(|e| e.to_string())?;
                    let matching: HashSet<_> = changes.deltas().flat_map(|d| [d.old_file().path().map(normalized), d.new_file().path().map(normalized)].into_iter().flatten())
                        .filter(|p| relevant.contains(p)).collect();
                    budget.evidence += matching.len(); budget.check()?;
                    pairs.insert(pair, matching.into_iter().collect());
                }
                for path in &pairs[&pair] {
                    let kind = if commit.parent_count() > 1 { "parent-merge-submodule" } else { "parent-submodule-update" };
                    record(&mut authors, commit, path, witness(commit, &parent_path, kind, before, after, Some(&parent.repository_path), context)?, budget)?;
                }
            }
            Ok(())
        })?;
    }
    budget.check()?;
    perf_log(&format!("comparison_introductions: commits={} compared_paths={} authors={} parent_context={} pointer_pairs={} evidence={} (local objects only; first-parent integration diffs)", budget.commits, relevant.len(), authors.len(), parent.is_some(), budget.pointers, budget.evidence), budget.started.elapsed());
    Ok(authors.into_values().map(|mut author| { author.paths = author.evidence.keys().cloned().collect(); author }).collect())
}

#[tauri::command]
pub async fn comparison_introductions(repository_path: String, submodule_path: Option<String>, owner: Option<String>, repository_name: Option<String>, left_revision: String, right_revision: String, comparison_paths: Option<Vec<String>>, parent_comparison: Option<ParentComparison>) -> Result<Vec<Introducer>, String> {
    off_main_thread(move || {
        let started = Instant::now();
        validate_path(&repository_path)?;
        let path = match (owner, repository_name, submodule_path) {
            (Some(owner), Some(name), None) => { let (owner, name) = validate_github_module_identity(&owner, &name)?; github_module_cache_path(&owner, &name) },
            (None, None, Some(path)) => validate_submodule(&repository_path, &path)?,
            (None, None, None) => PathBuf::from(repository_path),
            _ => return Err("Invalid comparison repository context".into()),
        };
        let repo = Repository::open(&path).or_else(|_| Repository::open_bare(&path)).map_err(|e| e.to_string())?;
        let result = collect_introductions(&repo, Oid::from_str(&left_revision).map_err(|e| e.to_string())?, Oid::from_str(&right_revision).map_err(|e| e.to_string())?, comparison_paths, parent_comparison.as_ref());
        result.map_err(|e| {
            perf_log(&format!("comparison_introductions: failed parent_context={} reason={e}", parent_comparison.is_some()), started.elapsed());
            format!("Introduced-by analysis could not complete. No filtered results were applied. {e}")
        })
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::Fixture;

    fn child(parent: &Fixture) -> Fixture {
        let path = parent.path.join("modules/unit");
        fs::create_dir_all(&path).unwrap();
        Fixture { repo: Repository::init(&path).unwrap(), path }
    }
    fn pointer(parent: &Fixture, author: &str, child: Oid, parents: &[Oid]) -> Oid {
        parent.entries(author, &[("modules/unit", &child.to_string(), 0o160000)], parents)
    }
    fn context(parent: &Fixture, left: Oid, right: Oid) -> ParentComparison {
        ParentComparison { repository_path:parent.path.to_string_lossy().into_owned(), relative_path:"modules/unit".into(), left_revision:left.to_string(), right_revision:right.to_string() }
    }
    fn paths<'a>(authors: &'a [Introducer], email: &str) -> Vec<&'a str> {
        authors.iter().find(|a| a.email == email).map(|a| a.paths.iter().map(String::as_str).collect()).unwrap_or_default()
    }
    fn membership(authors: &[Introducer]) -> Vec<(&str, &Vec<String>)> {
        authors.iter().map(|a| (a.key.as_str(), &a.paths)).collect()
    }

    #[test]
    fn intermediate_introductions_use_endpoint_paths_not_endpoint_author_or_survival() {
        let f=Fixture::new();
        let base=f.commit("base@test", &[("folder/f","base"),("folder/reverted","base")], &[]);
        let a=f.commit("alice@test", &[("folder/f","alice"),("folder/reverted","temporary")], &[base]);
        let b=f.commit("bob@test", &[("folder/f","bob overwrites"),("folder/reverted","base")], &[a]);
        let end=f.commit("endpoint@test", &[("folder/f","bob overwrites"),("folder/reverted","base"),("other/f","outside")], &[b]);
        let result=collect_introductions(&f.repo,base,end,Some(vec!["folder/f".into()]),None).unwrap();
        assert_eq!(paths(&result,"alice@test"),["folder/f"]);
        assert_eq!(paths(&result,"bob@test"),["folder/f"]);
        assert!(paths(&result,"endpoint@test").is_empty());
        assert_eq!(result[0].evidence["folder/f"][0].commit,a.to_string());
        assert_eq!(result[0].evidence["folder/f"][0].kind,"commit");
        let all=collect_introductions(&f.repo,base,end,None,None).unwrap();
        assert!(!all.iter().any(|a| a.paths.contains(&"folder/reverted".into())));
        let full=compare_git_tree_file(&f.path.to_string_lossy(),"folder/f",&base.to_string(),&end.to_string()).unwrap();
        assert_eq!(full.local_content,"base"); assert_eq!(full.remote_content,"bob overwrites");
    }

    #[test]
    fn merge_introduction_is_against_first_parent_not_a_personal_touch() {
        let f=Fixture::new();
        let base=f.commit("base@test",&[],&[]);
        let left=f.commit("alice@test",&[("ours","a")],&[base]);
        let right=f.commit("bob@test",&[("theirs","b")],&[base]);
        let merge=f.commit("integrator@test",&[("ours","a"),("theirs","b"),("resolution","manual or automatic")],&[left,right]);
        let result=collect_introductions(&f.repo,base,merge,None,None).unwrap();
        assert_eq!(paths(&result,"integrator@test"),["resolution","theirs"]);
        assert_eq!(paths(&result,"bob@test"),["theirs"]);
        let proof=&result.iter().find(|a| a.email=="integrator@test").unwrap().evidence["theirs"][0];
        assert_eq!(proof.kind,"merge"); assert_eq!(proof.parent.as_deref(),Some(left.to_string().as_str()));
        assert!(!proof.date.is_empty());
        let normal=super::super::collect(&f.repo,base,merge).unwrap();
        assert!(normal.iter().find(|a| a.email=="integrator@test").unwrap().paths.is_empty());
        let discarded=f.commit("integrator@test",&[("ours","a")],&[left,right]);
        assert!(paths(&collect_introductions(&f.repo,base,discarded,None,None).unwrap(),"integrator@test").is_empty(),"a merge taking none of the incoming changes is not an introduction");
    }

    #[test]
    fn divergent_reversed_ranges_keep_membership_and_exclude_common_ancestors() {
        let f=Fixture::new();
        let base=f.commit("common@test",&[("f","base")],&[]);
        let left=f.commit("alice@test",&[("f","left")],&[base]);
        let right=f.commit("bob@test",&[("f","right")],&[base]);
        let forward=collect_introductions(&f.repo,left,right,None,None).unwrap();
        let reverse=collect_introductions(&f.repo,right,left,None,None).unwrap();
        assert_eq!(membership(&forward),membership(&reverse));
        assert_eq!(forward.len(),2);
        assert!(paths(&forward,"common@test").is_empty());
        assert!(collect_introductions(&f.repo,left,left,None,None).unwrap().is_empty());
    }

    #[test]
    fn parent_update_keeps_ten_imported_items_but_personal_filter_only_keeps_one() {
        let parent=Fixture::new(); let sub=child(&parent);
        let base=sub.commit("base@test",&[],&[]);
        let names:Vec<_>=(0..10).map(|i| format!("src/f{i}")).collect();
        let files:Vec<_>=names.iter().map(|s| (s.as_str(),"Bob's work")).collect();
        let imported=sub.commit("bob@test",&files,&[base]);
        let mut own=files.clone(); own[0].1="Alice's later edit";
        let end=sub.commit("alice@test",&own,&[imported]);
        let p0=pointer(&parent,"parent-base@test",base,&[]);
        let p1=pointer(&parent,"alice@test",end,&[p0]);
        let ctx=context(&parent,p0,p1);
        let result=collect_introductions(&sub.repo,base,end,None,Some(&ctx)).unwrap();
        assert_eq!(paths(&result,"alice@test").len(),10);
        let normal=super::super::collect(&sub.repo,base,end).unwrap();
        assert_eq!(normal.iter().find(|a| a.email=="alice@test").unwrap().paths,["src/f0"]);
        let proof=&result.iter().find(|a| a.email=="alice@test").unwrap().evidence["src/f1"][0];
        assert_eq!(proof.kind,"parent-submodule-update"); assert_eq!(proof.commit,p1.to_string());
        assert_eq!(proof.before_revision.as_deref(),Some(base.to_string().as_str()));
        assert_eq!(proof.after_revision.as_deref(),Some(end.to_string().as_str()));
        assert_eq!(proof.witness_path,"modules/unit"); assert_eq!(proof.parent_repository.as_deref(),Some(ctx.repository_path.as_str()));
        let reverse=collect_introductions(&sub.repo,end,base,None,Some(&context(&parent,p1,p0))).unwrap();
        assert_eq!(membership(&result),membership(&reverse));
        let direct=collect_introductions(&sub.repo,base,end,None,None).unwrap();
        assert_eq!(paths(&direct,"alice@test"),["src/f0"],"an arbitrary child comparison must not guess a parent interval");
    }

    #[test]
    fn intermediate_parent_updates_intersect_each_pointer_diff_with_the_child_folder_scope() {
        let parent=Fixture::new(); let sub=child(&parent);
        let base=sub.commit("base@test",&[],&[]);
        let a=sub.commit("writer@test",&[("one/f","first")],&[base]);
        let b=sub.commit("writer@test",&[("one/f","first"),("two/f","second")],&[a]);
        let p0=pointer(&parent,"base@test",base,&[]);
        let p1=pointer(&parent,"first-updater@test",a,&[p0]);
        let p2=pointer(&parent,"second-updater@test",b,&[p1]);
        let ctx=context(&parent,p0,p2);
        let result=collect_introductions(&sub.repo,base,b,None,Some(&ctx)).unwrap();
        assert_eq!(paths(&result,"first-updater@test"),["one/f"]);
        assert_eq!(paths(&result,"second-updater@test"),["two/f"]);
        let scoped=collect_introductions(&sub.repo,base,b,Some(vec!["two/f".into()]),Some(&ctx)).unwrap();
        assert!(paths(&scoped,"first-updater@test").is_empty());
        assert_eq!(paths(&scoped,"second-updater@test"),["two/f"]);
        assert!(collect_introductions(&sub.repo,base,b,Some(vec![]),Some(&ctx)).unwrap().is_empty());
    }

    #[test]
    fn parent_merge_imports_child_changes_without_claiming_internal_edits() {
        let parent=Fixture::new(); let sub=child(&parent);
        let base=sub.commit("base@test",&[],&[]);
        let end=sub.commit("writer@test",&[("f","new")],&[base]);
        let p0=pointer(&parent,"base@test",base,&[]);
        let left=pointer(&parent,"unchanged@test",base,&[p0]);
        let right=pointer(&parent,"updater@test",end,&[p0]);
        let merge=pointer(&parent,"integrator@test",end,&[left,right]);
        let result=collect_introductions(&sub.repo,base,end,None,Some(&context(&parent,left,merge))).unwrap();
        assert_eq!(paths(&result,"integrator@test"),["f"]);
        assert_eq!(paths(&result,"writer@test"),["f"]);
        assert_eq!(result.iter().find(|a| a.email=="integrator@test").unwrap().evidence["f"][0].kind,"parent-merge-submodule");
        assert!(super::super::collect(&sub.repo,base,end).unwrap().iter().all(|a| a.email!="integrator@test"));
    }

    #[test]
    fn mismatched_parent_revisions_repository_or_unsafe_path_fail_closed() {
        let parent=Fixture::new(); let sub=child(&parent);
        let base=sub.commit("base@test",&[],&[]); let end=sub.commit("writer@test",&[("f","new")],&[base]);
        let p0=pointer(&parent,"base@test",base,&[]); let p1=pointer(&parent,"updater@test",end,&[p0]);
        let ctx=context(&parent,p0,p1);
        assert!(collect_introductions(&sub.repo,end,base,None,Some(&ctx)).err().unwrap().contains("no longer match"));
        let wrong=Fixture::new();
        assert!(parent_repository(&wrong.repo,base,end,&ctx).err().unwrap().contains("different submodule"));
        let mut unsafe_ctx=ctx.clone(); unsafe_ctx.relative_path="../../outside".into();
        assert!(collect_introductions(&sub.repo,base,end,None,Some(&unsafe_ctx)).is_err());
    }

    #[test]
    fn missing_intermediate_child_objects_are_incomplete_not_no_contribution_and_never_fetched() {
        let parent=Fixture::new(); let sub=child(&parent);
        let base=sub.commit("base@test",&[],&[]); let end=sub.commit("writer@test",&[("f","new")],&[base]);
        let missing=Oid::from_str("1111111111111111111111111111111111111111").unwrap();
        let p0=pointer(&parent,"base@test",base,&[]); let p1=pointer(&parent,"first@test",missing,&[p0]);
        let p2=pointer(&parent,"second@test",end,&[p1]);
        sub.repo.remote("origin","https://must-not-be-contacted.invalid/unit").unwrap();
        let error=collect_introductions(&sub.repo,base,end,None,Some(&context(&parent,p0,p2))).err().unwrap();
        assert!(error.contains("incomplete") && error.contains("unavailable locally") && error.contains("No fetch"));
        assert!(sub.repo.find_commit(missing).is_err());
        // Atomic parent pointer attribution itself does not require child objects.
        let parent_result=collect_introductions(&parent.repo,p0,p2,None,None).unwrap();
        assert_eq!(paths(&parent_result,"first@test"),["modules/unit"]);
    }

    #[test]
    fn parent_folder_conversion_is_not_misread_as_a_new_submodule_addition() {
        let parent=Fixture::new(); let sub=child(&parent);
        let base=sub.commit("base@test",&[],&[]); let end=sub.commit("writer@test",&[("f","new")],&[base]);
        let p0=pointer(&parent,"base@test",base,&[]);
        let folder=parent.commit("converter@test",&[("modules/unit/f","copied contents")],&[p0]);
        let p2=pointer(&parent,"converter@test",end,&[folder]);
        let error=collect_introductions(&sub.repo,base,end,None,Some(&context(&parent,p0,p2))).err().unwrap();
        assert!(error.contains("folder/submodule conversion") && error.contains("No partial results"));
        // Parent-level atomic changes remain inspectable; only the unsupported
        // cross-repository expansion fails instead of inventing file ownership.
        assert!(!collect_introductions(&parent.repo,p0,p2,None,None).unwrap().is_empty());
    }

    #[test]
    fn conversion_introductions_are_structural_and_keep_later_file_edits_distinct() {
        let f = Fixture::new();
        let base = f.entries("base@test", &[("unit", "1111111111111111111111111111111111111111", 0o160000)], &[]);
        let unpacked = f.commit("converter@test", &[("unit/code", "copied"), ("unit/model", "binary\0import")], &[base]);
        let tip = f.commit("converter@test", &[("unit/code", "personal edit"), ("unit/model", "binary\0import")], &[unpacked]);
        for (left, right, side) in [(base, tip, "right"), (tip, base, "left")] {
            let result = collect_introductions(&f.repo, left, right, Some(vec!["unit/code".into(), "unit/model".into()]), None).unwrap();
            let author = &result[0]; assert_eq!(author.paths, ["unit/code", "unit/model"]);
            assert_eq!(author.evidence["unit/model"][0].kind, "folder-submodule-conversion");
            let events = &author.evidence["unit/code"];
            assert_eq!(events.len(), 2);
            assert!(events.iter().any(|e| e.commit == tip.to_string() && e.kind == "commit"));
            assert!(events.iter().any(|e| e.commit == unpacked.to_string() && e.kind == "folder-submodule-conversion" && e.witness_path == "unit"));
            assert!(events.iter().all(|e| e.context.side == side));
        }
        let merged = f.commit("integrator@test", &[("unit/code", "personal edit"), ("unit/model", "binary\0import")], &[base, tip]);
        let result = collect_introductions(&f.repo, base, merged, None, None).unwrap();
        let integrator = result.iter().find(|a| a.email == "integrator@test").unwrap();
        assert_eq!(integrator.evidence["unit/code"][0].kind, "merge-folder-submodule-conversion");
        let touched = collect(&f.repo, base, merged).unwrap();
        assert!(touched.iter().find(|a| a.email == "integrator@test").unwrap().paths.is_empty());
    }

    #[test]
    fn introduction_events_keep_both_sides_and_parent_repository_context() {
        let f = Fixture::new();
        let base = f.commit("base@test", &[("f", "base")], &[]);
        let a = f.commit("alice@test", &[("f", "a")], &[base]);
        let b = f.commit("alice@test", &[("f", "b")], &[base]);
        let result = collect_introductions(&f.repo, a, b, None, None).unwrap();
        assert_eq!(result[0].evidence["f"].len(), 2);
        for event in &result[0].evidence["f"] {
            assert_eq!(event.context.side, if event.commit == a.to_string() { "left" } else { "right" });
        }
        let parent=Fixture::new(); let sub=child(&parent);
        let old=sub.commit("writer@test", &[("f", "old")], &[]);
        let new=sub.commit("writer@test", &[("f", "new")], &[old]);
        let p0=pointer(&parent,"base@test",old,&[]);
        let p1=pointer(&parent,"updater@test",new,&[p0]);
        for (left,right,pa,pb,side) in [(old,new,p0,p1,"right"),(new,old,p1,p0,"left")] {
            let result=collect_introductions(&sub.repo,left,right,None,Some(&context(&parent,pa,pb))).unwrap();
            let updater=result.iter().find(|a| a.email=="updater@test").unwrap();
            let event=&updater.evidence["f"][0];
            assert_eq!(event.kind,"parent-submodule-update"); assert_eq!(event.context.side,side);
            assert_eq!(event.context.repository_path,normalized(parent.repo.workdir().unwrap()));
            assert_ne!(event.context.repository_path,normalized(sub.repo.workdir().unwrap()));
            assert_eq!(event.before_revision,Some(old.to_string())); assert_eq!(event.after_revision,Some(new.to_string()));
        }
    }

    #[test]
    fn metadata_does_not_modify_refs_index_or_worktree_and_bounds_requests() {
        let parent=Fixture::new(); let sub=child(&parent);
        let base=sub.commit("base@test",&[],&[]); let end=sub.commit("writer@test",&[("f","new")],&[base]);
        let p0=pointer(&parent,"base@test",base,&[]); let p1=pointer(&parent,"updater@test",end,&[p0]);
        sub.repo.reference("refs/heads/work",end,true,"test").unwrap(); sub.repo.set_head("refs/heads/work").unwrap();
        fs::write(sub.path.join("f"),"uncommitted").unwrap();
        let mut index=sub.repo.index().unwrap(); index.add_path(Path::new("f")).unwrap(); index.write().unwrap();
        let before=fs::read(sub.repo.path().join("index")).unwrap();
        collect_introductions(&sub.repo,base,end,None,Some(&context(&parent,p0,p1))).unwrap();
        assert_eq!(fs::read(sub.repo.path().join("index")).unwrap(),before);
        assert_eq!(sub.repo.head().unwrap().target(),Some(end));
        assert_eq!(fs::read_to_string(sub.path.join("f")).unwrap(),"uncommitted");
        let mut budget=Budget::new(); budget.pointers=513; assert!(budget.check().is_err());
        budget.pointers=0; budget.commits=100_001; assert!(budget.check().is_err());
        budget.commits=0; budget.evidence=200_001; assert!(budget.check().is_err());
        fs::write(sub.repo.path().join("shallow"),format!("{base}\n")).unwrap();
        let reopened=Repository::open(&sub.path).unwrap();
        assert!(collect_introductions(&reopened,base,end,None,None).err().unwrap().contains("shallow"));
    }
}
