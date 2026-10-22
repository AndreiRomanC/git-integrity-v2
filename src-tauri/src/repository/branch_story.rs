//! Creation evidence only: current merge-base is deliberately not a fallback.
use super::*;

fn creation_oid(repo: &Repository, branch: &str, expected_tip: Oid) -> Option<String> {
    let started = Instant::now();
    let mut reason = "local_branch_unavailable";
    let mut reflog_entries = 0;
    let result = (|| {
        let reference = repo.find_branch(branch, BranchType::Local).ok()?;
        reason = "branch_tip_unavailable";
        let actual_tip = reference.get().target()?;
        reason = "branch_tip_changed";
        if actual_tip != expected_tip { return None; }
        reason = "branch_ref_name_unavailable";
        let name = reference.get().name()?;
        reason = "reflog_unavailable";
        let log = repo.reflog(name).ok()?;
        reflog_entries = log.len();
        reason = "reflog_empty";
        let oldest = log.get(log.len().checked_sub(1)?)?;
        reason = "oldest_entry_not_creation";
        if !oldest.id_old().is_zero() || !oldest.message()?.starts_with("branch: Created from ") { return None; }
        reason = "creation_record_found";
        Some(oldest.id_new().to_string())
    })();
    perf_log(&format!("graph_start_creation: repo={} source=local_reflog tip={expected_tip} creation={} reason={reason} reflog_entries={reflog_entries} no_extra_queries=true",
        anonymized_repository_id(&repo.workdir().unwrap_or_else(|| repo.path()).to_string_lossy()), result.as_deref().unwrap_or("none")), started.elapsed());
    result
}

#[tauri::command]
pub async fn branch_story_creation(repository_path: String, branch: String, tip: String) -> Result<Option<String>, String> {
    off_main_thread(move || {
        validate_path(&repository_path)?;
        let repo = internal_repository(&repository_path)?;
        let tip = Oid::from_str(&tip).map_err(|e| e.to_string())?;
        Ok(creation_oid(&repo, &branch, tip))
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn start_diagnostics_preserve_original_creation_and_advanced_merge_base() {
        let path = std::env::temp_dir().join(format!("ddt-story-merge-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let repo = Repository::init(&path).unwrap();
        repo.config().unwrap().set_bool("core.logallrefupdates", true).unwrap();
        let tree = repo.find_tree(repo.treebuilder(None).unwrap().write().unwrap()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let commit = |message: &str, parents: &[Oid]| {
            let parents: Vec<_> = parents.iter().map(|id| repo.find_commit(*id).unwrap()).collect();
            repo.commit(None, &sig, &sig, message, &tree, &parents.iter().collect::<Vec<_>>()).unwrap()
        };
        let a = commit("A", &[]); let b = commit("B", &[a]);
        repo.branch("feature", &repo.find_commit(b).unwrap(), false).unwrap();
        let c = commit("C", &[b]); let d = commit("D", &[c]);
        let c1 = commit("C1", &[b]); let d1 = commit("D1", &[c1]);
        let merge = commit("Merge main into feature", &[d1, d]); let tip = commit("F1", &[merge]);
        repo.reference("refs/heads/feature", tip, true, "advance feature").unwrap();
        repo.reference("refs/remotes/origin/main", d, true, "local fixture").unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        assert_eq!(creation_oid(&repo, "feature", tip), Some(b.to_string()));
        let shared = tauri::async_runtime::block_on(super::super::branches::graph_head_main_merge_base(path.to_string_lossy().into_owned())).unwrap().unwrap();
        assert_eq!(shared.oid, d.to_string()); assert_eq!(shared.base_ref, "origin/main");
        assert_eq!(repo.head().unwrap().target(), Some(tip), "diagnostics must not move HEAD");
        drop(tree); drop(repo);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn branch_story_creation_preserves_birth_after_commits_and_refuses_expired_or_moved_ref() {
        let path = std::env::temp_dir().join(format!("ddt-story-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let repo = Repository::init(&path).unwrap();
        repo.config().unwrap().set_bool("core.logallrefupdates", true).unwrap();
        let tree = repo.find_tree(repo.treebuilder(None).unwrap().write().unwrap()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let base = repo.commit(None, &sig, &sig, "base", &tree, &[]).unwrap();
        let base_commit = repo.find_commit(base).unwrap();
        repo.branch("feature", &base_commit, false).unwrap();
        let tip = repo.commit(Some("refs/heads/feature"), &sig, &sig, "feature", &tree, &[&base_commit]).unwrap();
        assert_eq!(creation_oid(&repo, "feature", tip), Some(base.to_string()));
        assert_eq!(creation_oid(&repo, "feature", base), None);
        assert_eq!(creation_oid(&repo, "origin/feature", tip), None);
        repo.reflog_delete("refs/heads/feature").unwrap();
        assert_eq!(creation_oid(&repo, "feature", tip), None);
        drop(tree); drop(base_commit); drop(repo);
        fs::remove_dir_all(path).unwrap();
    }
}
