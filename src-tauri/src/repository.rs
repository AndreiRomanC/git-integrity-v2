use serde::Serialize;
use git2::{BranchType, ObjectType, Repository, Sort, Status, StatusOptions};
use std::{collections::{HashMap, HashSet}, fs, path::{Component, Path, PathBuf}, process::Command, sync::{Mutex, OnceLock}, time::{Instant, Duration, UNIX_EPOCH}};

#[derive(Clone, Default)]
struct GitMetadata {
    tracked: HashSet<String>,
    submodules: HashSet<String>,
    statuses: Vec<(String, String)>,
    unpushed: HashSet<String>,
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

// Paths touched by commits that are on the current branch but not yet on its
// upstream — a file here is fully committed (clean working tree, matches
// HEAD exactly) but that commit hasn't reached the server. Same TTL-cached
// pattern as the index metadata above, since it doesn't depend on which
// folder is being browsed either.
static UNPUSHED_PATHS_CACHE: OnceLock<Mutex<HashMap<String, (Instant, HashSet<String>)>>> = OnceLock::new();

fn unpushed_paths_cache() -> &'static Mutex<HashMap<String, (Instant, HashSet<String>)>> {
    UNPUSHED_PATHS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// `load_repository` runs a full, unscoped status scan (every file in the
// working tree) on essentially every action — this app calls it again right
// after almost anything (stage, commit, stash, fetch...). On a large repo
// that scan is the single most expensive thing this app does, and Windows'
// filesystem stat() calls are typically slower than macOS/Linux's, making it
// worse there specifically. The same short TTL used everywhere else means a
// burst of actions within a few seconds reuses one scan instead of repeating
// it — invalidate_git_metadata() below clears this too, so nothing here is
// ever seen after a mutation actually changed something.
static FULL_STATUS_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<(String, String, bool)>)>>> = OnceLock::new();

fn full_status_cache() -> &'static Mutex<HashMap<String, (Instant, Vec<(String, String, bool)>)>> {
    FULL_STATUS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_full_statuses(repository: &Repository, repository_path: &str) -> Result<Vec<(String, String, bool)>, String> {
    if let Some((cached_at, statuses)) = full_status_cache().lock().unwrap().get(repository_path) {
        if cached_at.elapsed() < GIT_METADATA_TTL { return Ok(statuses.clone()); }
    }
    let statuses = internal_statuses(repository, None)?;
    full_status_cache().lock().unwrap().insert(repository_path.to_string(), (Instant::now(), statuses.clone()));
    Ok(statuses)
}

fn unpushed_paths(repository: &str) -> HashSet<String> {
    (|| -> Option<HashSet<String>> {
        let repo = internal_repository(repository).ok()?;
        let head = repo.head().ok()?;
        if repo.head_detached().unwrap_or(true) { return Some(HashSet::new()); }
        let local_oid = head.target()?;
        let branch = head.shorthand()?.to_string();
        drop(head);
        let upstream_oid = repo.refname_to_id(&format!("refs/remotes/origin/{branch}")).ok()?;
        if upstream_oid == local_oid { return Some(HashSet::new()); }
        let mut walk = repo.revwalk().ok()?; walk.push(local_oid).ok()?; let _ = walk.hide(upstream_oid);
        let mut paths = HashSet::new();
        for oid in walk.take(50).flatten() {
            let Ok(commit) = repo.find_commit(oid) else { continue };
            let Ok(tree) = commit.tree() else { continue };
            let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
            if let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) {
                for delta in diff.deltas() {
                    if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) { paths.insert(normalized(path)); }
                }
            }
        }
        Some(paths)
    })().unwrap_or_default()
}

fn cached_unpushed_paths(repository: &str) -> HashSet<String> {
    if let Some((cached_at, data)) = unpushed_paths_cache().lock().unwrap().get(repository) {
        if cached_at.elapsed() < GIT_METADATA_TTL { return data.clone(); }
    }
    let data = unpushed_paths(repository);
    unpushed_paths_cache().lock().unwrap().insert(repository.to_string(), (Instant::now(), data.clone()));
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
    // Only set for submodules: true when its "M" status is because it has new
    // local commits not yet pushed (a version bump), not because its own
    // working tree has genuinely uncommitted edits — the Explorer row can then
    // say "New version" instead of "Modified locally".
    submodule_has_unpushed_commits: bool,
    // True when this file is fully committed (no working-tree status at all)
    // but the commit that last touched it isn't on the upstream branch yet —
    // or, for a folder, when something inside it is in that state. Lets the
    // UI say "not pushed yet" instead of leaving a just-committed file looking
    // identical to one that was never touched.
    unpushed: bool,
}

#[derive(Serialize)]
pub struct EntryDetails {
    name: String,
    relative_path: String,
    kind: String,
    status: String,
    tracked: bool,
    unpushed: bool,
    size: u64,
    modified: u64,
    item_count: Option<usize>,
    submodule_url: Option<String>,
    submodule_branch: Option<String>,
    submodule_push_status: Option<String>,
    submodule_unpushed_commits: Vec<PublishCommit>,
    last_commit_id: Option<String>,
    last_commit_subject: Option<String>,
    last_commit_author: Option<String>,
    last_commit_date: Option<String>,
    // For a submodule, `last_commit_*` above is the *parent's* commit that last
    // touched the gitlink — usually just "Update submodule to <sha>", not
    // informative on its own. These are the submodule's own HEAD commit, i.e.
    // the one that actually carries the real change description.
    submodule_commit_id: Option<String>,
    submodule_commit_subject: Option<String>,
    submodule_commit_author: Option<String>,
    submodule_commit_date: Option<String>,
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

// `GIT_TERMINAL_PROMPT=0` + a null stdin are the real fix for the app
// "freezing" on some machines (reported worse on Windows): without them, if a
// remote needs credentials that aren't already cached (no saved token, an
// expired credential-manager entry, a corporate proxy asking for a login…),
// `git` tries to prompt for a username/password on a terminal that doesn't
// exist in a GUI app — and just hangs forever waiting for input nobody can
// ever provide, instead of failing with a clear error. With this, the same
// situation now fails fast with Git's own "could not read... terminal
// prompts disabled" message, which `handleError` on the frontend already
// recognizes and gives credential-setup guidance for.
fn configure_git_command(command: &mut Command) {
    command.env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null());
    // A network call that genuinely can't reach the server — a dead proxy, a
    // firewall silently dropping packets, a VPN that just fell over — has no
    // built-in way to give up on its own, and could otherwise sit there
    // forever with the app looking "frozen" (reported specifically on
    // Windows, where this class of network failure is more common). These
    // only abort a connection that is making *zero* progress; a real,
    // actively-transferring clone/fetch/push on a 30GB+ repo is never cut
    // off just for being slow. `-o ConnectTimeout` only applies when the
    // user hasn't already customized their own SSH command, so it never
    // overrides a deliberate existing setup.
    command.arg("-c").arg("http.lowSpeedLimit=1000").arg("-c").arg("http.lowSpeedTime=20");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() { command.env("GIT_SSH_COMMAND", "ssh -o ConnectTimeout=15"); }
}

// A last-resort backstop for a git subprocess that is well and truly stuck —
// not just slow (the low-speed-limit config above already handles a stalled
// HTTP transfer; this catches everything else: a wedged credential/OS-level
// prompt slipping past GIT_TERMINAL_PROMPT, a filesystem lock held forever,
// an SSH connection that hangs past its own timeout on some networks). Ten
// minutes is generous enough that even a real, actively-transferring clone
// or fetch on the 30GB+ monorepo over a slow connection should finish well
// within it; a git command that hasn't produced anything in that long is
// not "big data", it's actually stuck. The child is force-killed on timeout
// so the app itself is never left waiting on it either.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

fn run_with_timeout(mut command: Command) -> Result<std::process::Output, String> {
    let child = command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| format!("Cannot start Git: {e}"))?;
    let id = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
    match rx.recv_timeout(GIT_COMMAND_TIMEOUT) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("Git process error: {error}")),
        Err(_) => {
            #[cfg(unix)] { let _ = Command::new("kill").arg("-9").arg(id.to_string()).status(); }
            #[cfg(windows)] { let _ = Command::new("taskkill").args(["/F", "/PID"]).arg(id.to_string()).status(); }
            Err("Git command timed out after 10 minutes — check your network connection and try again".into())
        }
    }
}

fn git(path: &str, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    // `-c` overrides must come before the subcommand to be recognized as
    // global git config, not passed through to it — configure_git_command's
    // own `-c` flags need to land here, before `args` (which starts with the
    // subcommand), not after.
    configure_git_command(&mut command);
    command.arg("-C").arg(path).arg("-c").arg("color.ui=false").args(args);
    let output = run_with_timeout(command)?;
    if output.status.success() { Ok(String::from_utf8_lossy(&output.stdout).into_owned()) }
    else { Err(String::from_utf8_lossy(&output.stderr).trim().to_string()) }
}

#[derive(Serialize)]
pub struct RawGitResult { stdout: String, stderr: String, success: bool }

// The command console's "run any git command" escape hatch — scoped to whatever
// folder the caller passes (the folder currently being browsed, or a selected
// submodule), using `git -C <path>` exactly like the rest of this file's shell
// calls. `Command::args` passes each token as a literal argument straight to the
// `git` binary — never through a shell — so there is no shell-injection surface
// here regardless of what the user types (no `;`, `&&`, backticks etc. have any
// special meaning). It genuinely can run destructive commands if asked to
// (that's the point), so the frontend must confirm before anything recognizably
// destructive; this only enforces that the first token isn't literally "git"
// again (a common typo: pasting "git status" here instead of just "status").
#[tauri::command]
pub fn run_git_command(repository_path: String, args: String) -> Result<RawGitResult, String> {
    validate_path(&repository_path)?;
    let mut parts: Vec<&str> = args.split_whitespace().collect();
    // Typing the full, natural command ("git status") is just as valid as the
    // short form ("status") — strip a leading "git" token instead of
    // rejecting it, so this behaves like a real terminal either way.
    if parts.first() == Some(&"git") { parts.remove(0); }
    if parts.is_empty() { return Err("Type a git subcommand, e.g. \"status\" or \"log --oneline -10\"".into()); }
    let mut command = Command::new("git");
    configure_git_command(&mut command);
    command.arg("-C").arg(&repository_path).arg("-c").arg("color.ui=false").args(&parts);
    let output = run_with_timeout(command)?;
    invalidate_git_metadata(&repository_path);
    Ok(RawGitResult {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
    })
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
        let code = if value.contains(Status::CONFLICTED) { "U" } else if value.contains(Status::WT_NEW) { "??" } else if value.intersects(Status::WT_DELETED | Status::INDEX_DELETED) { "D" } else if value.intersects(Status::INDEX_NEW) { "A" } else if value.intersects(Status::WT_RENAMED | Status::INDEX_RENAMED) { "R" } else { "M" };
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
    let unpushed = cached_unpushed_paths(repository);
    GitMetadata { statuses: statuses.unwrap_or_else(|| worktree_status(repository, scope)), tracked, submodules, unpushed }
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
    unpushed_paths_cache().lock().unwrap().remove(repository);
    full_status_cache().lock().unwrap().remove(repository);
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

// For every submodule whose own checked-out commit is clean (no uncommitted
// changes of its own) and differs from what the parent's index currently
// records, auto-record that new commit into the parent — the same
// reconciliation `commit_submodule`/push already do explicitly, applied
// generally here so it doesn't matter *how* the submodule ended up on a new
// commit (its dedicated "Commit submodule" button, a raw git command typed
// in the console, or committing one of its files directly): the parent
// catches up the moment anything reloads it, instead of silently staying
// unaware and making "Unpublished commits" look wrong by comparison.
fn sync_submodule_gitlinks(repository_path: &str) {
    let Ok(parent) = internal_repository(repository_path) else { return };
    let Ok(index) = parent.index() else { return };
    let gitlinks: Vec<(String, git2::Oid)> = index.iter().filter(|entry| entry.mode == 0o160000).map(|entry| (String::from_utf8_lossy(&entry.path).into_owned(), entry.id)).collect();
    drop(index);
    for (relative, recorded_oid) in gitlinks {
        let absolute = Path::new(repository_path).join(&relative);
        let Ok(sub_repo) = internal_repository(absolute.to_str().unwrap_or_default()) else { continue };
        let Some(head_oid) = sub_repo.head().ok().and_then(|head| head.target()) else { continue };
        if head_oid == recorded_oid { continue; }
        let Ok(dirty) = internal_statuses(&sub_repo, None) else { continue };
        if !dirty.is_empty() { continue; } // has uncommitted changes of its own — leave it for the user to commit first
        drop(sub_repo);
        let _ = record_pushed_submodule_in_parent(repository_path, &relative, Some(head_oid));
    }
}

#[tauri::command]
pub fn load_repository(path: String, force: Option<bool>) -> Result<RepositoryData, String> {
    validate_path(&path)?;
    // The manual Refresh action exists specifically for "something changed
    // outside this app (a terminal, VS Code...) and I want a truly fresh
    // read" — the short status-scan cache below must never serve it a stale
    // scan from moments earlier just because nothing *this app* did
    // triggered an invalidation.
    if force.unwrap_or(false) { invalidate_git_metadata(&path); }
    sync_submodule_gitlinks(&path);
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

    let internal = cached_full_statuses(&repo, &path)?;
    let statuses = internal.iter().map(|(path, status, _)| (path.clone(), status.clone())).collect::<Vec<_>>();
    let changes = internal.into_iter().map(|(path, status, staged)| Change { status, path, staged }).collect();

    replace_git_metadata(&path, statuses);

    Ok(RepositoryData { repository: RepositoryInfo { path, name, current_branch }, branches, commits, changes, stashes })
}

// A file living inside a submodule belongs to that submodule's own Git index,
// not the parent's — the parent's index only ever has one gitlink entry for
// the whole submodule, never its individual files. Splitting the requested
// paths by which repository they actually belong to (and recursing into the
// submodule for its share) is what makes staging/unstaging work no matter
// which folder you browsed into to select the file.
fn partition_by_submodule(repository_path: &str, files: Vec<String>) -> (Vec<String>, HashMap<String, Vec<String>>) {
    let mut own = Vec::new();
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
    for file in files {
        match resolve_submodule_boundary(repository_path, &file) {
            // Only redirect for a path *inside* a submodule (a file within
            // it). The submodule's own path (empty inner_relative) must stay
            // in `own` and go through the normal gitlink-staging logic below
            // — treating it as "recurse into the submodule with an empty
            // path" silently staged nothing and made the submodule itself
            // impossible to stage/commit from the Working tree drawer.
            Some((sub_path, inner_relative)) if !inner_relative.is_empty() => grouped.entry(sub_path).or_default().push(inner_relative),
            _ => own.push(file),
        }
    }
    (own, grouped)
}

#[tauri::command]
pub fn stage_files(path: String, files: Vec<String>) -> Result<(), String> {
    validate_path(&path)?;
    let (files, submodule_groups) = partition_by_submodule(&path, files);
    for (sub_path, inner_files) in submodule_groups { stage_files(sub_path, inner_files)?; }
    if files.is_empty() { return Ok(()); }
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
    let (files, submodule_groups) = partition_by_submodule(&path, files);
    for (sub_path, inner_files) in submodule_groups { unstage_files(sub_path, inner_files)?; }
    if files.is_empty() { return Ok(()); }
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

// Creates and switches to a new branch inside a submodule's own repository —
// same as `create_branch`, just resolved to the submodule's path first, and
// with the parent's index refreshed afterward so it stays consistent with
// what every other submodule-state-changing action in this app already does
// (the commit itself doesn't change, but this keeps "modified" status honest).
#[tauri::command]
pub fn create_submodule_branch(repository_path: String, relative_path: String, branch: String) -> Result<(), String> {
    if branch.trim().is_empty() { return Err("Branch name cannot be empty".into()); }
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    create_branch(absolute.to_string_lossy().into_owned(), branch)?;
    let parent = internal_repository(&repository_path)?;
    let mut submodule = parent.find_submodule(&relative_path).map_err(|error| error.message().to_string())?;
    submodule.add_to_index(true).map_err(|error| format!("Branch created, but the parent index could not be updated: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
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

// "Fetch current" only ever fetched the first configured remote — a project
// with more than one remote (a second push mirror, an upstream, etc.) had no
// single-click way to update from all of them at once.
#[tauri::command]
pub fn fetch_all_remotes(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    git(&repository_path, &["fetch", "--all"]).map_err(|detail| format!("Fetch failed: {detail}"))?;
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

#[derive(Serialize, Clone)]
pub struct ConflictedFile { path: String, has_ours: bool, has_theirs: bool }

#[derive(Serialize)]
pub struct MergeOutcome { status: String, message: String, conflicts: Vec<ConflictedFile> }

#[derive(Serialize)]
pub struct ConflictSides { ancestor: Option<String>, ours: Option<String>, theirs: Option<String> }

fn blob_text(repo: &Repository, id: Option<git2::Oid>) -> Option<String> {
    let id = id?;
    let blob = repo.find_blob(id).ok()?;
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

fn gather_conflicts(repo: &Repository) -> Result<Vec<ConflictedFile>, String> {
    let index = repo.index().map_err(|error| error.message().to_string())?;
    let conflicts = index.conflicts().map_err(|error| error.message().to_string())?;
    let mut files = Vec::new();
    for conflict in conflicts.flatten() {
        let path = conflict.our.as_ref().or(conflict.their.as_ref()).or(conflict.ancestor.as_ref())
            .map(|entry| String::from_utf8_lossy(&entry.path).into_owned());
        if let Some(path) = path {
            files.push(ConflictedFile { path, has_ours: conflict.our.is_some(), has_theirs: conflict.their.is_some() });
        }
    }
    Ok(files)
}

// Generic merge — works identically on the main repository or a submodule's own
// repository (a submodule is just another repository at a different path), used
// both as the explicit "Merge branch…" action and as the fallback offered when a
// fast-forward-only pull refuses because of a real divergence.
//
// All merge/conflict commands below take (repository_path, target_path, ...) —
// same convention as the submodule action commands — so the same code works
// identically on the main repository (target_path == "") or on a submodule
// (target_path == the submodule's path within the parent), which is just
// another repository at a different location on disk.
fn resolve_target_repository(repository_path: &str, target_path: &str) -> Result<String, String> {
    if target_path.trim().is_empty() { return Ok(repository_path.to_string()); }
    Ok(validate_submodule(repository_path, target_path)?.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn merge_branch(repository_path: String, target_path: String, source_ref: String) -> Result<MergeOutcome, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let mut repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Clean {
        return Err("A merge (or other operation) is already in progress here. Resolve or abort it first.".into());
    }
    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() {
        return Err("There are uncommitted changes here. Commit or stash them first, so a merge can't mix them up with incoming changes.".into());
    }
    if repo.head_detached().unwrap_or(true) {
        return Err("This is a detached HEAD (not on a branch), so there is nothing to merge into.".into());
    }
    let current_branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).ok_or("Could not determine the current branch")?;
    let reference = repo.resolve_reference_from_short_name(source_ref.trim()).map_err(|_| format!("Could not find branch \"{source_ref}\" — fetch first if it's a remote branch."))?;
    let annotated = repo.reference_to_annotated_commit(&reference).map_err(|error| error.message().to_string())?;
    let (analysis, _) = repo.merge_analysis(&[&annotated]).map_err(|error| error.message().to_string())?;

    if analysis.is_up_to_date() {
        return Ok(MergeOutcome { status: "up_to_date".into(), message: format!("{current_branch} is already up to date with {source_ref}."), conflicts: vec![] });
    }
    if analysis.is_fast_forward() {
        let target = annotated.id();
        let mut local = repo.find_reference(&format!("refs/heads/{current_branch}")).map_err(|error| error.message().to_string())?;
        local.set_target(target, "fast-forward merge").map_err(|error| error.message().to_string())?;
        repo.set_head(&format!("refs/heads/{current_branch}")).map_err(|error| error.message().to_string())?;
        let mut checkout = git2::build::CheckoutBuilder::new(); checkout.force();
        repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?;
        invalidate_git_metadata(&repository_path);
        return Ok(MergeOutcome { status: "fast_forwarded".into(), message: format!("Fast-forwarded {current_branch} to {source_ref}."), conflicts: vec![] });
    }

    let mut checkout = git2::build::CheckoutBuilder::new();
    // Conflicts are expected here — write the standard `<<<<<<<`/`=======`/`>>>>>>>`
    // marker files to disk instead of aborting, so they can be resolved below.
    checkout.allow_conflicts(true).conflict_style_merge(true).force();
    repo.merge(&[&annotated], None, Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    drop(annotated); drop(reference);

    let index = repo.index().map_err(|error| error.message().to_string())?;
    if index.has_conflicts() {
        let conflicts = gather_conflicts(&repo)?;
        let count = conflicts.len();
        return Ok(MergeOutcome { status: "conflicts".into(), message: format!("Merging {source_ref} produced {count} conflict{}. Resolve them, then complete the merge.", if count == 1 { "" } else { "s" }), conflicts });
    }

    // No conflicts — the merge resolved automatically; finish it with a commit
    // right away instead of leaving the repository in a pending-merge state.
    drop(index);
    let oid = complete_merge_internal(&mut repo, &repository_path, &format!("Merge {source_ref} into {current_branch}"))?;
    Ok(MergeOutcome { status: "merged".into(), message: format!("Merged {source_ref} into {current_branch} ({}).", &oid[..8.min(oid.len())]), conflicts: vec![] })
}

// The conflict tools (list/resolve) work generically on whatever the index
// currently has conflicted — a merge left mid-resolution, or a stash pop
// that couldn't apply cleanly. Finishing them is different in each case
// (a merge needs a merge commit; a stash pop doesn't), so the caller needs
// to know which situation it's actually looking at.
#[tauri::command]
pub fn merge_in_progress(repository_path: String, target_path: String) -> Result<bool, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    Ok(repo.state() == git2::RepositoryState::Merge)
}

#[tauri::command]
pub fn list_conflicts(repository_path: String, target_path: String) -> Result<Vec<ConflictedFile>, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    gather_conflicts(&repo)
}

#[tauri::command]
pub fn conflict_sides(repository_path: String, target_path: String, relative_path: String) -> Result<ConflictSides, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?; let relative = normalized(&relative);
    let repo = internal_repository(&repository_path)?;
    let index = repo.index().map_err(|error| error.message().to_string())?;
    let conflicts = index.conflicts().map_err(|error| error.message().to_string())?;
    for conflict in conflicts.flatten() {
        let path = conflict.our.as_ref().or(conflict.their.as_ref()).or(conflict.ancestor.as_ref()).map(|entry| String::from_utf8_lossy(&entry.path).into_owned());
        if path.as_deref() != Some(relative.as_str()) { continue; }
        return Ok(ConflictSides {
            ancestor: blob_text(&repo, conflict.ancestor.as_ref().map(|entry| entry.id)),
            ours: blob_text(&repo, conflict.our.as_ref().map(|entry| entry.id)),
            theirs: blob_text(&repo, conflict.their.as_ref().map(|entry| entry.id)),
        });
    }
    Err(format!("{relative_path} is not a conflicted file"))
}

#[tauri::command]
pub fn resolve_conflict(repository_path: String, target_path: String, relative_path: String, resolution: String) -> Result<(), String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let repo = internal_repository(&repository_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    match resolution.as_str() {
        "ours" | "theirs" => {
            let index = repo.index().map_err(|error| error.message().to_string())?;
            let conflicts = index.conflicts().map_err(|error| error.message().to_string())?;
            let target = normalized(&relative);
            let mut content = None;
            for conflict in conflicts.flatten() {
                let entry = if resolution == "ours" { conflict.our } else { conflict.their };
                let Some(entry) = entry else { continue };
                if String::from_utf8_lossy(&entry.path) != target { continue; }
                content = blob_text(&repo, Some(entry.id));
                break;
            }
            let content = content.ok_or_else(|| format!("No {resolution} version exists for {relative_path} (it may have been added only on one side — deleting or keeping the existing file may be more appropriate)."))?;
            fs::write(&absolute, content).map_err(|error| format!("Cannot write {}: {error}", absolute.display()))?;
        }
        "manual" => { if !absolute.exists() { return Err(format!("{relative_path} does not exist on disk — nothing to mark resolved.")); } }
        other => return Err(format!("Unknown resolution kind \"{other}\"")),
    }
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    index.add_path(&relative).map_err(|error| error.message().to_string())?;
    index.write().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

fn complete_merge_internal(repo: &mut Repository, repository_path: &str, message: &str) -> Result<String, String> {
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    if index.has_conflicts() { return Err("There are still unresolved conflicts.".into()); }
    let mut merge_heads = Vec::new();
    repo.mergehead_foreach(|oid| { merge_heads.push(*oid); true }).map_err(|error| error.message().to_string())?;
    let head_commit = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let mut parents = Vec::new();
    if let Some(commit) = head_commit.as_ref() { parents.push(commit.clone()); }
    for oid in &merge_heads { if let Ok(commit) = repo.find_commit(*oid) { parents.push(commit); } }
    let tree_id = index.write_tree_to(repo).map_err(|error| error.message().to_string())?;
    let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?;
    let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?;
    let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
    let oid = repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &parent_refs).map_err(|error| error.message().to_string())?;
    repo.cleanup_state().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(repository_path);
    Ok(oid.to_string())
}

#[tauri::command]
pub fn complete_merge(repository_path: String, target_path: String, message: String) -> Result<String, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Merge commit message cannot be empty".into()); }
    let mut repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Merge {
        return Err("There is no merge in progress here.".into());
    }
    complete_merge_internal(&mut repo, &repository_path, message.trim())
}

#[tauri::command]
pub fn abort_merge(repository_path: String, target_path: String) -> Result<(), String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    if repo.state() != git2::RepositoryState::Merge {
        return Err("There is no merge in progress here.".into());
    }
    let head_commit = repo.head().map_err(|error| error.message().to_string())?.peel_to_commit().map_err(|error| error.message().to_string())?;
    let mut checkout = git2::build::CheckoutBuilder::new(); checkout.force();
    repo.reset(head_commit.as_object(), git2::ResetType::Hard, Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    repo.cleanup_state().map_err(|error| error.message().to_string())?;
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
    let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.push(local_oid).map_err(|error| error.message().to_string())?;
    // Hide everything reachable from ANY of this remote's branches, not only
    // the one sharing this local branch's name. A brand new local branch
    // (e.g. just created, no upstream yet) that descends from — or sits right
    // at — a commit already on the server under a different branch name
    // doesn't actually need to re-push that shared history; only what isn't
    // reachable from anything already on this remote is genuinely new.
    // Without this, "Publish" on any new branch showed its *entire* ancestry
    // as "WILL PUSH", even commits from years ago already sitting on origin.
    if let Ok(references) = repo.references_glob(&format!("refs/remotes/{remote}/*")) {
        for reference in references.flatten() { if let Some(oid) = reference.target() { let _ = walk.hide(oid); } }
    }
    let mut commits = walk.flatten().take(100).filter_map(|oid| repo.find_commit(oid).ok().map(|commit| PublishCommit { id: oid.to_string(), subject: commit.summary().unwrap_or("No message").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()) })).collect::<Vec<_>>(); commits.reverse();
    Ok(PublishStatus { branch: branch.into(), remote: remote.into(), remote_branch, commits })
}

#[tauri::command]
pub fn publish_branch(repository_path: String, branch: String, remote: String, username: String, access_token: String, upto_commit: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if branch.trim().is_empty() || remote.trim().is_empty() { return Err("Choose a local branch and a remote".into()); }
    let repo = internal_repository(&repository_path)?; let branch = branch.trim(); let remote_name = remote.trim(); let local_oid = repo.refname_to_id(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    // Git can only push a *contiguous* range of history — there's no way to
    // publish a commit while holding back an older one it depends on. So
    // "leave some commits unpublished" can only ever mean "stop at an earlier
    // point": push up to `upto_commit` (an ancestor of the branch tip, or the
    // tip itself for a normal full push) instead of the tip directly.
    let push_oid = if upto_commit.trim().is_empty() { local_oid } else {
        let object = repo.revparse_single(upto_commit.trim()).map_err(|error| format!("Cannot resolve {upto_commit}: {}", error.message()))?;
        let oid = object.peel_to_commit().map_err(|error| error.message().to_string())?.id();
        let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.push(local_oid).map_err(|error| error.message().to_string())?;
        if !walk.flatten().any(|id| id == oid) { return Err("The selected commit isn't part of this branch's history".into()); }
        oid
    };
    if access_token.trim().is_empty() {
        // No explicit token was entered — prefer the system `git` binary, which
        // transparently reuses the user's already-working SSH agent, credential
        // helper, or OS keychain. libgit2's own credential search is much narrower
        // and can fail here ("failed to acquire username/password") even when a
        // plain `git push` in a terminal works fine for the same repository.
        git(&repository_path, &["push", remote_name, &format!("{push_oid}:refs/heads/{branch}")])
            .map_err(|detail| if detail.to_lowercase().contains("authentication") || detail.contains("403") || detail.contains("could not read") { "Push authentication failed. Either make sure `git push` works for this repository from a terminal, or enter a Git username and Personal Access Token in Publish credentials.".to_string() } else if detail.to_lowercase().contains("non-fast-forward") || detail.to_lowercase().contains("fetch first") { "Push rejected because the server branch has newer commits. Pull/fetch those commits first, then publish again.".to_string() } else { format!("Push failed: {detail}") })?;
    } else {
        // git2's push refspecs need the source side to resolve to a reference,
        // not a bare commit id — point a scratch local ref at it, push that,
        // then remove the scratch ref regardless of outcome.
        let scratch_ref = "refs/heads/__git-integrity-partial-publish__";
        repo.reference(scratch_ref, push_oid, true, "scratch ref for a partial publish").map_err(|error| error.message().to_string())?;
        let mut remote = repo.find_remote(remote_name).map_err(|error| error.message().to_string())?; let mut options = authenticated_push_options(username, access_token);
        let push_result = remote.push(&[&format!("{scratch_ref}:refs/heads/{branch}")], Some(&mut options));
        let _ = repo.find_reference(scratch_ref).and_then(|mut reference| reference.delete());
        push_result.map_err(|error| { let detail = error.message(); if detail.contains("username/password") || detail.contains("authentication") || detail.contains("401") || detail.contains("403") { "Push authentication failed. Check the username and Personal Access Token in Publish credentials (not your account password).".to_string() } else if detail.contains("non-fast-forward") { "Push rejected because the server branch has newer commits. Pull/fetch those commits first, then publish again.".to_string() } else { format!("Push failed: {detail}") } })?; drop(remote);
    }
    repo.reference(&format!("refs/remotes/{remote_name}/{branch}"), push_oid, true, "successful publish").map_err(|error| format!("Push succeeded, but local server tracking could not be updated: {}", error.message()))?;
    let mut config = repo.config().map_err(|error| error.message().to_string())?; config.set_str(&format!("branch.{branch}.remote"), remote_name).map_err(|error| error.message().to_string())?; config.set_str(&format!("branch.{branch}.merge"), &format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn submodule_repository(repository_path: String, relative_path: String) -> Result<RepositoryData, String> {
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    load_repository(absolute.to_string_lossy().into_owned(), None)
}

#[tauri::command]
pub fn load_directory(repository_path: String, relative_path: String) -> Result<Vec<DirectoryEntry>, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    if !absolute.is_dir() { return Err("The selected path is not a folder".into()); }

    // Browsing *into* a submodule crosses into a different Git repository —
    // the parent's own status/tracked info only ever covers the submodule as
    // one opaque gitlink entry, never the files inside it (that made every
    // file inside a submodule look permanently "untracked" from the parent's
    // point of view, and staging one silently did nothing since it wasn't
    // part of the parent's tree at all). Source status from the submodule's
    // own index/worktree instead when we're inside one, while still returning
    // `relative_path` in the same parent-relative path space the rest of the
    // UI already navigates in.
    let boundary = resolve_submodule_boundary(&repository_path, &relative_path);
    let (status_repo, status_scope) = match &boundary {
        Some((sub_path, inner_relative)) => (sub_path.as_str(), inner_relative.as_str()),
        None => (repository_path.as_str(), relative_path.as_str()),
    };
    let git_metadata = cached_git_metadata(status_repo, status_scope);
    let mut entries = Vec::new();

    for item in fs::read_dir(&absolute).map_err(|error| error.to_string())? {
        let item = item.map_err(|error| error.to_string())?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" { continue; }
        let relative_string = normalized(&relative.join(&name));
        let status_key = if boundary.is_some() { normalized(&Path::new(status_scope).join(&name)) } else { relative_string.clone() };
        let metadata = fs::symlink_metadata(item.path()).map_err(|error| error.to_string())?;
        let kind = if git_metadata.submodules.contains(&status_key) { "submodule" }
            else if metadata.file_type().is_symlink() { "symlink" }
            else if metadata.is_dir() { "folder" }
            else { "file" }.to_string();
        let tracked_prefix = format!("{status_key}/");
        let tracked = git_metadata.submodules.contains(&status_key) || git_metadata.tracked.contains(&status_key) || git_metadata.tracked.iter().any(|path| path.starts_with(&tracked_prefix));
        let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|value| value.as_secs()).unwrap_or(0);
        let submodule_has_unpushed_commits = kind == "submodule" && submodule_push_status(item.path().to_str().unwrap_or_default()).is_some();
        let unpushed = if kind == "folder" { git_metadata.unpushed.iter().any(|path| path == &status_key || path.starts_with(&tracked_prefix)) } else { git_metadata.unpushed.contains(&status_key) };
        entries.push(DirectoryEntry { name, relative_path: relative_string, kind, status: status_for(&status_key, &git_metadata.statuses), tracked, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, submodule_has_unpushed_commits, unpushed });
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
    // Same reasoning as `load_directory`: an item strictly inside a submodule
    // needs status/tracked/history from the submodule's own repository, not
    // the parent's (which only knows the submodule as one opaque gitlink).
    // The submodule's own root path is deliberately excluded here (empty
    // inner path) so it keeps being treated as a submodule object at the
    // parent level, same as before.
    let boundary = resolve_submodule_boundary(&repository_path, &relative_string).filter(|(_, inner)| !inner.is_empty());
    let (status_repo, status_scope): (&str, &str) = match &boundary {
        Some((sub_path, inner_relative)) => (sub_path.as_str(), inner_relative.as_str()),
        None => (&repository_path, &relative_string),
    };
    let git_metadata = cached_git_metadata(status_repo, status_scope);
    let kind = if git_metadata.submodules.contains(status_scope) { "submodule" }
        else if metadata.file_type().is_symlink() { "symlink" }
        else if metadata.is_dir() { "folder" } else { "file" }.to_string();
    let prefix = format!("{status_scope}/");
    let tracked = git_metadata.tracked.contains(status_scope) || git_metadata.tracked.iter().any(|path| path.starts_with(&prefix));
    let status = status_for(status_scope, &git_metadata.statuses);
    let unpushed = if kind == "folder" { git_metadata.unpushed.iter().any(|path| path == status_scope || path.starts_with(&prefix)) } else { git_metadata.unpushed.contains(status_scope) };
    let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|value| value.as_secs()).unwrap_or(0);
    let item_count = metadata.is_dir().then(|| fs::read_dir(&absolute).map(|items| items.count()).unwrap_or(0));

    let last = match &boundary {
        Some((sub_path, inner_relative)) => last_commit_touching_path(sub_path, Path::new(inner_relative)),
        None => last_commit_touching_path(&repository_path, &relative),
    };
    let submodule_url = (kind == "submodule").then(|| submodule_value(&repository_path, &relative_string, "url")).flatten();
    let submodule_branch = (kind == "submodule").then(|| submodule_value(&repository_path, &relative_string, "branch")).flatten();
    let submodule_push_status = (kind == "submodule").then(|| submodule_push_status(&absolute.to_string_lossy())).flatten();
    let submodule_unpushed_commits = if kind == "submodule" { submodule_unpushed_commits(&absolute.to_string_lossy()) } else { Vec::new() };
    let submodule_commit = (kind == "submodule").then(|| {
        let repo = internal_repository(absolute.to_str().unwrap_or_default()).ok()?;
        let commit = repo.head().ok()?.peel_to_commit().ok()?;
        let author_name = commit.author().name().unwrap_or("Unknown").to_string();
        Some((commit.id().to_string(), commit.summary().unwrap_or("No message").to_string(), author_name, short_date(commit.time().seconds())))
    }).flatten();

    Ok(EntryDetails {
        name: absolute.file_name().and_then(|name| name.to_str()).unwrap_or(&relative_string).to_string(), relative_path: relative_string,
        kind, status, tracked, unpushed, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, item_count, submodule_url, submodule_branch, submodule_push_status, submodule_unpushed_commits,
        last_commit_id: last.as_ref().map(|value| value.0.clone()),
        last_commit_subject: last.as_ref().map(|value| value.1.clone()), last_commit_author: last.as_ref().map(|value| value.2.clone()), last_commit_date: last.as_ref().map(|value| value.3.clone()),
        submodule_commit_id: submodule_commit.as_ref().map(|value| value.0.clone()),
        submodule_commit_subject: submodule_commit.as_ref().map(|value| value.1.clone()), submodule_commit_author: submodule_commit.as_ref().map(|value| value.2.clone()), submodule_commit_date: submodule_commit.as_ref().map(|value| value.3.clone()),
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

// The actual commits behind `submodule_push_status`'s summary line — showing
// "3 commits not pushed" as a sentence and making the user guess which three
// isn't good enough; list them the same way the main project's "Unpublished
// commits" dialog already does.
fn submodule_unpushed_commits(sub_path: &str) -> Vec<PublishCommit> {
    (|| -> Option<Vec<PublishCommit>> {
        let repo = internal_repository(sub_path).ok()?;
        let head = repo.head().ok()?;
        let local_target = head.target()?;
        if repo.head_detached().unwrap_or(true) { return Some(Vec::new()); }
        let branch = head.shorthand()?.to_string();
        drop(head);
        let remote_target = repo.find_reference(&format!("refs/remotes/origin/{branch}")).ok()?.target()?;
        if remote_target == local_target { return Some(Vec::new()); }
        let mut walk = repo.revwalk().ok()?; walk.push(local_target).ok()?; let _ = walk.hide(remote_target);
        let mut commits: Vec<PublishCommit> = walk.take(50).flatten().filter_map(|oid| repo.find_commit(oid).ok().map(|commit| PublishCommit {
            id: oid.to_string(), subject: commit.summary().unwrap_or("No message").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()),
        })).collect();
        commits.reverse();
        Some(commits)
    })().unwrap_or_default()
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
    } else if version_kind == "remote" {
        // Picking a remote branch (e.g. "origin/main") from the list feels like
        // picking "main" — landing on a detached HEAD there is technically correct
        // Git behavior (you can't literally be "on" a remote-tracking ref) but
        // surprises users who expect to end up on a normal, attached branch. Mirror
        // what `git checkout <remote-branch>` actually does: if a same-named local
        // branch doesn't exist yet, create one tracking this commit and check that
        // out instead of detaching; if one already exists and already points at
        // this exact commit, just attach to it. Only fall back to a detached
        // checkout when a local branch of that name exists but points somewhere
        // else — moving it here could silently strand the user's own commits.
        let object = repo.revparse_single(&revision).map_err(|error| error.message().to_string())?;
        let commit = object.peel_to_commit().map_err(|error| error.message().to_string())?;
        let local_name = name.split_once('/').map(|(_, rest)| rest).unwrap_or(&name);
        match repo.find_branch(local_name, BranchType::Local) {
            Ok(existing) if existing.get().target() == Some(commit.id()) => {
                repo.set_head(&format!("refs/heads/{local_name}")).map_err(|error| error.message().to_string())?;
            }
            Ok(_) => { repo.set_head_detached(commit.id()).map_err(|error| error.message().to_string())?; }
            Err(_) => {
                repo.branch(local_name, &commit, false).map_err(|error| error.message().to_string())?;
                repo.set_head(&format!("refs/heads/{local_name}")).map_err(|error| error.message().to_string())?;
            }
        }
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
    // Only redirect for a path *inside* a submodule (a file within it) — the
    // submodule's own root path must still commit at the parent level (that's
    // how the gitlink bump gets recorded); "Commit submodule" is the separate,
    // existing action for committing changes inside the submodule itself.
    if let Some((sub_path, inner_relative)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        if !inner_relative.is_empty() { return commit_path(sub_path, inner_relative, message); }
    }
    let relative = safe_relative_path(&relative_path)?;
    // An empty string is the sentinel `commit_selected_internal` recognizes as
    // "whole repository" — git2 rejects "." as a literal pathspec outright (see
    // the handling below), so that can't be used here.
    let pathspec = if relative.as_os_str().is_empty() { String::new() } else { normalized(&relative) };
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
        if file.is_empty() {
            // Whole-repository scope ("Commit repository" with nothing selected).
            // git2 rejects "." as a literal pathspec ("repo path `.` should not
            // start with `.`"), and `add_all` alone never removes index entries
            // for files deleted from disk — pairing it with `update_all` (which
            // does drop them) makes this behave like `git add -A` for the whole
            // working tree, submodules included (add_all stages a submodule's
            // current HEAD as its gitlink automatically).
            index.add_all(Vec::<String>::new(), git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?;
            index.update_all(Vec::<String>::new(), None).map_err(|error| error.message().to_string())?;
            includes_submodule = true;
            continue;
        }
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
        if file.is_empty() {
            index.add_all(Vec::<String>::new(), git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?;
            index.update_all(Vec::<String>::new(), None).map_err(|error| error.message().to_string())?;
            continue;
        }
        let relative = Path::new(file); let absolute = Path::new(repository_path).join(relative);
        if gitlinks.contains_key(file) && absolute.exists() { if let Ok(mut submodule) = repo.find_submodule(file) { let _ = submodule.add_to_index(true); } continue; }
        if absolute.exists() { index.add_all([relative], git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?; } else { let _ = index.remove_all([relative], None); }
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
    let repo = internal_repository(&repository_path)?; let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.push_head().map_err(|error| error.message().to_string())?; let _ = walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME);

    struct Walked { oid: git2::Oid, parent_ids: Vec<git2::Oid>, included: bool, subject: String, author: String, date: String }
    let mut walked = Vec::new();
    for oid in walk.flatten().take(500) { if let Ok(commit) = repo.find_commit(oid) {
        let included = if relative.as_os_str().is_empty() { true } else { let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok()); let mut options = git2::DiffOptions::new(); options.pathspec(&relative); if let Ok(tree) = commit.tree() { repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options)).map(|diff| diff.deltas().next().is_some()).unwrap_or(false) } else { false } };
        walked.push(Walked { oid, parent_ids: commit.parent_ids().collect(), included, subject: commit.summary().unwrap_or("No message").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()) });
    } }

    // A commit's real Git parent may not itself have touched this path, so it
    // was never walked into the filtered list above — naively keeping the raw
    // parent id then produces a dangling reference to a commit that doesn't
    // exist in the response, which the graph renderer has no choice but to
    // drop, making that lane look like it starts or ends with no explanation.
    // Re-link every included commit to its nearest *included* ancestor(s),
    // skipping over excluded commits transitively — the same history
    // simplification plain `git log -- <path>` does — so the visible commits
    // keep their real, correct ancestry.
    let included_ids: HashSet<git2::Oid> = walked.iter().filter(|entry| entry.included).map(|entry| entry.oid).collect();
    let mut resolved: HashMap<git2::Oid, Vec<git2::Oid>> = HashMap::new();
    for entry in walked.iter().rev() {
        let mut ancestors = Vec::new();
        for parent in &entry.parent_ids {
            if included_ids.contains(parent) { if !ancestors.contains(parent) { ancestors.push(*parent); } }
            else if let Some(grand) = resolved.get(parent) { for grand_id in grand { if !ancestors.contains(grand_id) { ancestors.push(*grand_id); } } }
        }
        resolved.insert(entry.oid, ancestors);
    }

    Ok(walked.into_iter().filter(|entry| entry.included).map(|entry| {
        let parents = resolved.remove(&entry.oid).unwrap_or_default();
        Commit { id: entry.oid.to_string(), parents: parents.into_iter().map(|oid| oid.to_string()).collect(), subject: entry.subject, author: entry.author, date: entry.date, refs: Vec::new(), lane: 0 }
    }).collect())
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

// Stashes a single file/folder instead of the whole working tree. libgit2's
// stash API has no pathspec filter (it always stashes everything), so this
// shells out to real `git stash push -- <path>` — exactly what the CLI does
// under the hood for a scoped stash — through the same guarded `git()`
// helper used by the raw-console escape hatch (no shell, no credential
// prompt hang).
#[tauri::command]
pub fn stash_file(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if let Some((sub_path, inner_relative)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        if !inner_relative.is_empty() { return stash_file(sub_path, inner_relative); }
    }
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    if relative_string.is_empty() { return Err("Select a specific file or folder to stash".into()); }
    git(&repository_path, &["stash", "push", "--include-untracked", "--", &relative_string])?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn pop_stash(repository_path: String, stash_index: usize) -> Result<(), String> {
    validate_path(&repository_path)?;
    let mut repo = internal_repository(&repository_path)?;
    let mut options = git2::StashApplyOptions::new();
    // `stash_pop` (apply + drop) drops the stash entry unconditionally on a
    // successful *apply* — but libgit2 considers merge-style conflict markers
    // written into the index/workdir a successful apply, not a failure. Doing
    // apply and drop as two separate steps, only dropping when the apply left
    // no conflicts, mirrors the real `git stash pop` CLI's own safety net: a
    // conflicted pop keeps the stash entry around (the change is now merged
    // into the working tree either way, conflicted or not) so nothing is
    // silently lost if the conflict resolution is abandoned instead of
    // finished. Popping anything other than index 0 (not just the most
    // recent stash) lets the Stashes list restore a specific entry directly.
    repo.stash_apply(stash_index, Some(&mut options)).map_err(|error| format!("Cannot restore stashed work: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    let has_conflicts = repo.index().map(|index| index.has_conflicts()).unwrap_or(false);
    if !has_conflicts { repo.stash_drop(stash_index).map_err(|error| format!("Restored, but could not drop the stash entry: {}", error.message()))?; }
    Ok(())
}

// Discards a stash entry without applying it — for when you decide you don't
// need it after all, from the Stashes list.
#[tauri::command]
pub fn drop_stash(repository_path: String, stash_index: usize) -> Result<(), String> {
    validate_path(&repository_path)?;
    let mut repo = internal_repository(&repository_path)?;
    repo.stash_drop(stash_index).map_err(|error| format!("Cannot drop this stash: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

// Applies only the chosen files from a stash — not the whole entry. Uses
// git2's own checkout-path filtering (the same mechanism `stash_apply` uses
// internally to write the merged result to disk) so it's a real, correct
// git merge of just those paths, not a hand-rolled diff. The stash entry
// itself is left exactly as it was — nothing is dropped or rewritten —
// so restoring a few files first and the rest later is always safe; picking
// the same file again just re-applies the same (already-matching) content.
#[tauri::command]
pub fn restore_stash_paths(repository_path: String, stash_index: usize, paths: Vec<String>) -> Result<(), String> {
    validate_path(&repository_path)?;
    if paths.is_empty() { return Err("Select at least one file to restore".into()); }
    let selected: HashSet<String> = paths.iter().map(|path| safe_relative_path(path).map(|p| normalized(&p))).collect::<Result<_, _>>()?;
    let all_files = stash_entry_files(repository_path.clone(), stash_index)?;
    let remaining: Vec<String> = all_files.into_iter().filter(|file| !selected.contains(file)).collect();

    // The stash this entry becomes is identified by its own commit id, not
    // its stack position — pushing a fresh stash for the leftover files
    // below shifts every later entry's index up by one, so this entry has
    // to be found again afterward rather than assumed to still be at
    // `stash_index`.
    let original_oid = {
        let mut repo = internal_repository(&repository_path)?;
        let mut oid = None;
        repo.stash_foreach(|index, _, found| { if index == stash_index { oid = Some(*found); } true }).map_err(|error| error.message().to_string())?;
        oid.ok_or("That stash entry no longer exists")?
    };

    let mut repo = internal_repository(&repository_path)?;
    let mut options = git2::StashApplyOptions::new();
    // Apply the whole entry, not just the selected paths — a path-filtered
    // apply left the stash's own tree completely unaffected, so a restored
    // file still looked "still stashed" the moment the list was refreshed.
    // Applying everything, then re-stashing just the leftovers below, is
    // what actually makes a restored file gone from the entry for good.
    repo.stash_apply(stash_index, Some(&mut options)).map_err(|error| format!("Cannot restore: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);

    if repo.index().map(|index| index.has_conflicts()).unwrap_or(false) {
        // Leave the entry exactly as it is — same safety net as a full pop
        // — the caller's conflict-resolution flow takes over from here.
        return Ok(());
    }

    if remaining.is_empty() {
        // Nothing left to keep stashed: this was effectively a full pop.
        drop(repo);
        let mut repo = internal_repository(&repository_path)?;
        repo.stash_drop(stash_index).map_err(|error| format!("Restored, but could not drop the now-empty stash entry: {}", error.message()))?;
        return Ok(());
    }

    // Put the untouched files back into a fresh stash entry of their own —
    // real `git stash push` scoped to just those paths, so the just-restored
    // files stay exactly as they are: live, ordinary working-tree changes.
    drop(repo);
    let mut push_args = vec!["stash", "push", "--include-untracked", "--"];
    push_args.extend(remaining.iter().map(|path| path.as_str()));
    git(&repository_path, &push_args)?;

    let mut repo = internal_repository(&repository_path)?;
    let mut old_index = None;
    repo.stash_foreach(|index, _, found| { if *found == original_oid { old_index = Some(index); } true }).map_err(|error| error.message().to_string())?;
    if let Some(index) = old_index { repo.stash_drop(index).map_err(|error| format!("Restored, but could not clean up the original stash entry: {}", error.message()))?; }
    invalidate_git_metadata(&repository_path);
    Ok(())
}

// Discards a stash pop's conflict markers (working tree + index reset to
// HEAD) without touching the stash list — used when the user backs out of
// resolving a stash conflict instead of finishing it. Since `pop_stash` only
// drops the stash entry on a clean apply, the stashed change is still there
// to try again (or to pop and resolve differently) afterward.
#[tauri::command]
pub fn abort_stash_conflict(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let head_commit = repo.head().map_err(|error| error.message().to_string())?.peel_to_commit().map_err(|error| error.message().to_string())?;
    let mut checkout = git2::build::CheckoutBuilder::new(); checkout.force();
    repo.reset(head_commit.as_object(), git2::ResetType::Hard, Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

// Lists the files a specific stash entry would bring back — so "what's in
// this stash?" can be answered before popping it, not just after.
#[tauri::command]
pub fn stash_entry_files(repository_path: String, stash_index: usize) -> Result<Vec<String>, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let mut target_oid = None;
    let mut repo_for_walk = internal_repository(&repository_path)?;
    repo_for_walk.stash_foreach(|index, _, oid| { if index == stash_index { target_oid = Some(*oid); } true }).map_err(|error| error.message().to_string())?;
    let stash_oid = target_oid.ok_or("That stash entry no longer exists")?;
    let stash_commit = repo.find_commit(stash_oid).map_err(|error| error.message().to_string())?;
    let stash_tree = stash_commit.tree().map_err(|error| error.message().to_string())?;
    // Parent 0 is the commit the stash was based on (HEAD at stash time) —
    // diffing against it gives the tracked files this stash would change.
    // A brand new (untracked) file stashed with it never showed up in that
    // diff, though: it isn't recorded in the stash's own top tree at all —
    // only in a separate third parent commit (present only when the stash
    // included untracked files), which needs its own diff against the same
    // base to be found. Missing this made "Stash work" (which includes
    // untracked files by default) show none of its new files in the list.
    let base_tree = stash_commit.parent(0).and_then(|commit| commit.tree()).ok();
    let mut paths = std::collections::BTreeSet::new();
    let diff = repo.diff_tree_to_tree(base_tree.as_ref(), Some(&stash_tree), None).map_err(|error| error.message().to_string())?;
    for delta in diff.deltas() { if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) { paths.insert(normalized(path)); } }
    if let Some(untracked_tree) = stash_commit.parent(2).ok().and_then(|commit| commit.tree().ok()) {
        // The untracked-files parent's tree contains *only* the untracked
        // files themselves, not a full workdir snapshot — diffing it against
        // `base_tree` (which has every tracked file) would wrongly report
        // every tracked file base has and this tree doesn't as "deleted".
        // Diffing against an empty tree instead just lists what's actually
        // in it.
        let untracked_diff = repo.diff_tree_to_tree(None, Some(&untracked_tree), None).map_err(|error| error.message().to_string())?;
        for delta in untracked_diff.deltas() { if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) { paths.insert(normalized(path)); } }
    }
    Ok(paths.into_iter().collect())
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
    // Once the submodule itself has a new commit, its working copy already IS
    // the new version — record that in the parent right away instead of
    // leaving the two in sync only after a manual "Change version"/stage step.
    // Not having pushed yet doesn't change this: the parent should show "this
    // submodule is now on version X", not "modified", the moment X actually
    // exists as a real commit here, pushed or not.
    record_pushed_submodule_in_parent(&repository_path, &relative_path, Some(oid))?;
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

    // `dirty`/status checks above ran against the submodule's own cached status
    // entries (keyed by `sub_path`), separate from the parent's cache that
    // `record_pushed_submodule_in_parent` invalidates below — without this, opening
    // the submodule as its own repository view right after a push could still show
    // its pre-push status for up to the cache's TTL.
    invalidate_git_metadata(&sub_path);
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

    invalidate_git_metadata(&sub_path);
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
        assert!(!load_repository(parent_string.clone(), Some(true)).unwrap().changes.iter().any(|change| change.path == "README.md"));
        let added = add_submodule(parent_string.clone(), "components".into(), dependency.to_string_lossy().into_owned(), "engine".into(), String::new(), String::new()).unwrap();
        assert_eq!(added, "components/engine");
        assert!(parent.join(".gitmodules").exists());
        let repo = Repository::open(&parent).unwrap();
        assert_eq!(repo.index().unwrap().get_path(Path::new("components/engine"), 0).unwrap().mode, 0o160000);
        drop(repo);
        create_commit(parent_string.clone(), "P:89312 add engine".into()).unwrap();
        assert_eq!(entry_details(parent_string.clone(), "README.md".into()).unwrap().last_commit_subject.as_deref(), Some("Initial commit"));
        let engine_details = entry_details(parent_string.clone(), "components/engine".into()).unwrap();
        assert_eq!(engine_details.last_commit_subject.as_deref(), Some("P:89312 add engine"), "last_commit_* must stay the parent's gitlink-bump commit");
        assert_eq!(engine_details.submodule_commit_subject.as_deref(), Some("Initial commit"), "submodule_commit_* must be the submodule's own HEAD commit, not the parent's");
        assert!(engine_details.submodule_commit_id.is_some());
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
        assert_eq!(load_repository(cloned.clone(), Some(true)).unwrap().repository.name, "cloned-dependency");
        remove_git_path(cloned.clone(), "README.md".into()).unwrap();
        assert!(!Path::new(&cloned).join("README.md").exists());
        assert!(git(&cloned, &["diff", "--cached", "--name-only"]).unwrap().lines().any(|path| path == "README.md"));
        assert_eq!(browser_repository_url("git@github.com:team/project.git").as_deref(), Some("https://github.com/team/project"));
        assert_eq!(browser_repository_url("https://gitlab.example/team/project.git").as_deref(), Some("https://gitlab.example/team/project"));

        fs::remove_dir_all(base).unwrap();
    }

    fn setup_diverged_repo(suffix: u128) -> (PathBuf, String) {
        let repository = std::env::temp_dir().join(format!("git-integrity-merge-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "base\n").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Base"]);
        run_git(&repository, &["switch", "-c", "feature"]);
        (repository, "main".into())
    }

    #[test]
    fn merge_branch_fast_forwards_when_possible() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let (repository, _) = setup_diverged_repo(suffix);
        // feature has no new commits yet — main advances, feature should fast-forward to it.
        run_git(&repository, &["switch", "main"]);
        fs::write(repository.join("a.txt"), "advanced\n").unwrap();
        run_git(&repository, &["commit", "-am", "Advance main"]);
        run_git(&repository, &["switch", "feature"]);
        let path = repository.to_string_lossy().into_owned();
        let outcome = merge_branch(path.clone(), "".into(), "main".into()).unwrap();
        assert_eq!(outcome.status, "fast_forwarded");
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "advanced\n");
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn merge_branch_auto_merges_non_conflicting_changes() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let (repository, _) = setup_diverged_repo(suffix);
        fs::write(repository.join("b.txt"), "from feature\n").unwrap();
        run_git(&repository, &["add", "."]); run_git(&repository, &["commit", "-m", "Feature adds b.txt"]);
        run_git(&repository, &["switch", "main"]);
        fs::write(repository.join("c.txt"), "from main\n").unwrap();
        run_git(&repository, &["add", "."]); run_git(&repository, &["commit", "-m", "Main adds c.txt"]);
        run_git(&repository, &["switch", "feature"]);
        let path = repository.to_string_lossy().into_owned();
        let outcome = merge_branch(path.clone(), "".into(), "main".into()).unwrap();
        assert_eq!(outcome.status, "merged");
        assert!(repository.join("b.txt").exists()); assert!(repository.join("c.txt").exists());
        let repo = Repository::open(&repository).unwrap();
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().parent_count(), 2);
        assert_eq!(repo.state(), git2::RepositoryState::Clean, "a clean auto-merge must not leave the repo in a pending-merge state");
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn merge_branch_reports_conflicts_and_resolve_and_complete_work() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let (repository, _) = setup_diverged_repo(suffix);
        fs::write(repository.join("a.txt"), "feature version\n").unwrap();
        run_git(&repository, &["commit", "-am", "Feature changes a.txt"]);
        run_git(&repository, &["switch", "main"]);
        fs::write(repository.join("a.txt"), "main version\n").unwrap();
        run_git(&repository, &["commit", "-am", "Main changes a.txt"]);
        run_git(&repository, &["switch", "feature"]);
        let path = repository.to_string_lossy().into_owned();

        let outcome = merge_branch(path.clone(), "".into(), "main".into()).unwrap();
        assert_eq!(outcome.status, "conflicts");
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.conflicts[0].path, "a.txt");
        let repo = Repository::open(&repository).unwrap();
        assert_eq!(repo.state(), git2::RepositoryState::Merge);
        assert!(merge_in_progress(path.clone(), "".into()).unwrap(), "a real merge conflict should report merge_in_progress");
        let on_disk = fs::read_to_string(repository.join("a.txt")).unwrap();
        assert!(on_disk.contains("<<<<<<<"), "conflict markers should be written to disk");

        let sides = conflict_sides(path.clone(), "".into(), "a.txt".into()).unwrap();
        assert_eq!(sides.ours.as_deref(), Some("feature version\n"));
        assert_eq!(sides.theirs.as_deref(), Some("main version\n"));

        // Completing before resolving must fail — no silent commit with conflict markers baked in.
        assert!(complete_merge(path.clone(), "".into(), "Merge main".into()).is_err());

        resolve_conflict(path.clone(), "".into(), "a.txt".into(), "theirs".into()).unwrap();
        assert!(list_conflicts(path.clone(), "".into()).unwrap().is_empty());
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "main version\n");

        let oid = complete_merge(path.clone(), "".into(), "Merge main into feature".into()).unwrap();
        assert!(!oid.is_empty());
        let repo = Repository::open(&repository).unwrap();
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().parent_count(), 2);
        assert!(load_repository(path, Some(true)).unwrap().changes.is_empty());

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn abort_merge_restores_the_pre_merge_state() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let (repository, _) = setup_diverged_repo(suffix);
        fs::write(repository.join("a.txt"), "feature version\n").unwrap();
        run_git(&repository, &["commit", "-am", "Feature changes a.txt"]);
        run_git(&repository, &["switch", "main"]);
        fs::write(repository.join("a.txt"), "main version\n").unwrap();
        run_git(&repository, &["commit", "-am", "Main changes a.txt"]);
        run_git(&repository, &["switch", "feature"]);
        let path = repository.to_string_lossy().into_owned();

        merge_branch(path.clone(), "".into(), "main".into()).unwrap();
        assert_eq!(Repository::open(&repository).unwrap().state(), git2::RepositoryState::Merge);

        abort_merge(path.clone(), "".into()).unwrap();
        let repo = Repository::open(&repository).unwrap();
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "feature version\n", "aborting must restore the pre-merge working tree");
        assert!(load_repository(path, Some(true)).unwrap().changes.is_empty());

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn merge_and_conflict_commands_can_target_a_submodule_by_relative_path() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-merge-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        let sub_path = parent.join(&added);
        run_git(&sub_path, &["switch", "-c", "feature"]);
        fs::write(sub_path.join("module.txt"), "feature version\n").unwrap();
        run_git(&sub_path, &["commit", "-am", "Feature change"]);
        run_git(&sub_path, &["switch", "master"]);
        fs::write(sub_path.join("module.txt"), "master version\n").unwrap();
        run_git(&sub_path, &["commit", "-am", "Master change"]);
        run_git(&sub_path, &["switch", "feature"]);

        // Everything below is called with (parent_path, target_path=<submodule path>),
        // never a raw absolute path — this is exactly how the frontend addresses a
        // submodule for every other action in this app.
        let outcome = merge_branch(parent_string.clone(), added.clone(), "master".into()).unwrap();
        assert_eq!(outcome.status, "conflicts");
        assert_eq!(list_conflicts(parent_string.clone(), added.clone()).unwrap().len(), 1);

        resolve_conflict(parent_string.clone(), added.clone(), "module.txt".into(), "theirs".into()).unwrap();
        assert!(list_conflicts(parent_string.clone(), added.clone()).unwrap().is_empty());
        let oid = complete_merge(parent_string.clone(), added.clone(), "Merge master into feature".into()).unwrap();
        assert!(!oid.is_empty());
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "master version\n");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn committing_the_whole_repository_with_nothing_selected_works() {
        // "Commit repository" (nothing selected, browsing at the root) sends an
        // empty relative_path — commit_path used to turn that into pathspec "."
        // which git2 rejects outright, so this always failed, for any change.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-root-commit-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);

        fs::write(repository.join("a.txt"), "two").unwrap();
        fs::write(repository.join("new.txt"), "brand new").unwrap();
        let path = repository.to_string_lossy().into_owned();
        let committed = commit_path(path.clone(), "".into(), "Commit everything".into()).unwrap();
        assert!(!committed.is_empty());
        let head_files = git(&path, &["show", "--pretty=format:", "--name-only", "HEAD"]).unwrap();
        assert!(head_files.lines().any(|p| p == "a.txt"));
        assert!(head_files.lines().any(|p| p == "new.txt"));
        assert!(load_repository(path, Some(true)).unwrap().changes.is_empty(), "nothing should be left pending after committing the whole repository");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn fetch_all_remotes_updates_every_configured_remote_not_just_the_first() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-fetch-all-{suffix}"));
        let repository = base.join("main");
        let origin_remote = base.join("origin.git");
        let upstream_remote = base.join("upstream.git");
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        run_git(&base, &["init", "--bare", "origin.git"]);
        run_git(&base, &["init", "--bare", "upstream.git"]);
        run_git(&repository, &["remote", "add", "origin", origin_remote.to_str().unwrap()]);
        run_git(&repository, &["remote", "add", "upstream", upstream_remote.to_str().unwrap()]);
        run_git(&repository, &["push", "origin", "main"]);
        run_git(&repository, &["push", "upstream", "main"]);

        // Simulate someone else pushing directly to each bare remote, so a
        // fetch is the only way this checkout would find out about them.
        let clone_origin = base.join("clone-origin");
        run_git(&base, &["clone", origin_remote.to_str().unwrap(), clone_origin.to_str().unwrap()]);
        run_git(&clone_origin, &["config", "user.email", "test@example.com"]);
        run_git(&clone_origin, &["config", "user.name", "Test User"]);
        fs::write(clone_origin.join("a.txt"), "from origin").unwrap();
        run_git(&clone_origin, &["commit", "-am", "New on origin"]);
        run_git(&clone_origin, &["push", "origin", "main"]);

        let clone_upstream = base.join("clone-upstream");
        run_git(&base, &["clone", upstream_remote.to_str().unwrap(), clone_upstream.to_str().unwrap()]);
        run_git(&clone_upstream, &["config", "user.email", "test@example.com"]);
        run_git(&clone_upstream, &["config", "user.name", "Test User"]);
        fs::write(clone_upstream.join("a.txt"), "from upstream").unwrap();
        run_git(&clone_upstream, &["commit", "-am", "New on upstream"]);
        run_git(&clone_upstream, &["push", "origin", "main"]);

        let path = repository.to_string_lossy().into_owned();
        fetch_all_remotes(path.clone()).unwrap();
        let origin_head = git(&path, &["rev-parse", "refs/remotes/origin/main"]).unwrap().trim().to_string();
        let upstream_head = git(&path, &["rev-parse", "refs/remotes/upstream/main"]).unwrap().trim().to_string();
        let expected_origin = git(clone_origin.to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let expected_upstream = git(clone_upstream.to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        assert_eq!(origin_head, expected_origin, "origin should be up to date after fetch-all");
        assert_eq!(upstream_head, expected_upstream, "upstream should be up to date after fetch-all too, not just the first remote");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn committing_a_folder_with_a_modified_file_inside_it_works() {
        // Reported: "Commit this item" on a folder failed with "cannot create
        // blob from '<path>/admin': it is a directory" — the post-commit index
        // re-sync used `index.add_path`, which only accepts an actual file
        // blob, never a directory. `commit_selected_internal`'s first pass
        // (building the commit tree) already used `add_all`, which expands a
        // folder recursively; the second pass (re-syncing the on-disk index
        // after the commit) must do the same.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-folder-commit-{suffix}"));
        let admin = repository.join("admin");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);

        fs::write(admin.join("a.txt"), "two").unwrap();
        fs::write(admin.join("new.txt"), "brand new").unwrap();
        let path = repository.to_string_lossy().into_owned();
        let committed = commit_path(path.clone(), "admin".into(), "Commit admin folder".into()).unwrap();
        assert!(!committed.is_empty());
        let head_files = git(&path, &["show", "--pretty=format:", "--name-only", "HEAD"]).unwrap();
        assert!(head_files.lines().any(|p| p == "admin/a.txt"));
        assert!(head_files.lines().any(|p| p == "admin/new.txt"));
        assert!(load_repository(path, Some(true)).unwrap().changes.is_empty(), "nothing should be left pending after committing the folder");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn committing_the_whole_repository_picks_up_a_submodule_advanced_outside_the_app() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-root-commit-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        // Advance the submodule's HEAD the way a user would from a plain terminal
        // (or another tool) — entirely outside this app, so nothing here ever calls
        // `add_to_index` on it.
        run_git(&parent.join(&added), &["commit", "--allow-empty", "-m", "External commit inside submodule"]);
        let sub_repo = Repository::open(parent.join(&added)).unwrap();
        let expected_oid = sub_repo.head().unwrap().target().unwrap();

        // "Commit repository" with nothing selected uses relative_path == "" → pathspec "."
        commit_path(parent_string.clone(), "".into(), "Bump submodule".into()).unwrap();

        let repo = Repository::open(&parent).unwrap();
        let recorded_oid = repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id;
        assert_eq!(recorded_oid, expected_oid, "committing the whole repository should pick up the submodule's current HEAD even if it advanced outside the app");

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

        let data = load_repository(path.clone(), Some(true)).unwrap();
        assert!(!data.commits.iter().any(|commit| commit.parents.len() > 1), "the WIP stash commit (with its index/untracked parents) must never appear as a graph commit");
        assert!(!data.commits.iter().any(|commit| commit.refs.iter().any(|r| r == "stash")), "refs/stash must not be attached as a label on any commit");
        assert_eq!(data.stashes.len(), 1);
        assert_eq!(data.stashes[0].base_commit, base);

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn stash_file_sets_aside_only_the_chosen_file_leaving_others_modified() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-one-file-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        fs::write(repository.join("b.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);

        fs::write(repository.join("a.txt"), "two").unwrap();
        fs::write(repository.join("b.txt"), "two").unwrap();
        let path = repository.to_string_lossy().into_owned();
        stash_file(path.clone(), "a.txt".into()).unwrap();

        let data = load_repository(path.clone(), Some(true)).unwrap();
        assert!(!data.changes.iter().any(|change| change.path == "a.txt"), "a.txt should be set aside by the stash, not showing as a pending change");
        assert!(data.changes.iter().any(|change| change.path == "b.txt"), "b.txt was never selected — it must stay modified, untouched by the scoped stash");
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "one", "a.txt on disk should be back to the committed version once stashed");
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "two", "b.txt's edit must be left alone on disk");
        assert_eq!(data.stashes.len(), 1);

        pop_stash(path.clone(), 0).unwrap();
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "two", "popping the stash should bring a.txt's edit back");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn popping_a_conflicting_stash_keeps_the_entry_until_resolved_and_finished() {
        // Reported: "Conflicts while restoring stash. Resolve manually." with
        // no way to see what the stash contained or actually resolve it.
        // libgit2's stash_pop drops the stash entry on any *successful apply*
        // — but it treats conflict markers written into the index/workdir as
        // a successful apply, so the old implementation (apply+drop as one
        // call) silently discarded the stash even when conflicted, with no
        // way to recover it if the user backed out. This verifies the fix:
        // the entry survives a conflicted pop, resolving it via the normal
        // conflict tools clears it, and only then is the stash actually gone.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-conflict-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "base\n").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::write(repository.join("a.txt"), "stashed version\n").unwrap();
        stash_changes(path.clone()).unwrap();
        fs::write(repository.join("a.txt"), "conflicting new version\n").unwrap();
        run_git(&repository, &["commit", "-am", "Conflicting commit"]);

        pop_stash(path.clone(), 0).unwrap();
        let conflicts = list_conflicts(path.clone(), "".into()).unwrap();
        assert_eq!(conflicts.len(), 1, "a.txt should be listed as conflicted");
        assert!(!merge_in_progress(path.clone(), "".into()).unwrap(), "a stash-pop conflict must not be mistaken for a real merge in progress");
        assert!(load_repository(path.clone(), Some(true)).unwrap().stashes.len() == 1, "the stash entry must survive a conflicted pop, not be silently dropped");

        resolve_conflict(path.clone(), "".into(), "a.txt".into(), "theirs".into()).unwrap();
        assert!(list_conflicts(path.clone(), "".into()).unwrap().is_empty(), "resolving the only conflict should clear the list");
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "stashed version\n", "resolving to 'theirs' should keep the stashed content");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn aborting_a_stash_conflict_restores_head_and_keeps_the_stash_for_another_try() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-conflict-abort-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "base\n").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::write(repository.join("a.txt"), "stashed version\n").unwrap();
        stash_changes(path.clone()).unwrap();
        fs::write(repository.join("a.txt"), "conflicting new version\n").unwrap();
        run_git(&repository, &["commit", "-am", "Conflicting commit"]);
        pop_stash(path.clone(), 0).unwrap();

        abort_stash_conflict(path.clone()).unwrap();
        assert!(list_conflicts(path.clone(), "".into()).unwrap().is_empty(), "aborting should clear the conflict");
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "conflicting new version\n", "the working tree should be back to HEAD, not left half-merged");
        assert_eq!(load_repository(path.clone(), Some(true)).unwrap().stashes.len(), 1, "the stash itself must still be there to try again");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn stash_entry_files_lists_what_a_stash_would_change() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-file-list-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        fs::write(repository.join("b.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::write(repository.join("a.txt"), "two").unwrap();
        stash_file(path.clone(), "a.txt".into()).unwrap();

        let files = stash_entry_files(path.clone(), 0).unwrap();
        assert_eq!(files, vec!["a.txt".to_string()], "only a.txt was stashed — b.txt was never touched");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn stashes_can_be_popped_or_dropped_individually_by_index() {
        // The Stashes list needs to act on a *specific* entry, not always
        // "the most recent one" — verifies both pop_stash and drop_stash
        // take that index seriously rather than always touching index 0.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-by-index-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        fs::write(repository.join("b.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::write(repository.join("a.txt"), "a changed first").unwrap();
        stash_file(path.clone(), "a.txt".into()).unwrap();
        fs::write(repository.join("b.txt"), "b changed second").unwrap();
        stash_file(path.clone(), "b.txt".into()).unwrap();
        // Most recent stash (index 0) is now the b.txt one; a.txt's is index 1.
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["b.txt".to_string()]);
        assert_eq!(stash_entry_files(path.clone(), 1).unwrap(), vec!["a.txt".to_string()]);

        drop_stash(path.clone(), 1).unwrap();
        assert_eq!(load_repository(path.clone(), Some(true)).unwrap().stashes.len(), 1, "dropping index 1 should leave only the b.txt stash");
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "one", "dropping never applies the change — a.txt stays at its committed content");

        pop_stash(path.clone(), 0).unwrap();
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "b changed second", "popping should bring the change back");
        assert!(load_repository(path.clone(), Some(true)).unwrap().stashes.is_empty(), "the only remaining stash should be gone after a clean pop");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn restore_stash_paths_applies_only_the_chosen_files_and_leaves_the_rest_stashed() {
        // Reported: expected to pick individual files (or folders) out of a
        // stash one at a time, not always all-or-nothing. (Restoring one
        // file genuinely removing it from the stash's own list afterward is
        // covered separately, by restoring_a_file_actually_removes_it_from_
        // the_stash_afterward.)
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-partial-restore-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        fs::write(repository.join("b.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::write(repository.join("a.txt"), "a changed").unwrap();
        fs::write(repository.join("b.txt"), "b changed").unwrap();
        stash_changes(path.clone()).unwrap();

        restore_stash_paths(path.clone(), 0, vec!["a.txt".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "a changed", "a.txt should be restored");
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "one", "b.txt was not selected — it must stay untouched, still only in the stash");
        assert_eq!(load_repository(path.clone(), Some(true)).unwrap().stashes.len(), 1, "a stash entry must remain for the still-unrestored b.txt");

        restore_stash_paths(path.clone(), 0, vec!["b.txt".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "b changed", "b.txt should now be restored too");
        assert!(load_repository(path.clone(), Some(true)).unwrap().stashes.is_empty(), "nothing left stashed once both files are restored");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn stash_entry_files_includes_untracked_files_not_just_tracked_ones() {
        // Reported: "the Restore button does nothing" — root cause was that
        // a stash including an untracked (brand new) file never showed that
        // file in the list at all, so there was nothing to actually click.
        // A stash's own top-level tree only reflects *tracked* changes;
        // untracked files stashed alongside them live only in a separate
        // third parent commit, present whenever untracked files were
        // included (the default for "Stash work") — which the original
        // diff-against-just-the-base-commit missed entirely.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-untracked-list-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        // A stash containing *only* a brand new untracked file (nothing
        // tracked touched at all) — the case that returned an empty list.
        fs::write(repository.join("new.txt"), "brand new").unwrap();
        stash_file(path.clone(), "new.txt".into()).unwrap();
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["new.txt".to_string()]);
        restore_stash_paths(path.clone(), 0, vec!["new.txt".into()]).unwrap();
        assert!(repository.join("new.txt").exists(), "the untracked file should actually be restored to disk");

        // The more common case: "Stash work" mixing a tracked-modified file
        // with a brand new untracked one in the same stash. (new.txt is
        // still sitting on disk, untracked, from the restore above — it
        // legitimately gets swept into this stash too.)
        fs::write(repository.join("a.txt"), "modified").unwrap();
        fs::write(repository.join("brand.txt"), "brand new too").unwrap();
        stash_changes(path.clone()).unwrap();
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["a.txt".to_string(), "brand.txt".to_string(), "new.txt".to_string()], "the tracked file and both untracked files must all be listed");
        restore_stash_paths(path.clone(), 0, vec!["a.txt".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "modified", "restoring the tracked file should still work when the stash also has an untracked one");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn restoring_a_file_actually_removes_it_from_the_stash_afterward() {
        // Reported: after restoring a file, it kept showing up in the list
        // again — because the earlier implementation only checked the
        // selected files out of the stash without ever touching the stash
        // entry itself, so the immutable stash commit still "contained" it
        // regardless. Restoring must leave it genuinely gone from that
        // stash: remaining files (if any) end up in a fresh stash entry of
        // their own; if nothing is left, the old entry is dropped outright.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-shrink-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::create_dir_all(repository.join("OrdersFromSite")).unwrap();
        fs::write(repository.join("OrdersFromSite/ordersForm.css"), "body{}").unwrap();
        fs::write(repository.join("README.md"), "changed").unwrap();
        stash_changes(path.clone()).unwrap();
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["OrdersFromSite/ordersForm.css".to_string(), "README.md".to_string()]);

        restore_stash_paths(path.clone(), 0, vec!["OrdersFromSite/ordersForm.css".into()]).unwrap();
        assert!(repository.join("OrdersFromSite/ordersForm.css").exists(), "the restored file should be on disk");
        assert_eq!(load_repository(path.clone(), Some(true)).unwrap().stashes.len(), 1, "README.md is still unrestored — a stash entry should remain for it");
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["README.md".to_string()], "the restored file must be gone from the stash's own list now — not still shown as if untouched");

        restore_stash_paths(path.clone(), 0, vec!["README.md".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("README.md")).unwrap(), "changed");
        assert!(load_repository(path.clone(), Some(true)).unwrap().stashes.is_empty(), "restoring the last remaining file should drop the now-empty stash entry entirely");

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
        assert!(load_repository(path.clone(), Some(true)).unwrap().changes.iter().any(|change| change.path == "intro.css" && change.staged));

        commit_files(path.clone(), vec!["intro.css".into()], "Update intro.css".into()).unwrap();
        assert!(!load_repository(path.clone(), Some(true)).unwrap().changes.iter().any(|change| change.path == "intro.css"), "intro.css should no longer appear as a pending change right after commit");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn staging_and_unstaging_a_nested_file_actually_toggles_its_staged_flag() {
        // Reported: unchecking a file's checkbox in the Working tree drawer
        // still showed it as staged afterwards. Reproduces the checkbox's
        // exact round trip (stage, then unstage) on a file inside a
        // subfolder — not just at the repo root — since a path-separator
        // mismatch would only show up once a path has an actual subfolder
        // component in it.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-unstage-nested-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        fs::create_dir_all(base.join("src")).unwrap();
        fs::write(base.join("src/lib.rs"), "// existing").unwrap();
        run_git(&base, &["add", "."]); run_git(&base, &["commit", "-m", "Add src/lib.rs"]);
        // "src" must already be a tracked folder before this, not a wholly-new
        // one — a wholly-untracked directory is reported as a single status
        // entry for the folder itself (a real, separate perf optimization, not
        // a bug), which would make a nested new file's own path never appear
        // and produce a false positive here.
        fs::write(base.join("src/main.rs"), "fn main() {}").unwrap();
        let path = base.to_string_lossy().into_owned();

        stage_files(path.clone(), vec!["src/main.rs".into()]).unwrap();
        let staged = load_repository(path.clone(), Some(true)).unwrap();
        let change = staged.changes.iter().find(|change| change.path == "src/main.rs").expect("src/main.rs should be a pending change");
        assert!(change.staged, "src/main.rs should be staged after stage_files");

        unstage_files(path.clone(), vec!["src/main.rs".into()]).unwrap();
        let unstaged = load_repository(path.clone(), Some(true)).unwrap();
        let change = unstaged.changes.iter().find(|change| change.path == "src/main.rs").expect("src/main.rs should still be a pending change (untracked, not staged)");
        assert!(!change.staged, "src/main.rs should no longer be staged after unstage_files");

        fs::remove_dir_all(base).unwrap();
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

        // Committing inside the submodule now updates the parent's recorded
        // gitlink right away (no push needed) — the submodule's working copy
        // already IS the new version, so the parent should reflect that
        // immediately instead of still showing it as "modified".
        let parent_index_oid = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new("vendor/dep"), 0).unwrap().id };
        let submodule_head_oid = { let repo = Repository::open(&sub_path).unwrap(); let oid = repo.head().unwrap().target().unwrap(); oid };
        assert_eq!(parent_index_oid, submodule_head_oid, "the parent should record the submodule's new commit immediately after committing inside it, push or not");

        let parent_changes = load_repository(repo_path.clone(), Some(true)).unwrap().changes;
        assert!(!parent_changes.iter().any(|change| change.path == "vendor/dep"), "the submodule should already show as clean/version-changed, not modified, before any push");

        // Now push should succeed, and — since the commit is now safely on the
        // submodule's own server — the parent should be updated automatically so the
        // submodule stops showing as merely "modified locally".
        push_submodule(repo_path.clone(), "vendor/dep".into()).expect("push_submodule should succeed after a commit");
        let parent_index_oid_after_push = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new("vendor/dep"), 0).unwrap().id };
        let submodule_head_oid_after_push = { let repo = Repository::open(&sub_path).unwrap(); let oid = repo.head().unwrap().target().unwrap(); oid };
        assert_eq!(parent_index_oid_after_push, submodule_head_oid_after_push, "after a successful push, the parent should automatically record the new submodule commit");
        let parent_changes_after_push = load_repository(repo_path.clone(), Some(true)).unwrap().changes;
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
    fn switching_to_a_remote_branch_lands_attached_not_detached() {
        // Picking a remote-tracking entry like "origin/main" from "Change version"
        // used to always leave the submodule in detached HEAD, even when a local
        // branch of the same name existed (or could trivially be created) — this
        // reproduces the report and checks both cases end up attached.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-switch-remote-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dependency).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dependency.join("module.txt"), "v1").unwrap();
        for path in [&repository, &dependency] {
            run_git(path, &["init", "-b", "main"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["-c", "protocol.file.allow=always", "fetch", "origin"]);

        // Case 1: no local branch named "main" exists in the submodule's own
        // checkout yet (typical right after `submodule add`, which leaves it
        // detached) — selecting "origin/main" should create and attach to one.
        run_git(&sub_path, &["checkout", "--detach", "HEAD"]);
        assert!(Repository::open(&sub_path).unwrap().head_detached().unwrap());
        let versions = submodule_versions(repo_path.clone(), "vendor/dep".into()).unwrap();
        let origin_main = versions.versions.iter().find(|v| v.kind == "remote" && v.name == "origin/main").expect("origin/main should be listed").clone();
        switch_submodule_version(repo_path.clone(), "vendor/dep".into(), origin_main.revision.clone(), origin_main.kind.clone(), origin_main.name.clone()).unwrap();
        let sub_repo = Repository::open(&sub_path).unwrap();
        assert!(!sub_repo.head_detached().unwrap(), "selecting origin/main with no local main should attach, not detach");
        assert_eq!(sub_repo.head().unwrap().shorthand(), Some("main"));
        drop(sub_repo);

        // Case 2: a local branch of the same name already exists and already
        // points at that exact commit — selecting the remote entry should just
        // attach to the existing local branch, not error or duplicate it.
        run_git(&sub_path, &["checkout", "--detach", "HEAD"]);
        switch_submodule_version(repo_path.clone(), "vendor/dep".into(), origin_main.revision.clone(), origin_main.kind.clone(), origin_main.name.clone()).unwrap();
        let sub_repo = Repository::open(&sub_path).unwrap();
        assert!(!sub_repo.head_detached().unwrap());
        assert_eq!(sub_repo.head().unwrap().shorthand(), Some("main"));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn path_history_relinks_parents_across_commits_that_did_not_touch_the_path() {
        // Reproduces the graph-review report: filtering history to one path used to
        // keep the *raw* Git parent id even when that parent never touched the path
        // (so it isn't in the filtered list at all) — the frontend graph had no
        // choice but to silently drop that edge, making the lane look like it just
        // ends with no explanation. It must instead re-link to the nearest ancestor
        // that IS in the filtered list, skipping the excluded ones in between.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-path-history-relink-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "1").unwrap();
        fs::write(repository.join("b.txt"), "1").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "root touches a.txt"]); // included

        fs::write(repository.join("b.txt"), "2").unwrap();
        run_git(&repository, &["commit", "-am", "only b.txt changes"]); // excluded
        fs::write(repository.join("b.txt"), "3").unwrap();
        run_git(&repository, &["commit", "-am", "only b.txt changes again"]); // excluded

        fs::write(repository.join("a.txt"), "2").unwrap();
        run_git(&repository, &["commit", "-am", "a.txt changes again"]); // included

        let path = repository.to_string_lossy().into_owned();
        let history = path_history(path, "a.txt".into()).unwrap();
        assert_eq!(history.len(), 2, "only the two commits that touched a.txt should be listed: {:?}", history.iter().map(|c| &c.subject).collect::<Vec<_>>());
        assert_eq!(history[0].subject, "a.txt changes again");
        assert_eq!(history[1].subject, "root touches a.txt");
        assert_eq!(history[0].parents, vec![history[1].id.clone()], "the newer visible commit must be re-linked directly to the older visible one, skipping the two excluded commits in between");
        assert!(history[1].parents.is_empty(), "the root commit has no parent at all");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn create_submodule_branch_switches_and_keeps_the_parent_index_consistent() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-sub-new-branch-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        create_submodule_branch(parent_string.clone(), added.clone(), "feature-x".into()).unwrap();

        let sub_repo = Repository::open(parent.join(&added)).unwrap();
        assert!(!sub_repo.head_detached().unwrap());
        assert_eq!(sub_repo.head().unwrap().shorthand(), Some("feature-x"));
        drop(sub_repo);

        // Same commit, so nothing should look "modified" in the parent afterward.
        assert!(!load_repository(parent_string.clone(), Some(true)).unwrap().changes.iter().any(|change| change.path == added));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_brand_new_branch_only_shows_commits_not_already_on_any_remote_branch() {
        // Reproduces a real report: after creating a new branch, "Publish"
        // listed the branch's ENTIRE history (years of commits) as "WILL PUSH",
        // even though almost all of it already sat on the server under a
        // different branch name. `publish_status` used to only hide commits
        // already on `origin/<same-name>` — a brand new branch never has that
        // ref yet, so nothing was hidden and the whole shared ancestry looked
        // unpublished. It now hides everything reachable from ANY branch on
        // that remote, so only commits genuinely new anywhere on the server
        // show up — usually just the 1-2 commits made since branching.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-new-branch-publish-{suffix}"));
        let remote = std::env::temp_dir().join(format!("git-integrity-new-branch-remote-{suffix}.git"));
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare"]);
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Commit 1"]);
        fs::write(repository.join("a.txt"), "two").unwrap();
        run_git(&repository, &["commit", "-am", "Commit 2"]);
        run_git(&repository, &["remote", "add", "origin", remote.to_str().unwrap()]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        let path = repository.to_string_lossy().into_owned();

        // main IS published — nothing unexpected there.
        assert_eq!(publish_status(path.clone(), "main".into(), "origin".into()).unwrap().commits.len(), 0);

        // A brand new branch created right at main's tip, with no new work of
        // its own yet, shares 100% of its history with origin/main — it should
        // have nothing new to publish, not its whole 2-commit ancestry.
        create_branch(path.clone(), "feature-x".into()).unwrap();
        let unpublished = publish_status(path.clone(), "feature-x".into(), "origin".into()).unwrap();
        assert_eq!(unpublished.commits.len(), 0, "a new branch with no commits of its own should have nothing new to publish, even though origin/feature-x doesn't exist yet");

        // Now make one genuinely new commit on it — only *that* should show up.
        fs::write(repository.join("a.txt"), "three").unwrap();
        run_git(&repository, &["commit", "-am", "Commit 3 on feature-x"]);
        let unpublished = publish_status(path.clone(), "feature-x".into(), "origin".into()).unwrap();
        assert_eq!(unpublished.commits.len(), 1);
        assert_eq!(unpublished.commits[0].subject, "Commit 3 on feature-x");

        fs::remove_dir_all(repository).unwrap();
        fs::remove_dir_all(remote).unwrap();
    }

    #[test]
    fn staging_a_file_inside_a_submodule_stages_it_in_the_submodule_not_the_parent() {
        // Reproduces the report: browsing into a submodule's own files via the
        // plain Explorer (not the dedicated "Submodule branch map" swap) always
        // showed them as untracked/changed (the parent's status scan never sees
        // individual submodule files, only the submodule as a whole), and
        // staging one silently did nothing because `stage_files` only ever
        // touched the parent's index, which has no entry for it at all.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-stage-inside-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let file_path = format!("{added}/module.txt");

        fs::write(parent.join(&file_path), "changed content").unwrap();

        // The file must be correctly reported as modified — sourced from the
        // submodule's own status, not silently invisible to the parent.
        let listing = load_directory(parent_string.clone(), added.clone()).unwrap();
        let file_entry = listing.iter().find(|entry| entry.relative_path == file_path).expect("module.txt should be listed");
        assert_eq!(file_entry.status, "M", "a modified file inside a submodule must show its real status, not look permanently untracked");
        assert!(file_entry.tracked);

        let details = entry_details(parent_string.clone(), file_path.clone()).unwrap();
        assert_eq!(details.status, "M");

        stage_files(parent_string.clone(), vec![file_path.clone()]).unwrap();

        let sub_repo = Repository::open(parent.join(&added)).unwrap();
        let staged = sub_repo.statuses(None).unwrap().iter().any(|entry| entry.path() == Some("module.txt") && entry.status().contains(git2::Status::INDEX_MODIFIED));
        assert!(staged, "the file must end up staged in the submodule's own index");
        drop(sub_repo);

        // And the parent's own index must remain untouched by this — no bogus
        // entry for a path that was never part of its tree.
        let parent_repo = Repository::open(&parent).unwrap();
        assert!(parent_repo.index().unwrap().get_path(Path::new(&file_path), 0).is_none());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_brand_new_untracked_file_inside_a_submodule_can_be_committed_via_commit_path() {
        // The exact report: a brand new, never-before-tracked file ("nou.py")
        // created inside a submodule shows correctly as changed in the UI, but
        // committing it via the per-item "Commit this item" action (commit_path)
        // must not be blocked and must land inside the submodule's own history.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-new-file-in-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let file_path = format!("{added}/nou.py");

        fs::write(parent.join(&file_path), "print('hello')\n").unwrap();

        let listing = load_directory(parent_string.clone(), added.clone()).unwrap();
        let file_entry = listing.iter().find(|entry| entry.relative_path == file_path).expect("nou.py should be listed");
        assert_eq!(file_entry.status, "??", "a brand new file inside a submodule must show as untracked/new, not blank");

        let committed = commit_path(parent_string.clone(), file_path.clone(), "Add nou.py".into());
        assert!(committed.is_ok(), "committing a new file inside a submodule via commit_path must succeed, got: {:?}", committed);

        {
            let sub_repo = Repository::open(parent.join(&added)).unwrap();
            let head_files = sub_repo.head().unwrap().peel_to_tree().unwrap();
            assert!(head_files.get_path(Path::new("nou.py")).is_ok(), "nou.py should be in the submodule's HEAD commit");
        }

        // Confirm it no longer shows as a pending change afterward.
        let listing_after = load_directory(parent_string, added).unwrap();
        let file_entry_after = listing_after.iter().find(|entry| entry.relative_path == file_path).expect("nou.py should still be listed");
        assert_eq!(file_entry_after.status, "", "nou.py should no longer show as changed right after being committed");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn staging_and_committing_the_submodule_itself_from_the_working_tree_drawer_works() {
        // Reproduces the report: the Working tree drawer showed the submodule
        // itself ("test") as "M" (its checked-out commit had advanced past what
        // the parent's index recorded), but checking it and hitting Commit did
        // nothing. Cause: `resolve_submodule_boundary` matches the submodule's
        // own exact path too (with an empty inner path) — `partition_by_submodule`
        // was treating "stage/commit the submodule itself" the same as "stage a
        // file inside it", recursing into the submodule with an empty/bogus path
        // instead of running the normal gitlink-staging logic for it.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-stage-submodule-itself-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "test".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add test submodule".into()).unwrap();

        // Advance the submodule's HEAD (e.g. from a plain terminal) so it now
        // differs from what the parent's index recorded — this is exactly what
        // makes it show as "M" in the Working tree drawer. (`load_repository`
        // now auto-syncs this on its own — see
        // `commit_submodule_auto_updates_the_parent_even_without_a_push` and
        // the dedicated general-reconciliation test — so this exercises
        // `stage_files`/`commit_files` directly, without an intervening
        // `load_repository` call, to keep covering the actual regression:
        // `partition_by_submodule` mishandling the submodule's own path.)
        run_git(&parent.join(&added), &["commit", "--allow-empty", "-m", "Advance the submodule"]);
        let sub_repo = Repository::open(parent.join(&added)).unwrap();
        let expected_oid = sub_repo.head().unwrap().target().unwrap();
        drop(sub_repo);

        stage_files(parent_string.clone(), vec![added.clone()]).unwrap();
        let staged_changes = load_repository(parent_string.clone(), Some(true)).unwrap().changes;
        assert!(staged_changes.iter().any(|c| c.path == added && c.staged), "the submodule should be staged after checking it, not silently ignored");

        commit_files(parent_string.clone(), vec![added.clone()], "Bump test submodule".into()).unwrap();

        let repo = Repository::open(&parent).unwrap();
        let recorded_oid = repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id;
        assert_eq!(recorded_oid, expected_oid, "the parent's index should now record the submodule's new commit");
        assert!(!load_repository(parent_string, Some(true)).unwrap().changes.iter().any(|c| c.path == added), "the submodule should no longer show as changed after being committed");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn commit_submodule_auto_updates_the_parent_even_without_a_push() {
        // "After committing inside a submodule and it's already on the new
        // version, it should show as version-changed, not modified — even
        // without having pushed it yet." `commit_submodule` now records the new
        // commit in the parent right away (the same way a push already does),
        // instead of requiring a separate manual "Change version"/stage step.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-commit-submodule-auto-bump-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        fs::write(parent.join(&added).join("module.txt"), "v2").unwrap();
        let oid = commit_submodule(parent_string.clone(), added.clone(), "Update module".into()).unwrap();

        let repo = Repository::open(&parent).unwrap();
        let recorded = repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id;
        assert_eq!(recorded.to_string(), oid, "the parent must already record the submodule's new commit, with no push and no separate manual step");
        drop(repo);

        let changes = load_repository(parent_string, Some(true)).unwrap().changes;
        assert!(!changes.iter().any(|c| c.path == added), "the submodule must show as clean/version-changed, not modified, right after committing inside it: {:?}", changes.iter().map(|c| (&c.path, &c.status)).collect::<Vec<_>>());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn load_repository_reconciles_a_submodule_commit_made_outside_the_app_too() {
        // Reproduces the report: a submodule's own "N commits not yet pushed"
        // list showed real commits, but the sidebar's "Unpublished commits"
        // (the PARENT project's own count) stayed at 0. Cause: the automatic
        // parent-bump only ever ran from the dedicated `commit_submodule`
        // command — a commit made any other way inside the submodule (a raw
        // git command in the console, or committing one of its files
        // directly) never told the parent. `load_repository` now reconciles
        // this generally, regardless of how the submodule got its new commit.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-general-reconcile-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "test".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add test submodule".into()).unwrap();

        // Two commits made directly with git inside the submodule — nothing
        // in this app's own commands touched it.
        let sub_path = parent.join(&added);
        run_git(&sub_path, &["commit", "--allow-empty", "-m", "Test update"]);
        run_git(&sub_path, &["commit", "--allow-empty", "-m", "Test update"]);
        let expected_oid = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap();

        // A single reload must be enough to catch the parent up.
        load_repository(parent_string.clone(), Some(true)).unwrap();

        let repo = Repository::open(&parent).unwrap();
        let recorded = repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id;
        assert_eq!(recorded, expected_oid, "load_repository should have auto-recorded the submodule's new commit into the parent");
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().summary().unwrap_or(""), "Update submodule test to ".to_string() + &expected_oid.to_string()[..8], "the auto-bump should itself be a real commit in the parent, not just a staged index change");
        drop(repo);

        // And "Unpublished commits" (the parent's own count) must now see it.
        assert!(!load_repository(parent_string.clone(), Some(true)).unwrap().changes.iter().any(|c| c.path == added), "the submodule should show clean now that the parent has caught up");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_committed_but_unpushed_file_is_flagged_unpushed_not_modified() {
        // A file that's fully committed (clean working tree, matches HEAD
        // exactly) but whose commit hasn't reached the branch's upstream yet
        // should say "not pushed", not look identical to a file that was
        // never touched — and definitely not "modified" (nothing is modified).
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-unpushed-file-{suffix}"));
        let remote = std::env::temp_dir().join(format!("git-integrity-unpushed-remote-{suffix}.git"));
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare"]);
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::write(repository.join("src/a.txt"), "one").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial"]);
        run_git(&repository, &["remote", "add", "origin", remote.to_str().unwrap()]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "push", "-u", "origin", "main"]);
        let path = repository.to_string_lossy().into_owned();

        // Freshly pushed: nothing should be flagged.
        let listing = load_directory(path.clone(), "src".into()).unwrap();
        assert!(!listing.iter().find(|e| e.name == "a.txt").unwrap().unpushed);

        // Commit a change but don't push it.
        fs::write(repository.join("src/a.txt"), "two").unwrap();
        commit_path(path.clone(), "src/a.txt".into(), "Update a.txt".into()).unwrap();

        let listing = load_directory(path.clone(), "src".into()).unwrap();
        let entry = listing.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(entry.status, "", "the file is fully committed, so it must not show any working-tree status");
        assert!(entry.unpushed, "a committed-but-unpushed file must be flagged unpushed");

        // The containing folder should reflect it too.
        let root_listing = load_directory(path.clone(), "".into()).unwrap();
        let src_entry = root_listing.iter().find(|e| e.name == "src").unwrap();
        assert!(src_entry.unpushed, "a folder containing an unpushed file should be flagged too");

        let details = entry_details(path.clone(), "src/a.txt".into()).unwrap();
        assert!(details.unpushed);

        // After pushing (through the app's own command, which invalidates the
        // cache — a plain external `git push` wouldn't know to), it must clear.
        sync_repository(path.clone(), "push".into()).unwrap();
        let after_push = load_directory(path, "src".into()).unwrap();
        assert!(!after_push.iter().find(|e| e.name == "a.txt").unwrap().unpushed, "after push, the file must no longer be flagged unpushed");

        fs::remove_dir_all(repository).unwrap();
        fs::remove_dir_all(remote).unwrap();
    }

    #[test]
    fn publish_branch_can_stop_at_an_earlier_commit_leaving_newer_ones_local() {
        // "Deselecting" a commit before publish can only validly mean "stop
        // pushing here" — publish_branch's `upto_commit` pushes the branch up
        // to (and including) that commit, leaving anything newer unpublished.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-partial-publish-{suffix}"));
        let remote = std::env::temp_dir().join(format!("git-integrity-partial-publish-remote-{suffix}.git"));
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare"]);
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Commit 1"]);
        let commit1 = git(&repository.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        fs::write(repository.join("a.txt"), "two").unwrap();
        run_git(&repository, &["commit", "-am", "Commit 2"]);
        fs::write(repository.join("a.txt"), "three").unwrap();
        run_git(&repository, &["commit", "-am", "Commit 3"]);
        run_git(&repository, &["remote", "add", "origin", remote.to_str().unwrap()]);
        let path = repository.to_string_lossy().into_owned();

        publish_branch(path.clone(), "main".into(), "origin".into(), String::new(), String::new(), commit1.clone()).unwrap();

        let remote_head = git(&remote.to_string_lossy(), &["rev-parse", "refs/heads/main"]).unwrap().trim().to_string();
        assert_eq!(remote_head, commit1, "the server should be at exactly the chosen commit, not the branch tip");

        // The two newer commits must still show as unpublished locally.
        let status = publish_status(path.clone(), "main".into(), "origin".into()).unwrap();
        assert_eq!(status.commits.len(), 2, "commits 2 and 3 should still be pending, since only commit 1 was published");
        assert_eq!(status.commits[0].subject, "Commit 2");
        assert_eq!(status.commits[1].subject, "Commit 3");

        // Publishing the rest afterward (a normal full push) must succeed cleanly.
        publish_branch(path.clone(), "main".into(), "origin".into(), String::new(), String::new(), String::new()).unwrap();
        assert_eq!(publish_status(path, "main".into(), "origin".into()).unwrap().commits.len(), 0);

        fs::remove_dir_all(repository).unwrap();
        fs::remove_dir_all(remote).unwrap();
    }

    #[test]
    fn run_git_command_executes_scoped_to_the_given_folder_and_reports_stdout_stderr() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-run-git-command-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial"]);
        let path = repository.to_string_lossy().into_owned();

        let ok = run_git_command(path.clone(), "log --oneline -1".into()).unwrap();
        assert!(ok.success); assert!(ok.stdout.contains("Initial"));

        let failed = run_git_command(path.clone(), "show refs/heads/does-not-exist".into()).unwrap();
        assert!(!failed.success); assert!(!failed.stderr.is_empty());

        let with_git_prefix = run_git_command(path, "git status".into()).unwrap();
        assert!(with_git_prefix.success, "typing the full \"git status\" should work exactly like \"status\", not be rejected");

        fs::remove_dir_all(repository).unwrap();
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

        let changes = load_repository(repo_path.clone(), Some(true)).unwrap().changes;
        assert!(!changes.iter().any(|change| change.path == "vendor/dep"), "load_repository still lists the submodule as changed: {:?}", changes.iter().map(|c| (&c.path, &c.status)).collect::<Vec<_>>());

        let entries = load_directory(repo_path.clone(), "vendor".into()).unwrap();
        let dep_entry = entries.iter().find(|entry| entry.relative_path == "vendor/dep").expect("submodule entry should be listed");
        assert_eq!(dep_entry.status, "", "load_directory still reports a status for the submodule: {:?}", dep_entry.status);

        let details = entry_details(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(details.status, "", "entry_details still reports a status for the submodule: {:?}", details.status);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn parent_shows_the_submodule_bump_as_a_ready_to_push_commit_immediately_after_push() {
        // The exact flow the user asked about: modify a submodule, commit it, push
        // it — the parent should already show the new revision (auto-committed),
        // and the workspace should make it obvious there is now a new commit in
        // the parent ready to push, without any delay.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-parent-ready-to-push-{suffix}"));
        let repository = base.join("main");
        let parent_remote = base.join("main-remote.git");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::create_dir_all(&parent_remote).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v1").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        run_git(&parent_remote, &["init", "--bare"]);
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
        run_git(&repository, &["remote", "add", "origin", parent_remote.to_str().unwrap()]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["branch", "--set-upstream-to=origin/main", "main"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        // Before pushing the submodule, the parent has nothing new to push.
        let before = publish_status(repo_path.clone(), "main".into(), "origin".into()).unwrap();
        assert_eq!(before.commits.len(), 0, "parent should have nothing to push before the submodule is touched");

        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        commit_submodule(repo_path.clone(), "vendor/dep".into(), "Update module".into()).unwrap();
        push_submodule(repo_path.clone(), "vendor/dep".into()).unwrap();

        // The parent's working copy of the submodule must already be at the new revision.
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "v2");

        // And the parent itself must already show a locally-ready, unpushed commit
        // for that bump — immediately, no polling/delay needed.
        let after = publish_status(repo_path.clone(), "main".into(), "origin".into()).unwrap();
        assert_eq!(after.commits.len(), 1, "parent should show exactly one new commit ready to push (the submodule bump): {:?}", after.commits.iter().map(|c| &c.subject).collect::<Vec<_>>());
        assert!(after.commits[0].subject.contains("vendor/dep"), "the ready-to-push commit should be the submodule bump: {:?}", after.commits[0].subject);

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

        // The actual commit(s) behind that summary must be listed, not just counted.
        let unpushed = submodule_unpushed_commits(&sub_path.to_string_lossy());
        assert_eq!(unpushed.len(), 1);
        assert_eq!(unpushed[0].subject, "Local only");

        // After a successful push, the warning must clear.
        push_submodule(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(submodule_push_status(&sub_path.to_string_lossy()), None, "after push, the submodule should no longer report anything unpushed");
        assert!(submodule_unpushed_commits(&sub_path.to_string_lossy()).is_empty(), "after push, the unpushed commit list should be empty too");

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
        let parent_changes = load_repository(repo_path, Some(true)).unwrap().changes;
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
