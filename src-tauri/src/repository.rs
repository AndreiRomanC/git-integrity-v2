use serde::Serialize;
use git2::{BranchType, ObjectType, Repository, Sort, Status, StatusOptions};
use std::{collections::{HashMap, HashSet}, fs, path::{Component, Path, PathBuf}, process::Command, sync::{Mutex, OnceLock}, time::{Instant, Duration, UNIX_EPOCH}};

#[derive(Clone, Default)]
struct GitMetadata {
    tracked: HashSet<String>,
    submodules: HashSet<String>,
    statuses: Vec<(String, String)>,
}

// A full status scan (`build_git_metadata`) walks the entire working tree — on a
// large repository (tens of thousands of files) this can take a noticeable amount
// of time. Doing it on *every* folder click / entry selection made navigation on
// large repos painfully slow. A short time-to-live cache keeps navigation snappy
// while still picking up changes made outside the app (another editor, a build
// tool) within a few seconds, instead of requiring a full app restart to see them.
const GIT_METADATA_TTL: Duration = Duration::from_secs(4);

static GIT_METADATA_CACHE: OnceLock<Mutex<HashMap<String, (Instant, GitMetadata)>>> = OnceLock::new();

fn metadata_cache() -> &'static Mutex<HashMap<String, (Instant, GitMetadata)>> {
    GIT_METADATA_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// tracked/submodules (from the index) don't depend on which folder is being
// browsed, so callers that only need those — not the working-tree status scan —
// can use this instead of paying for a (possibly scoped) status scan they don't need.
static INDEX_METADATA_CACHE: OnceLock<Mutex<HashMap<String, (Instant, (HashSet<String>, HashSet<String>))>>> = OnceLock::new();

fn index_metadata_cache() -> &'static Mutex<HashMap<String, (Instant, (HashSet<String>, HashSet<String>))>> {
    INDEX_METADATA_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_index_metadata(repository: &str) -> (HashSet<String>, HashSet<String>) {
    if let Some((cached_at, data)) = index_metadata_cache().lock().unwrap().get(repository) {
        if cached_at.elapsed() < GIT_METADATA_TTL { return data.clone(); }
    }
    let data = index_metadata(repository);
    index_metadata_cache().lock().unwrap().insert(repository.to_string(), (Instant::now(), data.clone()));
    data
}

#[derive(Serialize)]
pub struct RepositoryInfo { path: String, name: String, current_branch: String }

#[derive(Serialize)]
pub struct Branch { name: String, current: bool, remote: bool }

#[derive(Serialize)]
pub struct Commit { id: String, parents: Vec<String>, subject: String, author: String, date: String, refs: Vec<String>, lane: usize }

#[derive(Serialize)]
pub struct Change { status: String, path: String, staged: bool }

#[derive(Serialize)]
pub struct StashEntry { index: usize, message: String, base_commit: String }

#[derive(Serialize)]
pub struct RepositoryData { repository: RepositoryInfo, branches: Vec<Branch>, commits: Vec<Commit>, changes: Vec<Change>, stashes: Vec<StashEntry> }

#[derive(Serialize)]
pub struct DirectoryEntry {
    name: String,
    relative_path: String,
    kind: String,
    status: String,
    tracked: bool,
    size: u64,
    modified: u64,
}

#[derive(Serialize)]
pub struct EntryDetails {
    name: String,
    relative_path: String,
    kind: String,
    status: String,
    tracked: bool,
    size: u64,
    modified: u64,
    item_count: Option<usize>,
    submodule_url: Option<String>,
    submodule_branch: Option<String>,
    submodule_push_status: Option<String>,
    last_commit_id: Option<String>,
    last_commit_subject: Option<String>,
    last_commit_author: Option<String>,
    last_commit_date: Option<String>,
}

#[derive(Serialize)]
pub struct SubmoduleVersion {
    name: String,
    revision: String,
    kind: String,
    current: bool,
    subject: String,
    author: String,
    date: String,
}

#[derive(Serialize)]
pub struct SubmoduleVersions {
    path: String,
    current_revision: String,
    current_branch: String,
    versions: Vec<SubmoduleVersion>,
}

#[derive(Serialize, Clone)]
pub struct CommanderEntry {
    name: String,
    relative_path: String,
    kind: String,
    size: u64,
}

#[derive(Serialize)]
pub struct CommanderRow {
    name: String,
    relative_path: String,
    local: Option<CommanderEntry>,
    remote: Option<CommanderEntry>,
    status: String,
}

#[derive(Serialize)]
pub struct CommanderDirectory {
    remote_ref: String,
    remote_revision: String,
    relative_path: String,
    rows: Vec<CommanderRow>,
}

#[derive(Serialize)]
pub struct FileComparison {
    relative_path: String,
    remote_ref: String,
    local_content: String,
    remote_content: String,
}

#[derive(Serialize)]
pub struct RemoteInfo { name: String, fetch_url: String, push_url: String }

#[derive(Serialize)]
pub struct TextFile { relative_path: String, content: String }

#[derive(Serialize)]
pub struct PublishCommit { id: String, subject: String, author: String, date: String }

#[derive(Serialize)]
pub struct PublishStatus { branch: String, remote: String, remote_branch: String, commits: Vec<PublishCommit> }

fn git(path: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C").arg(path).arg("-c").arg("color.ui=false")
        .args(args).output().map_err(|e| format!("Cannot start Git: {e}"))?;
    if output.status.success() { Ok(String::from_utf8_lossy(&output.stdout).into_owned()) }
    else { Err(String::from_utf8_lossy(&output.stderr).trim().to_string()) }
}

fn internal_repository(path: &str) -> Result<Repository, String> {
    Repository::discover(path).map_err(|error| format!("Cannot open Git repository: {error}"))
}

fn internal_statuses(repository: &Repository, scope: Option<&str>) -> Result<Vec<(String, String, bool)>, String> {
    let mut options = StatusOptions::new();
    // Not recursing into wholly-untracked directories avoids enumerating every file
    // inside them individually (a major cost on large repos with big non-gitignored
    // directories, e.g. build output) — libgit2 still reports one entry for the
    // directory itself, which is all `status_for` needs to flag it as changed.
    // `update_index(true)` opportunistically refreshes the on-disk index's cached
    // file stat info during the scan (the same trick plain `git status` uses) so
    // later scans can trust the cache instead of re-stat'ing unchanged files —
    // pure perf, doesn't change what's reported.
    options.include_untracked(true).recurse_untracked_dirs(false).include_ignored(false).update_index(true);
    // Rename detection (comparing added/deleted file contents to spot moves) is
    // the single most expensive part of a status scan on a huge repository with
    // many pending changes, and it's only cosmetic — a renamed file still shows
    // up correctly as separate add/delete entries without it. Worth paying for on
    // a small, scoped folder view; not worth it on the full unscoped repository-wide
    // scan that `load_repository` runs after almost every action.
    if scope.is_some() { options.renames_head_to_index(true).renames_index_to_workdir(true); }
    // Scanning the whole working tree on every folder click is what made navigation
    // painfully slow on large repositories (worse still on Windows, where the same
    // filesystem calls are typically slower than on macOS/Linux) — a pathspec limits
    // the scan to just the folder being viewed, so cost scales with that folder's
    // size instead of the entire repository's.
    if let Some(scope) = scope { if !scope.is_empty() { options.pathspec(scope); } }
    let statuses = repository.statuses(Some(&mut options)).map_err(|error| error.message().to_string())?;
    Ok(statuses.iter().filter_map(|entry| {
        let path = entry.path()?.to_string(); let value = entry.status();
        let staged = value.intersects(Status::INDEX_NEW | Status::INDEX_MODIFIED | Status::INDEX_DELETED | Status::INDEX_RENAMED | Status::INDEX_TYPECHANGE);
        let code = if value.contains(Status::WT_NEW) { "??" } else if value.intersects(Status::WT_DELETED | Status::INDEX_DELETED) { "D" } else if value.intersects(Status::INDEX_NEW) { "A" } else if value.intersects(Status::WT_RENAMED | Status::INDEX_RENAMED) { "R" } else { "M" };
        Some((path, code.to_string(), staged))
    }).collect())
}

fn short_date(seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(seconds).map(|date| format!("{:04}-{:02}-{:02}", date.year(), date.month() as u8, date.day())).unwrap_or_default()
}

fn remote_callbacks() -> git2::RemoteCallbacks<'static> {
    let mut callbacks = git2::RemoteCallbacks::new(); callbacks.credentials(|url, username, allowed| {
        if allowed.contains(git2::CredentialType::SSH_KEY) { git2::Cred::ssh_key_from_agent(username.unwrap_or("git")) }
        else if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) { let config = git2::Config::open_default()?; git2::Cred::credential_helper(&config, url, username) }
        else { git2::Cred::default() }
    }); callbacks
}

fn network_fetch_options() -> git2::FetchOptions<'static> { let mut options = git2::FetchOptions::new(); options.remote_callbacks(remote_callbacks()); options.prune(git2::FetchPrune::On); options }
fn network_push_options() -> git2::PushOptions<'static> { let mut options = git2::PushOptions::new(); options.remote_callbacks(remote_callbacks()); options }

fn authenticated_push_options(username: String, access_token: String) -> git2::PushOptions<'static> {
    if access_token.is_empty() { return network_push_options(); }
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(move |_url, remote_username, allowed| {
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            git2::Cred::userpass_plaintext(if username.is_empty() { remote_username.unwrap_or("git") } else { &username }, &access_token)
        } else if allowed.contains(git2::CredentialType::SSH_KEY) { git2::Cred::ssh_key_from_agent(remote_username.unwrap_or("git")) }
        else { git2::Cred::default() }
    });
    let mut options = git2::PushOptions::new(); options.remote_callbacks(callbacks); options
}

fn authenticated_fetch_options(username: String, access_token: String) -> git2::FetchOptions<'static> {
    if access_token.is_empty() { return network_fetch_options(); }
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(move |_url, remote_username, allowed| {
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            git2::Cred::userpass_plaintext(if username.is_empty() { remote_username.unwrap_or("git") } else { &username }, &access_token)
        } else if allowed.contains(git2::CredentialType::SSH_KEY) { git2::Cred::ssh_key_from_agent(remote_username.unwrap_or("git")) }
        else { git2::Cred::default() }
    });
    let mut options = git2::FetchOptions::new(); options.remote_callbacks(callbacks); options.prune(git2::FetchPrune::On); options
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() { return Err("Repository path is empty".into()); }
    if !Path::new(path).is_dir() { return Err("The selected folder does not exist".into()); }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return Err("Invalid repository-relative path".into());
    }
    Ok(path.to_path_buf())
}

fn normalized(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn submodule_value(repository: &str, path: &str, field: &str) -> Option<String> {
    let repo = internal_repository(repository).ok()?; let submodule = repo.submodules().ok()?.into_iter().find(|item| normalized(item.path()) == path)?;
    match field { "url" => submodule.url().map(String::from), "branch" => submodule.branch().map(String::from), _ => None }
}

fn worktree_status(repository: &str, scope: Option<&str>) -> Vec<(String, String)> {
    internal_repository(repository).ok().and_then(|repo| internal_statuses(&repo, scope).ok()).unwrap_or_default().into_iter().map(|(path, status, _)| (path, status)).collect()
}

// tracked/submodules come from the index (a compact binary read, no per-file stat
// calls) so scanning it fully is cheap regardless of repository size — only the
// working-tree status scan needs to be scoped to stay fast on large repositories.
fn index_metadata(repository: &str) -> (HashSet<String>, HashSet<String>) {
    let mut tracked = HashSet::new(); let mut submodules = HashSet::new();
    if let Ok(repo) = internal_repository(repository) {
        if let Ok(index) = repo.index() { for entry in index.iter() {
            let path = String::from_utf8_lossy(&entry.path).into_owned(); tracked.insert(path.clone());
            if entry.mode == 0o160000 { submodules.insert(path); }
        } }
    }
    (tracked, submodules)
}

fn build_git_metadata(repository: &str, scope: Option<&str>, statuses: Option<Vec<(String, String)>>) -> GitMetadata {
    let (tracked, submodules) = cached_index_metadata(repository);
    GitMetadata { statuses: statuses.unwrap_or_else(|| worktree_status(repository, scope)), tracked, submodules }
}

// Cache key includes the scope so different folders (and the unscoped "whole
// repository" view) are cached independently — browsing into a small folder inside
// a huge repository should be fast even if the repository-wide view was scanned
// moments ago, and vice versa.
fn metadata_cache_key(repository: &str, scope: &str) -> String { format!("{repository}\u{0}{scope}") }

fn cached_git_metadata(repository: &str, scope: &str) -> GitMetadata {
    let key = metadata_cache_key(repository, scope);
    if let Some((cached_at, metadata)) = metadata_cache().lock().unwrap().get(&key) {
        if cached_at.elapsed() < GIT_METADATA_TTL { return metadata.clone(); }
    }
    let metadata = build_git_metadata(repository, Some(scope), None);
    metadata_cache().lock().unwrap().insert(key, (Instant::now(), metadata.clone()));
    metadata
}

fn replace_git_metadata(repository: &str, statuses: Vec<(String, String)>) {
    // `statuses` here is always a full, unscoped scan (from load_repository, which
    // needs every change for the Changes drawer regardless of folder) — seed the
    // repository-wide cache entry with it instead of discarding that work.
    let metadata = build_git_metadata(repository, None, Some(statuses));
    metadata_cache().lock().unwrap().insert(metadata_cache_key(repository, ""), (Instant::now(), metadata));
}

fn invalidate_git_metadata(repository: &str) {
    // Cache keys are "{repository}\0{scope}" (one entry per folder that's been
    // browsed) — a mutation can affect any of them, so drop every scope cached for
    // this repository, not just the unscoped entry.
    let prefix = format!("{repository}\u{0}");
    metadata_cache().lock().unwrap().retain(|key, _| !key.starts_with(&prefix));
    index_metadata_cache().lock().unwrap().remove(repository);
}

fn remove_submodule_section(path: &Path, name: &str) -> Result<(), String> {
    if !path.exists() { return Ok(()); }
    let content = fs::read_to_string(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let expected = format!("[submodule \"{name}\"]");
    let mut skip = false;
    let mut kept = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            skip = trimmed == expected;
            if skip { continue; }
        }
        if !skip { kept.push(line); }
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) { kept.pop(); }
    let has_submodules = kept.iter().any(|line| line.trim().starts_with("[submodule \""));
    if path.file_name().and_then(|value| value.to_str()) == Some(".gitmodules") && !has_submodules {
        fs::remove_file(path).map_err(|error| format!("Cannot remove {}: {error}", path.display()))?;
    } else {
        let output = if kept.is_empty() { String::new() } else { format!("{}\n", kept.join("\n")) };
        fs::write(path, output).map_err(|error| format!("Cannot update {}: {error}", path.display()))?;
    }
    Ok(())
}

fn cleanup_submodule_registration(repo: &Repository, name: &str, relative: &Path) -> Result<(), String> {
    let workdir = repo.workdir().ok_or("Bare repositories cannot contain working submodules")?;
    remove_submodule_section(&workdir.join(".gitmodules"), name)?;
    remove_submodule_section(&repo.path().join("config"), name)?;
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    let _ = index.remove_path(relative);
    if workdir.join(".gitmodules").exists() { let _ = index.add_path(Path::new(".gitmodules")); }
    else { let _ = index.remove_path(Path::new(".gitmodules")); }
    index.write().map_err(|error| error.message().to_string())
}

fn status_for(path: &str, statuses: &[(String, String)]) -> String {
    let prefix = format!("{path}/");
    statuses.iter().find(|(changed, _)| changed == path).map(|(_, status)| status.clone())
        .or_else(|| statuses.iter().find(|(changed, _)| changed.starts_with(&prefix)).map(|_| "•".into()))
        .unwrap_or_default()
}

#[tauri::command]
pub fn choose_folder() -> Option<String> {
    rfd::FileDialog::new().pick_folder().map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn init_repository(path: String) -> Result<(), String> {
    validate_path(&path)?;
    Repository::init(&path).map(|_| ()).map_err(|error| error.message().to_string())
}

#[tauri::command]
pub fn clone_repository(url: String, parent_path: String, folder_name: String) -> Result<String, String> {
    validate_path(&parent_path)?;
    let url = url.trim(); let folder_name = folder_name.trim();
    if url.is_empty() { return Err("Repository URL cannot be empty".into()); }
    let folder = safe_relative_path(folder_name)?;
    if folder.components().count() != 1 || folder_name.is_empty() { return Err("Choose a simple local folder name".into()); }
    let destination = Path::new(&parent_path).join(&folder);
    if destination.exists() { return Err("The destination folder already exists".into()); }
    let mut builder = git2::build::RepoBuilder::new(); let fetch = network_fetch_options(); builder.fetch_options(fetch); builder.clone(url, &destination).map_err(|error| format!("Clone failed: {}", error.message()))?;
    Ok(destination.to_string_lossy().into_owned())
}

// Matches only URLs this app itself generates from a "P:PROJECT-NUMBER" pattern in
// a commit message (see commitSubjectHtml in the frontend), e.g.
// "https://polarion.vitesco.io/polarion/#/project/OMBMS/workitem?id=OMBMS-21610".
// Kept as a strict allowlist (not just "starts with the host") since this reaches
// a shell command (`open`/`cmd start`/`xdg-open`) with the URL as an argument.
fn is_generated_polarion_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://polarion.vitesco.io/polarion/#/project/") else { return false };
    let Some((project, tail)) = rest.split_once("/workitem?id=") else { return false };
    if project.is_empty() || !project.chars().all(|value| value.is_ascii_alphanumeric() || value == '_') { return false; }
    let Some((id_project, id_number)) = tail.split_once('-') else { return false };
    id_project == project && !id_number.is_empty() && id_number.chars().all(|value| value.is_ascii_digit())
}

#[tauri::command]
pub fn open_external_url(url: String) -> Result<(), String> {
    if !is_generated_polarion_url(&url) { return Err("Only generated Polarion links can be opened".into()); }
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(&url).status();
    #[cfg(target_os = "windows")]
    let status = Command::new("cmd").args(["/C", "start", "", &url]).status();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let status = Command::new("xdg-open").arg(&url).status();
    status.map_err(|error| error.to_string()).and_then(|result| result.success().then_some(()).ok_or_else(|| "Could not open the default browser".into()))
}

fn browser_repository_url(remote: &str) -> Option<String> {
    let value = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let url = if let Some(rest) = value.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?; format!("https://{host}/{path}")
    } else if let Some(rest) = value.strip_prefix("ssh://git@") {
        format!("https://{rest}")
    } else if value.starts_with("https://") || value.starts_with("http://") {
        value.to_string()
    } else { return None };
    Some(url.trim_end_matches(".git").to_string())
}

fn launch_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(url).status();
    #[cfg(target_os = "windows")]
    let status = Command::new("cmd").args(["/C", "start", "", url]).status();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let status = Command::new("xdg-open").arg(url).status();
    status.map_err(|error| error.to_string()).and_then(|result| result.success().then_some(()).ok_or_else(|| "Could not open the default browser".into()))
}

#[tauri::command]
pub fn open_repository_item(repository_path: String, relative_path: String, kind: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let repo = internal_repository(&repository_path)?; let remote = repo.find_remote("origin").or_else(|_| { let names = repo.remotes()?; let first = names.iter().flatten().next().ok_or_else(|| git2::Error::from_str("No remote is configured"))?; repo.find_remote(first) }).map_err(|error| error.message().to_string())?; let remote = remote.url().ok_or("The remote has no URL")?.to_string();
    let base = browser_repository_url(remote.trim()).ok_or("This remote URL cannot be opened in a browser")?;
    let branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).or_else(|| repo.head().ok().and_then(|head| head.target().map(|id| id.to_string()))).unwrap_or_else(|| "HEAD".into());
    let path = normalized(&relative);
    let url = if path.is_empty() { base } else {
        let segment = if kind == "file" { "blob" } else { "tree" };
        format!("{base}/{segment}/{branch}/{path}")
    };
    launch_browser(&url)
}

#[tauri::command]
pub fn open_commit_on_server(repository_path: String, commit_id: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let remote = repo.find_remote("origin").or_else(|_| { let names = repo.remotes()?; let first = names.iter().flatten().next().ok_or_else(|| git2::Error::from_str("No remote is configured"))?; repo.find_remote(first) }).map_err(|error| error.message().to_string())?;
    let remote = remote.url().ok_or("The remote has no URL")?.to_string();
    let base = browser_repository_url(remote.trim()).ok_or("This remote URL cannot be opened in a browser")?;
    launch_browser(&format!("{base}/commit/{}", commit_id.trim()))
}

#[tauri::command]
pub fn load_repository(path: String) -> Result<RepositoryData, String> {
    validate_path(&path)?;
    let mut repo = internal_repository(&path)?;
    let name = Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or("repository").to_string();
    let current_branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).unwrap_or_default();
    let mut branches = Vec::new();
    for branch_type in [BranchType::Local, BranchType::Remote] { if let Ok(iterator) = repo.branches(Some(branch_type)) { for item in iterator.flatten() {
        let branch_name = item.0.name().ok().flatten().unwrap_or("").to_string(); if branch_name.ends_with("/HEAD") { continue; }
        branches.push(Branch { current: branch_type == BranchType::Local && branch_name == current_branch, name: branch_name, remote: branch_type == BranchType::Remote });
    } } }

    // `refs/stash` is a real Git ref but its target is a synthetic WIP commit
    // (with an index/untracked-files "merge" parent structure) that has nothing
    // to do with real branch history — excluded here and surfaced separately as
    // `stashes` instead, so the graph only ever shows real ancestry.
    let mut refs_by_oid: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(references) = repo.references() { for reference in references.flatten() {
        if reference.name() == Some("refs/stash") { continue; }
        if let (Some(oid), Some(name)) = (reference.target(), reference.shorthand()) { refs_by_oid.entry(oid.to_string()).or_default().push(name.to_string()); }
    } }
    let mut commits = Vec::new(); let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).map_err(|error| error.message().to_string())?;
    if let Ok(references) = repo.references() { for reference in references.flatten() {
        if reference.name() == Some("refs/stash") { continue; }
        if let Ok(object) = reference.peel(ObjectType::Commit) { let _ = walk.push(object.id()); }
    } }
    for oid in walk.flatten().take(500) { if let Ok(commit) = repo.find_commit(oid) {
        commits.push(Commit { id: oid.to_string(), parents: commit.parent_ids().map(|id| id.to_string()).collect(), subject: commit.summary().unwrap_or("No message").to_string(), author: commit.author().name().unwrap_or("Unknown").to_string(), date: short_date(commit.time().seconds()), refs: refs_by_oid.remove(&oid.to_string()).unwrap_or_default(), lane: 0 });
    } }

    let mut raw_stashes: Vec<(usize, String, git2::Oid)> = Vec::new();
    let _ = repo.stash_foreach(|index, message, oid| { raw_stashes.push((index, message.to_string(), *oid)); true });
    let stashes = raw_stashes.into_iter().map(|(index, message, oid)| {
        let base_commit = repo.find_commit(oid).ok().and_then(|commit| commit.parent_id(0).ok()).map(|id| id.to_string()).unwrap_or_default();
        StashEntry { index, message, base_commit }
    }).collect();

    let internal = internal_statuses(&repo, None)?;
    let statuses = internal.iter().map(|(path, status, _)| (path.clone(), status.clone())).collect::<Vec<_>>();
    let changes = internal.into_iter().map(|(path, status, staged)| Change { status, path, staged }).collect();

    replace_git_metadata(&path, statuses);

    Ok(RepositoryData { repository: RepositoryInfo { path, name, current_branch }, branches, commits, changes, stashes })
}

#[tauri::command]
pub fn stage_files(path: String, files: Vec<String>) -> Result<(), String> {
    validate_path(&path)?;
    let repo = internal_repository(&path)?;
    let safe_files = files.into_iter().map(|file| safe_relative_path(file.trim_end_matches(|character| character == '/' || character == '\\'))).collect::<Result<Vec<_>, _>>()?;
    let mut submodule_paths = HashSet::new();
    for safe in &safe_files { if let Ok(mut submodule) = repo.find_submodule(&normalized(safe)) { submodule.add_to_index(true).map_err(|error| format!("Cannot stage submodule {}: {}", normalized(safe), error.message()))?; submodule_paths.insert(normalized(safe)); } }
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    for safe in safe_files { let normalized_safe = normalized(&safe); if submodule_paths.contains(&normalized_safe) { continue; } let absolute = Path::new(&path).join(&safe); if absolute.exists() { index.add_all([safe.as_path()], git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?; } else { let _ = index.remove_path(&safe); } }
    if !submodule_paths.is_empty() && Path::new(&path).join(".gitmodules").exists() { index.add_path(Path::new(".gitmodules")).map_err(|error| error.message().to_string())?; }
    index.write().map_err(|error| error.message().to_string())?; invalidate_git_metadata(&path); Ok(())
}

#[tauri::command]
pub fn unstage_files(path: String, files: Vec<String>) -> Result<(), String> {
    validate_path(&path)?;
    let repo = internal_repository(&path)?; let head = repo.head().and_then(|head| head.peel(ObjectType::Commit)).map_err(|error| error.message().to_string())?;
    let safe = files.iter().map(|file| safe_relative_path(file)).collect::<Result<Vec<_>, _>>()?; repo.reset_default(Some(&head), safe.iter()).map_err(|error| error.message().to_string())?; invalidate_git_metadata(&path); Ok(())
}

#[tauri::command]
pub fn create_commit(path: String, message: String) -> Result<(), String> {
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    let repo = internal_repository(&path)?; let mut index = repo.index().map_err(|error| error.message().to_string())?; let tree_id = index.write_tree().map_err(|error| error.message().to_string())?; let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?; let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?;
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok()); let parents: Vec<&git2::Commit<'_>> = parent.iter().collect(); repo.commit(Some("HEAD"), &signature, &signature, message.trim(), &tree, &parents).map_err(|error| error.message().to_string())?; invalidate_git_metadata(&path); Ok(())
}

#[tauri::command]
pub fn create_branch(path: String, branch: String) -> Result<(), String> {
    if branch.trim().is_empty() { return Err("Branch name cannot be empty".into()); }
    let repo = internal_repository(&path)?; let head = repo.head().and_then(|head| head.peel_to_commit()).map_err(|error| error.message().to_string())?; repo.branch(branch.trim(), &head, false).map_err(|error| error.message().to_string())?; drop(head); switch_branch(path, branch)
}

#[tauri::command]
pub fn switch_branch(path: String, branch: String) -> Result<(), String> {
    let repo = internal_repository(&path)?; let reference = format!("refs/heads/{}", branch.trim()); repo.find_reference(&reference).map_err(|error| error.message().to_string())?; repo.set_head(&reference).map_err(|error| error.message().to_string())?; let mut checkout = git2::build::CheckoutBuilder::new(); checkout.safe(); repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?; invalidate_git_metadata(&path); Ok(())
}

#[tauri::command]
pub fn read_text_file(repository_path: String, relative_path: String) -> Result<TextFile, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let bytes = fs::read(Path::new(&repository_path).join(&relative)).map_err(|error| error.to_string())?;
    if bytes.len() > 2_000_000 || bytes.contains(&0) { return Err("Only text files up to 2 MB can be edited".into()); }
    Ok(TextFile { relative_path, content: String::from_utf8(bytes).map_err(|_| "The file is not valid UTF-8 text")? })
}

#[tauri::command]
pub fn write_text_file(repository_path: String, relative_path: String, content: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let target = Path::new(&repository_path).join(&relative);
    if !target.is_file() { return Err("The selected path is not a file".into()); }
    if content.len() > 2_000_000 { return Err("Only text files up to 2 MB can be edited".into()); }
    fs::write(target, content).map_err(|error| error.to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn list_remotes(repository_path: String) -> Result<Vec<RemoteInfo>, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?; let names = repo.remotes().map_err(|error| error.message().to_string())?; let mut result = Vec::new();
    for name in names.iter().flatten() { if let Ok(remote) = repo.find_remote(name) { result.push(RemoteInfo { name: name.to_string(), fetch_url: remote.url().unwrap_or("").to_string(), push_url: remote.pushurl().or_else(|| remote.url()).unwrap_or("").to_string() }); } }
    Ok(result)
}

#[tauri::command]
pub fn fetch_remote(repository_path: String, remote: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let remote_name = remote.trim();
    let repo = internal_repository(&repository_path)?; repo.find_remote(remote_name).map_err(|error| error.message().to_string())?;
    // System `git` reuses the user's already-working credentials (SSH agent,
    // credential helper, OS keychain) instead of libgit2's narrower built-in search.
    git(&repository_path, &["fetch", remote_name]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn sync_repository(repository_path: String, action: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?; let head = repo.head().map_err(|error| error.message().to_string())?; let branch = head.shorthand().ok_or("Detached HEAD cannot be synchronized")?.to_string(); let upstream = repo.find_branch(&branch, BranchType::Local).and_then(|branch| branch.upstream()).map_err(|_| "The current branch has no upstream".to_string())?; let upstream_name = upstream.name().ok().flatten().ok_or("Invalid upstream")?.to_string(); let (remote_name, remote_branch) = upstream_name.split_once('/').ok_or("Invalid upstream branch")?; drop(upstream); drop(head);
    match action.as_str() {
        "pull" => { fetch_remote(repository_path.clone(), remote_name.into())?; let remote_ref = repo.find_reference(&format!("refs/remotes/{remote_name}/{remote_branch}")).map_err(|error| error.message().to_string())?; let target = remote_ref.target().ok_or("Remote branch has no target")?; let annotated = repo.find_annotated_commit(target).map_err(|error| error.message().to_string())?; let (analysis, _) = repo.merge_analysis(&[&annotated]).map_err(|error| error.message().to_string())?; if !analysis.is_fast_forward() && !analysis.is_up_to_date() { return Err("Pull requires a merge; only fast-forward pull is allowed".into()); } if analysis.is_fast_forward() { let mut local = repo.find_reference(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?; local.set_target(target, "fast-forward pull").map_err(|error| error.message().to_string())?; repo.set_head(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?; let mut checkout = git2::build::CheckoutBuilder::new(); checkout.safe(); repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?; } }
        "push" => { repo.find_remote(remote_name).map_err(|error| error.message().to_string())?; git(&repository_path, &["push", remote_name, &format!("{branch}:refs/heads/{remote_branch}")]).map_err(|detail| format!("Push failed: {detail}"))?; }
        _ => return Err("Unsupported synchronization action".into()), }
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn publish_status(repository_path: String, branch: String, remote: String) -> Result<PublishStatus, String> {
    validate_path(&repository_path)?;
    let branch = branch.trim(); let remote = remote.trim();
    if branch.is_empty() || remote.is_empty() { return Err("Choose a local branch and a remote".into()); }
    let repo = internal_repository(&repository_path)?; let local_oid = repo.refname_to_id(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    let remote_branch = format!("{remote}/{branch}");
    let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.push(local_oid).map_err(|error| error.message().to_string())?; if let Ok(remote_oid) = repo.refname_to_id(&format!("refs/remotes/{remote}/{branch}")) { let _ = walk.hide(remote_oid); } let mut commits = walk.flatten().take(100).filter_map(|oid| repo.find_commit(oid).ok().map(|commit| PublishCommit { id: oid.to_string(), subject: commit.summary().unwrap_or("No message").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()) })).collect::<Vec<_>>(); commits.reverse();
    Ok(PublishStatus { branch: branch.into(), remote: remote.into(), remote_branch, commits })
}

#[tauri::command]
pub fn publish_branch(repository_path: String, branch: String, remote: String, username: String, access_token: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if branch.trim().is_empty() || remote.trim().is_empty() { return Err("Choose a local branch and a remote".into()); }
    let repo = internal_repository(&repository_path)?; let branch = branch.trim(); let remote_name = remote.trim(); let local_oid = repo.refname_to_id(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    if access_token.trim().is_empty() {
        // No explicit token was entered — prefer the system `git` binary, which
        // transparently reuses the user's already-working SSH agent, credential
        // helper, or OS keychain. libgit2's own credential search is much narrower
        // and can fail here ("failed to acquire username/password") even when a
        // plain `git push` in a terminal works fine for the same repository.
        git(&repository_path, &["push", remote_name, &format!("{branch}:refs/heads/{branch}")])
            .map_err(|detail| if detail.to_lowercase().contains("authentication") || detail.contains("403") || detail.contains("could not read") { "Push authentication failed. Either make sure `git push` works for this repository from a terminal, or enter a Git username and Personal Access Token in Publish credentials.".to_string() } else if detail.to_lowercase().contains("non-fast-forward") || detail.to_lowercase().contains("fetch first") { "Push rejected because the server branch has newer commits. Pull/fetch those commits first, then publish again.".to_string() } else { format!("Push failed: {detail}") })?;
    } else {
        let mut remote = repo.find_remote(remote_name).map_err(|error| error.message().to_string())?; let mut options = authenticated_push_options(username, access_token); remote.push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], Some(&mut options)).map_err(|error| { let detail = error.message(); if detail.contains("username/password") || detail.contains("authentication") || detail.contains("401") || detail.contains("403") { "Push authentication failed. Check the username and Personal Access Token in Publish credentials (not your account password).".to_string() } else if detail.contains("non-fast-forward") { "Push rejected because the server branch has newer commits. Pull/fetch those commits first, then publish again.".to_string() } else { format!("Push failed: {detail}") } })?; drop(remote);
    }
    repo.reference(&format!("refs/remotes/{remote_name}/{branch}"), local_oid, true, "successful publish").map_err(|error| format!("Push succeeded, but local server tracking could not be updated: {}", error.message()))?;
    let mut config = repo.config().map_err(|error| error.message().to_string())?; config.set_str(&format!("branch.{branch}.remote"), remote_name).map_err(|error| error.message().to_string())?; config.set_str(&format!("branch.{branch}.merge"), &format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn submodule_repository(repository_path: String, relative_path: String) -> Result<RepositoryData, String> {
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    load_repository(absolute.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn load_directory(repository_path: String, relative_path: String) -> Result<Vec<DirectoryEntry>, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    if !absolute.is_dir() { return Err("The selected path is not a folder".into()); }

    let git_metadata = cached_git_metadata(&repository_path, &relative_path);
    let mut entries = Vec::new();

    for item in fs::read_dir(&absolute).map_err(|error| error.to_string())? {
        let item = item.map_err(|error| error.to_string())?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" { continue; }
        let entry_relative = relative.join(&name);
        let relative_string = normalized(&entry_relative);
        let metadata = fs::symlink_metadata(item.path()).map_err(|error| error.to_string())?;
        let kind = if git_metadata.submodules.contains(&relative_string) { "submodule" }
            else if metadata.file_type().is_symlink() { "symlink" }
            else if metadata.is_dir() { "folder" }
            else { "file" }.to_string();
        let tracked_prefix = format!("{relative_string}/");
        let tracked = git_metadata.submodules.contains(&relative_string) || git_metadata.tracked.contains(&relative_string) || git_metadata.tracked.iter().any(|path| path.starts_with(&tracked_prefix));
        let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|value| value.as_secs()).unwrap_or(0);
        entries.push(DirectoryEntry { name, relative_path: relative_string.clone(), kind, status: status_for(&relative_string, &git_metadata.statuses), tracked, size: if metadata.is_file() { metadata.len() } else { 0 }, modified });
    }
    entries.sort_by(|a, b| {
        let a_group = matches!(a.kind.as_str(), "folder" | "submodule");
        let b_group = matches!(b.kind.as_str(), "folder" | "submodule");
        b_group.cmp(&a_group).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

#[tauri::command]
pub fn entry_details(repository_path: String, relative_path: String) -> Result<EntryDetails, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    if relative.as_os_str().is_empty() { return Err("Select a file or folder".into()); }
    let absolute = Path::new(&repository_path).join(&relative);
    let metadata = fs::symlink_metadata(&absolute).map_err(|error| error.to_string())?;
    let relative_string = normalized(&relative);
    let git_metadata = cached_git_metadata(&repository_path, &relative_string);
    let kind = if git_metadata.submodules.contains(&relative_string) { "submodule" }
        else if metadata.file_type().is_symlink() { "symlink" }
        else if metadata.is_dir() { "folder" } else { "file" }.to_string();
    let prefix = format!("{relative_string}/");
    let tracked = git_metadata.tracked.contains(&relative_string) || git_metadata.tracked.iter().any(|path| path.starts_with(&prefix));
    let status = status_for(&relative_string, &git_metadata.statuses);
    let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|value| value.as_secs()).unwrap_or(0);
    let item_count = metadata.is_dir().then(|| fs::read_dir(&absolute).map(|items| items.count()).unwrap_or(0));

    let last = last_commit_touching_path(&repository_path, &relative);
    let submodule_url = (kind == "submodule").then(|| submodule_value(&repository_path, &relative_string, "url")).flatten();
    let submodule_branch = (kind == "submodule").then(|| submodule_value(&repository_path, &relative_string, "branch")).flatten();
    let submodule_push_status = (kind == "submodule").then(|| submodule_push_status(&absolute.to_string_lossy())).flatten();

    Ok(EntryDetails {
        name: absolute.file_name().and_then(|name| name.to_str()).unwrap_or(&relative_string).to_string(), relative_path: relative_string,
        kind, status, tracked, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, item_count, submodule_url, submodule_branch, submodule_push_status,
        last_commit_id: last.as_ref().map(|value| value.0.clone()),
        last_commit_subject: last.as_ref().map(|value| value.1.clone()), last_commit_author: last.as_ref().map(|value| value.2.clone()), last_commit_date: last.as_ref().map(|value| value.3.clone()),
    })
}

// Uses only already-known local refs (no fetch) so it's cheap enough to call every
// time a submodule is selected. Tells the user, at a glance, whether the commit
// they're looking at has actually reached the submodule's own remote yet.
fn submodule_push_status(sub_path: &str) -> Option<String> {
    let repo = internal_repository(sub_path).ok()?;
    let head = repo.head().ok()?;
    let local_target = head.target()?;
    if repo.head_detached().unwrap_or(true) { return Some("Detached — not on a branch, so it cannot be pushed as-is. Use \"Change version\" to switch to a branch first.".into()); }
    let branch = head.shorthand()?.to_string();
    drop(head);
    let remote_target = repo.find_reference(&format!("refs/remotes/origin/{branch}")).ok()?.target()?;
    if remote_target == local_target { return None; }
    let mut walk = repo.revwalk().ok()?; walk.push(local_target).ok()?; let _ = walk.hide(remote_target);
    let ahead = walk.take(50).count();
    Some(if ahead > 0 { format!("{ahead} commit{} not yet pushed to origin/{branch} — needs push", if ahead == 1 { "" } else { "s" }) } else { format!("Diverged from origin/{branch}") })
}

fn validate_submodule(repository_path: &str, relative_path: &str) -> Result<PathBuf, String> {
    let relative = safe_relative_path(relative_path)?;
    let normalized_path = normalized(&relative);
    if !cached_index_metadata(repository_path).1.contains(&normalized_path) { return Err("The selected folder is not a Git submodule".into()); }
    let absolute = Path::new(repository_path).join(relative);
    if !absolute.is_dir() { return Err("The submodule is not initialized".into()); }
    internal_repository(absolute.to_str().unwrap_or_default())?;
    Ok(absolute)
}

#[tauri::command]
pub fn submodule_versions(repository_path: String, relative_path: String) -> Result<SubmoduleVersions, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let repo = internal_repository(absolute.to_str().unwrap_or_default())?;
    let current_revision = repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    let current_branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).unwrap_or_default();
    let mut versions = Vec::new();
    for branch_type in [BranchType::Local, BranchType::Remote] { if let Ok(iterator) = repo.branches(Some(branch_type)) { for item in iterator.flatten() { let name = item.0.name().ok().flatten().unwrap_or("").to_string(); if name.ends_with("/HEAD") { continue; } if let Some(oid) = item.0.get().target() { if let Ok(commit) = repo.find_commit(oid) { let kind = if branch_type == BranchType::Local { "branch" } else { "remote" }; versions.push(SubmoduleVersion { name: name.clone(), revision: oid.to_string(), kind: kind.into(), current: kind == "branch" && name == current_branch, subject: commit.summary().unwrap_or("").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()) }); } } } } }
    let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; let _ = walk.set_sorting(Sort::TIME); if let Ok(head) = repo.head() { if let Some(oid) = head.target() { let _ = walk.push(oid); } }
    for oid in walk.flatten().take(30) { if let Ok(commit) = repo.find_commit(oid) { versions.push(SubmoduleVersion { name: oid.to_string()[..8].into(), revision: oid.to_string(), kind: "commit".into(), current: oid.to_string() == current_revision, subject: commit.summary().unwrap_or("").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()) }); } }
    Ok(SubmoduleVersions { path: relative_path, current_revision, current_branch, versions })
}

#[tauri::command]
pub fn add_submodule(repository_path: String, parent_path: String, url: String, folder_name: String, username: String, access_token: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    let parent = safe_relative_path(parent_path.trim())?;
    let folder_name = folder_name.trim();
    let folder = safe_relative_path(folder_name)?;
    if folder_name.is_empty() || folder.components().count() != 1 {
        return Err("Choose a simple folder name for the submodule".into());
    }
    let relative = parent.join(folder);
    let relative_string = normalized(&relative);
    if relative_string.is_empty() { return Err("The submodule destination is invalid".into()); }
    let url = url.trim();
    if url.is_empty() || url.starts_with('-') { return Err("Enter a valid Git repository URL".into()); }

    let repo = internal_repository(&repository_path)?;
    let destination = Path::new(&repository_path).join(&relative);
    let indexed = cached_index_metadata(&repository_path).0.contains(&relative_string);
    let stale_name = repo.submodules().ok().and_then(|items| items.into_iter()
        .find(|item| normalized(item.path()) == relative_string)
        .map(|item| item.name().unwrap_or(&relative_string).to_string()));
    // A failed/deleted uncommitted submodule may leave .gitmodules, config and
    // .git/modules entries behind. They must not prevent adding the same path again.
    if let Some(name) = stale_name {
        if indexed { return Err("This submodule is already tracked. Remove it from Git before adding it again".into()); }
        cleanup_submodule_registration(&repo, &name, &relative)?;
        for storage in [repo.path().join("modules").join(&relative), repo.path().join("modules").join(safe_relative_path(&name)?)] {
            if storage.is_dir() { fs::remove_dir_all(&storage).map_err(|error| format!("Cannot clean previous submodule metadata: {error}"))?; }
        }
    }
    if destination.exists() {
        let empty_orphan = destination.is_dir() && fs::read_dir(&destination).map(|mut entries| entries.next().is_none()).unwrap_or(false)
            && repo.submodules().map(|items| !items.into_iter().any(|item| normalized(item.path()) == relative_string)).unwrap_or(true);
        if !empty_orphan { return Err("The submodule destination already exists".into()); }
        fs::remove_dir(&destination).map_err(|error| format!("Cannot remove the previous empty attempt: {error}"))?;
        let old_storage = repo.path().join("modules").join(&relative);
        if old_storage.is_dir() { fs::remove_dir_all(old_storage).map_err(|error| format!("Cannot clean the previous failed attempt: {error}"))?; }
    }
    let mut submodule = repo.submodule(url, &relative, true)
        .map_err(|error| format!("Cannot prepare submodule: {}", error.message()))?;
    let mut options = git2::SubmoduleUpdateOptions::new();
    options.fetch(authenticated_fetch_options(username, access_token));
    let name = submodule.name().unwrap_or(&relative_string).to_string();
    let rollback = |message: String| {
        let _ = cleanup_submodule_registration(&repo, &name, &relative);
        if destination.is_dir() { let _ = fs::remove_dir_all(&destination); }
        let storage = repo.path().join("modules").join(&relative); if storage.is_dir() { let _ = fs::remove_dir_all(storage); }
        Err(message)
    };
    let cloned = match submodule.clone(Some(&mut options)) { Ok(cloned) => cloned, Err(error) => { let detail = error.message(); let message = if detail.contains("username/password") || detail.contains("authentication") { "Authentication required. Open ‘Private repository credentials’ and enter your Git server username plus a Personal Access Token (not your account password).".to_string() } else { format!("Cannot clone submodule: {detail}") }; return rollback(message); } };
    if cloned.head().ok().and_then(|head| head.target()).is_none() { return rollback("The server repository has no default commit to check out".into()); }
    let mut checkout = git2::build::CheckoutBuilder::new(); checkout.safe();
    if let Err(error) = cloned.checkout_head(Some(&mut checkout)) { return rollback(format!("Cannot check out submodule files: {}", error.message())); }
    if let Err(error) = submodule.add_finalize() { return rollback(format!("Cannot stage submodule: {}", error.message())); }
    if let Err(error) = submodule.add_to_index(true) { return rollback(format!("Cannot add the submodule link to the parent index: {}", error.message())); }
    if let Ok(mut index) = repo.index() { if let Err(error) = index.add_path(Path::new(".gitmodules")).and_then(|_| index.write()) { return rollback(format!("Cannot stage .gitmodules: {}", error.message())); } }
    invalidate_git_metadata(&repository_path);
    Ok(relative_string)
}

#[tauri::command]
pub fn switch_submodule_version(repository_path: String, relative_path: String, revision: String, version_kind: String, name: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let repo = internal_repository(absolute.to_str().unwrap_or_default())?;
    if version_kind == "branch" {
        // `name` is the actual branch name (e.g. "main"); `revision` is only the SHA
        // it currently points at and is NOT a valid ref on its own — using it here
        // produced "reference 'refs/heads/<sha>' not found" for every local branch.
        let branch_name = if name.is_empty() { revision.clone() } else { name.clone() };
        let reference = format!("refs/heads/{branch_name}"); repo.find_reference(&reference).map_err(|error| format!("Branch '{branch_name}' not found: {}", error.message()))?; repo.set_head(&reference).map_err(|error| error.message().to_string())?;
    } else {
        let object = repo.revparse_single(&revision).map_err(|error| error.message().to_string())?; repo.set_head_detached(object.id()).map_err(|error| error.message().to_string())?;
    }
    let mut checkout = git2::build::CheckoutBuilder::new(); checkout.safe(); repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    let selected = repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    drop(repo);
    let parent = internal_repository(&repository_path)?;
    let mut submodule = parent.find_submodule(&relative_path).map_err(|error| error.message().to_string())?;
    submodule.add_to_index(true).map_err(|error| format!("Version changed, but the parent index could not be updated: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    Ok(selected)
}

#[tauri::command]
pub fn change_submodule_url(repository_path: String, relative_path: String, url: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let relative = normalized(&safe_relative_path(&relative_path)?);
    let url = url.trim();
    if url.is_empty() || url.starts_with('-') { return Err("Enter a valid Git repository URL".into()); }
    let mut repo = internal_repository(&repository_path)?; let name = repo.submodules().map_err(|error| error.message().to_string())?.into_iter().find(|item| normalized(item.path()) == relative).map(|item| item.name().unwrap_or("").to_string()).ok_or("Submodule configuration was not found")?; repo.submodule_set_url(&name, url).map_err(|error| error.message().to_string())?;
    let subrepo = internal_repository(absolute.to_str().unwrap_or_default())?; subrepo.remote_set_url("origin", url).map_err(|error| error.message().to_string())?; subrepo.find_remote("origin").map_err(|error| error.message().to_string())?; git(absolute.to_str().unwrap_or_default(), &["fetch", "origin"]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn remove_git_path(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let relative = normalized(&safe_relative_path(&relative_path)?);
    if relative.is_empty() { return Err("The repository root cannot be removed".into()); }
    let (tracked, _) = cached_index_metadata(&repository_path);
    let prefix = format!("{relative}/");
    if !tracked.contains(&relative) && !tracked.iter().any(|path| path.starts_with(&prefix)) {
        return Err("This item is not tracked by Git. Remove it with the operating system if intended".into());
    }
    let repo = internal_repository(&repository_path)?;
    let submodule_name = repo.submodules().ok().and_then(|items| items.into_iter().find(|item| normalized(item.path()) == relative).map(|item| item.name().unwrap_or(&relative).to_string()));
    let target = Path::new(&repository_path).join(&relative); if target.is_dir() { fs::remove_dir_all(&target).map_err(|error| error.to_string())?; } else if target.exists() { fs::remove_file(&target).map_err(|error| error.to_string())?; }
    if let Some(name) = submodule_name {
        cleanup_submodule_registration(&repo, &name, Path::new(&relative))?;
        let modules = repo.path().join("modules").join(safe_relative_path(&name)?);
        if modules.is_dir() { fs::remove_dir_all(modules).map_err(|error| format!("Cannot remove internal submodule data: {error}"))?; }
    }
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    let paths: Vec<PathBuf> = index.iter().filter_map(|entry| {
        let path = String::from_utf8_lossy(&entry.path);
        (path == relative || path.starts_with(&prefix)).then(|| PathBuf::from(path.as_ref()))
    }).collect();
    for path in paths { index.remove_path(&path).map_err(|error| error.message().to_string())?; }
    let gitmodules = Path::new(&repository_path).join(".gitmodules");
    if gitmodules.exists() { index.add_path(Path::new(".gitmodules")).map_err(|error| error.message().to_string())?; }
    else { let _ = index.remove_path(Path::new(".gitmodules")); }
    index.write().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn delete_local_path(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    if relative_string.is_empty() { return Err("The repository root cannot be deleted".into()); }
    // Destructive checks must reflect the current index, not an older Explorer snapshot.
    let (tracked, _) = cached_index_metadata(&repository_path);
    let prefix = format!("{relative_string}/");
    if tracked.contains(&relative_string) || tracked.iter().any(|path| path.starts_with(&prefix)) {
        return Err("This item is tracked. Use Remove from Git so the deletion can be committed".into());
    }
    let repo = internal_repository(&repository_path)?;
    let registered_name = repo.submodules().ok().and_then(|items| items.into_iter()
        .find(|item| normalized(item.path()) == relative_string)
        .map(|item| item.name().unwrap_or(&relative_string).to_string()));
    let target = Path::new(&repository_path).join(&relative);
    if target.is_dir() { fs::remove_dir_all(&target).map_err(|error| format!("Cannot delete local folder: {error}"))?; }
    else if target.exists() { fs::remove_file(&target).map_err(|error| format!("Cannot delete local file: {error}"))?; }
    else if registered_name.is_none() && !repo.path().join("modules").join(&relative).exists() { return Err("The local item no longer exists".into()); }
    if let Some(name) = registered_name.as_deref() { cleanup_submodule_registration(&repo, name, &relative)?; }
    let mut storage_paths = vec![repo.path().join("modules").join(&relative)];
    if let Some(name) = registered_name { storage_paths.push(repo.path().join("modules").join(safe_relative_path(&name)?)); }
    storage_paths.sort(); storage_paths.dedup();
    for storage in storage_paths { if storage.is_dir() { fs::remove_dir_all(&storage).map_err(|error| format!("Cannot remove submodule metadata: {error}"))?; } }
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn commit_path(repository_path: String, relative_path: String, message: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    let relative = safe_relative_path(&relative_path)?;
    let pathspec = if relative.as_os_str().is_empty() { ".".to_string() } else { normalized(&relative) };
    commit_selected_internal(&repository_path, &[pathspec], message.trim())
}

#[tauri::command]
pub fn commit_files(repository_path: String, files: Vec<String>, message: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    if files.is_empty() { return Err("Select at least one file".into()); }
    let safe_files: Vec<String> = files.iter().map(|file| safe_relative_path(file.trim_end_matches(|character| character == '/' || character == '\\')).map(|path| normalized(&path))).collect::<Result<_, _>>()?;
    commit_selected_internal(&repository_path, &safe_files, message.trim())
}

fn commit_selected_internal(repository_path: &str, files: &[String], message: &str) -> Result<String, String> {
    let repo = internal_repository(repository_path)?;
    // A submodule folder can be deleted straight from disk (Finder/terminal, or a
    // failed clone) without going through this app's own removal flow, leaving it
    // still registered as a gitlink. In that case `add_to_index` would need to
    // open the submodule's own .git to read its current HEAD, which no longer
    // exists — only ask it to prepare the submodule when its working directory is
    // actually still there; otherwise this is really a deletion, handled below.
    for file in files { let absolute = Path::new(repository_path).join(file); if !absolute.exists() { continue; } if let Ok(mut submodule) = repo.find_submodule(file) { submodule.add_to_index(true).map_err(|error| format!("Cannot prepare submodule {file} for commit: {}", error.message()))?; } }
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok()); let parent_tree = parent.as_ref().and_then(|commit| commit.tree().ok()); let mut index = repo.index().map_err(|error| error.message().to_string())?;
    let gitlinks: HashMap<String, git2::IndexEntry> = index.iter().filter(|entry| entry.mode == 0o160000).map(|entry| (String::from_utf8_lossy(&entry.path).into_owned(), entry)).collect();
    if let Some(tree) = parent_tree.as_ref() { index.read_tree(tree).map_err(|error| error.message().to_string())?; } else { index.clear().map_err(|error| error.message().to_string())?; }
    let mut includes_submodule = false;
    for file in files {
        let path = Path::new(file); let absolute = Path::new(repository_path).join(path);
        // Only reuse the existing gitlink entry unchanged when the submodule is
        // still present on disk — if it was deleted, fall through to the normal
        // add/remove handling below so the deletion actually gets committed.
        if let Some(entry) = gitlinks.get(file) { if absolute.exists() { index.add(entry).map_err(|error| error.message().to_string())?; includes_submodule = true; continue; } }
        if absolute.exists() { index.add_all([path], git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?; } else { let _ = index.remove_all([path], None); }
    }
    if includes_submodule && Path::new(repository_path).join(".gitmodules").exists() { index.add_path(Path::new(".gitmodules")).map_err(|error| error.message().to_string())?; }
    let tree_id = index.write_tree_to(&repo).map_err(|error| error.message().to_string())?; if parent_tree.as_ref().map(|tree| tree.id()) == Some(tree_id) { return Err("There are no changes to commit in the selected files".into()); } let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?; let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?; let parents: Vec<&git2::Commit<'_>> = parent.iter().collect(); let oid = repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &parents).map_err(|error| error.message().to_string())?;
    // `index` above was repurposed as an in-memory scratch copy (parent tree plus
    // only the selected files) to build the commit tree, and `repo.index()` returns
    // that same cached instance rather than a fresh read — so it must not be
    // written back to .git/index as-is, or every other pending file not part of
    // this (possibly scoped/partial) commit would silently lose its staged status.
    // Force-reload the real on-disk index first, then sync just the committed
    // files into it so they stop showing as staged, leaving every other entry
    // (which was never touched on disk) untouched.
    index.read(true).map_err(|error| error.message().to_string())?;
    for file in files {
        let relative = Path::new(file); let absolute = Path::new(repository_path).join(relative);
        if gitlinks.contains_key(file) && absolute.exists() { if let Ok(mut submodule) = repo.find_submodule(file) { let _ = submodule.add_to_index(true); } continue; }
        if absolute.exists() { index.add_path(relative).map_err(|error| error.message().to_string())?; } else { let _ = index.remove_path(relative); }
    }
    index.write().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(repository_path); Ok(oid.to_string())
}

#[tauri::command]
pub fn restore_file(repository_path: String, relative_path: String, source_ref: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if let Some((sub_path, inner_relative)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        return restore_file(sub_path, inner_relative, source_ref);
    }
    let relative = safe_relative_path(&relative_path)?; if relative.as_os_str().is_empty() { return Err("Select a file".into()); }
    let repo = internal_repository(&repository_path)?;
    let object = repo.revparse_single(source_ref.trim()).or_else(|_| repo.revparse_single(&format!("refs/remotes/{}", source_ref.trim()))).map_err(|error| format!("Cannot resolve {source_ref}: {}", error.message()))?;
    let commit = object.peel_to_commit().map_err(|error| error.message().to_string())?; let tree = commit.tree().map_err(|error| error.message().to_string())?;
    let entry = tree.get_path(&relative).map_err(|_| format!("{} does not exist in {source_ref}", normalized(&relative)))?; let blob = repo.find_blob(entry.id()).map_err(|error| error.message().to_string())?;
    let destination = Path::new(&repository_path).join(&relative);
    if let Some(parent) = destination.parent() { fs::create_dir_all(parent).map_err(|error| error.to_string())?; }
    fs::write(destination, blob.content()).map_err(|error| error.to_string())?;
    if source_ref.trim() == "HEAD" {
        repo.reset_default(Some(&object), [relative.as_path()]).map_err(|error| format!("File restored, but staging could not be reset: {}", error.message()))?;
    }
    invalidate_git_metadata(&repository_path); Ok(())
}

#[tauri::command]
pub fn restore_remote_file(repository_path: String, relative_path: String, remote_ref: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if let Some((sub_path, inner_relative)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        let sub_remote_ref = default_remote_ref(&sub_path).ok_or("This submodule has no remote-tracking branch. Fetch the submodule first.")?;
        return restore_remote_file(sub_path, inner_relative, sub_remote_ref);
    }
    let remote_ref = remote_ref.trim();
    let (remote, _) = remote_ref.split_once('/').ok_or("Choose a remote branch such as origin/main")?;
    let repo = internal_repository(&repository_path)?; repo.find_remote(remote).map_err(|_| "The selected remote is not configured".to_string())?;
    git(&repository_path, &["fetch", remote]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    restore_file(repository_path, relative_path, remote_ref.to_string())
}

// entry_details only needs the single most recent commit that touched a path (for
// its "Last Commit" section) — it used to get this via `path_history(...).next()`,
// which computed the *entire* matching history (walking up to 500 commits, diffing
// each one) just to throw away everything after the first result. This walks the
// same way but stops the instant a match is found, which is the overwhelmingly
// common case (most viewed files were touched somewhat recently) and was, on a
// large/long-lived repository, one of the biggest remaining sources of the
// "selecting anything is slow, and it gets worse the deeper you navigate" feeling
// — every single click paid for a full history walk regardless of depth.
fn last_commit_touching_path(repository_path: &str, relative: &Path) -> Option<(String, String, String, String)> {
    let repo = internal_repository(repository_path).ok()?;
    let mut walk = repo.revwalk().ok()?; walk.push_head().ok()?; let _ = walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME);
    for oid in walk.flatten().take(2000) {
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let matches = if relative.as_os_str().is_empty() { true } else {
            let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
            let mut options = git2::DiffOptions::new(); options.pathspec(relative);
            commit.tree().ok().and_then(|tree| repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options)).ok()).map(|diff| diff.deltas().next().is_some()).unwrap_or(false)
        };
        if matches {
            return Some((oid.to_string(), commit.summary().unwrap_or("No message").to_string(), commit.author().name().unwrap_or("Unknown").to_string(), short_date(commit.time().seconds())));
        }
    }
    None
}

#[tauri::command]
pub fn path_history(repository_path: String, relative_path: String) -> Result<Vec<Commit>, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let repo = internal_repository(&repository_path)?; let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.push_head().map_err(|error| error.message().to_string())?; let _ = walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME); let mut commits = Vec::new();
    for oid in walk.flatten().take(500) { if let Ok(commit) = repo.find_commit(oid) { let include = if relative.as_os_str().is_empty() { true } else { let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok()); let mut options = git2::DiffOptions::new(); options.pathspec(&relative); if let Ok(tree) = commit.tree() { repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options)).map(|diff| diff.deltas().next().is_some()).unwrap_or(false) } else { false } }; if include { commits.push(Commit { id: oid.to_string(), parents: commit.parent_ids().map(|id| id.to_string()).collect(), subject: commit.summary().unwrap_or("No message").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()), refs: Vec::new(), lane: 0 }); } } }
    Ok(commits)
}

fn resolve_commit(repository: &str, reference: &str) -> Result<String, String> {
    let repo = internal_repository(repository)?; repo.revparse_single(reference).or_else(|_| repo.revparse_single(&format!("refs/remotes/{reference}"))).and_then(|object| object.peel(ObjectType::Commit)).map(|object| object.id().to_string()).map_err(|error| error.message().to_string())
}

fn remote_directory_entries(repository: &str, commit: &str, relative_path: &str) -> Result<HashMap<String, CommanderEntry>, String> {
    let repo = internal_repository(repository)?; let oid = git2::Oid::from_str(commit).map_err(|error| error.message().to_string())?; let commit = repo.find_commit(oid).map_err(|error| error.message().to_string())?; let root = commit.tree().map_err(|error| error.message().to_string())?; let tree = if relative_path.is_empty() { root } else { let entry = match root.get_path(Path::new(relative_path)) { Ok(entry) => entry, Err(_) => return Ok(HashMap::new()) }; repo.find_tree(entry.id()).map_err(|error| error.message().to_string())? };
    let mut entries = HashMap::new(); for entry in tree.iter() { let name = entry.name().unwrap_or("").to_string(); let path = if relative_path.is_empty() { name.clone() } else { format!("{relative_path}/{name}") }; let kind = if entry.filemode() == 0o160000 { "submodule" } else if entry.kind() == Some(ObjectType::Tree) { "folder" } else { "file" }; let size = if entry.kind() == Some(ObjectType::Blob) { repo.find_blob(entry.id()).map(|blob| blob.size() as u64).unwrap_or(0) } else { 0 }; entries.insert(name.clone(), CommanderEntry { name, relative_path: path, kind: kind.into(), size }); }
    Ok(entries)
}

fn changed_paths_against(repository: &str, commit: &str, relative_path: &str) -> HashSet<String> {
    let Ok(repo) = internal_repository(repository) else { return HashSet::new() }; let Ok(oid) = git2::Oid::from_str(commit) else { return HashSet::new() }; let Ok(tree) = repo.find_commit(oid).and_then(|commit| commit.tree()) else { return HashSet::new() }; let mut options = git2::DiffOptions::new(); if !relative_path.is_empty() { options.pathspec(relative_path); } let Ok(diff) = repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut options)) else { return HashSet::new() }; diff.deltas().filter_map(|delta| delta.new_file().path().or_else(|| delta.old_file().path()).map(normalized)).collect()
}

// The first remote-tracking branch found for a repository, used as a fallback
// comparison target when browsing crosses into a submodule (whose own remote is
// independent of whatever remote branch the parent project happens to be comparing
// against).
fn default_remote_ref(repository: &str) -> Option<String> {
    let repo = internal_repository(repository).ok()?;
    let iter = repo.branches(Some(BranchType::Remote)).ok()?;
    for item in iter.flatten() {
        if let Some(name) = item.0.name().ok().flatten() {
            if !name.ends_with("/HEAD") { return Some(name.to_string()); }
        }
    }
    None
}

// A parent repository's tree only records a single gitlink entry for an entire
// submodule — it has no knowledge of files *inside* it. Browsing or comparing a
// path inside a submodule must therefore be redirected to that submodule's own
// repository and its own remote, or every file in it would incorrectly and
// permanently show as "local-only" regardless of whether it was ever pushed.
fn resolve_submodule_boundary(repository_path: &str, relative_path: &str) -> Option<(String, String)> {
    let (_, submodules) = cached_index_metadata(repository_path);
    let submodule_path = submodules.iter().find(|sub| relative_path == sub.as_str() || relative_path.starts_with(&format!("{sub}/")))?.clone();
    let absolute_sub = Path::new(repository_path).join(&submodule_path);
    let sub_path_string = absolute_sub.to_string_lossy().into_owned();
    let inner_relative = if relative_path == submodule_path { String::new() } else { relative_path[submodule_path.len() + 1..].to_string() };
    Some((sub_path_string, inner_relative))
}

#[tauri::command]
pub fn compare_remote_directory(repository_path: String, relative_path: String, remote_ref: String) -> Result<CommanderDirectory, String> {
    validate_path(&repository_path)?;
    if let Some((sub_path, inner_relative)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        let sub_remote_ref = default_remote_ref(&sub_path).ok_or("This submodule has no remote-tracking branch. Fetch the submodule first.")?;
        return compare_remote_directory(sub_path, inner_relative, sub_remote_ref);
    }
    let relative = safe_relative_path(&relative_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    if absolute.exists() && !absolute.is_dir() { return Err("The local path is not a folder".into()); }
    let commit = resolve_commit(&repository_path, &remote_ref)?;
    let mut remote = remote_directory_entries(&repository_path, &commit, &relative_path)?;
    let changed = changed_paths_against(&repository_path, &commit, &relative_path);
    let git_metadata = cached_git_metadata(&repository_path, &relative_path);
    let mut rows = Vec::new();

    let local_items = if absolute.is_dir() {
        fs::read_dir(&absolute).map_err(|error| error.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };
    for item in local_items {
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" { continue; }
        let path = if relative_path.is_empty() { name.clone() } else { format!("{relative_path}/{name}") };
        let metadata = fs::symlink_metadata(item.path()).map_err(|error| error.to_string())?;
        let kind = if git_metadata.submodules.contains(&path) { "submodule" } else if metadata.is_dir() { "folder" } else { "file" };
        let local = CommanderEntry { name: name.clone(), relative_path: path.clone(), kind: kind.into(), size: if metadata.is_file() { metadata.len() } else { 0 } };
        let remote_entry = remote.remove(&name);
        let prefix = format!("{path}/");
        let has_changes = changed.iter().any(|changed_path| changed_path == &path || changed_path.starts_with(&prefix));
        let status = match &remote_entry {
            None => "local-only",
            Some(remote_entry) if remote_entry.kind != local.kind => "type-changed",
            Some(_) if has_changes => "modified",
            Some(_) => "same",
        }.to_string();
        rows.push(CommanderRow { name, relative_path: path, local: Some(local), remote: remote_entry, status });
    }
    for (name, remote_entry) in remote {
        rows.push(CommanderRow { relative_path: remote_entry.relative_path.clone(), name, local: None, remote: Some(remote_entry), status: "remote-only".into() });
    }
    rows.sort_by(|a, b| {
        let a_folder = a.local.as_ref().or(a.remote.as_ref()).map(|entry| entry.kind.as_str() == "folder").unwrap_or(false);
        let b_folder = b.local.as_ref().or(b.remote.as_ref()).map(|entry| entry.kind.as_str() == "folder").unwrap_or(false);
        b_folder.cmp(&a_folder).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(CommanderDirectory { remote_ref, remote_revision: commit, relative_path, rows })
}

#[tauri::command]
pub fn compare_file_contents(repository_path: String, relative_path: String, remote_ref: String) -> Result<FileComparison, String> {
    validate_path(&repository_path)?;
    if let Some((sub_path, inner_relative)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        let sub_remote_ref = default_remote_ref(&sub_path).ok_or("This submodule has no remote-tracking branch. Fetch the submodule first.")?;
        return compare_file_contents(sub_path, inner_relative, sub_remote_ref);
    }
    let relative = safe_relative_path(&relative_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    // The file may exist only on the remote (not yet fetched locally), so a missing
    // local file is a valid state here, not an error — it just renders as empty/absent.
    let local = match fs::read(&absolute) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("Cannot read local file: {error}")),
    };
    if local.len() > 1_000_000 || local.contains(&0) { return Err("Binary files and files over 1 MB are not shown in the text compare view".into()); }
    let commit = resolve_commit(&repository_path, &remote_ref)?; let repo = internal_repository(&repository_path)?; let oid = git2::Oid::from_str(&commit).map_err(|error| error.message().to_string())?; let tree = repo.find_commit(oid).and_then(|commit| commit.tree()).map_err(|error| error.message().to_string())?; let entry = tree.get_path(&relative).map_err(|_| "The file does not exist in the selected remote revision".to_string())?; let remote = repo.find_blob(entry.id()).map_err(|error| error.message().to_string())?.content().to_vec();
    if remote.len() > 1_000_000 || remote.contains(&0) { return Err("Binary files and files over 1 MB are not shown in the text compare view".into()); }
    Ok(FileComparison { relative_path, remote_ref, local_content: String::from_utf8_lossy(&local).into_owned(), remote_content: String::from_utf8_lossy(&remote).into_owned() })
}

#[tauri::command]
pub fn stash_changes(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let mut repo = internal_repository(&repository_path)?;
    let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?;
    repo.stash_save2(&signature, None, Some(git2::StashFlags::INCLUDE_UNTRACKED)).map_err(|error| format!("Cannot stash changes: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn pop_stash(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let mut repo = internal_repository(&repository_path)?;
    let mut options = git2::StashApplyOptions::new();
    repo.stash_pop(0, Some(&mut options)).map_err(|error| format!("Cannot restore stashed work: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn rename_branch(repository_path: String, old_name: String, new_name: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if new_name.trim().is_empty() { return Err("Branch name cannot be empty".into()); }
    let repo = internal_repository(&repository_path)?;
    let mut branch = repo.find_branch(old_name.trim(), BranchType::Local).map_err(|error| error.message().to_string())?;
    branch.rename(new_name.trim(), false).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn delete_branch(repository_path: String, branch_name: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let current = repo.head().ok().and_then(|head| head.shorthand().map(String::from));
    if current.as_deref() == Some(branch_name.trim()) { return Err("Cannot delete the currently checked out branch. Switch to another branch first".into()); }
    let mut branch = repo.find_branch(branch_name.trim(), BranchType::Local).map_err(|error| error.message().to_string())?;
    branch.delete().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn commit_submodule(repository_path: String, relative_path: String, message: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let repo = internal_repository(&sub_path)?;
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    index.add_all(["*"], git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?;
    index.write().map_err(|error| error.message().to_string())?;
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parent_tree_id = parent.as_ref().and_then(|commit| commit.tree().ok()).map(|tree| tree.id());
    let tree_id = index.write_tree().map_err(|error| error.message().to_string())?;
    if parent_tree_id == Some(tree_id) { return Err("There are no changes to commit in this submodule".into()); }
    let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?;
    let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this submodule".to_string())?;
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    let oid = repo.commit(Some("HEAD"), &signature, &signature, message.trim(), &tree, &parents).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&sub_path);
    Ok(oid.to_string())
}

#[derive(Serialize, Debug)]
pub struct PushSubmoduleResult { revision: String, branch: String }

#[tauri::command]
pub fn push_submodule(repository_path: String, relative_path: String) -> Result<PushSubmoduleResult, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let repo = internal_repository(&sub_path)?;
    let local_target = repo.head().ok().and_then(|head| head.target());

    // Warn explicitly about uncommitted edits before pushing — otherwise a push can
    // silently "succeed" while leaving the user's freshest work off the server, with
    // no indication anything was left behind.
    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() {
        let files: Vec<String> = dirty.iter().take(5).map(|(path, status, _)| format!("{status} {path}")).collect();
        let more = if dirty.len() > 5 { format!(" (+{} more)", dirty.len() - 5) } else { String::new() };
        return Err(format!("This submodule has uncommitted changes that will NOT be pushed:\n{}{more}\n\nCommit them first, then push.", files.join("\n")));
    }

    repo.find_remote("origin").map_err(|_| "No 'origin' remote configured for this submodule".to_string())?;
    // Use the system `git` binary (not libgit2) for network operations here: it
    // transparently reuses the user's already-working SSH agent, credential helper,
    // and OS keychain, instead of libgit2's much narrower built-in credential search
    // — which is what produced "failed to acquire username/password" even though a
    // plain `git push` in a terminal works fine for the same repository.
    let _ = git(&sub_path, &["fetch", "origin"]);

    // Submodules are very commonly checked out in detached HEAD (git's normal state
    // after `git submodule update`/clone) — libgit2's `shorthand()` misleadingly
    // returns the literal string "HEAD" for a detached HEAD instead of `None`, which
    // previously let a bogus "HEAD" branch name slip through and reach `git push` as
    // an unqualified ref, producing "not a full refname". Resolve a real destination
    // branch instead: the checked-out branch if there is one, else the branch recorded
    // in .gitmodules, else the remote's default branch.
    let branch = if !repo.head_detached().unwrap_or(true) {
        repo.head().ok().and_then(|head| head.shorthand().map(String::from))
    } else { None }
        .or_else(|| submodule_value(&repository_path, &relative_path, "branch"))
        .or_else(|| git(&sub_path, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).ok().map(|value| value.trim().trim_start_matches("origin/").to_string()).filter(|value| !value.is_empty()));
    let branch = match branch {
        Some(value) if !value.trim().is_empty() => value,
        _ => return Err("This submodule is in detached HEAD (not on a branch) and no default branch could be determined. Use \"Change version\" to switch to a branch first, then push.".into()),
    };

    let remote_target = repo.find_reference(&format!("refs/remotes/origin/{branch}")).ok().and_then(|reference| reference.target());
    if remote_target.is_some() && remote_target == local_target {
        return Err(format!("Nothing to push — this submodule has no commits ahead of origin/{branch}. Commit your changes in the submodule first."));
    }
    git(&sub_path, &["push", "origin", &format!("HEAD:refs/heads/{branch}")]).map_err(|detail| {
        if detail.contains("non-fast-forward") || detail.contains("[rejected]") || detail.contains("fetch first") {
            format!("Push rejected — origin/{branch} has commits you don't have locally (someone else pushed there, or it moved since the last fetch). Fetch the submodule, review/merge the new commits, then push again — or, if you're the only one using this remote, use \"Force push submodule\" to overwrite it.\n\nGit's message: {detail}")
        } else { format!("Push failed: {detail}") }
    })?;

    record_pushed_submodule_in_parent(&repository_path, &relative_path, local_target)?;
    Ok(PushSubmoduleResult { revision: local_target.map(|oid| oid.to_string()).unwrap_or_default(), branch })
}

// Once a commit is safely on the submodule's own server, it is no longer "only
// local" — automatically record that new commit in the parent project too, so the
// submodule stops showing as modified. This mirrors clicking "Commit this item" on
// the submodule, done here for you right after a successful push.
fn record_pushed_submodule_in_parent(repository_path: &str, relative_path: &str, local_target: Option<git2::Oid>) -> Result<(), String> {
    let parent = internal_repository(repository_path)?;
    let mut submodule = parent.find_submodule(relative_path).map_err(|error| format!("Pushed, but could not update the parent's reference: {}", error.message()))?;
    submodule.add_to_index(true).map_err(|error| format!("Pushed, but could not stage the updated submodule reference in the parent: {}", error.message()))?;
    let short_sha = local_target.map(|oid| oid.to_string()[..8.min(oid.to_string().len())].to_string()).unwrap_or_default();
    match commit_selected_internal(repository_path, &[normalized(Path::new(relative_path))], &format!("Update submodule {relative_path} to {short_sha}")) {
        Ok(_) => {}
        // "nothing to commit" happens if the parent's index already matched (e.g. it
        // was committed by hand right before pushing) — not an error worth surfacing.
        Err(message) if message.contains("no changes to commit") => {}
        Err(message) => return Err(format!("Pushed successfully, but could not update the parent project: {message}")),
    }
    invalidate_git_metadata(repository_path);
    Ok(())
}

#[tauri::command]
pub fn force_push_submodule(repository_path: String, relative_path: String) -> Result<PushSubmoduleResult, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let repo = internal_repository(&sub_path)?;
    let local_target = repo.head().ok().and_then(|head| head.target());

    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() {
        let files: Vec<String> = dirty.iter().take(5).map(|(path, status, _)| format!("{status} {path}")).collect();
        let more = if dirty.len() > 5 { format!(" (+{} more)", dirty.len() - 5) } else { String::new() };
        return Err(format!("This submodule has uncommitted changes that will NOT be pushed:\n{}{more}\n\nCommit them first, then push.", files.join("\n")));
    }
    if repo.head_detached().unwrap_or(true) {
        return Err("This submodule is in detached HEAD (not on a branch). Use \"Change version\" to switch to a branch first, then push.".into());
    }
    let branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).ok_or("Could not determine the current branch")?;

    repo.find_remote("origin").map_err(|_| "No 'origin' remote configured for this submodule".to_string())?;
    // --force: intentionally overwrites whatever commit origin/<branch> currently
    // points at, discarding any commits there aren't in this local history. Only
    // safe when nobody else's work lives on that remote branch — the frontend
    // requires an explicit, separate confirmation before calling this.
    git(&sub_path, &["push", "--force", "origin", &format!("HEAD:refs/heads/{branch}")]).map_err(|detail| format!("Force push failed: {detail}"))?;

    record_pushed_submodule_in_parent(&repository_path, &relative_path, local_target)?;
    Ok(PushSubmoduleResult { revision: local_target.map(|oid| oid.to_string()).unwrap_or_default(), branch })
}

#[tauri::command]
pub fn fetch_submodule(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let repo = internal_repository(&sub_path)?;
    repo.find_remote("origin").map_err(|_| "No 'origin' remote configured for this submodule".to_string())?;
    git(&sub_path, &["fetch", "origin"])?;
    invalidate_git_metadata(&sub_path);
    Ok(())
}

#[tauri::command]
pub fn pull_submodule(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let repo = internal_repository(&sub_path)?;

    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() {
        return Err("This submodule has uncommitted changes. Commit or discard them before pulling, so a fast-forward can't overwrite anything.".into());
    }
    if repo.head_detached().unwrap_or(true) {
        return Err("This submodule is in detached HEAD (not on a branch), so there is nothing to pull into. Use \"Change version\" to switch to a branch first.".into());
    }
    let branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).ok_or("Could not determine the current branch")?;

    repo.find_remote("origin").map_err(|_| "No 'origin' remote configured for this submodule".to_string())?;
    git(&sub_path, &["fetch", "origin"])?;

    let remote_ref = repo.find_reference(&format!("refs/remotes/origin/{branch}")).map_err(|error| format!("origin/{branch} not found after fetch: {}", error.message()))?;
    let target = remote_ref.target().ok_or("origin's branch has no commits")?;
    let annotated = repo.find_annotated_commit(target).map_err(|error| error.message().to_string())?;
    let (analysis, _) = repo.merge_analysis(&[&annotated]).map_err(|error| error.message().to_string())?;
    if analysis.is_up_to_date() { return Err(format!("Already up to date with origin/{branch}.")); }
    if !analysis.is_fast_forward() {
        return Err(format!("Cannot fast-forward — your local commit(s) and origin/{branch} have diverged (both have commits the other doesn't). This needs a manual merge or rebase in a terminal inside the submodule folder; it can't be done safely from here."));
    }
    let mut local = repo.find_reference(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    local.set_target(target, "fast-forward pull").map_err(|error| error.message().to_string())?;
    repo.set_head(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    // `.safe()` alone can silently skip files it mistakenly believes are locally
    // modified (a stat/mtime-cache false positive in libgit2, not a real conflict) —
    // we already verified the working tree is clean above, so force() is safe here
    // and guarantees the checkout actually lands instead of silently no-op'ing.
    let mut checkout = git2::build::CheckoutBuilder::new(); checkout.force();
    repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&sub_path);
    Ok(())
}

#[derive(Serialize, Clone)]
pub struct BlameLine { author: String, date: String, message: String }

#[derive(Serialize)]
pub struct FileBlame { lines: Vec<BlameLine> }

#[tauri::command]
pub fn file_blame(repository_path: String, relative_path: String) -> Result<FileBlame, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let repo = internal_repository(&repository_path)?;
    let content = fs::read_to_string(Path::new(&repository_path).join(&relative)).unwrap_or_default();
    let line_count = content.lines().count();
    let empty = BlameLine { author: String::new(), date: String::new(), message: String::new() };
    let mut lines: Vec<BlameLine> = vec![empty; line_count];
    let blame = repo.blame_file(&relative, None).map_err(|error| error.message().to_string())?;
    for hunk in blame.iter() {
        if let Ok(commit) = repo.find_commit(hunk.final_commit_id()) {
            let author = commit.author().name().unwrap_or("Unknown").to_string();
            let date = short_date(commit.time().seconds());
            let message = commit.summary().unwrap_or("").to_string();
            let start = hunk.final_start_line();
            let count = hunk.lines_in_hunk();
            for offset in 0..count {
                if start == 0 { continue; }
                let idx = start - 1 + offset;
                if idx < lines.len() { lines[idx] = BlameLine { author: author.clone(), date: date.clone(), message: message.clone() }; }
            }
        }
    }
    Ok(FileBlame { lines })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn run_git(path: &Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(path).args(args).status().unwrap();
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn create_libgit2_repository(path: &Path, file: &str) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join(file), "content").unwrap();
        let repo = Repository::init(path).unwrap();
        let mut index = repo.index().unwrap(); index.add_path(Path::new(file)).unwrap(); index.write().unwrap();
        let tree_id = index.write_tree().unwrap(); let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "Initial commit", &tree, &[]).unwrap();
    }

    #[test]
    fn submodule_add_switch_and_full_removal_are_internal() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        fs::create_dir_all(parent.join("components")).unwrap();
        let parent_string = parent.to_string_lossy().into_owned();
        fs::create_dir_all(parent.join("failed")).unwrap(); fs::create_dir_all(parent.join(".git/modules/failed")).unwrap();
        delete_local_path(parent_string.clone(), "failed".into()).unwrap();
        assert!(!parent.join("failed").exists()); assert!(!parent.join(".git/modules/failed").exists());
        fs::write(parent.join("README.md"), "changed").unwrap(); stage_files(parent_string.clone(), vec!["README.md".into()]).unwrap();
        restore_file(parent_string.clone(), "README.md".into(), "HEAD".into()).unwrap();
        assert!(!load_repository(parent_string.clone()).unwrap().changes.iter().any(|change| change.path == "README.md"));
        let added = add_submodule(parent_string.clone(), "components".into(), dependency.to_string_lossy().into_owned(), "engine".into(), String::new(), String::new()).unwrap();
        assert_eq!(added, "components/engine");
        assert!(parent.join(".gitmodules").exists());
        let repo = Repository::open(&parent).unwrap();
        assert_eq!(repo.index().unwrap().get_path(Path::new("components/engine"), 0).unwrap().mode, 0o160000);
        drop(repo);
        create_commit(parent_string.clone(), "P:89312 add engine".into()).unwrap();
        assert_eq!(entry_details(parent_string.clone(), "README.md".into()).unwrap().last_commit_subject.as_deref(), Some("Initial commit"));
        assert_eq!(entry_details(parent_string.clone(), "components/engine".into()).unwrap().last_commit_subject.as_deref(), Some("P:89312 add engine"));
        let versions = submodule_versions(parent_string.clone(), added.clone()).unwrap();
        switch_submodule_version(parent_string.clone(), added.clone(), versions.current_revision, "commit".into(), String::new()).unwrap();
        remove_git_path(parent_string, added).unwrap();
        assert!(!parent.join("components/engine").exists());
        assert!(!parent.join(".gitmodules").exists());
        let repo = Repository::open(&parent).unwrap();
        assert!(repo.index().unwrap().get_path(Path::new("components/engine"), 0).is_none());
        assert!(!repo.path().join("modules/components/engine").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn explorer_reads_files_folders_and_submodules() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::create_dir_all(&dependency).unwrap();
        fs::write(repository.join("src/main.c"), "int main(void) { return 0; }").unwrap();
        fs::write(dependency.join("README.md"), "dependency").unwrap();

        for path in [&repository, &dependency] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial commit"]);
        }
        run_git(&dependency, &["switch", "-c", "release/2.4"]);
        fs::write(dependency.join("README.md"), "release version").unwrap();
        run_git(&dependency, &["commit", "-am", "Release version"]);
        run_git(&dependency, &["switch", "main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dependency"]);
        run_git(&repository, &["commit", "-am", "Add dependency"]);

        let path = repository.to_string_lossy().into_owned();
        let entries = load_directory(path.clone(), "".into()).unwrap();
        assert!(entries.iter().any(|entry| entry.relative_path == "src" && entry.kind == "folder"));
        assert!(entries.iter().any(|entry| entry.relative_path == "vendor" && entry.kind == "folder"));
        let cached_start = std::time::Instant::now();
        for _ in 0..100 { assert!(!load_directory(path.clone(), "".into()).unwrap().is_empty()); }
        assert!(cached_start.elapsed().as_millis() < 1000, "cached navigation took {:?}", cached_start.elapsed());
        let nested = load_directory(path.clone(), "vendor".into()).unwrap();
        assert!(nested.iter().any(|entry| entry.relative_path == "vendor/dependency" && entry.kind == "submodule"));
        let details = entry_details(path, "vendor/dependency".into()).unwrap();
        assert_eq!(details.kind, "submodule");
        assert!(details.submodule_url.as_deref().unwrap_or_default().contains("dependency"));
        let versions = submodule_versions(repository.to_string_lossy().into_owned(), "vendor/dependency".into()).unwrap();
        let release = versions.versions.iter().find(|version| version.name.ends_with("release/2.4")).unwrap();
        let switched = switch_submodule_version(repository.to_string_lossy().into_owned(), "vendor/dependency".into(), release.revision.clone(), release.kind.clone(), release.name.clone()).unwrap();
        assert_eq!(switched, release.revision);
        assert!(worktree_status(repository.to_str().unwrap(), None).iter().any(|(path, _)| path == "vendor/dependency"));

        fs::write(repository.join("src/main.c"), "int main(void) { return 1; }").unwrap();
        fs::write(repository.join("unrelated.txt"), "keep staged").unwrap();
        run_git(&repository, &["add", "unrelated.txt"]);
        let committed = commit_path(repository.to_string_lossy().into_owned(), "src/main.c".into(), "Commit only main.c".into()).unwrap();
        assert!(!committed.is_empty());
        let head_files = git(repository.to_str().unwrap(), &["show", "--pretty=format:", "--name-only", "HEAD"]).unwrap();
        assert!(head_files.lines().any(|path| path == "src/main.c"));
        assert!(!head_files.lines().any(|path| path == "unrelated.txt"));
        let still_staged = git(repository.to_str().unwrap(), &["diff", "--cached", "--name-only"]).unwrap();
        assert!(still_staged.lines().any(|path| path == "unrelated.txt"));
        assert!(!path_history(repository.to_string_lossy().into_owned(), "src/main.c".into()).unwrap().is_empty());
        let commander = compare_remote_directory(repository.to_string_lossy().into_owned(), "".into(), "HEAD~1".into()).unwrap();
        assert!(commander.rows.iter().any(|row| row.relative_path == "src" && row.status == "modified"));
        assert!(commander.rows.iter().any(|row| row.relative_path == "unrelated.txt" && row.status == "local-only"));
        let comparison = compare_file_contents(repository.to_string_lossy().into_owned(), "src/main.c".into(), "HEAD~1".into()).unwrap();
        assert_ne!(comparison.local_content, comparison.remote_content);
        let editable = read_text_file(repository.to_string_lossy().into_owned(), "src/main.c".into()).unwrap();
        assert!(editable.content.contains("main"));
        write_text_file(repository.to_string_lossy().into_owned(), "src/main.c".into(), "int main(void) { return 2; }".into()).unwrap();
        assert!(read_text_file(repository.to_string_lossy().into_owned(), "src/main.c".into()).unwrap().content.contains("return 2"));
        restore_file(repository.to_string_lossy().into_owned(), "src/main.c".into(), "HEAD".into()).unwrap();
        assert!(!read_text_file(repository.to_string_lossy().into_owned(), "src/main.c".into()).unwrap().content.contains("return 2"));
        run_git(&repository, &["remote", "add", "origin", repository.to_str().unwrap()]);
        run_git(&repository, &["fetch", "origin"]);
        fs::write(repository.join("src/main.c"), "int main(void) { return 3; }").unwrap();
        restore_remote_file(repository.to_string_lossy().into_owned(), "src/main.c".into(), "origin/main".into()).unwrap();
        assert!(!read_text_file(repository.to_string_lossy().into_owned(), "src/main.c".into()).unwrap().content.contains("return 3"));
        assert_eq!(submodule_repository(repository.to_string_lossy().into_owned(), "vendor/dependency".into()).unwrap().repository.name, "dependency");
        assert_eq!(list_remotes(repository.to_string_lossy().into_owned()).unwrap().len(), 1);
        let cloned = clone_repository(dependency.to_string_lossy().into_owned(), base.to_string_lossy().into_owned(), "cloned-dependency".into()).unwrap();
        assert_eq!(load_repository(cloned.clone()).unwrap().repository.name, "cloned-dependency");
        remove_git_path(cloned.clone(), "README.md".into()).unwrap();
        assert!(!Path::new(&cloned).join("README.md").exists());
        assert!(git(&cloned, &["diff", "--cached", "--name-only"]).unwrap().lines().any(|path| path == "README.md"));
        assert_eq!(browser_repository_url("git@github.com:team/project.git").as_deref(), Some("https://github.com/team/project"));
        assert_eq!(browser_repository_url("https://gitlab.example/team/project.git").as_deref(), Some("https://gitlab.example/team/project"));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn committing_a_manually_deleted_submodule_folder_stages_the_removal() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-deleted-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "test".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add test submodule".into()).unwrap();

        // Simulate the user deleting the submodule folder outside the app (Finder/
        // terminal) instead of using the app's own removal flow — the working
        // directory (and its .git) is gone, but the gitlink is still registered.
        fs::remove_dir_all(parent.join("test")).unwrap();
        assert!(!parent.join("test").exists());

        let result = commit_files(parent_string.clone(), vec!["test".into()], "Remove deleted submodule".into());
        assert!(result.is_ok(), "expected the deletion to commit cleanly, got: {:?}", result);
        let repo = Repository::open(&parent).unwrap();
        assert!(repo.index().unwrap().get_path(Path::new("test"), 0).is_none(), "the gitlink entry should be gone from the index after committing the deletion");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn stash_does_not_pollute_the_commit_graph() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-graph-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let base = git(repository.to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();

        fs::write(repository.join("a.txt"), "two").unwrap();
        fs::write(repository.join("untracked.txt"), "new").unwrap();
        let path = repository.to_string_lossy().into_owned();
        stash_changes(path.clone()).unwrap();

        let data = load_repository(path.clone()).unwrap();
        assert!(!data.commits.iter().any(|commit| commit.parents.len() > 1), "the WIP stash commit (with its index/untracked parents) must never appear as a graph commit");
        assert!(!data.commits.iter().any(|commit| commit.refs.iter().any(|r| r == "stash")), "refs/stash must not be attached as a label on any commit");
        assert_eq!(data.stashes.len(), 1);
        assert_eq!(data.stashes[0].base_commit, base);

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn committing_all_staged_files_stops_them_showing_as_staged() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-commit-clears-staged-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("intro.css"), "body{}").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);

        fs::write(repository.join("intro.css"), "body{color:red}").unwrap();
        let path = repository.to_string_lossy().into_owned();
        stage_files(path.clone(), vec!["intro.css".into()]).unwrap();
        assert!(load_repository(path.clone()).unwrap().changes.iter().any(|change| change.path == "intro.css" && change.staged));

        commit_files(path.clone(), vec!["intro.css".into()], "Update intro.css".into()).unwrap();
        assert!(!load_repository(path.clone()).unwrap().changes.iter().any(|change| change.path == "intro.css"), "intro.css should no longer appear as a pending change right after commit");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn a_wholly_new_untracked_folder_is_still_flagged_without_full_recursion() {
        // internal_statuses no longer recurses into brand-new untracked directories
        // (a performance fix for large repos) — this verifies that a new folder full
        // of files still shows up as untracked at the folder level, which is all the
        // Explorer UI needs.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-untracked-folder-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        fs::create_dir_all(base.join("brand-new-folder/nested")).unwrap();
        fs::write(base.join("brand-new-folder/one.txt"), "a").unwrap();
        fs::write(base.join("brand-new-folder/nested/two.txt"), "b").unwrap();

        let path = base.to_string_lossy().into_owned();
        let entries = load_directory(path.clone(), "".into()).unwrap();
        let folder_entry = entries.iter().find(|entry| entry.relative_path == "brand-new-folder").expect("the new folder should be listed");
        assert!(!folder_entry.tracked, "a wholly new folder should not be marked as tracked");
        assert!(!folder_entry.status.is_empty(), "load_directory should flag the new folder with a status (e.g. untracked/changed), got empty status");

        let details = entry_details(path, "brand-new-folder".into()).unwrap();
        assert!(!details.tracked, "entry_details should also report the new folder as untracked");
        assert!(!details.status.is_empty(), "entry_details should flag the new folder with a status too, got empty status");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_commit_and_push_are_scoped_to_the_submodule() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-subpush-{suffix}"));
        let repository = base.join("main");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v1").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        for path in [&repository, &dep_seed] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        // Pushing before there is anything new to push must fail with a clear message.
        let push_before_commit = push_submodule(repo_path.clone(), "vendor/dep".into());
        assert!(push_before_commit.is_err(), "expected an error when pushing with nothing new, got Ok");
        let message = push_before_commit.unwrap_err();
        assert!(message.to_lowercase().contains("commit") || message.to_lowercase().contains("nothing") || message.to_lowercase().contains("up to date") || message.to_lowercase().contains("up-to-date"), "message should explain there is nothing to push / not committed yet, got: {message}");

        // Modifying the file but NOT committing must block push with a clear,
        // specific warning — silently pushing an older commit while leaving fresh
        // edits behind would be worse than doing nothing.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        let push_with_uncommitted = push_submodule(repo_path.clone(), "vendor/dep".into());
        assert!(push_with_uncommitted.is_err(), "expected an error when pushing with uncommitted changes, got Ok");
        let uncommitted_message = push_with_uncommitted.unwrap_err();
        assert!(uncommitted_message.to_lowercase().contains("uncommitted") || uncommitted_message.to_lowercase().contains("commit"), "message should warn about uncommitted changes, got: {uncommitted_message}");

        // Commit through our command only, then push must succeed.
        commit_submodule(repo_path.clone(), "vendor/dep".into(), "Update module".into()).expect("commit_submodule should succeed");

        // The parent repo's recorded gitlink must be untouched by the submodule commit alone.
        let parent_index_oid = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new("vendor/dep"), 0).unwrap().id };
        let submodule_head_oid = { let repo = Repository::open(&sub_path).unwrap(); let oid = repo.head().unwrap().target().unwrap(); oid };
        assert_ne!(parent_index_oid, submodule_head_oid, "parent gitlink moved automatically; it should stay put until explicitly updated");

        // The parent repo shows the submodule as having local (uncommitted-to-parent) content changes.
        let parent_changes = load_repository(repo_path.clone()).unwrap().changes;
        assert!(parent_changes.iter().any(|change| change.path == "vendor/dep"), "parent status should flag the submodule as differing");

        // Now push should succeed, and — since the commit is now safely on the
        // submodule's own server — the parent should be updated automatically so the
        // submodule stops showing as merely "modified locally".
        push_submodule(repo_path.clone(), "vendor/dep".into()).expect("push_submodule should succeed after a commit");
        let parent_index_oid_after_push = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new("vendor/dep"), 0).unwrap().id };
        let submodule_head_oid_after_push = { let repo = Repository::open(&sub_path).unwrap(); let oid = repo.head().unwrap().target().unwrap(); oid };
        assert_eq!(parent_index_oid_after_push, submodule_head_oid_after_push, "after a successful push, the parent should automatically record the new submodule commit");
        let parent_changes_after_push = load_repository(repo_path.clone()).unwrap().changes;
        assert!(!parent_changes_after_push.iter().any(|change| change.path == "vendor/dep"), "the submodule should no longer show as modified in the parent after push auto-commits the new pointer");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn push_submodule_works_from_a_detached_head() {
        // Submodules are checked out in detached HEAD by default (git's normal
        // behavior for `git submodule add`/`update`), not on a branch. This
        // reproduces that exact state and verifies push resolves a real branch
        // instead of trying to push the literal ref name "HEAD".
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-subpush-detached-{suffix}"));
        let repository = base.join("main");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v1").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        for path in [&repository, &dep_seed] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        // Force detached HEAD, mirroring the real state most submodules are in.
        let current_sha = git(&sub_path.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        run_git(&sub_path, &["checkout", "--detach", &current_sha]);
        assert!(Repository::open(&sub_path).unwrap().head_detached().unwrap(), "test setup should leave the submodule detached");

        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        commit_submodule(repo_path.clone(), "vendor/dep".into(), "Detached commit".into()).expect("commit_submodule should succeed while detached");
        assert!(Repository::open(&sub_path).unwrap().head_detached().unwrap(), "committing must not implicitly attach HEAD to a branch");

        let push_result = push_submodule(repo_path.clone(), "vendor/dep".into());
        assert!(push_result.is_ok(), "push from a detached HEAD should resolve a real branch and succeed, got: {:?}", push_result);

        let remote_main = git(&dep_remote.to_string_lossy(), &["log", "-1", "--format=%s", "main"]).unwrap();
        assert!(remote_main.contains("Detached commit"), "the pushed commit should have reached the remote's main branch, remote log: {remote_main}");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn switch_submodule_version_to_a_local_branch_uses_its_name_not_its_sha() {
        // Reproduces the exact bug report: "Change version" on a submodule entry of
        // kind "branch" was building the ref from the commit SHA instead of the
        // branch's actual name, so it always failed with
        // "reference 'refs/heads/<sha>' not found" for any local branch.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-switch-branch-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dependency).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dependency.join("module.txt"), "v1").unwrap();
        for path in [&repository, &dependency] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        // Create a genuine second LOCAL branch inside the submodule's own checkout,
        // matching what a user would see after working directly inside a submodule.
        run_git(&sub_path, &["branch", "feature-x"]);

        let versions = submodule_versions(repo_path.clone(), "vendor/dep".into()).unwrap();
        let feature_branch = versions.versions.iter().find(|version| version.kind == "branch" && version.name == "feature-x").expect("feature-x should be listed as a local branch");

        let switched = switch_submodule_version(repo_path.clone(), "vendor/dep".into(), feature_branch.revision.clone(), feature_branch.kind.clone(), feature_branch.name.clone());
        assert!(switched.is_ok(), "switching to a local branch by name should succeed, got: {:?}", switched);
        assert!(!Repository::open(&sub_path).unwrap().head_detached().unwrap(), "switching to a branch must leave HEAD attached to it, not detached");
        assert_eq!(Repository::open(&sub_path).unwrap().head().unwrap().shorthand(), Some("feature-x"));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn commander_view_compares_files_inside_a_submodule_against_its_own_remote() {
        // Reproduces the reported issue: after committing and pushing a change
        // *inside* a submodule (to the submodule's own remote), browsing into that
        // submodule from "Local <-> Remote" kept showing every file as "local-only"
        // forever, because the comparison was resolving paths against the PARENT
        // repository's tree, which has no knowledge of a submodule's internal files.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-commander-submodule-{suffix}"));
        let repository = base.join("main");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v1").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        for path in [&repository, &dep_seed] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        // Modify, commit, and push a file inside the submodule, to its own remote —
        // exactly the workflow being verified.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        commit_submodule(repo_path.clone(), "vendor/dep".into(), "Update module".into()).unwrap();
        push_submodule(repo_path.clone(), "vendor/dep".into()).unwrap();

        let comparison = compare_remote_directory(repo_path.clone(), "vendor/dep".into(), "origin/main".into()).unwrap();
        let module_row = comparison.rows.iter().find(|row| row.name == "module.txt").expect("module.txt should be listed when browsing inside the submodule");
        assert_eq!(module_row.status, "same", "after commit+push inside the submodule, the file should compare as in sync with the submodule's own remote, got status: {}", module_row.status);

        let file_comparison = compare_file_contents(repo_path, "vendor/dep/module.txt".into(), "origin/main".into()).unwrap();
        assert_eq!(file_comparison.local_content, file_comparison.remote_content, "local and remote content should match after push");
        assert_eq!(file_comparison.local_content, "v2");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_status_is_clean_everywhere_after_push_including_explorer_and_details() {
        // Checks every status source the UI actually reads (load_repository's change
        // list, load_directory's per-row status, and entry_details), not just one of
        // them, to catch any inconsistency between them after a push auto-commits the
        // parent's pointer.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-status-after-push-{suffix}"));
        let repository = base.join("main");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v1").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        for path in [&repository, &dep_seed] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        commit_submodule(repo_path.clone(), "vendor/dep".into(), "Update module".into()).unwrap();
        push_submodule(repo_path.clone(), "vendor/dep".into()).unwrap();

        let changes = load_repository(repo_path.clone()).unwrap().changes;
        assert!(!changes.iter().any(|change| change.path == "vendor/dep"), "load_repository still lists the submodule as changed: {:?}", changes.iter().map(|c| (&c.path, &c.status)).collect::<Vec<_>>());

        let entries = load_directory(repo_path.clone(), "vendor".into()).unwrap();
        let dep_entry = entries.iter().find(|entry| entry.relative_path == "vendor/dep").expect("submodule entry should be listed");
        assert_eq!(dep_entry.status, "", "load_directory still reports a status for the submodule: {:?}", dep_entry.status);

        let details = entry_details(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(details.status, "", "entry_details still reports a status for the submodule: {:?}", details.status);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_push_status_reports_unpushed_commits_and_clears_after_push() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-push-status-{suffix}"));
        let repository = base.join("main");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v1").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        for path in [&repository, &dep_seed] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        // Freshly cloned and in sync: nothing to report.
        assert_eq!(submodule_push_status(&sub_path.to_string_lossy()), None, "a freshly synced submodule should report no pending push");

        // Commit locally without pushing: should report exactly 1 unpushed commit.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        commit_submodule(repo_path.clone(), "vendor/dep".into(), "Local only".into()).unwrap();
        let status = submodule_push_status(&sub_path.to_string_lossy());
        assert!(status.as_deref().is_some_and(|message| message.contains("1 commit") && message.contains("needs push")), "expected an unpushed-commit message, got: {status:?}");

        // After a successful push, the warning must clear.
        push_submodule(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(submodule_push_status(&sub_path.to_string_lossy()), None, "after push, the submodule should no longer report anything unpushed");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pull_submodule_fast_forwards_but_refuses_a_real_divergence() {
        // Reproduces the exact reported scenario: someone else pushed a new commit to
        // the submodule's remote that the local checkout doesn't have. `pull_submodule`
        // should fast-forward cleanly in that case, but must refuse (not guess) when
        // local and remote have both moved in incompatible directions.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-pull-submodule-{suffix}"));
        let repository = base.join("main");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v1").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        for path in [&repository, &dep_seed] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        // Simulate "someone else pushed": a second clone commits and pushes ahead.
        let other_clone = base.join("other-clone");
        run_git(&base, &["-c", "protocol.file.allow=always", "clone", dep_remote.to_str().unwrap(), "other-clone"]);
        run_git(&other_clone, &["config", "user.email", "test@example.com"]);
        run_git(&other_clone, &["config", "user.name", "Someone Else"]);
        fs::write(other_clone.join("module.txt"), "from someone else").unwrap();
        run_git(&other_clone, &["commit", "-am", "Someone else's commit"]);
        run_git(&other_clone, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);

        // Our local submodule is still on the old commit and has nothing of its own —
        // this must fast-forward cleanly.
        pull_submodule(repo_path.clone(), "vendor/dep".into()).expect("a clean fast-forward pull should succeed");
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "from someone else", "pull should have brought in the other clone's content");

        // Now create a REAL divergence: local commits something new, and the remote
        // (via the other clone) also moves again — neither is an ancestor of the other.
        fs::write(sub_path.join("module.txt"), "local edit").unwrap();
        commit_submodule(repo_path.clone(), "vendor/dep".into(), "Local divergent commit".into()).unwrap();
        fs::write(other_clone.join("module.txt"), "remote diverges too").unwrap();
        run_git(&other_clone, &["commit", "-am", "Remote diverges too"]);
        run_git(&other_clone, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);

        let diverged = pull_submodule(repo_path.clone(), "vendor/dep".into());
        assert!(diverged.is_err(), "a real divergence must not be silently resolved, got Ok");
        let message = diverged.unwrap_err();
        assert!(message.to_lowercase().contains("diverged") || message.to_lowercase().contains("manual"), "expected a message explaining manual resolution is needed, got: {message}");

        // force_push_submodule must resolve exactly this stuck situation by
        // overwriting the remote with the local history.
        let forced = force_push_submodule(repo_path.clone(), "vendor/dep".into());
        assert!(forced.is_ok(), "force_push_submodule should succeed even when diverged, got: {:?}", forced);
        let remote_content = git(&dep_remote.to_string_lossy(), &["show", "main:module.txt"]).unwrap();
        assert_eq!(remote_content.trim(), "local edit", "the remote should now match the local (forced) content");
        let parent_changes = load_repository(repo_path).unwrap().changes;
        assert!(!parent_changes.iter().any(|change| change.path == "vendor/dep"), "the parent should be auto-updated after a force push too");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn polarion_url_validation_accepts_generated_links_and_rejects_everything_else() {
        assert!(is_generated_polarion_url("https://polarion.vitesco.io/polarion/#/project/OMBMS/workitem?id=OMBMS-21610"));
        assert!(is_generated_polarion_url("https://polarion.vitesco.io/polarion/#/project/A_B1/workitem?id=A_B1-1"));

        // Wrong host, wrong shape, mismatched project, or an attempt to smuggle a
        // different destination must all be rejected — this reaches a shell command.
        assert!(!is_generated_polarion_url("https://evil.example/polarion/#/project/OMBMS/workitem?id=OMBMS-21610"));
        assert!(!is_generated_polarion_url("https://polarion.vitesco.io/polarion/#/project/OMBMS/workitem?id=OTHER-21610"));
        assert!(!is_generated_polarion_url("https://polarion.vitesco.io/polarion/#/project/OMBMS/workitem?id=OMBMS-"));
        assert!(!is_generated_polarion_url("https://polarion.vitesco.io/polarion/#/project/OMBMS/workitem?id=OMBMS-12x"));
        assert!(!is_generated_polarion_url("https://polarion.vitesco.io/polarion/#/project//workitem?id=-21610"));
        assert!(!is_generated_polarion_url("javascript:alert(1)"));
        assert!(!is_generated_polarion_url("https://polarion.vitesco.io/polarion/#/project/OMBMS/workitem?id=OMBMS-21610\" & calc.exe"));
    }

    #[test]
    fn browser_repository_url_builds_a_commit_link_for_enterprise_github_too() {
        let base = browser_repository_url("git@github.vitesco.io:eng/sw-prj-OMBMS_000U0.git").expect("should parse an SSH enterprise GitHub URL");
        assert_eq!(base, "https://github.vitesco.io/eng/sw-prj-OMBMS_000U0");
        assert_eq!(format!("{base}/commit/49032750e188cfb56b0c72834feef071a4d9cc13"), "https://github.vitesco.io/eng/sw-prj-OMBMS_000U0/commit/49032750e188cfb56b0c72834feef071a4d9cc13");
    }
}
