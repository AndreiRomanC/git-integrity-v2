//! Creation evidence only: current merge-base is deliberately not a fallback.
use super::*;

fn creation_oid(repo: &Repository, branch: &str, expected_tip: Oid) -> Option<String> {
    let reference = repo.find_branch(branch, BranchType::Local).ok()?;
    if reference.get().target()? != expected_tip { return None; }
    let name = reference.get().name()?;
    let log = repo.reflog(name).ok()?;
    let oldest = log.get(log.len().checked_sub(1)?)?;
    if !oldest.id_old().is_zero() || !oldest.message()?.starts_with("branch: Created from ") { return None; }
    Some(oldest.id_new().to_string())
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
