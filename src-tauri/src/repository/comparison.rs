//! Read-only, exact-repository entry points for scoped comparisons. No fetch,
//! checkout, extra status validation or recursive submodule command is run.
//! Workspace diffs still inspect local paths within the requested scope.
use super::*;

// A missing tree object also produces NotFound. Only a missing *entry in a
// successfully read tree* means that a path is absent from the snapshot.
// Walking components keeps unreadable history from becoming an empty side.
pub(super) fn snapshot_path_entry(repo: &Repository, root: &git2::Tree, path: &Path) -> Result<Option<git2::TreeEntry<'static>>, git2::Error> {
    let mut tree = root.clone();
    let mut parts = path.iter().peekable();
    while let Some(part) = parts.next() {
        let name = part.to_str().ok_or_else(|| git2::Error::from_str("Snapshot path is not UTF-8"))?;
        let Some(entry) = tree.get_name(name).map(|entry| entry.to_owned()) else { return Ok(None); };
        if parts.peek().is_none() { return Ok(Some(entry)); }
        // Files and gitlinks have no children in this repository's tree.
        if entry.kind() != Some(ObjectType::Tree) { return Ok(None); }
        tree = repo.find_tree(entry.id())?;
    }
    Ok(None)
}

pub(super) fn snapshot_error(repository: &str, path: &str, side: &str, revision: &str, stage: &str, error: git2::Error) -> String {
    let commit = Oid::from_str(revision).map(|oid| oid.to_string()).unwrap_or_else(|_| "unresolved".into());
    // No raw refs, paths, error text or file contents in the shared log.
    perf_log(&format!("comparison_read: repo={} scope={} side={side} commit={commit} ref_id={} stage={stage} result=error code={:?} class={:?}",
        anonymized_repository_id(repository), anonymized_repository_id(path), anonymized_repository_id(revision), error.code(), error.class()), Duration::ZERO);
    let action = if stage == "open-repository" { "Cannot open the exact comparison repository".to_owned() }
        else { format!("Cannot read {side} snapshot ({stage})") };
    format!("{action}: {}. Comparison is incomplete; this is not an absent path.", error.message())
}

pub(super) fn snapshot_commit<'a>(repo: &'a Repository, repository: &str, path: &str, reference: &str, side: &str) -> Result<git2::Commit<'a>, String> {
    repo.revparse_single(reference).or_else(|_| repo.revparse_single(&format!("refs/remotes/{reference}")))
        .and_then(|object| object.peel_to_commit())
        .map_err(|error| snapshot_error(repository, path, side, reference, "resolve-commit", error))
}

pub(super) fn snapshot_entry_info(repo: &Repository, entry: &git2::TreeEntry, path: &str) -> Result<GitTreeEntry, git2::Error> {
    let kind = if entry.filemode() == 0o160000 { "submodule" } else if entry.kind() == Some(ObjectType::Tree) { "folder" } else { "file" };
    // Never read a gitlink's commit from the parent object database.
    let size = if kind == "file" { repo.find_blob(entry.id())?.size() as u64 } else { 0 };
    Ok(GitTreeEntry {
        entry: CommanderEntry { name: entry.name().unwrap_or("").into(), relative_path: path.into(), kind: kind.into(), size,
            revision: (kind == "submodule").then(|| entry.id().to_string()) },
        oid: entry.id(), filemode: entry.filemode(),
    })
}

#[derive(Serialize)]
pub struct ComparisonContext {
    pub(super) repository: RepositoryInfo,
    branches: Vec<Branch>,
}

#[tauri::command]
pub async fn comparison_context(repository_path: String) -> Result<ComparisonContext, String> {
    off_main_thread(move || {
        validate_path(&repository_path)?;
        // open(), not discover(): an uninitialized submodule must not silently
        // turn into a comparison of the enclosing superproject.
        let repo = Repository::open(&repository_path).map_err(|e| e.message().to_string())?;
        let detached = repo.head_detached().unwrap_or(false);
        let head = repo.head().ok();
        let current = if detached { String::new() } else { head.as_ref().and_then(|h| h.shorthand()).unwrap_or("").to_owned() };
        let head_oid = head.and_then(|h| h.peel_to_commit().ok()).map(|c| c.id().to_string()).unwrap_or_default();
        let mut branches = Vec::new();
        for item in repo.branches(None).map_err(|e| e.message().to_string())? {
            let (branch, kind) = item.map_err(|e| e.message().to_string())?;
            let name = branch.name().ok().flatten().unwrap_or("").to_owned();
            if name.is_empty() || name.ends_with("/HEAD") { continue; }
            let remote = kind == BranchType::Remote;
            let upstream = if remote { None } else { branch.upstream().ok().and_then(|b| b.get().shorthand().map(str::to_owned)) };
            branches.push(Branch { current: !remote && name == current, name, remote, upstream });
        }
        let repository = RepositoryInfo {
            name: Path::new(&repository_path).file_name().and_then(|p| p.to_str()).unwrap_or("Repository").into(),
            path: repository_path, current_branch: current, head_oid, head_detached: detached,
            gitdir: repo.path().to_string_lossy().into_owned(), submodule_url: None,
        };
        Ok(ComparisonContext { repository, branches })
    }).await
}

#[tauri::command]
pub async fn compare_workspace_revision(repository_path: String, relative_path: String, revision: String, flat: bool) -> Result<CommanderDirectory, String> {
    off_main_thread(move || {
        validate_path(&repository_path)?;
        Repository::open(&repository_path).map_err(|e| e.message().to_string())?;
        if flat { compare_workspace_file_list_exact(&repository_path, &relative_path, &revision) }
        else { compare_workspace_directory_exact(repository_path, relative_path, revision) }
    }).await
}

fn workspace_file(repository_path: &str, relative_path: &str, revision: &str) -> Result<FileComparison, String> {
    validate_path(repository_path)?;
    let relative = safe_relative_path(relative_path)?;
    let repo = Repository::open(repository_path).map_err(|e| e.message().to_string())?;
    let commit = resolve_commit(repository_path, revision)?;
    let remote = tree_file_content(repository_path, &commit, relative_path, true)?;
    let absolute = Path::new(repository_path).join(&relative);
    let index_entry = repo.index().ok().and_then(|i| i.get_path(&relative, 0));
    let local = if let Some(entry) = index_entry.filter(|entry| entry.mode == 0o160000) {
        let oid = Repository::open(&absolute).ok().and_then(|sub| sub.head().ok().and_then(|h| h.target())).unwrap_or(entry.id);
        Some(format!("Subproject commit {oid}\n").into_bytes())
    } else {
        match fs::read(&absolute) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("Cannot read working-tree file: {e}")),
        }
    };
    let (local_exists, remote_exists) = (local.is_some(), remote.is_some());
    let (local, remote) = (local.unwrap_or_default(), remote.unwrap_or_default());
    if local.len() > 1_000_000 || remote.len() > 1_000_000 || local.contains(&0) || remote.contains(&0) {
        return Err("Binary files and files over 1 MB are not shown in the text compare view".into());
    }
    Ok(FileComparison { relative_path: normalized(&relative), remote_ref: commit,
        local_content: String::from_utf8_lossy(&local).into_owned(), remote_content: String::from_utf8_lossy(&remote).into_owned(), local_exists, remote_exists })
}

#[tauri::command]
pub async fn compare_workspace_revision_file(repository_path: String, relative_path: String, revision: String) -> Result<FileComparison, String> {
    off_main_thread(move || workspace_file(&repository_path, &relative_path, &revision)).await
}
