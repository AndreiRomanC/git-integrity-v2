use super::{ensure_github_module_cache, exact_commit_oid, off_main_thread, short_date};
use git2::{Oid, Repository, Sort};
use serde::Serialize;

const PAGE_SIZE: usize = 100;

#[derive(Serialize)]
pub struct HistoryCommit {
    id: String,
    parents: Vec<String>,
    subject: String,
    message: String,
    author: String,
    date: String,
}

fn history_commit(commit: &git2::Commit<'_>) -> HistoryCommit {
    HistoryCommit {
        id: commit.id().to_string(),
        parents: commit.parent_ids().map(|id| id.to_string()).collect(),
        subject: commit.summary().unwrap_or("").into(),
        message: String::from_utf8_lossy(commit.message_bytes()).into_owned(),
        author: commit.author().name().unwrap_or("Unknown").into(),
        date: short_date(commit.time().seconds()),
    }
}

#[derive(Serialize)]
pub struct HistoryPage {
    commits: Vec<HistoryCommit>,
    has_more: bool,
}

// Walk the immutable tips shown in Browse, including tag-only ancestry in
// the all-refs graph. The cache's HEAD and branch names are not this session's selection.
fn history_page(repo: &Repository, tips: &[Oid], offset: usize) -> Result<HistoryPage, String> {
    let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).map_err(|e| e.to_string())?;
    for &id in tips {
        let object = repo.find_object(id, None)
            .map_err(|_| format!("Revision {id} is unavailable. Refresh the refs in Browse and retry."))?;
        match object.peel_to_commit() {
            Ok(commit) => walk.push(commit.id()).map_err(|e| e.to_string())?,
            Err(_) if tips.len() == 1 => return Err(format!("{id} does not point to a commit.")),
            Err(_) => continue, // Git also permits tags on trees and blobs.
        }
    }
    let mut commits = Vec::new();
    for (index, oid) in walk.enumerate() {
        let oid = oid.map_err(|e| e.to_string())?;
        if index < offset { continue; }
        if commits.len() == PAGE_SIZE { return Ok(HistoryPage { commits, has_more: true }); }
        let commit = repo.find_commit(oid).map_err(|e| e.to_string())?;
        commits.push(history_commit(&commit));
    }
    Ok(HistoryPage { commits, has_more: false })
}

fn inspect_tip_objects(repository_path: &str, owner: &str, repository_name: &str, revisions: &[String]) -> Result<(Repository, Vec<Oid>), String> {
    let tips = revisions.iter().map(|s| exact_commit_oid(s).ok_or("Expected a full commit ID.")).collect::<Result<Vec<_>, _>>()?;
    let ids: Vec<_> = revisions.iter().map(String::as_str).collect();
    let path = ensure_github_module_cache(repository_path, owner, repository_name, &ids)?;
    Ok((Repository::open_bare(path).map_err(|e| e.to_string())?, tips))
}

#[tauri::command]
pub async fn github_module_history(
    repository_path: String, owner: String, repository_name: String,
    revisions: Vec<String>, offset: usize,
) -> Result<HistoryPage, String> {
    off_main_thread(move || {
        if revisions.is_empty() { return Err("Choose a branch, tag or commit first.".into()); }
        let (repo, tips) = inspect_tip_objects(&repository_path, &owner, &repository_name, &revisions)?;
        history_page(&repo, &tips, offset)
    }).await
}

// Explicit request from Inspect. Read each advertised tip once, without
// walking its ancestry or querying the server separately for every row.
#[tauri::command]
pub async fn github_module_tip_details(repository_path: String, owner: String, repository_name: String, revisions: Vec<String>) -> Result<Vec<HistoryCommit>, String> {
    off_main_thread(move || {
        if revisions.is_empty() { return Ok(Vec::new()); }
        let (repo, tips) = inspect_tip_objects(&repository_path, &owner, &repository_name, &revisions)?;
        let mut details = Vec::new();
        for id in tips {
            let object = repo.find_object(id, None).map_err(|_| format!("Revision {id} is unavailable. Refresh refs in Browse."))?;
            if let Ok(commit) = object.peel_to_commit() { details.push(history_commit(&commit)); }
        }
        Ok(details)
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_inspect_history_stays_on_pinned_tips_and_pages_a_bare_graph() {
        let path = std::env::temp_dir().join(format!("gdd-inspect-history-{}", std::process::id()));
        let repo = Repository::init_bare(&path).unwrap();
        let sig = git2::Signature::now("Author", "test@example.org").unwrap();
        let tree = repo.find_tree(repo.treebuilder(None).unwrap().write().unwrap()).unwrap();
        let commit = |name: &str, parent: Option<Oid>| {
            let parents: Vec<_> = parent.iter().map(|oid| repo.find_commit(*oid).unwrap()).collect();
            repo.commit(None, &sig, &sig, name, &tree, &parents.iter().collect::<Vec<_>>()).unwrap()
        };
        let base = commit("Base", None);
        let side = commit("Tagged side", Some(base));
        repo.tag_lightweight("v1", repo.find_commit(side).unwrap().as_object(), false).unwrap();
        let mut tip = base;
        for i in 0..PAGE_SIZE { tip = commit(&format!("Change {i}"), Some(tip)); }
        repo.branch("main", &repo.find_commit(tip).unwrap(), false).unwrap();
        let selected = history_page(&repo, &[side], 0).unwrap();
        assert_eq!(selected.commits.len(), 2);
        assert_eq!(selected.commits[0].id, side.to_string());
        assert_eq!(selected.commits[0].parents, vec![base.to_string()]);
        assert_eq!(selected.commits[0].author, "Author");
        let first = history_page(&repo, &[tip, side], 0).unwrap();
        let next = history_page(&repo, &[tip, side], PAGE_SIZE).unwrap();
        assert!(first.has_more);
        assert!(!next.has_more);
        let ids: std::collections::HashSet<_> = first.commits.iter().chain(&next.commits).map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), PAGE_SIZE + 2);
        assert!(ids.contains(side.to_string().as_str()));
        assert_eq!(next.commits.last().unwrap().id, base.to_string());
        assert!(history_page(&repo, &[tree.id()], 0).is_err());
        assert!(repo.is_bare());
        drop(tree); drop(repo);
        std::fs::remove_dir_all(path).unwrap();
    }
}
