use serde::Serialize;
use git2::{BranchType, ObjectType, Oid, Repository, Sort, Status, StatusOptions};
use std::{collections::{HashMap, HashSet, VecDeque}, fs, path::{Component, Path, PathBuf}, process::Command, sync::{Arc, Mutex, OnceLock}, time::{Instant, Duration, UNIX_EPOCH}};

pub mod stash;
pub mod branches;
pub mod command_console;
pub mod remotes;
#[cfg(test)]
use stash::{abort_stash_conflict, drop_stash, list_stashes, list_submodule_stashes, pop_stash, restore_stash_paths, stash_changes, stash_entry_files, stash_file};
#[cfg(test)]
use branches::{branch_creation_context, checkout_commit, create_branch, create_branch_at_commit, create_submodule_branch, delete_branch, graph_branch_divergence, rename_branch, restore_exact_checkpoint_inner, switch_branch};
#[cfg(test)]
use branches::merge::{abort_merge, complete_merge, conflict_sides, list_conflicts, merge_branch, merge_in_progress, resolve_conflict};
#[cfg(test)]
use command_console::{is_definitely_read_only_terminal_command, is_read_only_git_subcommand, run_git_command, run_terminal_command_inner, tokenize_git_args};
use remotes::fetch_all_remotes_inner;
#[cfg(test)]
use remotes::{list_remotes, sync_repository_inner};

// Temporary performance diagnostics: appends "<label>: <ms>ms" lines to a log
// file so real-world slowness can be diagnosed without guessing. Safe to leave
// in — each write is a single cheap append, guarded so a logging failure never
// breaks the actual operation. Log path is printed once by `perf_log_path()`.
fn perf_log_path() -> PathBuf {
    // A macOS `.app` is a signed bundle. Writing beside its executable means
    // writing inside `Contents/MacOS`, which invalidates the bundle seal after
    // the first launch and can make Finder refuse later launches. Keep logs in
    // the standard per-user Logs directory instead. Tests use the temporary
    // directory so they never touch a developer's real Library.
    #[cfg(all(target_os = "macos", test))]
    return std::env::temp_dir().join("git-integrity-perf.log");
    #[cfg(all(target_os = "macos", not(test)))]
    {
        if let Some(user_directory) = std::env::var_os("HOME") {
            let log_directory = PathBuf::from(user_directory).join("Library/Logs/Git DrillDown");
            if fs::create_dir_all(&log_directory).is_ok() {
                return log_directory.join("git-integrity-perf.log");
            }
        }
        return std::env::temp_dir().join("git-integrity-perf.log");
    }

    // Next to the executable, not the OS temp folder — much easier to find in
    // practice than hunting through %TEMP%. Falls back to temp dir only if the
    // exe's own folder isn't writable (e.g. installed under Program Files).
    #[cfg(not(target_os = "macos"))]
    if let Ok(exe) = std::env::current_exe() { if let Some(dir) = exe.parent() {
        let candidate = dir.join("git-integrity-perf.log");
        if fs::OpenOptions::new().create(true).append(true).open(&candidate).is_ok() { return candidate; }
    } }
    #[cfg(not(target_os = "macos"))]
    std::env::temp_dir().join("git-integrity-perf.log")
}

// Logged once per process, before the first real perf_log line — answers
// "which build produced this log" without cross-referencing the UI's footer
// separately (the exact confusion that let a stale exe go untested for hours
// earlier), directly in the artifact that actually gets copied off a
// Windows machine and sent back.
static PERF_LOG_SESSION_HEADER_WRITTEN: OnceLock<()> = OnceLock::new();

fn perf_log_session_header() {
    PERF_LOG_SESSION_HEADER_WRITTEN.get_or_init(|| {
        use std::io::Write;
        let line = format!("=== session start: build={} ===\n", env!("GIT_DRILLDOWN_BUILD_SHA"));
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(perf_log_path()) {
            let _ = file.write_all(line.as_bytes());
        }
    });
}

// A short, stable, non-reversible stand-in for a repository's real disk path
// — repository paths often contain project/customer names the log shouldn't
// have to carry every time it's shared back for diagnosis, while a
// consistent short id still lets multiple log lines (or multiple sessions)
// be recognized as the same repository.
fn anonymized_repository_id(repository_path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    repository_path.hash(&mut hasher);
    format!("repo-{:08x}", (hasher.finish() & 0xffff_ffff) as u32)
}

fn perf_log(label: &str, elapsed: Duration) {
    perf_log_session_header();
    use std::io::Write;
    let line = format!("[{:>7.1}ms] {}\n", elapsed.as_secs_f64() * 1000.0, label);
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(perf_log_path()) {
        let _ = file.write_all(line.as_bytes());
    }
}

#[derive(Clone, Default)]
// tracked/submodules/unpushed are repository-wide (not scoped to whatever
// folder is being browsed) and can have hundreds of thousands of entries on
// a large repository — wrapped in Arc so that every place that used to
// `.clone()` a GitMetadata (once per distinct folder scope cached, and again
// on every cache hit for it) does an O(1) refcount bump instead of a real
// O(total tracked files) HashSet deep-copy. `statuses` stays owned: it's
// already scoped to actual changes, which is normally far smaller than the
// total tracked set, so cloning it plainly is fine.
struct GitMetadata {
    tracked: Arc<HashSet<String>>,
    submodules: Arc<HashSet<String>>,
    statuses: Vec<(String, String)>,
    unpushed: Arc<HashSet<String>>,
}

// A full status scan (`build_git_metadata`) walks the entire working tree — on a
// large repository (tens of thousands of files) this can take a noticeable amount
// of time. Doing it on *every* folder click / entry selection made navigation on
// large repos painfully slow.
//
// A real Windows perf log showed this concretely: with the old 4-second TTL,
// ordinary human browsing (much slower-paced than 4 seconds between clicks)
// missed the cache on almost every navigation, paying a real scoped status
// scan each time (1.5-3.5s per folder on that repository). A 4-second TTL
// only ever protected a burst of clicks landing within the same 4 seconds —
// for anything slower, which is most real browsing, it did nothing.
// Same reasoning already established (and proven safe) for
// INDEX_METADATA_TTL below applies here too: correctness doesn't depend on
// doing this scan within some short window — invalidate_git_metadata already
// clears it immediately after any mutation this app performs, and the
// Refresh action exists specifically for "something changed outside the app
// and I want a truly fresh read". A much longer TTL trades a few seconds of
// possible staleness toward externally-made changes (rare, and Refresh
// covers it explicitly) for navigation actually being fast in practice
// instead of only in the best case.
const GIT_METADATA_TTL: Duration = Duration::from_secs(300);

// index_metadata/unpushed_paths (tracked files, submodules, unpushed-commit
// paths) only change on actions that mutate the index or HEAD — staging,
// committing, checkout, submodule updates — and every such action already
// calls invalidate_git_metadata() to drop these entries immediately. So,
// unlike the status-scan cache above (which must stay short-lived to notice
// changes made *outside* the app), correctness here doesn't depend on time
// at all — a long TTL is just a safety net for edits made in another editor
// or a terminal, which Refresh already exists to pick up on demand. Reusing
// the 4s TTL here forced a full index walk (every tracked file/submodule)
// on almost every folder click during normal browsing; a much longer TTL
// keeps that walk to roughly once per browsing session instead.
const INDEX_METADATA_TTL: Duration = Duration::from_secs(300);

static GIT_METADATA_CACHE: OnceLock<Mutex<HashMap<String, (Instant, GitMetadata)>>> = OnceLock::new();
static GIT_METADATA_SCAN_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn metadata_cache() -> &'static Mutex<HashMap<String, (Instant, GitMetadata)>> {
    GIT_METADATA_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn metadata_scan_lock(key: &str) -> Arc<Mutex<()>> {
    let mut locks = GIT_METADATA_SCAN_LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    locks.entry(key.to_string()).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
}

// tracked/submodules (from the index) don't depend on which folder is being
// browsed, so callers that only need those — not the working-tree status scan —
// can use this instead of paying for a (possibly scoped) status scan they don't need.
static INDEX_METADATA_CACHE: OnceLock<Mutex<HashMap<String, (Instant, (Arc<HashSet<String>>, Arc<HashSet<String>>))>>> = OnceLock::new();

fn index_metadata_cache() -> &'static Mutex<HashMap<String, (Instant, (Arc<HashSet<String>>, Arc<HashSet<String>>))>> {
    INDEX_METADATA_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// Returns Arc-wrapped sets — every caller used to get an owned `.clone()` of
// these (some, like partition_by_submodule before its fix, once per file in
// a loop), which on a repository with hundreds of thousands of tracked files
// meant a real O(total tracked files) HashSet deep-copy every time, cache hit
// or not. Cloning an Arc is an O(1) refcount bump regardless of set size.
fn cached_index_metadata(repository: &str) -> (Arc<HashSet<String>>, Arc<HashSet<String>>) {
    if let Some((cached_at, data)) = index_metadata_cache().lock().unwrap().get(repository) {
        if cached_at.elapsed() < INDEX_METADATA_TTL { return data.clone(); }
    }
    let (tracked, submodules) = index_metadata(repository);
    let data = (Arc::new(tracked), Arc::new(submodules));
    index_metadata_cache().lock().unwrap().insert(repository.to_string(), (Instant::now(), data.clone()));
    data
}

// Paths touched by commits that are on the current branch but not yet on its
// upstream — a file here is fully committed (clean working tree, matches
// HEAD exactly) but that commit hasn't reached the server. Same TTL-cached
// pattern as the index metadata above, since it doesn't depend on which
// folder is being browsed either.
static UNPUSHED_PATHS_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Arc<HashSet<String>>)>>> = OnceLock::new();

fn unpushed_paths_cache() -> &'static Mutex<HashMap<String, (Instant, Arc<HashSet<String>>)>> {
    UNPUSHED_PATHS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// Real evidence from a Windows perf log: rapid checkbox clicking in the
// Working tree drawer fires one stage_files/unstage_files call per click,
// with nothing serializing them — Tauri dispatches each to its own thread,
// so they genuinely run concurrently. Of 19 stage_files calls logged, only 5
// ever reached their own TOTAL line; the other 14 stopped right after
// building the add/remove lists and never logged add_all at all, which only
// happens if add_all itself returned an Err (aborting the function via `?`
// before that log line) — almost certainly libgit2 failing to acquire
// `.git/index.lock` because another concurrent call already held it. Best
// case that's a failed operation the user has to retry; worst case, two
// racing reads-then-writes of the index silently drop one side's staging
// with no error at all. One lock per repository, held for the duration of
// any command that mutates the index or HEAD, makes them queue up instead
// of racing.
static REPO_WRITE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

// Folds every way the *same* repository path can be spelled down to one key:
// `\` vs `/` separators (a path pasted from Windows Explorer's address bar),
// and — Windows only — letter case (NTFS is case-insensitive, so `C:\Repo`
// and `c:\repo` are the same repository; a real Unix filesystem is normally
// case-*sensitive*, where lowercasing here would wrongly merge two genuinely
// different repositories, e.g. `/Users/x/Foo` and `/Users/x/foo`, so that
// fold is deliberately platform-gated, not universal). Canonicalizing
// (resolving `..`/symlinks to the real absolute path) catches the rest —
// two different-looking paths that are actually the same directory. Falls
// back to the normalized-but-uncanonicalized form when the path doesn't
// exist (canonicalize needs a real path) rather than erroring; still folds
// separators/case even then.
fn repo_lock_key(path: &str) -> String {
    let normalized_path = path.replace('\\', "/");
    let resolved = fs::canonicalize(&normalized_path).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or(normalized_path);
    if cfg!(windows) { resolved.to_lowercase() } else { resolved }
}

fn repo_write_lock(repository: &str) -> Arc<Mutex<()>> {
    let key = repo_lock_key(repository);
    let mut locks = REPO_WRITE_LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    locks.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
}

// Call right after acquiring a repo_write_lock guard — logs how long this
// operation waited behind another one already running on the *same*
// repository (0 when nothing was contending), tagged with a non-secret
// label identifying which command/context this is, so a backed-up queue is
// visible in the perf log without ever including a commit message, file
// path, or credential.
fn log_repo_write_lock_acquired(repository_path: &str, label: &str, queue_wait: Duration) {
    perf_log(&format!("repo_write_lock: [{label}] acquired (repo={}, queue_wait={:.1}ms)", anonymized_repository_id(repository_path), queue_wait.as_secs_f64() * 1000.0), Duration::ZERO);
}

// Tauri does *not* automatically move a synchronous `#[tauri::command]`'s
// work off the thread that delivered the IPC call — confirmed against
// Tauri's own generated wrapper (tauri-macros' `body_blocking`): it calls
// straight through, on that same thread, with no dispatch elsewhere. That
// thread is the platform webview's own IPC callback (the native UI thread
// on both macOS/WKWebView and Windows/WebView2), so a slow synchronous
// command freezes the whole window for its duration. Only an `async fn`
// command gets off it — Tauri's own docs recommend exactly this — and only
// if that `async fn` actually awaits something that itself runs elsewhere,
// which is what this does: hands the *entire*, unchanged, still-fully-
// synchronous body to Tokio's dedicated blocking-thread pool (a pool built
// for exactly this — long, blocking, non-async work — separate from both
// the UI thread and Tokio's async worker threads) and awaits it there.
// Every "heavy" command (pr_status, load/refresh status, stage/commit,
// fetch/push, submodule operations) routes through this. A repository
// handle is always opened *inside* `body`, never passed across the
// boundary, so git2::Repository's own thread-affinity story never enters
// into whether this compiles — only the plain, owned inputs and the
// `Result<T, String>` output need to be `Send`, and every command's
// parameters/return types already are.
async fn off_main_thread<T: Send + 'static>(body: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    match tauri::async_runtime::spawn_blocking(body).await {
        Ok(result) => result,
        Err(join_error) => Err(format!("Internal error: a background task panicked ({join_error})")),
    }
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
        if cached_at.elapsed() < GIT_METADATA_TTL {
            perf_log("cached_full_statuses: HIT", Duration::ZERO);
            return Ok(statuses.clone());
        }
    }
    // Share the same per-repository single-flight gate as refresh_status and
    // stage_all. Without this, a fast repository open could start its
    // background refresh_status scan while a normal load_repository (or a
    // forced folder reload) simultaneously starts this long-cache scan of the
    // exact same working tree. On large Windows repositories that meant two
    // multi-second/minute `git status` walks fighting for the same disk. This
    // does not make the cache any staler: it only waits for an in-flight scan
    // and re-checks the cache before deciding whether a real scan is still
    // needed.
    let lock_handle = status_scan_lock(repository_path);
    let _guard = lock_handle.lock().unwrap();
    if let Some((cached_at, statuses)) = full_status_cache().lock().unwrap().get(repository_path) {
        if cached_at.elapsed() < GIT_METADATA_TTL {
            perf_log("cached_full_statuses: HIT (after a concurrent scan)", Duration::ZERO);
            return Ok(statuses.clone());
        }
    }
    let step = Instant::now();
    let statuses = internal_statuses(repository, None)?;
    perf_log("cached_full_statuses: MISS, scanned", step.elapsed());
    full_status_cache().lock().unwrap().insert(repository_path.to_string(), (Instant::now(), statuses.clone()));
    Ok(statuses)
}

// A real macOS perf log on a ~14GB/100,000-file repository caught this
// precisely: opening the Working tree drawer (refresh_status, ~0.9s)
// immediately followed by Stage all running its *own*, separately-scanned
// ~0.9s full status scan of the exact same thing a moment later — and,
// separately, two refresh_status calls firing back to back. Neither
// refresh_status nor stage_all can just use cached_full_statuses above
// (GIT_METADATA_TTL is 300s, tuned for *navigation*, where staleness toward
// external changes matters much less than not re-scanning on every click) —
// that would undo the whole reason refresh_status/stage_all exist: catching
// a file that was just added or removed from outside the app right now, not
// up to 5 minutes from now. But genuinely fresh external changes happen on a
// human timescale of several seconds at minimum; two calls to either of
// these within a couple hundred milliseconds of each other are essentially
// always the same "what changed" question asked twice in one user gesture,
// not two different moments worth separately scanning for. Shares the same
// full_status_cache entry as cached_full_statuses above (whichever ran more
// recently benefits both), just with a much shorter freshness threshold.
const FRESH_STATUS_REUSE_WINDOW: Duration = Duration::from_secs(2);

static STATUS_SCAN_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn status_scan_lock(repository: &str) -> Arc<Mutex<()>> {
    let mut locks = STATUS_SCAN_LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    locks.entry(repository.to_string()).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
}

fn recent_full_statuses(repository: &Repository, repository_path: &str) -> Result<Vec<(String, String, bool)>, String> {
    // Fast path — no lock needed if a scan from a moment ago is still fresh
    // enough (the common case this exists for: refresh_status then stage_all
    // right after, or two refresh_status calls close together).
    if let Some((cached_at, statuses)) = full_status_cache().lock().unwrap().get(repository_path) {
        if cached_at.elapsed() < FRESH_STATUS_REUSE_WINDOW {
            perf_log("recent_full_statuses: HIT (reuse window)", Duration::ZERO);
            return Ok(statuses.clone());
        }
    }
    // Single-flight for genuinely concurrent callers (both arriving before
    // either has written a result yet): the second one blocks here instead
    // of starting its own redundant scan, then re-checks the cache — which
    // the first caller will have just populated — before ever falling
    // through to an actual second scan.
    let lock_handle = status_scan_lock(repository_path);
    let _guard = lock_handle.lock().unwrap();
    if let Some((cached_at, statuses)) = full_status_cache().lock().unwrap().get(repository_path) {
        if cached_at.elapsed() < FRESH_STATUS_REUSE_WINDOW {
            perf_log("recent_full_statuses: HIT (reuse window, after a concurrent scan)", Duration::ZERO);
            return Ok(statuses.clone());
        }
    }
    let step = Instant::now();
    let statuses = internal_statuses(repository, None)?;
    perf_log("recent_full_statuses: MISS, fresh scan", step.elapsed());
    full_status_cache().lock().unwrap().insert(repository_path.to_string(), (Instant::now(), statuses.clone()));
    Ok(statuses)
}

fn fresh_full_statuses(repository: &Repository, repository_path: &str, label: &str) -> Result<Vec<(String, String, bool)>, String> {
    let lock_handle = status_scan_lock(repository_path);
    let _guard = lock_handle.lock().unwrap();
    let step = Instant::now();
    let statuses = internal_statuses(repository, None)?;
    perf_log(&format!("{label}: fresh full status scan"), step.elapsed());
    full_status_cache().lock().unwrap().insert(repository_path.to_string(), (Instant::now(), statuses.clone()));
    Ok(statuses)
}

// The branch's real configured upstream — branch.<local>.remote +
// branch.<local>.merge, via git2's own Branch::upstream() — not a hardcoded
// assumption that the remote is named `origin` and that the remote-tracking
// branch shares the local branch's name. A repository with no `origin` at
// all, or a branch tracking a differently-named branch on a differently-named
// remote (a "release"/mirror remote, say), resolves correctly either way.
// None when there's no configured upstream, the remote no longer exists, or
// the remote-tracking ref hasn't been fetched yet — callers treat that as
// "can't tell what's unpushed", not as an error.
fn upstream_oid(repo: &Repository, local_branch_name: &str) -> Option<git2::Oid> {
    upstream_ref(repo, local_branch_name).map(|(oid, _)| oid)
}

// Same, but also returns the upstream's display shorthand (`<remote>/<branch>`,
// exactly as git/git2 would show it) for user-facing messages — reads
// correctly even when the remote isn't named `origin` or the remote branch
// has a different name than the local one.
fn upstream_ref(repo: &Repository, local_branch_name: &str) -> Option<(git2::Oid, String)> {
    let upstream = repo.find_branch(local_branch_name, BranchType::Local).ok()?.upstream().ok()?;
    let target = upstream.get().target()?;
    let shorthand = upstream.get().shorthand()?.to_string();
    Some((target, shorthand))
}

fn unpushed_paths(repository: &str) -> HashSet<String> {
    (|| -> Option<HashSet<String>> {
        let repo = internal_repository(repository).ok()?;
        let head = repo.head().ok()?;
        if repo.head_detached().unwrap_or(true) { return Some(HashSet::new()); }
        let local_oid = head.target()?;
        let branch = head.shorthand()?.to_string();
        drop(head);
        let Some(upstream_oid) = upstream_oid(&repo, &branch) else { return Some(HashSet::new()); };
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

fn cached_unpushed_paths(repository: &str) -> Arc<HashSet<String>> {
    if let Some((cached_at, data)) = unpushed_paths_cache().lock().unwrap().get(repository) {
        if cached_at.elapsed() < INDEX_METADATA_TTL { return data.clone(); }
    }
    let data = Arc::new(unpushed_paths(repository));
    unpushed_paths_cache().lock().unwrap().insert(repository.to_string(), (Instant::now(), data.clone()));
    data
}

// One consolidated snapshot per changed, visible submodule. It answers the
// dirty/HEAD/origin questions in one repository open and one status scan,
// instead of separate synchronous scans for each badge. Clean synchronized
// rows do not enter this cache at all. It is deliberately invalidated only
// by actions that can affect a submodule, not by unrelated parent file work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmoduleRemoteRelation { OnOrigin, PushNeeded, SyncNeeded, DetachedNeedsBranch, NoOrigin, Unknown }

#[derive(Clone, Debug)]
struct SubmoduleStateSnapshot {
    dirty: bool,
    head_oid: Option<Oid>,
    remote_relation: SubmoduleRemoteRelation,
    // None means detached HEAD; Some(name) means attached to that local
    // branch. Free to capture here: remote_relation() below already reads
    // repo.head_detached()/repo.head().shorthand() on this same, already-open
    // repo — this just keeps what it found instead of discarding it.
    attached_branch: Option<String>,
}

static SUBMODULE_STATE_CACHE: OnceLock<Mutex<HashMap<String, (Instant, SubmoduleStateSnapshot)>>> = OnceLock::new();

fn submodule_state_cache() -> &'static Mutex<HashMap<String, (Instant, SubmoduleStateSnapshot)>> {
    SUBMODULE_STATE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// Single-flight per submodule path, same shape as status_scan_lock: two
// folders that happen to share a submodule (or two rapid navigations before
// either finishes) block on the one real scan instead of each starting
// their own redundant `submodule_push_status` call.
static SUBMODULE_UNPUSHED_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn submodule_unpushed_lock(sub_path: &str) -> Arc<Mutex<()>> {
    let mut locks = SUBMODULE_UNPUSHED_LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    locks.entry(sub_path.to_string()).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
}

fn remote_relation(repo: &Repository, head_oid: Oid) -> SubmoduleRemoteRelation {
    if repo.find_remote("origin").is_err() { return SubmoduleRemoteRelation::NoOrigin; }
    if repo.head_detached().unwrap_or(true) {
        let on_origin = repo.references_glob("refs/remotes/origin/*").ok().is_some_and(|references| references.flatten().any(|reference| {
            reference.target().is_some_and(|remote_oid| remote_oid == head_oid || repo.graph_descendant_of(remote_oid, head_oid).unwrap_or(false))
        }));
        return if on_origin { SubmoduleRemoteRelation::OnOrigin } else { SubmoduleRemoteRelation::DetachedNeedsBranch };
    }
    let branch = match repo.head().ok().and_then(|head| head.shorthand().map(str::to_string)) {
        Some(branch) => branch,
        None => return SubmoduleRemoteRelation::Unknown,
    };
    let remote_oid = match repo.find_reference(&format!("refs/remotes/origin/{branch}")).ok().and_then(|reference| reference.target()) {
        Some(oid) => oid,
        None => return SubmoduleRemoteRelation::PushNeeded,
    };
    match repo.graph_ahead_behind(head_oid, remote_oid) {
        Ok((0, 0)) => SubmoduleRemoteRelation::OnOrigin,
        Ok((ahead, 0)) if ahead > 0 => SubmoduleRemoteRelation::PushNeeded,
        Ok(_) => SubmoduleRemoteRelation::SyncNeeded,
        Err(_) => SubmoduleRemoteRelation::Unknown,
    }
}

fn inspect_submodule_state_in(repo: &Repository) -> SubmoduleStateSnapshot {
    let head_oid = repo.head().ok().and_then(|head| head.target());
    let dirty = !internal_statuses(repo, None).unwrap_or_default().is_empty();
    let remote_relation = head_oid.map(|oid| remote_relation(repo, oid)).unwrap_or(SubmoduleRemoteRelation::Unknown);
    let attached_branch = if repo.head_detached().unwrap_or(true) { None } else { repo.head().ok().and_then(|head| head.shorthand().map(str::to_string)) };
    SubmoduleStateSnapshot { dirty, head_oid, remote_relation, attached_branch }
}

fn inspect_submodule_state(sub_path: &str) -> SubmoduleStateSnapshot {
    internal_submodule_repository(Path::new(sub_path)).map(|repo| inspect_submodule_state_in(&repo)).unwrap_or(SubmoduleStateSnapshot {
        dirty: false, head_oid: None, remote_relation: SubmoduleRemoteRelation::Unknown, attached_branch: None,
    })
}

fn fresh_submodule_state(sub_path: &str) -> SubmoduleStateSnapshot {
    let lock_handle = submodule_unpushed_lock(sub_path);
    let _guard = lock_handle.lock().unwrap();
    let value = inspect_submodule_state(sub_path);
    submodule_state_cache().lock().unwrap().insert(sub_path.to_string(), (Instant::now(), value.clone()));
    value
}

fn cached_submodule_state(sub_path: &str) -> SubmoduleStateSnapshot {
    if let Some((cached_at, value)) = submodule_state_cache().lock().unwrap().get(sub_path) {
        if cached_at.elapsed() < INDEX_METADATA_TTL { return value.clone(); }
    }
    let lock_handle = submodule_unpushed_lock(sub_path);
    let _guard = lock_handle.lock().unwrap();
    // Re-check after acquiring the lock — a concurrent caller for this exact
    // submodule may have just finished the real scan while this one waited.
    if let Some((cached_at, value)) = submodule_state_cache().lock().unwrap().get(sub_path) {
        if cached_at.elapsed() < INDEX_METADATA_TTL { return value.clone(); }
    }
    let value = inspect_submodule_state(sub_path);
    submodule_state_cache().lock().unwrap().insert(sub_path.to_string(), (Instant::now(), value.clone()));
    value
}

#[derive(Serialize)]
// current_branch is "" (never the literal string "HEAD" — git2's own
// Reference::shorthand() returns exactly that for a detached HEAD, which
// this app used to pass straight through and display/treat as if it were a
// real branch name) whenever head_detached is true. head_oid is always the
// real commit HEAD points at, valid whether attached or not — the only
// reliable "where am I" for a detached checkout (extremely common for a
// submodule right after `git submodule update`/"Reset submodule", both of
// which intentionally leave it that way).
// gitdir/submodule_url exist mainly for diagnosing "which repository is
// this actually talking to" reports (a submodule's history view silently
// resolving to the parent, or vice versa) — see submodule_repository_inner
// and openSubmoduleGraph's own logging. gitdir is always real (every
// repository, submodule or not, has one); submodule_url is only ever
// populated for a submodule's own RepositoryInfo, never the parent's.
pub struct RepositoryInfo { path: String, name: String, current_branch: String, head_oid: String, head_detached: bool, gitdir: String, submodule_url: Option<String> }

#[derive(Serialize)]
pub struct Branch { name: String, current: bool, remote: bool }

// kind is one of "local_branch" | "remote_branch" | "tag" — deliberately a
// plain String at this JSON boundary (matching DirectoryEntry.kind's own
// convention in this file) rather than a wire-serialized enum; the small,
// closed set of values is still enforced at the one place that actually
// produces them, collect_ref_seeds_and_badges, not scattered across call
// sites. "head" is intentionally never emitted here — HEAD is not a real
// refs/* entry, and is attached client-side instead (see refsBadges in
// app.js), the same way it already was before this struct existed.
#[derive(Serialize, Clone)]
pub struct CommitRef { name: String, kind: String }

#[derive(Serialize)]
pub struct Commit { id: String, parents: Vec<String>, subject: String, author: String, date: String, refs: Vec<CommitRef>, lane: usize }

#[derive(Serialize)]
pub struct Change { status: String, path: String, staged: bool }

#[derive(Serialize)]
pub struct StashEntry { index: usize, message: String, base_commit: String }

// A stash belongs to one exact Git repository. The parent project and every
// submodule have separate refs/stash values, so the frontend carries this
// explicit scope with the list instead of accidentally opening/restoring the
// parent's stash after an operation that actually targeted a submodule.
#[derive(Serialize)]
pub struct StashScope { repository_path: String, repository_name: String, current_branch: String, is_submodule: bool, relative_path: Option<String>, stashes: Vec<StashEntry> }

// commits_truncated is true when the DAG actually has more history beyond
// `commits` — never inferred by the frontend from "exactly 500 came back" (a
// repository with precisely 500 reachable commits would falsely look
// truncated); the backend already knows for certain, from one extra peek
// past the limit.
#[derive(Serialize)]
pub struct RepositoryData { repository: RepositoryInfo, branches: Vec<Branch>, commits: Vec<Commit>, changes: Vec<Change>, stashes: Vec<StashEntry>, submodule_paths: Vec<String>, commits_truncated: bool }

// Deliberately no `changes` field at all — status isn't computed yet when
// this returns. The frontend treats its absence as "status pending" (shows
// "Loading status…", disables Stage/Delete/Commit/checkout) until a
// follow-up refresh_status call fills it in.
#[derive(Serialize)]
pub struct FastRepositoryData { repository: RepositoryInfo, branches: Vec<Branch>, commits: Vec<Commit>, stashes: Vec<StashEntry>, submodule_paths: Vec<String>, commits_truncated: bool }

#[derive(Serialize)]
pub struct DirectoryEntry {
    name: String,
    relative_path: String,
    kind: String,
    status: String,
    tracked: bool,
    size: u64,
    modified: u64,
    // Only set for submodules: true when this submodule itself has commits
    // not yet pushed to its own remote. Used only for the more detailed
    // "New version locally (not pushed yet)" wording in Entry Details — see
    // submodule_is_dirty below for what actually decides "New version" vs
    // "Modified" in the first place.
    submodule_has_unpushed_commits: bool,
    // Only set for submodules: false means its "M" status can only be a
    // gitlink version bump (staged, pushed or not — see the
    // submodule-status-labels report) rather than genuinely uncommitted
    // content inside it, because the submodule's own working tree/index is
    // clean. This used to be conflated with submodule_has_unpushed_commits
    // above — which meant a submodule whose new commit was already safely
    // pushed, but not yet committed in the parent, fell back to a plain
    // "Modified" label indistinguishable from an ordinary dirty file. "M" +
    // !submodule_is_dirty is the correct, complete signal for "New version"
    // regardless of push state; "M" + submodule_is_dirty is genuinely dirty
    // content and should look like any other modification.
    submodule_is_dirty: bool,
    // A compact, mutually-exclusive workflow state derived from the
    // submodule HEAD/working tree, origin tracking ref, and the parent
    // repository's HEAD/index gitlinks. The frontend only translates this
    // code into copy; it never guesses Git state from a generic "M".
    submodule_state: String,
    // Whether submodule_current_branch below is a real answer. A clean,
    // fully-synced submodule deliberately never opens its own repository at
    // all here (see the submodule_snapshot gate a few lines down in
    // load_directory_inner) — unconditionally checking attached/detached on
    // every submodule row, clean or not, would mean opening every one of
    // them on every folder listing, which is exactly the kind of per-row
    // filesystem cost that made navigation slow on a large repository in the
    // first place. So this stays false (and submodule_current_branch stays
    // None, meaning "unknown", not "detached") for the common case, and is
    // only ever true riding along on a submodule row that already had a
    // reason to be inspected.
    submodule_checked: bool,
    // Cheap filesystem signal: a registered submodule whose worktree folder
    // exists but has no .git file/directory is known to Git, yet not
    // initialized locally. This lets the UI offer a narrow "Initialize this
    // submodule" action without opening every clean submodule repository.
    submodule_initialized: bool,
    // Only meaningful when submodule_checked is true. None means detached
    // HEAD; Some(name) means attached to that local branch.
    submodule_current_branch: Option<String>,
    // True when this file is fully committed (no working-tree status at all)
    // but the commit that last touched it isn't on the upstream branch yet —
    // or, for a folder, when something inside it is in that state. Lets the
    // UI say "not pushed yet" instead of leaving a just-committed file looking
    // identical to one that was never touched.
    unpushed: bool,
    // False only from list_directory_fast (the filesystem-only listing used
    // for the very first render while opening a repository) — status/tracked/
    // unpushed above are meaningless placeholders in that case, not "clean"
    // or "untracked", and the frontend must show a loading state instead of
    // treating them as real answers. Always true from the normal
    // load_directory, which always has real status by the time it returns.
    status_known: bool,
    // True when this exact file, or at least one path below this folder, is
    // preserved in any stash belonging to the repository currently being
    // browsed. Independent from current worktree status: a path may have a
    // stashed backup and also have newer local edits at the same time.
    stashed: bool,
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
    submodule_web_url: Option<String>,
    submodule_branch: Option<String>,
    submodule_push_status: Option<String>,
    submodule_unpushed_commits: Vec<PublishCommit>,
    // See DirectoryEntry's own doc comment — same signal, same reason.
    submodule_is_dirty: bool,
    submodule_state: String,
    submodule_initialized: bool,
    // Entry details always open the selected submodule repository once when it
    // exists, so this is a real current checkout signal there: Some(branch)
    // means attached to that local branch, None means detached/unavailable.
    submodule_current_branch: Option<String>,
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
    submodule_commit_tags: Vec<String>,
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
    // Only populated for kind == "tag": the local branch (if any) whose tip
    // currently sits on the same commit the tag points to. None means the
    // tag's commit isn't the tip of any local branch (detached if checked out).
    attached_branch: Option<String>,
    // Submodule-branch-selector report, point 3: only populated for
    // kind == "branch" (a local branch) — the upstream's own shorthand
    // ("<remote>/<branch>") when one is actually configured
    // (branch.<name>.remote/.merge), and how far ahead/behind it this local
    // branch is. All None for a local branch with no configured upstream,
    // and always None for every other kind (remote/tag/commit) — an
    // upstream is a property of a *local* branch, never of the others.
    upstream: Option<String>,
    ahead: Option<usize>,
    behind: Option<usize>,
    // For branch/remote rows only: whether the active checkout is reachable
    // from this branch tip, and how many commits newer that tip is. This is
    // ancestry context for detached HEAD, not a claim that HEAD is attached.
    contains_current: bool,
    commits_after_current: Option<usize>,
    // Only populated for kind == "commit": known branches whose tip contains
    // *this specific* commit (never just the active checkout — see
    // contains_current above for that, a different question). Sorted
    // closest-tip-first, same ordering as current_containing_branches below.
    // Lets the History list answer "which branch is this old commit even
    // on?" for any row, not only the one currently checked out.
    containing_branches: Vec<String>,
}

#[derive(Serialize)]
pub struct SubmoduleVersions {
    path: String,
    current_revision: String,
    current_branch: String,
    // The gitlink currently recorded in the parent index. Exposed in this
    // same response so the dialog can distinguish "restored project version"
    // from an arbitrary detached checkout without another backend request.
    parent_revision: String,
    // Known refs whose history contains the active commit. This is context
    // for a detached checkout, never a claim that HEAD is attached to one.
    current_containing_branches: Vec<String>,
    // Known branch whose tip feeds the bounded history list. Empty means no
    // branch contains detached HEAD, so history starts at HEAD itself.
    history_context_branch: String,
    history_limit: usize,
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
pub struct TextFile { relative_path: String, content: String }

#[derive(Serialize, Debug)]
pub struct PublishCommit { id: String, subject: String, author: String, date: String }

#[derive(Serialize)]
pub struct FolderRestorePreview {
    folder: String,
    source_revision: String,
    source_id: String,
    source_subject: String,
    source_author: String,
    source_date: String,
    tracked_changes: Vec<Change>,
    clean_candidates: Vec<String>,
}

#[derive(Serialize)]
pub struct PublishStatus {
    branch: String,
    remote: String,
    remote_branch: String,
    commits: Vec<PublishCommit>,
    ahead: usize,
    behind: usize,
    remote_branch_exists: bool,
}

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

fn run_with_timeout(command: Command) -> Result<std::process::Output, String> {
    run_with_timeout_labeled(command, GIT_COMMAND_TIMEOUT, "Git", "10 minutes")
}

fn run_with_timeout_labeled(mut command: Command, timeout: Duration, program_label: &str, timeout_label: &str) -> Result<std::process::Output, String> {
    // Put the child in its own process group (Unix) so a timeout kill can
    // target the whole group, not just this one PID — see the comment on
    // the kill call below for why that matters. Must be set before spawn().
    #[cfg(unix)] { use std::os::unix::process::CommandExt; command.process_group(0); }
    let child = command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| format!("Cannot start {program_label}: {e}"))?;
    let id = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("{program_label} process error: {error}")),
        Err(_) => {
            // A negative PID tells kill to signal the whole process GROUP,
            // not just this one process — process_group(0) above made this
            // child (e.g. the Terminal's shell, or `git` itself for
            // anything that shells out further, like `git submodule
            // update` spawning one git process per submodule) the leader
            // of its own group, so this reaches every descendant. Killing
            // only the top PID left those children orphaned and still
            // running after a timeout, free to keep writing to the working
            // tree/index in the background — exactly what forces a later
            // `git reset --hard` to recover, and can leave a submodule
            // checked out empty from a clone interrupted mid-way with
            // nothing left to finish or clean it up.
            #[cfg(unix)] { let _ = Command::new("kill").arg("-9").arg(format!("-{id}")).status(); }
            // /T is the Windows equivalent: kill the whole process tree
            // rooted at this PID, not just cmd.exe itself.
            #[cfg(windows)] { let _ = Command::new("taskkill").args(["/F", "/T", "/PID"]).arg(id.to_string()).status(); }
            Err(format!("{program_label} command timed out after {timeout_label} — check your network connection and try again"))
        }
    }
}

// A short, discreet trail of the actual git commands this app just ran on
// the user's behalf — for the status bar's own quiet "what just happened"
// hint and its double-click history, not a replacement for the Terminal's
// own transcript (which already covers commands the *user* typed directly;
// recording here is scoped to this one shared helper specifically so it
// never doubles up with that). Bounded so a long session can't grow this
// without limit; oldest entries are simply dropped.
const RECENT_GIT_COMMANDS_LIMIT: usize = 50;
static RECENT_GIT_COMMANDS: OnceLock<Mutex<VecDeque<(String, String, Instant, bool)>>> = OnceLock::new();

fn recent_git_commands_store() -> &'static Mutex<VecDeque<(String, String, Instant, bool)>> {
    RECENT_GIT_COMMANDS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn record_git_command(path: &str, args: &[&str], success: bool) {
    // The repository/submodule this ran against, not its full (possibly
    // anonymization-worthy) path — just enough to tell two concurrent
    // targets apart at a glance.
    let repo_hint = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_string();
    let command = format!("git {}", args.join(" "));
    let mut store = recent_git_commands_store().lock().unwrap();
    if store.len() >= RECENT_GIT_COMMANDS_LIMIT { store.pop_front(); }
    store.push_back((repo_hint, command, Instant::now(), success));
}

#[derive(Serialize)]
pub struct RecentGitCommand { repo_hint: String, command: String, seconds_ago: f64, success: bool }

// Newest first. Read fresh on demand (no push/event channel) — this is a
// deliberately low-stakes, glanceable feature, not something that needs to
// stay open a socket for.
#[tauri::command]
pub fn recent_git_commands() -> Vec<RecentGitCommand> {
    recent_git_commands_store().lock().unwrap().iter().rev()
        .map(|(repo_hint, command, when, success)| RecentGitCommand { repo_hint: repo_hint.clone(), command: command.clone(), seconds_ago: when.elapsed().as_secs_f64(), success: *success })
        .collect()
}

fn git(path: &str, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    // `-c` overrides must come before the subcommand to be recognized as
    // global git config, not passed through to it — configure_git_command's
    // own `-c` flags need to land here, before `args` (which starts with the
    // subcommand), not after.
    configure_git_command(&mut command);
    command.arg("-C").arg(path).arg("-c").arg("color.ui=false").args(args);
    // A spawn failure or a timeout (run_with_timeout's own Err cases) is
    // exactly the kind of event the command history most needs to explain —
    // record it as a failure here too, not only the "ran, exited non-zero"
    // case below, so a hung command (the exact failure mode that motivated
    // this log in the first place) never leaves a silent gap right when it
    // would matter most.
    let output = match run_with_timeout(command) {
        Ok(output) => output,
        Err(error) => { record_git_command(path, args, false); return Err(error); }
    };
    record_git_command(path, args, output.status.success());
    if output.status.success() { Ok(String::from_utf8_lossy(&output.stdout).into_owned()) }
    else { Err(String::from_utf8_lossy(&output.stderr).trim().to_string()) }
}

fn git_owned(path: &str, args: Vec<String>) -> Result<String, String> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    git(path, &refs)
}

fn internal_repository(path: &str) -> Result<Repository, String> {
    Repository::discover(path).map_err(|error| format!("Cannot open Git repository: {error}"))
}

// Repository::discover (what internal_repository above uses — correct there,
// since a user opening a repository may have picked any subfolder and
// expects the enclosing root to be found) walks *up* through parent
// directories looking for a `.git`. That is exactly wrong for a submodule:
// this app always already knows precisely which directory the submodule's
// own repository must be rooted at, from the parent's own index — if that
// directory's own Git metadata is missing or broken, the only correct
// answer is a clear error, never silently climbing to the *parent* and
// treating it as if it were the submodule. Reproduced and confirmed: a
// submodule directory registered in the index but with .git deleted (an
// interrupted clone, manual tampering, or a submodule left over from a
// failed operation) made every submodule-targeting command sharing this
// resolution — Submodule Branch Map chief among them, but also commit/push/
// pull/reset/switch-version — silently operate on the *parent* repository's
// own branches and commits, while still showing the submodule's own name
// and path in the UI (those come from the request parameters, not from
// whichever repository actually got opened) — a correctness/safety issue,
// not just a display glitch.
fn internal_submodule_repository(absolute_path: &Path) -> Result<Repository, String> {
    let not_initialized = "Submodule repository is not initialized or its Git metadata is missing. Initialize/reset the submodule first.";
    // Repository::open, unlike discover, never searches parent directories —
    // it finds valid Git metadata rooted exactly at this path (a .git
    // directory, or the gitlink *file* pointing at .git/modules/<name>,
    // which open() follows correctly, matching a real submodule checkout) or
    // it fails outright.
    let repo = Repository::open(absolute_path).map_err(|_| not_initialized.to_string())?;
    // Defense in depth, not a substitute for open()'s own no-search
    // guarantee: confirm the workdir actually opened, canonicalized, really
    // is this exact directory.
    let workdir = repo.workdir().ok_or_else(|| not_initialized.to_string())?;
    let canonical_workdir = workdir.canonicalize().map_err(|_| not_initialized.to_string())?;
    let canonical_expected = absolute_path.canonicalize().map_err(|_| not_initialized.to_string())?;
    if canonical_workdir != canonical_expected { return Err(not_initialized.to_string()); }
    Ok(repo)
}

fn internal_statuses(repository: &Repository, scope: Option<&str>) -> Result<Vec<(String, String, bool)>, String> {
    let mut options = StatusOptions::new();
    // Correctness over saved time here: not recursing into wholly-untracked
    // directories was a deliberate speed trade that backfired — copying a new,
    // non-gitignored folder full of files (e.g. 245 new source files added from
    // outside the app) collapsed into a *single* status entry for the folder
    // itself, so those files simply didn't appear in the Changes list at all.
    // Plain `git status` recurses into untracked directories by default for
    // exactly this reason; matching that here is what actually makes new files
    // visible and individually selectable, which matters more than the scan
    // time saved on the (rare) case of a huge non-gitignored directory.
    // Deliberately NOT update_index(true): it opportunistically refreshes the
    // on-disk index's cached file stat info *during the scan* (the same
    // trick plain `git status` uses) — which means it writes to .git/index.
    // A status scan is called from many places that never take
    // repo_write_lock (navigation, refresh_status) — a scan racing an actual
    // stage/commit (which does hold that lock, but only around its *own* write, not
    // around every concurrent status scan elsewhere) could interleave writes
    // to the same index file. A function documented and relied on as
    // read-only must not write at all, regardless of the perf upside.
    options.include_untracked(true).recurse_untracked_dirs(true).include_ignored(false);
    // Do not enable rename detection here, including for a scoped directory.
    // It compares added/deleted contents and a Windows trace measured 27-33s
    // merely opening the large `work` scope. Rename detection is cosmetic for
    // status: the same operation remains accurately visible as delete + add,
    // while Stage/Commit still record the exact resulting tree.
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

// A trailing '/' or '\' (routinely present when a folder path is pasted from
// Windows Explorer's address bar, or built by joining path segments with a
// separator) survives completely unnoticed through `Path` — `components()`
// treats "foo/" and "foo" the same, but `to_string_lossy()`/`Path::to_path_buf()`
// preserve the literal trailing separator in the string. That string is what
// eventually reaches libgit2 as a pathspec (e.g. commit_path -> normalized() ->
// index.add_all/write_tree), where a trailing separator makes it reject the
// whole path outright ("invalid path"), even though the folder genuinely
// exists. Stripped once, here, since virtually every command that accepts a
// repository-relative path from the frontend already funnels through this —
// a single fix point instead of every caller having to remember to trim it
// (stage_files already did its own ad hoc version of this before this fix).
fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    let trimmed = value.trim_end_matches(['/', '\\']);
    let path = Path::new(trimmed);
    if path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return Err("Invalid repository-relative path".into());
    }
    Ok(path.to_path_buf())
}

fn normalized(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

// A directory with its own `.git` (file or folder) inside it — an
// unregistered embedded repository (not a registered submodule; those are
// already routed elsewhere before a path would reach this check) — makes
// libgit2 treat it as an opaque repository boundary and reject the whole
// pathspec with a bare "invalid path: '<dir>/'" that gives no indication
// why. Confirmed by direct reproduction: staging a directory containing
// `.git` fails with exactly that message. Checked explicitly wherever a
// directory is about to be handed to index.add_all, so the real cause is
// reported instead.
fn embedded_git_repo_path(absolute: &Path) -> bool {
    absolute.join(".git").exists()
}

fn embedded_git_repo_error(relative: &str) -> String {
    format!("\"{relative}\" contains its own .git and can't be staged as a plain folder. Either delete its .git folder and stage it as regular files, or add it properly as a Git submodule instead (Explorer → right-click the parent folder → Add submodule).")
}

// Returns (url, branch) from a single `repo.submodules()` scan/find — callers
// that need both (entry_details did) used to call a single-field version of
// this twice, each re-parsing .gitmodules/config and re-scanning the
// submodule list from scratch just to read one field out of the same entry.
fn submodule_url_and_branch(repository: &str, path: &str) -> (Option<String>, Option<String>) {
    (|| -> Option<(Option<String>, Option<String>)> {
        let repo = internal_repository(repository).ok()?;
        let submodule = repo.submodules().ok()?.into_iter().find(|item| normalized(item.path()) == path)?;
        Some((submodule.url().map(String::from), submodule.branch().map(String::from)))
    })().unwrap_or((None, None))
}

fn submodule_value(repository: &str, path: &str, field: &str) -> Option<String> {
    let (url, branch) = submodule_url_and_branch(repository, path);
    match field { "url" => url, "branch" => branch, _ => None }
}

fn worktree_status(repository: &str, scope: Option<&str>) -> Vec<(String, String)> {
    // If a full, unscoped scan is already sitting fresh in cache (load_repository
    // needs one anyway, for the Changes drawer, on essentially every action), reuse
    // it here filtered by prefix instead of asking libgit2 for a second scan of
    // this one folder right after. Deliberately NOT a replacement for the scoped
    // scan below, only an opportunistic skip of it: a scoped, pathspec-limited
    // scan of one small folder is far cheaper than a *fresh* full-repository scan
    // when browsing far from any recent reload (the common case on a large
    // repository, and the reason the scoped scan exists at all) — this only
    // helps the case where a full scan was *just* computed for something else.
    if let Some((cached_at, statuses)) = full_status_cache().lock().unwrap().get(repository) {
        if cached_at.elapsed() < GIT_METADATA_TTL {
            return match scope {
                None | Some("") => statuses.iter().map(|(path, status, _)| (path.clone(), status.clone())).collect(),
                Some(scope) => {
                    let prefix = format!("{scope}/");
                    statuses.iter().filter(|(path, _, _)| path == scope || path.starts_with(&prefix)).map(|(path, status, _)| (path.clone(), status.clone())).collect()
                }
            };
        }
    }
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
        if cached_at.elapsed() < GIT_METADATA_TTL {
            perf_log(&format!("cached_git_metadata: HIT ({scope})"), Duration::ZERO);
            return metadata.clone();
        }
    }
    // Rapid navigation can ask for the same folder twice before the first
    // expensive status scan completes. Serialize only that exact
    // repository+scope key, then reuse the result produced by the winner.
    let lock_handle = metadata_scan_lock(&key);
    let _guard = lock_handle.lock().unwrap();
    if let Some((cached_at, metadata)) = metadata_cache().lock().unwrap().get(&key) {
        if cached_at.elapsed() < GIT_METADATA_TTL {
            perf_log(&format!("cached_git_metadata: HIT after concurrent scan ({scope})"), Duration::ZERO);
            return metadata.clone();
        }
    }
    let step = Instant::now();
    let metadata = build_git_metadata(repository, Some(scope), None);
    perf_log(&format!("cached_git_metadata: MISS, scanned ({scope})"), step.elapsed());
    metadata_cache().lock().unwrap().insert(key, (Instant::now(), metadata.clone()));
    metadata
}

// Sorted, binary-searchable views of the (repository-wide, not per-folder)
// tracked/unpushed sets — built once per repository and reused across every
// folder navigated to, instead of collecting-and-sorting a fresh Vec on every
// single `load_directory`/`entry_details` call regardless of whether the
// underlying sets actually changed since the last one. Same long TTL and the
// same invalidation point as `cached_index_metadata`/`cached_unpushed_paths`,
// since it's derived from exactly those and just as unaffected by which
// folder is being browsed.
struct SortedLookups { tracked: Vec<String>, unpushed: Vec<String> }

static SORTED_LOOKUPS_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Arc<SortedLookups>)>>> = OnceLock::new();

fn sorted_lookups_cache() -> &'static Mutex<HashMap<String, (Instant, Arc<SortedLookups>)>> {
    SORTED_LOOKUPS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_sorted_lookups(repository: &str) -> Arc<SortedLookups> {
    if let Some((cached_at, data)) = sorted_lookups_cache().lock().unwrap().get(repository) {
        if cached_at.elapsed() < INDEX_METADATA_TTL { return data.clone(); }
    }
    let (tracked, _) = cached_index_metadata(repository);
    let unpushed = cached_unpushed_paths(repository);
    let mut tracked_sorted: Vec<String> = tracked.iter().cloned().collect();
    tracked_sorted.sort_unstable();
    let mut unpushed_sorted: Vec<String> = unpushed.iter().cloned().collect();
    unpushed_sorted.sort_unstable();
    let data = Arc::new(SortedLookups { tracked: tracked_sorted, unpushed: unpushed_sorted });
    sorted_lookups_cache().lock().unwrap().insert(repository.to_string(), (Instant::now(), data.clone()));
    data
}

fn has_sorted_prefix(sorted: &[String], prefix: &str) -> bool {
    let idx = sorted.partition_point(|candidate| candidate.as_str() < prefix);
    sorted.get(idx).map(|candidate| candidate.starts_with(prefix)).unwrap_or(false)
}

fn replace_git_metadata(repository: &str, statuses: Vec<(String, String)>) {
    // `statuses` here is always a full, unscoped scan (from load_repository, which
    // needs every change for the Changes drawer regardless of folder) — seed the
    // repository-wide cache entry with it instead of discarding that work.
    let metadata = build_git_metadata(repository, None, Some(statuses));
    metadata_cache().lock().unwrap().insert(metadata_cache_key(repository, ""), (Instant::now(), metadata));
}

// submodule_state_cache is deliberately NOT cleared by this or by
// invalidate_scoped_and_index_metadata below: this runs after essentially
// every mutation, including an ordinary file stage/commit that has nothing
// to do with any submodule's own state — clearing it here would force the
// next fold/load to redo real, expensive submodule I/O regardless, defeating
// its TTL entirely on the most common action in the app. It's invalidated
// explicitly instead, wherever it's actually relevant: see
// invalidate_submodule_sync below.
fn invalidate_git_metadata(repository: &str) {
    invalidate_scoped_and_index_metadata(repository);
    full_status_cache().lock().unwrap().remove(repository);
}

// The cheap part of invalidate_git_metadata: evicting cache entries costs
// nothing beyond the HashMap operation itself — each one's real cost (an
// actual filesystem/libgit2 scan) is only paid lazily, scoped to exactly the
// folder next opened. Shared with invalidate_git_metadata_for_submodule_checkout
// below, which handles full_status_cache differently.
fn invalidate_scoped_and_index_metadata(repository: &str) {
    // Cache keys are "{repository}\0{scope}" (one entry per folder that's been
    // browsed) — a mutation can affect any of them, so drop every scope cached for
    // this repository, not just the unscoped entry.
    let prefix = format!("{repository}\u{0}");
    metadata_cache().lock().unwrap().retain(|key, _| !key.starts_with(&prefix));
    index_metadata_cache().lock().unwrap().remove(repository);
    unpushed_paths_cache().lock().unwrap().remove(repository);
    sorted_lookups_cache().lock().unwrap().remove(repository);
}

// A submodule checkout-only operation (switching version, restoring the
// project-recorded commit, resetting to upstream) can only ever change ONE
// thing from the *parent* repository's point of view: this submodule's own
// gitlink status entry (dirty vs. matching what's recorded) — nothing else
// in the parent's own working tree is touched. invalidate_git_metadata's
// blanket full_status_cache eviction forces the next status need to redo a
// full, unscoped scan of the entire repository — on a large repository this
// was measured at 5-40+ seconds *per call*, and this exact operation was
// called repeatedly while a submodule version was worked out interactively.
// Patch just the one entry that could have changed instead, via a
// pathspec-limited rescan (same StatusOptions internal_statuses always uses,
// so the result for that one path is identical to what a full scan would
// have produced) — every other already-cached path's status is left
// untouched, and the cache's own TTL clock is left running from when it was
// last fully verified, not reset by this partial patch. If nothing is
// cached yet, there is nothing to patch — the next status need just does an
// ordinary full scan, same as before this function existed. Everything else
// invalidate_git_metadata clears (the scoped per-folder Explorer cache,
// index/unpushed/sorted-lookups caches) is still fully cleared exactly as
// before: those are cheap to evict, so there is no correctness trade-off in
// leaving that part alone.
fn invalidate_git_metadata_for_submodule_checkout(repository_path: &str, submodule_relative_path: &str) {
    invalidate_scoped_and_index_metadata(repository_path);
    let step = Instant::now();
    let mut cache = full_status_cache().lock().unwrap();
    let Some((cached_at, statuses)) = cache.get(repository_path) else {
        perf_log(&format!("invalidate_git_metadata_for_submodule_checkout: nothing cached to patch ({submodule_relative_path})"), step.elapsed());
        return;
    };
    let cached_at = *cached_at;
    let mut patched: Vec<(String, String, bool)> = statuses.iter().filter(|(path, _, _)| path != submodule_relative_path).cloned().collect();
    match internal_repository(repository_path).and_then(|repo| internal_statuses(&repo, Some(submodule_relative_path))) {
        Ok(fresh) => {
            let now_dirty = !fresh.is_empty();
            patched.extend(fresh);
            cache.insert(repository_path.to_string(), (cached_at, patched));
            perf_log(&format!("invalidate_git_metadata_for_submodule_checkout: patched in place, avoided a full rescan ({submodule_relative_path}, now {})", if now_dirty { "dirty" } else { "clean" }), step.elapsed());
        }
        // Could not verify this one path — do not guess. Falls back to
        // exactly the old behavior: the next status need does a full scan.
        Err(_) => {
            cache.remove(repository_path);
            perf_log(&format!("invalidate_git_metadata_for_submodule_checkout: could not verify, fell back to a full clear ({submodule_relative_path})"), step.elapsed());
        }
    }
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

fn parent_gitlink_oid(repo: &Repository, relative_path: &str, from_index: bool) -> Option<Oid> {
    if from_index {
        return repo.index().ok()?.get_path(Path::new(relative_path), 0).map(|entry| entry.id);
    }
    repo.head().ok()?.peel_to_commit().ok()?.tree().ok()?.get_path(Path::new(relative_path)).ok().map(|entry| entry.id())
}

fn submodule_workflow_state(parent: &Repository, relative_path: &str, parent_unpushed: bool, snapshot: &SubmoduleStateSnapshot) -> String {
    if snapshot.dirty { return "changes_inside".into(); }
    let submodule_head = match snapshot.head_oid {
        Some(oid) => oid,
        None => return "unavailable".into(),
    };
    match snapshot.remote_relation {
        SubmoduleRemoteRelation::PushNeeded => return "local_commit_push_needed".into(),
        SubmoduleRemoteRelation::SyncNeeded => return "sync_needed".into(),
        SubmoduleRemoteRelation::DetachedNeedsBranch => return "detached_choose_branch".into(),
        SubmoduleRemoteRelation::NoOrigin => return "origin_missing".into(),
        SubmoduleRemoteRelation::Unknown => return "status_unknown".into(),
        SubmoduleRemoteRelation::OnOrigin => {}
    }
    let index_oid = parent_gitlink_oid(parent, relative_path, true);
    if index_oid != Some(submodule_head) { return "on_origin_stage_project".into(); }
    let committed_oid = parent_gitlink_oid(parent, relative_path, false);
    if committed_oid != Some(submodule_head) { return "on_origin_commit_project".into(); }
    if parent_unpushed { return "project_commit_push_needed".into(); }
    "synced".into()
}

// Lets the UI show exactly which commit this binary was built from — set at
// compile time in build.rs. Answers "am I actually running the new build?"
// by looking at the app itself instead of a file's modified date, which is
// what caused a stale Windows executable to go untested for hours.
#[tauri::command]
pub fn build_info() -> String { env!("GIT_DRILLDOWN_BUILD_SHA").to_string() }

// Lets the frontend write into the exact same perf log the backend uses (see
// `perf_log` above) — `elapsed_ms` comes from `performance.now()` on the JS
// side, since that's real wall-clock time for work that happens entirely in
// the webview (invoke round-trip, DOM rebuild) and never touches Rust at all.
// One combined log instead of "check the file, then also open devtools"
// makes it obvious whether a slow navigation is the backend call or the
// frontend's own rendering.
#[tauri::command]
pub fn frontend_perf_log(label: String, elapsed_ms: f64) { perf_log(&format!("frontend: {label}"), Duration::from_secs_f64(elapsed_ms.max(0.0) / 1000.0)); }

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
pub fn clone_repository(url: String, parent_path: String, folder_name: String, branch: Option<String>, recurse_submodules: Option<bool>) -> Result<String, String> {
    validate_path(&parent_path)?;
    let url = url.trim(); let folder_name = folder_name.trim();
    if url.is_empty() { return Err("Repository URL cannot be empty".into()); }
    let folder = safe_relative_path(folder_name)?;
    if folder.components().count() != 1 || folder_name.is_empty() { return Err("Choose a simple local folder name".into()); }
    let destination = Path::new(&parent_path).join(&folder);
    if destination.exists() { return Err("The destination folder already exists".into()); }
    let mut builder = git2::build::RepoBuilder::new();
    if let Some(branch) = branch.as_deref().map(str::trim).filter(|branch| !branch.is_empty()) {
        if branch.starts_with('-') || branch.contains("..") || branch.contains('\\') {
            return Err("Choose a valid branch name to clone".into());
        }
        builder.branch(branch);
    }
    let fetch = network_fetch_options(); builder.fetch_options(fetch); builder.clone(url, &destination).map_err(|error| format!("Clone failed: {}", error.message()))?;
    if recurse_submodules.unwrap_or(false) {
        let destination_string = destination.to_string_lossy().into_owned();
        git(&destination_string, &["submodule", "update", "--init", "--recursive"])
            .map_err(|error| format!("Repository cloned, but submodules could not be initialized: {error}"))?;
    }
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

// Matches only the exact shape returned for a PullRequestSummary.url — e.g.
// "https://github.com/owner/repo/pull/42" — plus the exact `/owner/repo/pulls`
// fallback page Git DrillDown itself constructs when API authentication is
// unavailable. Not a fixed-host allowlist like Polarion's, since Enterprise
// hosts vary; the path and every identifier remain strictly validated because
// the URL reaches an OS browser-launch command.
fn is_generated_pull_request_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let (rest, fragment) = rest.split_once('#').map(|(base, fragment)| (base, Some(fragment))).unwrap_or((rest, None));
    let Some((host, path)) = rest.split_once('/') else { return false };
    if host.is_empty() || !host.chars().all(|value| value.is_ascii_alphanumeric() || value == '.' || value == '-') { return false; }
    let is_identifier = |value: &str| !value.is_empty() && value.chars().all(|value| value.is_ascii_alphanumeric() || value == '.' || value == '-' || value == '_');
    match path.split('/').collect::<Vec<_>>().as_slice() {
        [owner, repo, "pull", number] => {
            let review_fragment_ok = fragment.map(|value| value.strip_prefix("pullrequestreview-")
                .map(|id| !id.is_empty() && id.chars().all(|character| character.is_ascii_digit())).unwrap_or(false)).unwrap_or(true);
            is_identifier(owner) && is_identifier(repo) && !number.is_empty() && number.chars().all(|value| value.is_ascii_digit()) && review_fragment_ok
        },
        [owner, repo, "pulls"] => is_identifier(owner) && is_identifier(repo) && fragment.is_none(),
        _ => false,
    }
}

#[tauri::command]
pub fn open_external_url(url: String) -> Result<(), String> {
    if !is_generated_polarion_url(&url) && !is_generated_pull_request_url(&url) { return Err("Only generated Polarion or pull request links can be opened".into()); }
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(&url).status();
    #[cfg(target_os = "windows")]
    let status = Command::new("cmd").args(["/C", "start", "", &url]).status();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let status = Command::new("xdg-open").arg(&url).status();
    status.map_err(|error| error.to_string()).and_then(|result| result.success().then_some(()).ok_or_else(|| "Could not open the default browser".into()))
}

// Check-run Details links are supplied by GitHub itself and may point to an
// Enterprise integration (Collaborator/Jenkins), not back to `/pull/<n>`.
// Keep this separate from the narrow PR-link opener and accept only HTTPS
// links on GitHub or the corporate vitesco.io domain. On Windows, use the
// shell URL handler directly rather than interpolating the URL into `cmd`.
#[tauri::command]
pub fn open_status_check_url(url: String) -> Result<(), String> {
    let Some(rest) = url.strip_prefix("https://") else { return Err("A check Details link must use HTTPS".into()); };
    let host = rest.split('/').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    if !(is_github_like_host(&host) || host == "vitesco.io" || host.ends_with(".vitesco.io")) {
        return Err(format!("Refusing to open a check Details link from an untrusted host: {host}"));
    }
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(&url).status();
    #[cfg(target_os = "windows")]
    let status = Command::new("explorer.exe").arg(&url).status();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let status = Command::new("xdg-open").arg(&url).status();
    status.map_err(|error| error.to_string()).and_then(|result| result.success().then_some(()).ok_or_else(|| "Could not open the check Details link".into()))
}

// UTRUD is a legacy internal tool, previously only reachable via Windows
// Explorer's "Send to" menu (a per-user .bat under
// AppData\Roaming\Microsoft\Windows\SendTo that just forwards whatever's
// selected as `%*` to "C:\LegacyApp\UTRUD\2.0.0\UTRUD.bat"). This gives the
// same launch from inside the app for any selected folder in the
// tree — `spawn` (not `status`/`output`) because UTRUD opens and runs its
// own window independently, the same "fire and forget" way Send To behaves;
// waiting on it here would block the app until the user closes UTRUD.
#[cfg(target_os = "windows")]
const UTRUD_BATCH_PATH: &str = r"C:\LegacyApp\UTRUD\2.0.0\UTRUD.bat";

// Split out so the cwd/argument logic can be exercised by a plain unit test
// without actually spawning a Windows process. UTRUD's own script builds its
// results path by appending the selected folder's *name* to its process's
// current directory — mirroring Explorer's "Send to", where the current
// directory is the *parent* of whatever you right-clicked, not the item
// itself. Setting current_dir to the selected folder itself overcorrected:
// UTRUD's own `cwd + name` logic doubled the final folder name (for example
// ".../r/r"). The absolute selected-folder path stays the argument either
// way; only cwd must be the parent.
fn utrud_command_parts(absolute: &Path) -> (PathBuf, PathBuf) {
    let cwd = absolute.parent().map(Path::to_path_buf).unwrap_or_else(|| absolute.to_path_buf());
    (cwd, absolute.to_path_buf())
}

#[cfg(target_os = "windows")]
fn utrud_batch_literal(path: &Path) -> String {
    // Batch files expand `%NAME%` even inside quotes. A normal Windows path
    // almost never contains `%`, but if it does, doubling it is the correct
    // way to write a literal percent in a generated .cmd file.
    path.display().to_string().replace('%', "%%")
}

#[cfg(target_os = "windows")]
fn create_utrud_launcher_script(cwd: &Path, argument: &Path) -> Result<PathBuf, String> {
    let temp = std::env::temp_dir().join(format!(
        "git-drilldown-run-utrud-{}-{}.cmd",
        std::process::id(),
        std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|value| value.as_nanos()).unwrap_or(0)
    ));
    let script = format!(
        "@echo off\r\n\
echo Git Drill Down UTRUD launcher\r\n\
echo Working directory: \"{}\"\r\n\
echo Selected folder:   \"{}\"\r\n\
echo UTRUD launcher:    \"{}\"\r\n\
echo.\r\n\
if not exist \"{}\" (\r\n\
  echo UTRUD failed to start.\r\n\
  echo The configured UTRUD launcher does not exist.\r\n\
  pause\r\n\
  exit /b 1\r\n\
)\r\n\
cd /d \"{}\"\r\n\
call \"{}\" \"{}\"\r\n\
if errorlevel 1 (\r\n\
  echo.\r\n\
  echo UTRUD failed to start.\r\n\
  pause\r\n\
)\r\n",
        utrud_batch_literal(cwd),
        utrud_batch_literal(argument),
        UTRUD_BATCH_PATH.replace('%', "%%"),
        UTRUD_BATCH_PATH.replace('%', "%%"),
        utrud_batch_literal(cwd),
        UTRUD_BATCH_PATH.replace('%', "%%"),
        utrud_batch_literal(argument),
    );
    fs::write(&temp, script).map_err(|error| format!("Could not create temporary UTRUD launcher: {error}"))?;
    Ok(temp)
}

#[tauri::command]
pub fn run_utrud(repository_path: String, relative_path: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    if !absolute.is_dir() { return Err("The selected path is not a folder".into()); }
    let (cwd, argument) = utrud_command_parts(&absolute);
    #[cfg(target_os = "windows")]
    {
        let batch = Path::new(UTRUD_BATCH_PATH);
        if !batch.is_file() {
            return Err(format!("UTRUD was not started because its launcher was not found at {UTRUD_BATCH_PATH}. Install UTRUD or update its configured path."));
        }
        perf_log(&format!("run_utrud: cwd={} arg={}", cwd.display(), argument.display()), Duration::ZERO);
        // Start a separate visible command process, like Explorer's Send To
        // action. Use a generated .cmd wrapper instead of a nested
        // `cmd /C start ... cmd /C call ...` command line: the nested form is
        // very sensitive to Windows quoting and has produced literal
        // `\"C:\...\"` launcher names on real machines. The wrapper keeps
        // the effective command obvious and identical to Send To:
        //   call "C:\LegacyApp\UTRUD\2.0.0\UTRUD.bat" "<selected folder>"
        // On failure the console stays open with the actual batch error.
        let launcher = create_utrud_launcher_script(&cwd, &argument)?;
        let status = Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&launcher)
            .current_dir(&cwd)
            .status().map_err(|error| format!("Windows could not create the UTRUD process: {error}"))?;
        if !status.success() { return Err(format!("Windows rejected the UTRUD launch request (exit code {:?}).", status.code())); }
        Ok(format!("UTRUD launch requested for {}. If the tool fails, its console remains open with the reason.", argument.display()))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (cwd, argument);
        Err("UTRUD is only available on Windows".into())
    }
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

// A submodule URL in `.gitmodules` is very often stored *relative* (`../sibling`
// so the same file works over SSH and HTTPS). Git resolves it against the
// parent's own remote URL when it clones/syncs the submodule; a `../` strips
// one path segment off the parent URL, so `git@host:eng/parent.git` + `../dep`
// becomes `git@host:eng/dep` — a *sibling* of the parent, never a child.
fn is_relative_git_url(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("./") || u.starts_with("../")
}

// Mirror git's own `relative_url()` (remote.c): split the base into an
// authority prefix that must be preserved verbatim (`scheme://host`, or the
// `git@host:` of an scp-like URL) and a path we can pop segments from, then
// apply each component of `relative` — `.` is a no-op, `..` pops one segment,
// anything else is appended. Returns `relative` unchanged when it isn't
// actually relative.
fn resolve_relative_git_url(base: &str, relative: &str) -> String {
    let rel = relative.trim();
    if !is_relative_git_url(rel) { return rel.to_string(); }
    let base = base.trim().trim_end_matches('/');

    let (prefix, path): (String, &str) = if let Some(idx) = base.find("://") {
        let after = &base[idx + 3..];
        match after.find('/') {
            Some(slash) => (format!("{}{}", &base[..idx + 3], &after[..slash]), &after[slash..]),
            None => (base.to_string(), ""),
        }
    } else if let Some((host, rest)) = base.split_once(':') {
        // scp-like `git@host:path` — but not a Windows drive letter (`C:\...`)
        // or a bare path that merely contains a colon.
        if host.len() > 1 && !host.contains(['/', '\\']) && !rest.starts_with(['/', '\\']) {
            (format!("{host}:"), rest)
        } else {
            (String::new(), base)
        }
    } else {
        (String::new(), base)
    };

    let had_leading_slash = path.starts_with('/');
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => { segments.pop(); }
            other => segments.push(other),
        }
    }
    let mut joined = segments.join("/");
    if had_leading_slash { joined.insert(0, '/'); }
    format!("{prefix}{joined}")
}

fn first_remote_url(repo: &Repository) -> Option<String> {
    repo.find_remote("origin").ok()
        .or_else(|| repo.remotes().ok().and_then(|names| {
            names.iter().flatten().next().and_then(|name| repo.find_remote(name).ok())
        }))
        .and_then(|remote| remote.url().map(str::to_string))
}

// Corporate GitHub repositories require the clone-independent form in
// `.gitmodules`: `../../ORG/REPO`. Keep accepting the convenient full URL
// users copy from a browser, but persist the portable form. The submodule's
// own `origin` is resolved separately below, so normal fetch/push operations
// still use a complete network URL.
fn portable_submodule_configured_url(url: &str) -> String {
    let trimmed = url.trim();
    let Some(repository) = parse_github_repo(trimmed) else { return trimmed.to_string() };
    if !repository.host.eq_ignore_ascii_case("github.vitesco.io") { return trimmed.to_string(); }
    let suffix = if trimmed.trim_end_matches('/').ends_with(".git") { ".git" } else { "" };
    format!("../../{}/{}{}", repository.owner, repository.repo, suffix)
}

// Resolve only for actual I/O. The returned URL must never be written back
// to `.gitmodules`: doing that would turn a portable project definition into
// an HTTPS/SSH-specific one and fail the enterprise submodule policy check.
fn resolved_submodule_io_url(parent: &Repository, configured_url: &str) -> Result<String, String> {
    if !is_relative_git_url(configured_url) { return Ok(configured_url.trim().to_string()); }
    let parent_remote = first_remote_url(parent).ok_or(
        "This portable submodule URL needs the parent repository to have a remote (normally origin) so it can be resolved"
    )?;
    Ok(resolve_relative_git_url(&parent_remote, configured_url))
}

// The browser base URL for a submodule's *own* repository. Prefers the
// submodule's resolved `origin` (what `git submodule update` wrote into its
// `.git/config`) when that is absolute; otherwise resolves the `.gitmodules`
// URL against the parent repository's remote, exactly as git would — so a
// relative `../dep` lands on the sibling of the parent, not a child path
// under it.
fn submodule_browser_base(parent_repo_path: &str, submodule_rel_path: &str) -> Result<String, String> {
    let relative = safe_relative_path(submodule_rel_path)?;
    let wanted = normalized(&relative);
    let parent = internal_repository(parent_repo_path)?;
    let parent_remote = first_remote_url(&parent);

    let own_origin = internal_submodule_repository(&Path::new(parent_repo_path).join(&relative)).ok()
        .and_then(|repo| first_remote_url(&repo));
    let configured = parent.submodules().ok().into_iter().flatten()
        .find(|item| normalized(item.path()) == wanted)
        .and_then(|item| item.url().map(String::from));

    // Deliberately never fall back to the *parent's* own URL here: that is
    // exactly the wrong-destination bug this function exists to prevent. If
    // nothing submodule-specific is known, say so.
    let resolved = if let Some(url) = own_origin.as_deref().filter(|u| !is_relative_git_url(u)) {
        url.to_string()
    } else if let Some(cfg) = configured.filter(|u| !u.trim().is_empty()) {
        if is_relative_git_url(&cfg) {
            let base = parent_remote.as_deref()
                .ok_or("The parent repository has no remote to resolve this submodule's relative URL against")?;
            resolve_relative_git_url(base, &cfg)
        } else {
            cfg
        }
    } else if let Some(url) = own_origin {
        // Only a relative origin and no .gitmodules entry to cross-check —
        // still resolve it against the parent rather than hand back `../x`.
        match parent_remote.as_deref() {
            Some(base) => resolve_relative_git_url(base, &url),
            None => return Err("This submodule's only remote URL is relative and the parent has no remote to resolve it against".into()),
        }
    } else {
        return Err("This submodule has no remote URL of its own to open".into());
    };

    browser_repository_url(resolved.trim()).ok_or_else(|| "This submodule's remote URL cannot be opened in a browser".into())
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
    let repo = internal_repository(&repository_path)?;
    let remote = first_remote_url(&repo).ok_or("The remote has no URL")?;
    let base = browser_repository_url(remote.trim()).ok_or("This remote URL cannot be opened in a browser")?;
    let branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).or_else(|| repo.head().ok().and_then(|head| head.target().map(|id| id.to_string()))).unwrap_or_else(|| "HEAD".into());
    let path = normalized(&relative);
    let url = if path.is_empty() { base } else {
        let segment = if kind == "file" { "blob" } else { "tree" };
        format!("{base}/{segment}/{branch}/{path}")
    };
    launch_browser(&url)
}

// Open a submodule's *own* repository on the server. A submodule is not a
// folder inside the parent's repo — going to `<parent-url>/tree/<branch>/<path>`
// (what `open_repository_item` would do) lands on the parent's gitlink, not the
// submodule's project. This resolves the submodule's actual remote instead,
// handling the common relative-`.gitmodules`-URL case (`../dep` -> sibling).
#[tauri::command]
pub fn open_submodule_on_server(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let base = submodule_browser_base(&repository_path, &relative_path)?;
    launch_browser(&base)
}

#[tauri::command]
pub fn open_commit_on_server(repository_path: String, commit_id: String, submodule_path: Option<String>) -> Result<(), String> {
    validate_path(&repository_path)?;
    let base = match submodule_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(sub) => submodule_browser_base(&repository_path, sub)?,
        None => {
            let repo = internal_repository(&repository_path)?;
            let remote = first_remote_url(&repo).ok_or("The remote has no URL")?;
            browser_repository_url(remote.trim()).ok_or("This remote URL cannot be opened in a browser")?
        }
    };
    launch_browser(&format!("{base}/commit/{}", commit_id.trim()))
}

// A submodule whose own checked-out commit differs from what the parent's
// index currently records — however that happened (its dedicated "Commit
// submodule" button, a raw git command typed in the console, `git pull` run
// directly inside it, another tool entirely) — is not "reconciled" by
// anything passive. It just shows as modified, exactly like any other
// uncommitted change, the moment the regular status scan sees it (git2's
// own status walk already compares a submodule's HEAD against the parent's
// recorded gitlink — no extra scan needed for that). Staging a new gitlink
// into the parent's index only ever happens as the direct, explicit result
// of the user's own action: pushing the submodule (stage_pushed_submodule_in_parent,
// called only from push_submodule/force_push_submodule, and from
// commit_submodule when it also pushed — never from a Refresh, navigation,
// or any other passive reload) — and even then it is only ever staged, never
// committed into the parent's history on its own (see the
// submodule-publish-safety report: committing the parent automatically here
// used to let "Publish main project" push a gitlink that pointed at a
// submodule commit which existed only locally). This used to also run
// speculatively from every load_repository call (even non-forced ones, at
// one point) specifically to catch an *externally*-made submodule commit —
// which is exactly the safety issue this removes: opening or refreshing a
// repository must never silently create a commit in it that nobody asked
// for, no matter how that submodule got to its new commit. It cost real
// time for it too (18.5 seconds on one load_repository call, on a real
// Windows repository with many submodules) for a guarantee this app no
// longer makes.

// Called explicitly wherever it's actually relevant — a command that
// specifically changes a submodule's own commit/registration — rather than
// from the general invalidate_git_metadata (which also fires on every
// ordinary file stage/commit that has nothing to do with submodules).
fn invalidate_submodule_sync(repository: &str) {
    // Prefix match, not a single exact key: submodule_state_cache is keyed
    // by each submodule's own absolute path (repository/relative/...), not
    // by the parent's path.
    let prefix = format!("{repository}/");
    submodule_state_cache().lock().unwrap().retain(|key, _| key != repository && !key.starts_with(&prefix));
}

// A fast, read-only, additive first phase for *opening a repository specifically*
// — validates and canonicalizes it (via internal_repository/Repository::discover,
// same as everywhere else), reads its name/current branch, and returns
// branches/commits/stashes, but does none of the genuinely expensive part (a
// real Windows perf log measured 15-23s in the full status scan on
// repositories with many files): no status scan. The frontend shows this
// immediately (folders/files navigable, tracked/status shown as "Loading
// status…", Stage/Delete/Commit/checkout disabled) then calls refresh_status
// in the background to complete it.
//
// Deliberately a separate command, not a new mode of load_repository: that
// function is relied on (and tested) to do a single, synchronous, fully
// reconciled load — used for Refresh and after every mutating action, where
// "did an externally-made submodule commit get caught" and "is status
// correct right now" are exactly the guarantees needed. This only replaces
// the *very first* open of a repository, where showing structure immediately
// and filling in status a moment later is a better trade than blocking on
// both up front. The modest duplication with load_repository's own
// branches/commits/stashes logic is intentional: reusing a shared helper
// would mean any future change to it risks affecting both, when only one of
// them needs to change here.
// The first page of history load_repository/open_repository_fast return —
// not "all of history", however small a repository's real total is. Kept
// small on purpose (perf, not a hard protocol limit): load_older_commits
// below is how the graph view gets the rest, one page at a time, on
// explicit request.
const GRAPH_COMMIT_WINDOW: usize = 500;

// The single source both the revwalk's seed set and every commit's ref
// badges come from — previously two *separate* full passes over
// repo.references() (one building an oid->names map, one re-walking the
// same refs again just to seed the walker), duplicated near-verbatim across
// open_repository_fast/load_repository/load_older_commits. One pass now
// does both, and fixes a real bug the old version had: it used
// `reference.target()` for every ref's badge, which for an annotated tag
// is the tag *object's* own OID, not the commit's — so an annotated tag's
// badge could never be found under any commit's real (peeled) OID, it was
// keyed under an OID no commit in the walk ever has, and just silently
// dropped. `.peel(Commit)` resolves both an annotated tag (through its tag
// object) and a lightweight tag (already a direct commit ref) to the same
// real commit either way, and is what the seed set is built from too — a
// tag's badge and its seed OID can now never disagree.
//
// Intentionally processes only refs/heads, refs/remotes and refs/tags —
// refs/stash's synthetic WIP commit, refs/notes, and this app's own
// transient scratch refs (e.g. push_submodule's partial-publish ref) are
// never a real branch or release marker and must never be shown as one.
// "HEAD" is not in this namespace at all — it stays a client-side-only
// badge (see refsBadges/isHead in app.js), exactly as before this existed.
fn collect_ref_seeds_and_badges(repo: &Repository) -> (Vec<git2::Oid>, HashMap<String, Vec<CommitRef>>) {
    let mut seeds = Vec::new();
    let mut by_commit: HashMap<String, Vec<CommitRef>> = HashMap::new();
    let Ok(references) = repo.references() else { return (seeds, by_commit) };
    for reference in references.flatten() {
        let Some(name) = reference.name() else { continue };
        let kind = if name.starts_with("refs/heads/") {
            "local_branch"
        } else if name.starts_with("refs/remotes/") {
            // origin/HEAD is a *symbolic* pointer at another remote branch,
            // not a branch of its own — excluded here for the same reason
            // the plain `branches` list (built separately, from
            // repo.branches()) already excludes anything ending in "/HEAD".
            if name.ends_with("/HEAD") { continue; }
            "remote_branch"
        } else if name.starts_with("refs/tags/") {
            "tag"
        } else {
            continue;
        };
        let Ok(object) = reference.peel(ObjectType::Commit) else { continue };
        let oid = object.id();
        seeds.push(oid);
        let shorthand = reference.shorthand().unwrap_or(name).to_string();
        by_commit.entry(oid.to_string()).or_default().push(CommitRef { name: shorthand, kind: kind.to_string() });
    }
    (seeds, by_commit)
}

fn tags_pointing_at_commit(repo: &Repository, target: git2::Oid) -> Vec<String> {
    let mut tags = Vec::new();
    let Ok(names) = repo.tag_names(None) else { return tags };
    for name in names.iter().flatten() {
        let Ok(reference) = repo.find_reference(&format!("refs/tags/{name}")) else { continue };
        let Ok(object) = reference.peel(ObjectType::Commit) else { continue };
        if object.id() == target { tags.push(name.to_string()); }
    }
    tags.sort();
    tags
}

#[tauri::command]
pub async fn open_repository_fast(path: String) -> Result<FastRepositoryData, String> {
    off_main_thread(move || open_repository_fast_inner(path)).await
}

fn open_repository_fast_inner(path: String) -> Result<FastRepositoryData, String> {
    let started = Instant::now();
    perf_log(&format!("open_repository_fast: START ({})", anonymized_repository_id(&path)), Duration::ZERO);
    validate_path(&path)?;
    let mut repo = internal_repository(&path)?;
    // The path passed in is whatever the user selected (Repository::discover
    // walks *up* from it to find .git) — not necessarily the repository's
    // actual root. Everywhere else in this command uses `path` as-is only
    // for cosmetic purposes (the display name) or gets superseded by the
    // frontend's own subsequent calls anyway; the one place that genuinely
    // matters is the `repository.path` this returns, which the frontend then
    // uses for every follow-up call (refresh_status, load_directory, every
    // mutation) — using the real canonical workdir here, not the raw
    // selection, is what makes those consistently address the same root
    // regardless of which subfolder the user happened to pick when opening.
    let path = repo.workdir().and_then(|dir| dir.to_str()).map(str::to_string).unwrap_or(path);
    let name = Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or("repository").to_string();
    let head_detached = repo.head_detached().unwrap_or(false);
    let current_branch = if head_detached { String::new() } else { repo.head().ok().and_then(|head| head.shorthand().map(String::from)).unwrap_or_default() };
    let head_oid = repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    let step = Instant::now();
    let mut branches = Vec::new();
    for branch_type in [BranchType::Local, BranchType::Remote] { if let Ok(iterator) = repo.branches(Some(branch_type)) { for item in iterator.flatten() {
        let branch_name = item.0.name().ok().flatten().unwrap_or("").to_string(); if branch_name.ends_with("/HEAD") { continue; }
        branches.push(Branch { current: branch_type == BranchType::Local && branch_name == current_branch, name: branch_name, remote: branch_type == BranchType::Remote });
    } } }
    perf_log("open_repository_fast: branches", step.elapsed());

    let step = Instant::now();
    let (seed_oids, mut refs_by_oid) = collect_ref_seeds_and_badges(&repo);
    let mut commits = Vec::new(); let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).map_err(|error| error.message().to_string())?;
    for oid in seed_oids { let _ = walk.push(oid); }
    let mut commits_truncated = false;
    for (index, oid) in walk.flatten().enumerate() {
        if index >= GRAPH_COMMIT_WINDOW { commits_truncated = true; break; }
        if let Ok(commit) = repo.find_commit(oid) {
            commits.push(Commit { id: oid.to_string(), parents: commit.parent_ids().map(|id| id.to_string()).collect(), subject: commit.summary().unwrap_or("No message").to_string(), author: commit.author().name().unwrap_or("Unknown").to_string(), date: short_date(commit.time().seconds()), refs: refs_by_oid.remove(&oid.to_string()).unwrap_or_default(), lane: 0 });
        }
    }
    perf_log("open_repository_fast: refs+revwalk+commits", step.elapsed());

    let step = Instant::now();
    let mut raw_stashes: Vec<(usize, String, git2::Oid)> = Vec::new();
    let _ = repo.stash_foreach(|index, message, oid| { raw_stashes.push((index, message.to_string(), *oid)); true });
    let stashes = raw_stashes.into_iter().map(|(index, message, oid)| {
        let base_commit = repo.find_commit(oid).ok().and_then(|commit| commit.parent_id(0).ok()).map(|id| id.to_string()).unwrap_or_default();
        StashEntry { index, message, base_commit }
    }).collect();
    perf_log("open_repository_fast: stashes", step.elapsed());

    // Index-only (no working-tree scan) — cheap and exactly what lets the
    // frontend recognize "this path is inside a submodule" purely client-side
    // on every navigation click, without asking the backend each time (see
    // submodule_navigation_status, used only when this list disagrees with
    // what the frontend already believes, e.g. a submodule added since).
    let submodule_paths: Vec<String> = cached_index_metadata(&path).1.iter().cloned().collect();
    let gitdir = repo.path().to_string_lossy().into_owned();
    perf_log("open_repository_fast: TOTAL", started.elapsed());
    Ok(FastRepositoryData { repository: RepositoryInfo { path, name, current_branch, head_oid, head_detached, gitdir, submodule_url: None }, branches, commits, stashes, submodule_paths, commits_truncated })
}

#[tauri::command]
pub async fn load_repository(path: String, force: Option<bool>) -> Result<RepositoryData, String> {
    off_main_thread(move || load_repository_inner(path, force)).await
}

// The synchronous core, callable directly (never crossing an `.await`) by
// anything that already runs off the main thread by virtue of its own
// #[tauri::command] wrapper — submodule_repository, most notably, which
// needs this exact same full load for the submodule's own path.
fn load_repository_inner(path: String, force: Option<bool>) -> Result<RepositoryData, String> {
    let load_started = Instant::now();
    perf_log(&format!("load_repository: START ({}, force={})", anonymized_repository_id(&path), force.unwrap_or(false)), Duration::ZERO);
    validate_path(&path)?;
    // The manual Refresh action exists specifically for "something changed
    // outside this app (a terminal, VS Code...) and I want a truly fresh
    // read" — the short status-scan cache below must never serve it a stale
    // scan from moments earlier just because nothing *this app* did
    // triggered an invalidation.
    if force.unwrap_or(false) { invalidate_git_metadata(&path); invalidate_submodule_sync(&path); }
    let mut repo = internal_repository(&path)?;
    let name = Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or("repository").to_string();
    let head_detached = repo.head_detached().unwrap_or(false);
    let current_branch = if head_detached { String::new() } else { repo.head().ok().and_then(|head| head.shorthand().map(String::from)).unwrap_or_default() };
    let head_oid = repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    let step = Instant::now();
    let mut branches = Vec::new();
    for branch_type in [BranchType::Local, BranchType::Remote] { if let Ok(iterator) = repo.branches(Some(branch_type)) { for item in iterator.flatten() {
        let branch_name = item.0.name().ok().flatten().unwrap_or("").to_string(); if branch_name.ends_with("/HEAD") { continue; }
        branches.push(Branch { current: branch_type == BranchType::Local && branch_name == current_branch, name: branch_name, remote: branch_type == BranchType::Remote });
    } } }
    perf_log("load_repository: branches", step.elapsed());

    // `refs/stash` is a real Git ref but its target is a synthetic WIP commit
    // (with an index/untracked-files "merge" parent structure) that has nothing
    // to do with real branch history — excluded here and surfaced separately as
    // `stashes` instead, so the graph only ever shows real ancestry.
    let step = Instant::now();
    let (seed_oids, mut refs_by_oid) = collect_ref_seeds_and_badges(&repo);
    let mut commits = Vec::new(); let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).map_err(|error| error.message().to_string())?;
    for oid in seed_oids { let _ = walk.push(oid); }
    let mut commits_truncated = false;
    for (index, oid) in walk.flatten().enumerate() {
        if index >= GRAPH_COMMIT_WINDOW { commits_truncated = true; break; }
        if let Ok(commit) = repo.find_commit(oid) {
            commits.push(Commit { id: oid.to_string(), parents: commit.parent_ids().map(|id| id.to_string()).collect(), subject: commit.summary().unwrap_or("No message").to_string(), author: commit.author().name().unwrap_or("Unknown").to_string(), date: short_date(commit.time().seconds()), refs: refs_by_oid.remove(&oid.to_string()).unwrap_or_default(), lane: 0 });
        }
    }
    perf_log("load_repository: refs+revwalk+commits", step.elapsed());

    let step = Instant::now();
    let mut raw_stashes: Vec<(usize, String, git2::Oid)> = Vec::new();
    let _ = repo.stash_foreach(|index, message, oid| { raw_stashes.push((index, message.to_string(), *oid)); true });
    let stashes = raw_stashes.into_iter().map(|(index, message, oid)| {
        let base_commit = repo.find_commit(oid).ok().and_then(|commit| commit.parent_id(0).ok()).map(|id| id.to_string()).unwrap_or_default();
        StashEntry { index, message, base_commit }
    }).collect();
    perf_log("load_repository: stashes", step.elapsed());

    let step = Instant::now();
    let internal = cached_full_statuses(&repo, &path)?;
    perf_log("load_repository: cached_full_statuses", step.elapsed());
    let statuses = internal.iter().map(|(path, status, _)| (path.clone(), status.clone())).collect::<Vec<_>>();
    let changes = internal.into_iter().map(|(path, status, staged)| Change { status, path, staged }).collect();

    replace_git_metadata(&path, statuses);
    let submodule_paths: Vec<String> = cached_index_metadata(&path).1.iter().cloned().collect();
    let gitdir = repo.path().to_string_lossy().into_owned();

    perf_log("load_repository: TOTAL", load_started.elapsed());
    Ok(RepositoryData { repository: RepositoryInfo { path, name, current_branch, head_oid, head_detached, gitdir, submodule_url: None }, branches, commits, changes, stashes, submodule_paths, commits_truncated })
}

#[derive(Serialize)]
pub struct OlderCommitsPage { commits: Vec<Commit>, has_more: bool }

// The graph view's explicit "Load older" — never triggered automatically.
// Re-walks the same ref set load_repository/open_repository_fast used (so
// the ordering is identical), skips everything up to and including
// `after_commit_id` (the oldest commit currently on screen), then returns
// the next page. Read-only: no HEAD/branch/index/remote change of any kind.
#[tauri::command]
pub fn load_older_commits(repository_path: String, after_commit_id: String, limit: Option<usize>) -> Result<OlderCommitsPage, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let limit = limit.unwrap_or(GRAPH_COMMIT_WINDOW);
    let after_oid = git2::Oid::from_str(&after_commit_id).map_err(|error| error.message().to_string())?;

    let (seed_oids, mut refs_by_oid) = collect_ref_seeds_and_badges(&repo);
    let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).map_err(|error| error.message().to_string())?;
    for oid in seed_oids { let _ = walk.push(oid); }

    let mut found_marker = false;
    let mut commits = Vec::new();
    let mut has_more = false;
    for oid in walk.flatten() {
        if !found_marker { if oid == after_oid { found_marker = true; } continue; }
        if commits.len() >= limit { has_more = true; break; }
        if let Ok(commit) = repo.find_commit(oid) {
            commits.push(Commit { id: oid.to_string(), parents: commit.parent_ids().map(|id| id.to_string()).collect(), subject: commit.summary().unwrap_or("No message").to_string(), author: commit.author().name().unwrap_or("Unknown").to_string(), date: short_date(commit.time().seconds()), refs: refs_by_oid.remove(&oid.to_string()).unwrap_or_default(), lane: 0 });
        }
    }
    if !found_marker { return Err("This history has moved on since it was last loaded — refresh and try again.".into()); }
    Ok(OlderCommitsPage { commits, has_more })
}

#[derive(Serialize)]
pub struct TagDetails {
    name: String,
    commit_id: String,
    annotated: bool,
    // Only present for an annotated tag — a lightweight tag is just a named
    // pointer at a commit, with no message/tagger/date of its own to show.
    message: Option<String>,
    tagger: Option<String>,
    date: Option<String>,
}

// Only reached when the user actually clicks a tag badge in the graph — the
// bulk per-commit payload (Commit.refs) deliberately stays minimal (just
// name+kind), matching this app's established "compact list, full detail on
// demand" pattern (the same shape commit selection itself already uses).
// `repository_path` is whichever repository is actually on screen — the
// frontend already resolves this to the submodule's own path when the
// graph is showing a submodule's history, never the parent's.
#[tauri::command]
pub fn tag_details(repository_path: String, tag_name: String) -> Result<TagDetails, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let reference = repo.find_reference(&format!("refs/tags/{tag_name}")).map_err(|error| format!("Tag '{tag_name}' not found: {}", error.message()))?;
    let direct_target = reference.target().ok_or("This tag reference has no direct target")?;
    let commit_id = reference.peel(ObjectType::Commit).map_err(|error| error.message().to_string())?.id().to_string();
    // An annotated tag's ref points at a real tag *object* (find_tag
    // succeeds); a lightweight tag's ref points straight at the commit, so
    // there is no tag object to find at that oid at all.
    let result = match repo.find_tag(direct_target) {
        Ok(tag_object) => TagDetails {
            name: tag_name, commit_id, annotated: true,
            message: tag_object.message().map(|message| message.trim().to_string()).filter(|message| !message.is_empty()),
            tagger: tag_object.tagger().and_then(|signature| signature.name().map(str::to_string)),
            date: tag_object.tagger().map(|signature| short_date(signature.when().seconds())),
        },
        Err(_) => TagDetails { name: tag_name, commit_id, annotated: false, message: None, tagger: None, date: None },
    };
    Ok(result)
}

// A lightweight "what changed" refresh — status only, no branches/commits/
// stashes. For "files copied in from outside the app should just show up" —
// the app can't watch the filesystem itself, but it can cheaply re-check status at the moments that
// actually matter (regaining window focus, opening the Working tree drawer)
// instead of only on a full manual Refresh or the next unrelated action.
// Always a fresh scan, deliberately bypassing the status cache — the whole
// point is "what's actually on disk right now".
#[tauri::command]
pub async fn refresh_status(repository_path: String) -> Result<Vec<Change>, String> {
    off_main_thread(move || refresh_status_inner(repository_path)).await
}

fn refresh_status_inner(repository_path: String) -> Result<Vec<Change>, String> {
    let started = Instant::now();
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let internal = recent_full_statuses(&repo, &repository_path)?;
    let statuses = internal.iter().map(|(path, status, _)| (path.clone(), status.clone())).collect::<Vec<_>>();
    let changes = internal.into_iter().map(|(path, status, staged)| Change { status, path, staged }).collect();
    replace_git_metadata(&repository_path, statuses);
    perf_log("refresh_status: TOTAL", started.elapsed());
    Ok(changes)
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
    // Fetched once for the whole batch, not once per file — see the comment on
    // resolve_submodule_boundary_from for why that distinction matters a lot on
    // a large repository.
    let (_, submodules) = cached_index_metadata(repository_path);
    for file in files {
        match resolve_submodule_boundary_from(&submodules, repository_path, &file) {
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

// "Stage all" used to just be the frontend sending every path from its own
// `state.changes` — a snapshot from whenever that was last loaded, which can
// already be stale by the time the button is pressed (files added/deleted
// from outside the app since then wouldn't be in it at all). This reads the
// real, current disk state directly instead — a genuine `git add -A`
// equivalent — then reuses stage_files_inner's already-tested logic
// (add_path for files, add_all for real directories, the embedded-.git
// check) to apply it, so it inherits that protection automatically instead
// of needing its own copy of it.
#[tauri::command]
pub async fn stage_all(repository_path: String, scope: String) -> Result<StageResult, String> {
    off_main_thread(move || {
    let started = Instant::now();
    perf_log(&format!("stage_all: START (scope={scope:?})"), Duration::ZERO);
    let result = stage_all_inner(&repository_path, &scope);
    match &result {
        Ok(outcome) => perf_log(&format!("stage_all: TOTAL ({} staged, {} skipped dirty submodules)", outcome.staged_paths.len(), outcome.skipped_dirty_submodules.len()), started.elapsed()),
        Err(error) => perf_log(&format!("stage_all: ERROR: {error}"), started.elapsed()),
    }
    result
    }).await
}

fn stage_all_inner(repository_path: &str, scope: &str) -> Result<StageResult, String> {
    if let Some((sub_path, inner_scope)) = resolve_submodule_boundary(repository_path, scope) {
        return stage_all_inner(&sub_path, &inner_scope);
    }
    let step = Instant::now();
    let repo = internal_repository(repository_path)?;
    // Stage all is a mutating command, so correctness must beat the short
    // status-reuse optimization used by passive refreshes. A file copied in
    // from Explorer/right before clicking Stage all must be discovered even
    // if a refresh_status call populated the cache a few milliseconds ago.
    // Keep the same single-flight lock to avoid concurrent status walks, but
    // deliberately take a fresh snapshot here before deciding what to stage.
    let full = fresh_full_statuses(&repo, repository_path, "stage_all")?;
    let paths: Vec<String> = if scope.is_empty() {
        full.into_iter().map(|(path, _, _)| path).collect()
    } else {
        let prefix = format!("{scope}/");
        full.into_iter().filter(|(path, _, _)| path == scope || path.starts_with(&prefix)).map(|(path, _, _)| path).collect()
    };
    perf_log(&format!("stage_all: status ready ({} paths, scope={scope:?})", paths.len()), step.elapsed());
    if paths.is_empty() { return Ok(StageResult::default()); }
    // stage_files_inner itself already tells apart what genuinely changed in
    // the index from a submodule that was merely dirty inside with no real
    // gitlink change to record — reused verbatim as stage_all's own result
    // instead of stage_all reporting "N paths asked for" as if that were
    // "N paths staged" (the report this fixes: 4 submodules, each with an
    // unchanged HEAD, "staged" and counted as 4 while the index recorded
    // nothing new at all).
    stage_files(repository_path.to_string(), paths)
}

// What actually happened, path by path — never just a count of what was
// *asked for*. staged_paths is exactly what really changed in the index
// (a real file, a real directory's contents, or a submodule whose HEAD
// genuinely differs from what the parent had recorded); skipped_dirty_submodules
// is a submodule that was asked for but had nothing meaningful to stage —
// its own working tree has uncommitted changes, but its HEAD hasn't moved
// past what the parent already records, so there is no new gitlink pointer
// to write. See stage_files_inner's own doc comment for why this distinction
// is load-bearing, not cosmetic.
#[derive(Serialize, Default, Debug)]
pub struct StageResult {
    staged_paths: Vec<String>,
    skipped_dirty_submodules: Vec<String>,
}

#[tauri::command]
pub fn stage_files(path: String, files: Vec<String>) -> Result<StageResult, String> {
    let started = Instant::now();
    let file_count = files.len();
    perf_log(&format!("stage_files: START ({file_count} files)"), Duration::ZERO);
    let result = stage_files_inner(&path, files);
    match &result {
        Ok(outcome) => perf_log(&format!("stage_files: TOTAL ({file_count} requested, {} staged, {} skipped dirty submodules)", outcome.staged_paths.len(), outcome.skipped_dirty_submodules.len()), started.elapsed()),
        Err(error) => perf_log(&format!("stage_files: ERROR ({file_count} files): {error}"), started.elapsed()),
    }
    result
}

// A submodule path being *asked* to stage is not the same as there being
// anything real to stage: the parent can only ever record one thing for a
// submodule — a commit SHA (the gitlink) — never the raw contents of
// whatever's uncommitted inside it. Confirmed against a real report: 4
// submodules, each with uncommitted internal changes but HEAD unchanged from
// what the parent already recorded, were "staged" (add_to_index wrote back
// the exact same SHA that was already there — a real no-op, but git2 doesn't
// error on it) and reported as 4 successfully staged paths, while the
// parent's index genuinely had nothing new in it. Only stage (and count) a
// submodule whose HEAD actually differs from the parent's currently recorded
// pointer for it; one that's merely dirty inside is left alone entirely —
// not staged, not silently reported as if it were.
fn stage_files_inner(path: &str, files: Vec<String>) -> Result<StageResult, String> {
    validate_path(path)?;
    let step = Instant::now();
    let (files, submodule_groups) = partition_by_submodule(path, files);
    perf_log(&format!("stage_files: partition_by_submodule ({} files)", files.len()), step.elapsed());
    let mut result = StageResult::default();
    // Staging files *inside* a submodule is entirely that submodule's own
    // repository — it never touches the parent's index at all. Done before
    // the parent's own lock is even acquired below (each recursive call
    // acquires and releases only *that* submodule's own lock in turn) so
    // this thread is never holding two different repositories' write locks
    // at once. That single-lock-at-a-time invariant is what makes the
    // parent-vs-submodule lock ordering here provably deadlock-free against
    // every other command that touches both (e.g. commit_submodule, which
    // takes the submodule's lock first and, only after releasing it,
    // separately takes the parent's — see its own comment) — two locks
    // taken one at a time, never nested, can't form a cycle with anything
    // else that follows the same rule.
    for (sub_path, inner_files) in submodule_groups {
        let inner = stage_files(sub_path, inner_files)?;
        result.staged_paths.extend(inner.staged_paths);
        result.skipped_dirty_submodules.extend(inner.skipped_dirty_submodules);
    }
    if files.is_empty() { return Ok(result); }
    // Real evidence this was needed, not theoretical: see repo_write_lock's
    // doc comment. Held for the rest of this function, so a burst of rapid
    // checkbox clicks queues up instead of racing on the same index.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(path, "stage_files", queue_started.elapsed());
    let repo = internal_repository(path)?;
    let safe_files = files.into_iter().map(|file| safe_relative_path(file.trim_end_matches(|character| character == '/' || character == '\\'))).collect::<Result<Vec<_>, _>>()?;
    let step = Instant::now();
    // `submodules.contains(...)` (an O(1) HashSet lookup against metadata
    // already fetched once for the whole batch) instead of calling
    // `repo.find_submodule(...)` — a real libgit2 lookup — for every single
    // file: only the (typically very few, if any) paths that are actually
    // submodule roots need that real lookup at all, to get the Submodule
    // handle add_to_index needs.
    let (_, submodules) = cached_index_metadata(path);
    let mut submodule_paths = HashSet::new();
    let step2 = Instant::now();
    let index_for_read = repo.index().map_err(|error| error.message().to_string())?;
    perf_log("stage_files: repo.index() (read, for submodule HEAD comparison)", step2.elapsed());
    for safe in &safe_files {
        let normalized_safe = normalized(safe);
        if !submodules.contains(&normalized_safe) { continue; }
        submodule_paths.insert(normalized_safe.clone());
        let recorded = index_for_read.get_path(Path::new(&normalized_safe), 0).map(|entry| entry.id);
        let absolute = Path::new(path).join(safe);
        let current_head = internal_submodule_repository(&absolute).ok().and_then(|sub_repo| sub_repo.head().ok().and_then(|head| head.target()));
        if recorded == current_head {
            // Nothing to stage — see stage_files_inner's own doc comment.
            result.skipped_dirty_submodules.push(normalized_safe);
            continue;
        }
        let mut submodule = repo.find_submodule(&normalized_safe).map_err(|error| format!("Cannot stage submodule {}: {}", normalized_safe, error.message()))?;
        submodule.add_to_index(true).map_err(|error| format!("Cannot stage submodule {}: {}", normalized_safe, error.message()))?;
        result.staged_paths.push(normalized_safe);
    }
    drop(index_for_read);
    perf_log("stage_files: submodule detection", step.elapsed());
    let step = Instant::now();
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    perf_log("stage_files: repo.index()", step.elapsed());
    // A real Windows perf log caught this precisely: staging a single known
    // file took 1.6-1.9 SECONDS, entirely inside a single add_all call for
    // that one exact path. add_all always does pathspec *matching* — a diff
    // between the working directory and the index — even for a literal,
    // exact path with nothing to expand; its cost scales with the size of
    // the working tree being diffed, not with how many paths were given.
    // add_path, by contrast, is a direct "hash this known file and insert an
    // entry for it" with no worktree-wide matching at all — the right tool
    // for a file whose exact path is already known (which is every ordinary
    // "stage this file" click). add_all is now used only for an actual
    // directory pathspec, which genuinely needs matching to discover what's
    // inside it.
    let step = Instant::now();
    let mut files_to_add: Vec<&Path> = Vec::new();
    let mut dirs_to_add: Vec<&Path> = Vec::new();
    let mut to_remove: Vec<&Path> = Vec::new();
    for safe in &safe_files {
        let normalized_safe = normalized(safe); if submodule_paths.contains(&normalized_safe) { continue; }
        let absolute = Path::new(path).join(safe);
        if absolute.is_dir() {
            if embedded_git_repo_path(&absolute) { return Err(embedded_git_repo_error(&normalized_safe)); }
            dirs_to_add.push(safe.as_path());
        }
        else if absolute.exists() { files_to_add.push(safe.as_path()); }
        else { to_remove.push(safe.as_path()); }
    }
    perf_log(&format!("stage_files: partition add/remove ({} files, {} dirs to add, {} to remove)", files_to_add.len(), dirs_to_add.len(), to_remove.len()), step.elapsed());
    let step = Instant::now();
    for file in &files_to_add { index.add_path(file).map_err(|error| error.message().to_string())?; }
    perf_log(&format!("stage_files: add_path ({} files)", files_to_add.len()), step.elapsed());
    let step = Instant::now();
    if !dirs_to_add.is_empty() { index.add_all(&dirs_to_add, git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?; }
    perf_log(&format!("stage_files: add_all for folders ({} dirs)", dirs_to_add.len()), step.elapsed());
    let step = Instant::now();
    for safe in &to_remove { let _ = index.remove_path(safe); }
    perf_log(&format!("stage_files: remove_path loop ({} files)", to_remove.len()), step.elapsed());
    if !submodule_paths.is_empty() && Path::new(path).join(".gitmodules").exists() { index.add_path(Path::new(".gitmodules")).map_err(|error| error.message().to_string())?; }
    let step = Instant::now();
    index.write().map_err(|error| error.message().to_string())?;
    perf_log("stage_files: index.write()", step.elapsed());
    invalidate_git_metadata(path);
    result.staged_paths.extend(files_to_add.iter().chain(dirs_to_add.iter()).chain(to_remove.iter()).map(|p| normalized(p)));
    Ok(result)
}

#[tauri::command]
pub fn unstage_files(path: String, files: Vec<String>) -> Result<(), String> {
    validate_path(&path)?;
    let (files, submodule_groups) = partition_by_submodule(&path, files);
    // Same single-lock-at-a-time reasoning as stage_files_inner: unstaging
    // files *inside* a submodule is entirely that submodule's own
    // repository, done before the parent's own lock is acquired below, one
    // submodule lock at a time, never nested with the parent's.
    for (sub_path, inner_files) in submodule_groups { unstage_files(sub_path, inner_files)?; }
    if files.is_empty() { return Ok(()); }
    // See repo_write_lock's doc comment — same concurrent-index race as
    // stage_files, from the exact same rapid-checkbox-click pattern.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&path, "unstage_files", queue_started.elapsed());
    let repo = internal_repository(&path)?; let head = repo.head().and_then(|head| head.peel(ObjectType::Commit)).map_err(|error| error.message().to_string())?;
    let safe = files.iter().map(|file| safe_relative_path(file)).collect::<Result<Vec<_>, _>>()?; repo.reset_default(Some(&head), safe.iter()).map_err(|error| error.message().to_string())?; invalidate_git_metadata(&path); Ok(())
}

#[tauri::command]
pub fn create_commit(path: String, message: String) -> Result<(), String> {
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&path, "create_commit", queue_started.elapsed());
    let repo = internal_repository(&path)?; let mut index = repo.index().map_err(|error| error.message().to_string())?; let tree_id = index.write_tree().map_err(|error| error.message().to_string())?; let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?; let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?;
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok()); let parents: Vec<&git2::Commit<'_>> = parent.iter().collect(); repo.commit(Some("HEAD"), &signature, &signature, message.trim(), &tree, &parents).map_err(|error| error.message().to_string())?; invalidate_git_metadata(&path); Ok(())
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
    // The frontend always calls this with the *parent's* repository_path,
    // even when relative_path points inside a submodule (its own editor has
    // no separate notion of "which repo" — see read_text_file's own call
    // sites) — invalidating only that path left the submodule's own cached
    // status stale until something else happened to refresh it, so a file
    // just edited *inside* a submodule kept showing as clean there (the
    // parent's own row for the submodule still updated correctly, since its
    // cache was invalidated — only the submodule's own, separately cached
    // view of itself was missed). Invalidate both.
    if let Some((sub_path, _inner)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        invalidate_git_metadata(&sub_path);
        // The consolidated Explorer state (dirty / push / parent step) has
        // its own per-submodule cache. An edit inside the submodule changes
        // that state even though it does not move HEAD.
        invalidate_submodule_sync(&repository_path);
    }
    invalidate_git_metadata(&repository_path);
    Ok(())
}

// ---- Pull request status (read-only, first incremental step) ----
//
// Prefer the `gh` CLI when it is installed: it already owns its credential
// storage and provides all the rich PR fields in one operation. Corporate
// Windows images do not always include `gh`, though, so absence of that
// optional tool must not disable the feature. In that case we query the
// GitHub / GitHub Enterprise GraphQL API directly and ask the repository's
// configured Git credential helper (normally Git Credential Manager on
// Windows) for the HTTPS credential it already uses. The secret remains in
// the Rust backend's memory for this request only: it is never returned to
// the frontend, persisted by Git DrillDown, or written to the perf log.

#[derive(Serialize)]
pub struct PullRequestSummary {
    number: u64,
    title: String,
    source_branch: String,
    target_branch: String,
    // "draft" | "open" | "merged" | "closed"
    state: String,
    // "mergeable" | "conflicting" | "calculating" | "unknown"
    mergeable: String,
    // "approved" | "changes_requested" | "review_required" | "none"
    review_summary: String,
    // The latest submitted review per reviewer plus any still-pending review
    // requests. This is collected in the same GitHub operation as the PR
    // card: never one request per person. Submitted reviews may carry their
    // exact GitHub permalink; pending requests deliberately do not pretend a
    // review exists yet.
    reviewers: Vec<PullRequestReviewerSummary>,
    // "passing" | "failing" | "pending" | "none"
    checks_status: String,
    // Individual checks from the same request. `details_url` is the exact
    // target behind GitHub's Details link (never guessed by the app).
    checks: Vec<PullRequestCheckSummary>,
    url: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct PullRequestCheckSummary {
    name: String,
    // "passing" | "failing" | "pending" | "unknown"
    status: String,
    details_url: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct PullRequestReviewerSummary {
    login: String,
    // "requested" | "approved" | "changes_requested" | "commented" |
    // "dismissed" | "pending"
    state: String,
    // Exact PullRequestReview permalink when GitHub supplies one. The normal
    // PR URL remains available separately on PullRequestSummary.
    review_url: String,
}

#[derive(Serialize)]
pub struct PrStatusResult {
    // "no_remote" | "unsupported_provider" | "auth_missing" | "api_error" |
    // "no_open_pr" | "no_upstream" | "partial_result" | "detached_head" |
    // "no_branch" | "superseded" | "ok" (one or more PRs in either list —
    // the frontend distinguishes "one" vs "multiple" from each list's own
    // .len())
    state: String,
    detail: String,
    // The branch actually checked against the server (the *remote* branch
    // name, which can differ from the local one) — for the UI's
    // "No open PR for <branch> in <repo>" message.
    branch: Option<String>,
    // The `host/owner/repo` the query ran against — same message.
    queried_repo: Option<String>,
    // True when the search could not check every relevant query (a
    // candidate repo errored, the candidate list was longer than the cap,
    // or the incoming-PR query itself failed) — an empty result under this
    // must never be presented as a confident "no PR" (see state
    // "partial_result").
    partial: bool,
    // Current branch is the PR's head/source — "Pull requests from this
    // branch". Still named pull_requests-shaped (PullRequestSummary), kept
    // as its own explicit field rather than a single ambiguous list: this
    // whole change exists because collapsing "from" and "into" together is
    // exactly how a real incoming PR (this branch as someone else's
    // *target*) went undetected before.
    outgoing_pull_requests: Vec<PullRequestSummary>,
    // Current branch is the PR's base/target — "Pull requests into this
    // branch". Never filtered by head owner/repo (point 4): an incoming PR
    // is expected to come from any other branch, fork, or owner — `--repo
    // --base` already scopes it to PRs that genuinely target this branch
    // in this repository, which is the only check that's actually correct
    // for this direction.
    incoming_pull_requests: Vec<PullRequestSummary>,
}

impl PrStatusResult {
    fn plain(state: &str, detail: impl Into<String>) -> Self {
        PrStatusResult { state: state.into(), detail: detail.into(), branch: None, queried_repo: None, partial: false, outgoing_pull_requests: Vec::new(), incoming_pull_requests: Vec::new() }
    }
}

// Every real-world GitHub remote URL shape — github.com *and* GitHub
// Enterprise (`github.vitesco.io`, `github.<company>.com`, ...) — with the
// host kept exactly as written, never rewritten to github.com. Non-GitHub
// hosts (a self-hosted Git server, GitLab, Bitbucket, Azure DevOps...) are
// deliberately left unrecognized — reported as "unsupported provider"
// rather than guessed at. A GitHub repo path is exactly `owner/repo`; a
// deeper path (a GitLab subgroup) is rejected.
#[derive(Debug, Clone, PartialEq)]
struct GitHubRepo { host: String, owner: String, repo: String }

impl GitHubRepo {
    // The `HOST/OWNER/REPO` argument `gh` expects. `gh` accepts the
    // host-qualified form for github.com too, so always including the host
    // is safe — and it's the only form that works for Enterprise.
    fn gh_repo_arg(&self) -> String { format!("{}/{}/{}", self.host, self.owner, self.repo) }
}

fn is_github_like_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "github.com" || h == "github" || h.starts_with("github.")
}

fn parse_github_repo(url: &str) -> Option<GitHubRepo> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);

    let (host, path) = if let Some(rest) = trimmed.strip_prefix("git@") {
        // scp-like: git@HOST:OWNER/REPO
        let (host, path) = rest.split_once(':')?;
        (host.to_string(), path.to_string())
    } else if let Some(rest) = trimmed.strip_prefix("ssh://") {
        // ssh://[user@]HOST[:port]/OWNER/REPO
        let rest = rest.rsplit_once('@').map(|(_, after)| after).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next().unwrap_or(host).to_string(), path.to_string())
    } else if let Some(rest) = trimmed.strip_prefix("https://").or_else(|| trimmed.strip_prefix("http://")) {
        // http(s)://[user@]HOST[:port]/OWNER/REPO
        let rest = rest.rsplit_once('@').map(|(_, after)| after).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next().unwrap_or(host).to_string(), path.to_string())
    } else {
        return None;
    };

    if !is_github_like_host(&host) { return None; }

    let mut parts = path.trim_matches('/').splitn(2, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches('/');
    if owner.is_empty() || repo.is_empty() || repo.contains('/') { return None; }
    Some(GitHubRepo { host: host.to_ascii_lowercase(), owner: owner.to_string(), repo: repo.to_string() })
}

fn map_pr_state(state: &str, is_draft: bool) -> String {
    if is_draft { return "draft".into(); }
    match state { "OPEN" => "open", "MERGED" => "merged", "CLOSED" => "closed", _ => "open" }.into()
}

fn map_mergeable(mergeable: &str) -> String {
    match mergeable { "MERGEABLE" => "mergeable", "CONFLICTING" => "conflicting", _ => "calculating" }.into()
}

fn map_review_summary(review_decision: &str) -> String {
    match review_decision {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes_requested",
        "REVIEW_REQUIRED" => "review_required",
        _ => "none",
    }.into()
}

fn map_reviewer_state(state: &str) -> String {
    match state {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes_requested",
        "COMMENTED" => "commented",
        "DISMISSED" => "dismissed",
        "PENDING" => "pending",
        _ => "commented",
    }.into()
}

fn reviewer_login(value: &serde_json::Value) -> Option<String> {
    value.get("login").and_then(|field| field.as_str())
        .or_else(|| value.get("name").and_then(|field| field.as_str()))
        .or_else(|| value.get("slug").and_then(|field| field.as_str()))
        .map(str::trim).filter(|login| !login.is_empty()).map(String::from)
}

fn pr_reviewers_from_json(item: &serde_json::Value) -> Vec<PullRequestReviewerSummary> {
    let mut reviewers = Vec::new();

    // Provider responses that include reviewRequests return the requested
    // reviewer objects directly. normalize_graphql_pr deliberately flattens
    // GraphQL's ReviewRequest nodes to that identical shape. The direct API
    // asks only for User.login: Team.name/slug require read:org on Enterprise,
    // and PR visibility must not depend on that additional permission.
    for requested in item.get("reviewRequests").and_then(|value| value.as_array()).into_iter().flatten() {
        let Some(login) = reviewer_login(requested) else { continue };
        if !reviewers.iter().any(|existing: &PullRequestReviewerSummary| existing.login.eq_ignore_ascii_case(&login)) {
            reviewers.push(PullRequestReviewerSummary { login, state: "requested".into(), review_url: String::new() });
        }
    }

    // GitHub defines latestReviews as at most the latest non-pending review
    // from each reviewer. If a request and a submitted review nevertheless
    // arrive together, the submitted state replaces the pending one.
    for review in item.get("latestReviews").and_then(|value| value.as_array()).into_iter().flatten() {
        let Some(login) = review.get("author").and_then(reviewer_login) else { continue };
        let summary = PullRequestReviewerSummary {
            login: login.clone(),
            state: map_reviewer_state(review.get("state").and_then(|value| value.as_str()).unwrap_or("")),
            review_url: review.get("url").and_then(|value| value.as_str()).unwrap_or("").to_string(),
        };
        if let Some(existing) = reviewers.iter_mut().find(|existing| existing.login.eq_ignore_ascii_case(&login)) {
            *existing = summary;
        } else {
            reviewers.push(summary);
        }
    }
    reviewers
}

// gh's statusCheckRollup is an array of check-run/status-context objects,
// each with its own conclusion/state — rolled up here into one overall
// answer the same way GitHub's own PR page badge does: any real failure
// wins, then anything still running, then "all passing", then "no checks
// configured at all".
fn map_checks_status(rollup: &[serde_json::Value]) -> String {
    if rollup.is_empty() { return "none".into(); }
    let mut any_pending = false;
    let mut any_success = false;
    for entry in rollup {
        let outcome = entry.get("conclusion").and_then(|v| v.as_str()).filter(|value| !value.is_empty())
            .or_else(|| entry.get("state").and_then(|v| v.as_str()))
            .or_else(|| entry.get("status").and_then(|v| v.as_str()))
            .unwrap_or("").to_uppercase();
        match outcome.as_str() {
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "FAILED" => return "failing".into(),
            "SUCCESS" | "SUCCESSFUL" | "COMPLETED" | "NEUTRAL" => any_success = true,
            "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | "STARTUP_FAILURE" => any_pending = true,
            _ => {}
        }
    }
    if any_pending { "pending".into() } else if any_success { "passing".into() } else { "none".into() }
}

fn map_check_status(entry: &serde_json::Value) -> String {
    let outcome = entry.get("conclusion").and_then(|v| v.as_str()).filter(|value| !value.is_empty())
        .or_else(|| entry.get("state").and_then(|v| v.as_str()))
        .or_else(|| entry.get("status").and_then(|v| v.as_str()))
        .unwrap_or("").to_uppercase();
    match outcome.as_str() {
        "SUCCESS" | "SUCCESSFUL" | "COMPLETED" | "NEUTRAL" => "passing",
        "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "FAILED" | "ACTION_REQUIRED" => "failing",
        "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | "WAITING" | "REQUESTED" => "pending",
        _ => "unknown",
    }.into()
}

fn pr_checks_from_json(rollup: &[serde_json::Value]) -> Vec<PullRequestCheckSummary> {
    rollup.iter().filter_map(|entry| {
        let name = pr_check_name(entry).unwrap_or_default();
        if name.is_empty() { return None; }
        let details_url = pr_check_details_url(entry).unwrap_or("").to_string();
        // Temporary diagnostic: never log the URL itself (could point at an
        // internal build server) or anything else from the entry — just
        // whether this check's rollup entry actually carried a details_url/
        // targetUrl at all, to tell apart "the API never gave us one" from
        // a parsing bug on our side.
        perf_log(&format!("pr_checks_from_json: check='{name}' has_details_url={}", !details_url.is_empty()), Duration::ZERO);
        Some(PullRequestCheckSummary { name: name.into(), status: map_check_status(entry), details_url })
    }).collect()
}

fn pr_check_name(entry: &serde_json::Value) -> Option<String> {
    entry.get("name").and_then(|v| v.as_str())
        .or_else(|| entry.get("context").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
}

fn pr_check_details_url(entry: &serde_json::Value) -> Option<&str> {
    entry.get("detailsUrl").and_then(|v| v.as_str())
        .or_else(|| entry.get("targetUrl").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn normalized_pr_check_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn set_pr_check_details_url(entry: &mut serde_json::Value, url: &str) {
    let field = if entry.get("context").is_some() { "targetUrl" } else { "detailsUrl" };
    if let Some(object) = entry.as_object_mut() {
        object.insert(field.into(), serde_json::Value::String(url.to_string()));
    }
}

fn collect_rest_check_detail_urls(status_payload: Option<&serde_json::Value>, check_runs_payload: Option<&serde_json::Value>) -> HashMap<String, String> {
    let mut urls = HashMap::new();
    if let Some(statuses) = status_payload.and_then(|payload| payload.get("statuses")).and_then(|value| value.as_array()) {
        for status in statuses {
            let Some(name) = status.get("context").and_then(|value| value.as_str()).map(str::trim).filter(|value| !value.is_empty()) else { continue };
            let Some(url) = status.get("target_url").and_then(|value| value.as_str()).map(str::trim).filter(|value| !value.is_empty()) else { continue };
            urls.entry(normalized_pr_check_name(name)).or_insert_with(|| url.to_string());
        }
    }
    if let Some(check_runs) = check_runs_payload.and_then(|payload| payload.get("check_runs")).and_then(|value| value.as_array()) {
        for check_run in check_runs {
            let Some(name) = check_run.get("name").and_then(|value| value.as_str()).map(str::trim).filter(|value| !value.is_empty()) else { continue };
            let Some(url) = check_run.get("details_url").and_then(|value| value.as_str())
                .or_else(|| check_run.get("html_url").and_then(|value| value.as_str()))
                .map(str::trim).filter(|value| !value.is_empty()) else { continue };
            urls.entry(normalized_pr_check_name(name)).or_insert_with(|| url.to_string());
        }
    }
    urls
}

fn fill_missing_pr_check_details_from_urls(pr: &mut serde_json::Value, urls: &HashMap<String, String>) -> usize {
    let Some(entries) = pr.get_mut("statusCheckRollup").and_then(|value| value.as_array_mut()) else { return 0 };
    let mut filled = 0usize;
    for entry in entries {
        if pr_check_details_url(entry).is_some() { continue; }
        let Some(name) = pr_check_name(entry) else { continue };
        let Some(url) = urls.get(&normalized_pr_check_name(&name)) else { continue };
        set_pr_check_details_url(entry, url);
        filled += 1;
    }
    filled
}

fn count_missing_pr_check_details(pr: &serde_json::Value) -> usize {
    pr.get("statusCheckRollup").and_then(|value| value.as_array())
        .map(|entries| entries.iter().filter(|entry| pr_check_name(entry).is_some() && pr_check_details_url(entry).is_none()).count())
        .unwrap_or(0)
}

fn decode_html_attr_minimal(value: &str) -> String {
    value.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn html_tag_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=");
    let start = tag.find(&needle)? + needle.len();
    let quote = tag[start..].chars().next()?;
    if quote != '"' && quote != '\'' { return None; }
    let value_start = start + quote.len_utf8();
    let value_end = tag[value_start..].find(quote)? + value_start;
    Some(decode_html_attr_minimal(&tag[value_start..value_end]))
}

fn absolute_github_html_url(base_url: &str, href: &str) -> String {
    let href = normalize_html_href(href);
    let href = href.trim();
    if href.starts_with("http://") || href.starts_with("https://") { return href.to_string(); }
    if href.starts_with('/') {
        if let Some(rest) = base_url.strip_prefix("https://").or_else(|| base_url.strip_prefix("http://")) {
            if let Some(host) = rest.split('/').next() {
                let scheme = if base_url.starts_with("http://") { "http" } else { "https" };
                return format!("{scheme}://{host}{href}");
            }
        }
    }
    href.to_string()
}

fn normalize_html_href(href: &str) -> String {
    let trimmed = href.trim();
    // Some copied GitHub snippets arrive through Markdown as
    // `[https://host/path](https://host/path)`. The real page uses a normal
    // href, but accepting this shape keeps the fallback testable from the
    // exact text users paste out of the browser/chat without making the
    // parser more permissive in any dangerous way.
    if trimmed.starts_with('[') {
        if let Some(open) = trimmed.find("](") {
            if trimmed.ends_with(')') && open + 2 < trimmed.len() - 1 {
                return trimmed[open + 2..trimmed.len() - 1].trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

fn html_text_content_minimal(fragment: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for ch in fragment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    decode_html_attr_minimal(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_status_action_href(block: &str) -> Option<String> {
    let mut remaining = block;
    while let Some(start) = remaining.find("<a") {
        remaining = &remaining[start..];
        let Some(end) = remaining.find('>') else { break };
        let tag = &remaining[..=end];
        remaining = &remaining[end + 1..];
        let class = html_tag_attr(tag, "class").unwrap_or_default();
        if !class.split_whitespace().any(|name| name == "status-actions") { continue; }
        let href = html_tag_attr(tag, "href")?;
        if href.trim().is_empty() { continue; }
        return Some(href);
    }
    None
}

fn first_status_item_name(block: &str) -> Option<String> {
    let strong_start = block.find("<strong")?;
    let after_strong = &block[strong_start..];
    let tag_end = after_strong.find('>')? + 1;
    let after_tag = &after_strong[tag_end..];
    let close = after_tag.find("</strong>")?;
    let name = html_text_content_minimal(&after_tag[..close]);
    if name.is_empty() { None } else { Some(name) }
}

fn collect_pr_check_detail_urls_from_html(pr_url: &str, html: &str) -> HashMap<String, String> {
    let mut urls = HashMap::new();
    let mut remaining = html;
    while let Some(start) = remaining.find("<a") {
        remaining = &remaining[start..];
        let Some(end) = remaining.find('>') else { break };
        let tag = &remaining[..=end];
        remaining = &remaining[end + 1..];
        let class = html_tag_attr(tag, "class").unwrap_or_default();
        if !class.split_whitespace().any(|name| name == "status-actions") { continue; }
        let Some(label) = html_tag_attr(tag, "aria-label") else { continue };
        let Some(raw_name) = label.strip_prefix("Details for ") else { continue };
        let name = raw_name.trim().trim_end_matches('.').trim();
        if name.is_empty() { continue; }
        let Some(href) = html_tag_attr(tag, "href") else { continue };
        if href.trim().is_empty() { continue; }
        urls.entry(normalized_pr_check_name(name)).or_insert_with(|| absolute_github_html_url(pr_url, &href));
    }
    // GitHub Enterprise has changed this markup several times. The strict
    // `aria-label="Details for X"` parser above is ideal when available,
    // but the real PR page also gives us a stable local structure: each
    // `.merge-status-item` block contains the check name in `<strong>` and
    // its Details link as `a.status-actions`. Parse that too so Collaborator,
    // Polarion and custom submodule checks keep working even if aria-labels
    // or whitespace differ.
    let mut block_search = html;
    while let Some(marker) = block_search.find("merge-status-item") {
        let block_start = block_search[..marker].rfind("<div").unwrap_or(marker);
        let after_marker = &block_search[marker + "merge-status-item".len()..];
        let block_end = after_marker.find("merge-status-item")
            .map(|next| marker + "merge-status-item".len() + next)
            .unwrap_or(block_search.len());
        let block = &block_search[block_start..block_end];
        if let (Some(name), Some(href)) = (first_status_item_name(block), first_status_action_href(block)) {
            urls.entry(normalized_pr_check_name(&name)).or_insert_with(|| absolute_github_html_url(pr_url, &href));
        }
        block_search = &block_search[block_end..];
    }
    urls
}

// Everything `pr_status` needs to know *before* it shells out to `gh` —
// resolved straight from Git, never from what the frontend believes.
struct PrQueryContext {
    // The repo the checked-out branch actually lives in (its tracking
    // remote, or `origin`/first remote). A PR "for this branch" is one
    // whose head repo is this and whose head ref is `head_branch`.
    head_repo: GitHubRepo,
    // Every distinct, same-host GitHub repo a PR for this branch could have
    // been opened *against* — `head_repo` first, then every other GitHub
    // remote's fetch *and* push URL (so a fork's PR against its upstream is
    // found without *assuming* which remote is the upstream, and a remote
    // whose push destination differs from its fetch URL is still covered).
    // Each is queried; results are unioned.
    candidate_bases: Vec<GitHubRepo>,
    // True when there were more same-host GitHub candidates than
    // PR_MAX_CANDIDATE_BASES allowed through — an empty result must be
    // reported as partial/incomplete, never a confident "no PR".
    candidates_truncated: bool,
    // The branch name *on the remote* (`branch.<local>.merge`), which can
    // differ from the local branch name.
    head_branch: String,
    local_branch: String,
    tracking_remote: String,
    had_upstream: bool,
    // Set when the frontend passed a branch that disagrees with real HEAD —
    // logged, never acted on.
    frontend_branch_mismatch: Option<String>,
}

enum PrContext { Ready(PrQueryContext), Terminal(PrStatusResult) }

// At most this many `gh` calls per pr_status (head repo + a few candidate
// bases) — a guard against a repo with a long list of GitHub remotes. They
// run concurrently (see pr_status_impl), so this is also the concurrency
// cap, not just a call-count cap.
const PR_MAX_CANDIDATE_BASES: usize = 4;

// Backend is the single source of truth: HEAD/branch/upstream come from Git
// on every call, for whatever repository path it was handed (the parent, or
// a submodule's own path). `frontend_branch` is advisory only — used to
// *detect and log* a mismatch (stale frontend state, an external
// checkout), never to decide what to query.
fn resolve_pr_query_context(repo: &Repository, frontend_branch: Option<&str>) -> PrContext {
    use PrContext::Terminal;
    let frontend_branch = frontend_branch.map(str::trim).filter(|b| !b.is_empty());

    if repo.head_detached().unwrap_or(false) {
        return Terminal(PrStatusResult::plain("detached_head",
            "HEAD is detached — not on a branch, so there's no branch to check for a pull request."));
    }
    let Some(local_branch) = repo.head().ok().filter(|head| head.is_branch()).and_then(|head| head.shorthand().map(String::from)) else {
        return Terminal(PrStatusResult::plain("no_branch", "No branch is currently checked out."));
    };

    let frontend_branch_mismatch = frontend_branch.filter(|fb| *fb != local_branch).map(String::from);

    // Upstream straight from config: branch.<local>.remote / branch.<local>.merge.
    let cfg = repo.config().ok();
    let tracking_remote_cfg = cfg.as_ref()
        .and_then(|c| c.get_string(&format!("branch.{local_branch}.remote")).ok())
        .filter(|remote| !remote.is_empty() && remote != ".");
    let remote_branch = cfg.as_ref()
        .and_then(|c| c.get_string(&format!("branch.{local_branch}.merge")).ok())
        .map(|merge_ref| merge_ref.strip_prefix("refs/heads/").unwrap_or(&merge_ref).to_string());
    let had_upstream = tracking_remote_cfg.is_some();

    let existing_remotes: Vec<String> = repo.remotes().ok()
        .map(|names| names.iter().flatten().map(String::from).collect())
        .unwrap_or_default();
    if existing_remotes.is_empty() {
        return Terminal(PrStatusResult::plain("no_remote", "This repository has no configured remote."));
    }
    // The tracking remote if it's set and still exists; else `origin`; else
    // whatever remote is first.
    let head_remote_name = tracking_remote_cfg.clone()
        .filter(|remote| existing_remotes.iter().any(|name| name == remote))
        .or_else(|| existing_remotes.iter().find(|name| name.as_str() == "origin").cloned())
        .unwrap_or_else(|| existing_remotes[0].clone());

    let Some(head_remote_url) = repo.find_remote(&head_remote_name).ok().and_then(|remote| remote.url().map(String::from)) else {
        return Terminal(PrStatusResult::plain("no_remote", format!("The '{head_remote_name}' remote has no URL.")));
    };
    let Some(head_repo) = parse_github_repo(&head_remote_url) else {
        return Terminal(PrStatusResult::plain("unsupported_provider",
            format!("Pull request status is only available for GitHub remotes so far — '{head_remote_name}' points at {head_remote_url}")));
    };

    // Candidate base repos = the head repo, then every *other* distinct
    // GitHub remote on the *same host* — mixing hosts would mean querying a
    // `gh` auth context that can't possibly own the PR (a fork/upstream pair
    // is never split across two different GitHub instances). No remote name
    // is treated as special: a fork's PR against its real upstream is found
    // because that upstream is one of the repo's remotes and gets queried
    // too — not because it's *named* "upstream". Both a remote's fetch URL
    // and its push URL (when explicitly different — remote.pushurl()) are
    // considered: what a branch's commits were actually pushed *to* is what
    // decides where its PR could be, and that isn't always the fetch URL.
    let mut candidate_bases = vec![head_repo.clone()];
    for name in &existing_remotes {
        let Ok(remote) = repo.find_remote(name) else { continue };
        let fetch_url = remote.url().map(str::to_string);
        let push_url = remote.pushurl().map(str::to_string);
        for url in [fetch_url, push_url].into_iter().flatten() {
            let Some(parsed) = parse_github_repo(&url) else { continue };
            if parsed.host != head_repo.host { continue; }
            if !candidate_bases.contains(&parsed) { candidate_bases.push(parsed); }
        }
    }
    let candidates_truncated = candidate_bases.len() > PR_MAX_CANDIDATE_BASES;
    candidate_bases.truncate(PR_MAX_CANDIDATE_BASES);

    PrContext::Ready(PrQueryContext {
        head_repo,
        candidate_bases,
        candidates_truncated,
        head_branch: remote_branch.unwrap_or_else(|| local_branch.clone()),
        local_branch,
        tracking_remote: head_remote_name,
        had_upstream,
        frontend_branch_mismatch,
    })
}

// Which side of the PR `branch` is being matched against — Head for the
// existing "PRs from this branch" query, Base for "PRs into this branch"
// (the direction the original report showed was never queried at all).
#[derive(Debug, Clone, Copy, PartialEq)]
enum PrQueryDirection { Head, Base }

// The variables of one `gh pr list` call — everything else is constant.
// `branch` is deliberately generic (not `head`): the same resolved remote
// branch name is correct as either side, depending on `direction`.
#[derive(Debug, Clone, PartialEq)]
struct PrGhQuery { repo: String, branch: String, direction: PrQueryDirection }

// The outcome of one `gh pr list` call, in a shape a test can fabricate
// without constructing a real `std::process::Output`.
#[derive(Debug)]
enum GhOutcome {
    Prs(Vec<serde_json::Value>),
    Failure { stderr: String },
    Unavailable { not_installed: bool, detail: String },
}

#[cfg(not(test))]
static GH_CLI_AVAILABLE: OnceLock<bool> = OnceLock::new();

// Detect once per process. A missing executable fails immediately, but the
// timeout also protects against a broken corporate installation that starts
// and then waits forever. Unit tests deliberately keep using the injected gh
// runner so they never depend on tools or credentials installed on the host.
#[cfg(not(test))]
fn gh_cli_available() -> bool {
    *GH_CLI_AVAILABLE.get_or_init(|| {
        let mut command = Command::new("gh");
        command.arg("--version").stdin(std::process::Stdio::null());
        run_with_timeout_labeled(command, Duration::from_secs(3), "gh", "3 seconds")
            .map(|output| output.status.success())
            .unwrap_or(false)
    })
}

#[cfg(test)]
fn gh_cli_available() -> bool { true }

fn github_graphql_endpoint(host: &str) -> String {
    if host.eq_ignore_ascii_case("github.com") {
        "https://api.github.com/graphql".into()
    } else {
        format!("https://{host}/api/graphql")
    }
}

fn github_token_from_environment(host: &str) -> Option<String> {
    let names: &[&str] = if host.eq_ignore_ascii_case("github.com") {
        &["GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    };
    names.iter().find_map(|name| std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty()))
}

fn parse_git_credential_password(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output).lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key == "password" && !value.is_empty()).then(|| value.to_string())
    })
}

// Ask the configured credential helper for the HTTPS credential already used
// by Git. Git Credential Manager returns it through this standard plumbing on
// Windows; macOS Keychain and other helpers use the same protocol. Interactive
// prompting is disabled because a GUI subprocess has no usable terminal and
// must never make the PR panel freeze. stdout contains a secret and therefore
// must never be included in an error or performance log.
fn github_token_from_git_credential_helper(repository_path: &str, repo: &GitHubRepo) -> Option<String> {
    use std::io::Write;

    let mut command = Command::new("git");
    command.arg("-C").arg(repository_path).args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Own process group (Unix) so a timeout kill below can reach the actual
    // credential helper git spawns as its child too — see
    // run_with_timeout_labeled's comment for why killing just this PID isn't
    // enough.
    #[cfg(unix)] { use std::os::unix::process::CommandExt; command.process_group(0); }
    let mut child = command.spawn().ok()?;
    let id = child.id();
    let request = format!("protocol=https\nhost={}\npath={}/{}.git\n\n", repo.host, repo.owner, repo.repo);
    let mut stdin = child.stdin.take()?;
    if stdin.write_all(request.as_bytes()).is_err() {
        let _ = child.kill();
        return None;
    }
    drop(stdin);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(output)) if output.status.success() => parse_git_credential_password(&output.stdout),
        Ok(_) => None,
        Err(_) => {
            #[cfg(unix)] { let _ = Command::new("kill").arg("-9").arg(format!("-{id}")).status(); }
            #[cfg(windows)] { let _ = Command::new("taskkill").args(["/F", "/T", "/PID"]).arg(id.to_string()).status(); }
            None
        }
    }
}

#[derive(Clone)]
struct GitHubGraphqlClient {
    http: reqwest::blocking::Client,
    endpoint: String,
    token: Option<String>,
}

impl GitHubGraphqlClient {
    fn discover(repository_path: &str, repo: &GitHubRepo) -> Result<Self, String> {
        let token = github_token_from_environment(&repo.host)
            .or_else(|| github_token_from_git_credential_helper(repository_path, repo));
        // GitHub's GraphQL API requires authentication even for public
        // repositories. Avoid several guaranteed-to-fail requests and give
        // one precise setup message instead.
        if token.is_none() {
            return Err(format!(
                "GitHub API authentication is required for {}. Git DrillDown could not obtain an HTTPS credential from Git Credential Manager. Make sure `git fetch` works over HTTPS for this host, or provide a read-only {}. The GitHub CLI is optional.",
                repo.host,
                if repo.host.eq_ignore_ascii_case("github.com") { "GH_TOKEN" } else { "GH_ENTERPRISE_TOKEN" },
            ));
        }
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("Git-DrillDown/0.1")
            .build()
            .map_err(|error| format!("Could not initialize the GitHub API client: {error}"))?;
        Ok(Self { http, endpoint: github_graphql_endpoint(&repo.host), token })
    }

    fn run(&self, query: &PrGhQuery) -> GhOutcome {
        let Some(repo) = parse_gh_repo_arg(&query.repo) else {
            return GhOutcome::Failure { stderr: "The GitHub repository identifier is invalid.".into() };
        };
        let query_text = match query.direction {
            PrQueryDirection::Head => GITHUB_PRS_BY_HEAD_QUERY,
            PrQueryDirection::Base => GITHUB_PRS_BY_BASE_QUERY,
        };
        let variables = serde_json::json!({
            "owner": repo.owner,
            "name": repo.repo,
            "branch": &query.branch,
        });
        let mut request = self.http.post(&self.endpoint).json(&serde_json::json!({
            "query": query_text,
            "variables": variables,
        }));
        if let Some(token) = &self.token { request = request.bearer_auth(token); }
        let response = match request.send() {
            Ok(response) => response,
            Err(error) => return GhOutcome::Failure { stderr: format!("GitHub API request failed: {error}") },
        };
        let status = response.status();
        let payload = match response.json::<serde_json::Value>() {
            Ok(payload) => payload,
            Err(error) => return GhOutcome::Failure { stderr: format!("GitHub API returned HTTP {status} with an unreadable response: {error}") },
        };
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return GhOutcome::Failure { stderr: format!("GitHub API authentication failed (HTTP {status}). The saved credential may be expired or may not have read access to this repository.") };
        }
        if !status.is_success() {
            return GhOutcome::Failure { stderr: format!("GitHub API request failed with HTTP {status}.") };
        }
        if let Some(errors) = payload.get("errors").and_then(|value| value.as_array()).filter(|errors| !errors.is_empty()) {
            let message = errors.iter().filter_map(|error| error.get("message").and_then(|value| value.as_str())).take(3).collect::<Vec<_>>().join("; ");
            return GhOutcome::Failure { stderr: if message.is_empty() { "GitHub API returned an error.".into() } else { format!("GitHub API error: {message}") } };
        }
        let Some(nodes) = payload.pointer("/data/repository/pullRequests/nodes").and_then(|value| value.as_array()) else {
            return GhOutcome::Failure { stderr: "GitHub API response did not contain a pull-request list.".into() };
        };
        GhOutcome::Prs(nodes.iter().map(|node| {
            let mut pr = normalize_graphql_pr(node);
            self.fill_missing_check_details(&repo, &mut pr);
            pr
        }).collect())
    }

    fn rest_get_json(&self, repo: &GitHubRepo, suffix: &str) -> Result<serde_json::Value, String> {
        let mut request = self.http.get(github_rest_endpoint(repo, suffix))
            .header("Accept", "application/vnd.github+json");
        if let Some(token) = &self.token { request = request.bearer_auth(token); }
        let response = request.send().map_err(|error| format!("GitHub REST API request failed: {error}"))?;
        let status = response.status();
        let payload = response.json::<serde_json::Value>()
            .map_err(|error| format!("GitHub REST API returned HTTP {status} with an unreadable response: {error}"))?;
        if !status.is_success() {
            return Err(format!("GitHub REST API request failed with HTTP {status}."));
        }
        Ok(payload)
    }

    fn get_text(&self, url: &str) -> Result<String, String> {
        let mut request = self.http.get(url).header("Accept", "text/html");
        if let Some(token) = &self.token { request = request.bearer_auth(token); }
        let response = request.send().map_err(|error| format!("GitHub HTML request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("GitHub HTML request failed with HTTP {status}."));
        }
        response.text().map_err(|error| format!("GitHub HTML response could not be read: {error}"))
    }

    fn fill_missing_check_details(&self, repo: &GitHubRepo, pr: &mut serde_json::Value) {
        let missing = count_missing_pr_check_details(pr);
        if missing == 0 { return; }
        let Some(head_sha) = pr.get("headSha").and_then(|value| value.as_str()).map(str::trim).filter(|value| !value.is_empty()) else {
            perf_log(&format!("pr_check_details_rest_fallback: missing={missing} skipped=no_head_sha"), Duration::ZERO);
            return;
        };
        let short_sha: String = head_sha.chars().take(8).collect();
        let status_payload = match self.rest_get_json(repo, &format!("commits/{head_sha}/status")) {
            Ok(payload) => Some(payload),
            Err(_) => {
                perf_log(&format!("pr_check_details_rest_fallback: sha={short_sha} status_api_failed=true"), Duration::ZERO);
                None
            }
        };
        let check_runs_payload = match self.rest_get_json(repo, &format!("commits/{head_sha}/check-runs?per_page=100")) {
            Ok(payload) => Some(payload),
            Err(_) => {
                perf_log(&format!("pr_check_details_rest_fallback: sha={short_sha} check_runs_api_failed=true"), Duration::ZERO);
                None
            }
        };
        let urls = collect_rest_check_detail_urls(status_payload.as_ref(), check_runs_payload.as_ref());
        let filled = fill_missing_pr_check_details_from_urls(pr, &urls);
        perf_log(&format!("pr_check_details_rest_fallback: sha={short_sha} missing={missing} filled={filled}"), Duration::ZERO);
        let missing_after_rest = count_missing_pr_check_details(pr);
        if missing_after_rest == 0 { return; }
        let Some(pr_url) = pr.get("url").and_then(|value| value.as_str()).map(str::trim).filter(|value| !value.is_empty()) else {
            perf_log(&format!("pr_check_details_html_fallback: sha={short_sha} missing={missing_after_rest} skipped=no_pr_url"), Duration::ZERO);
            return;
        };
        match self.get_text(pr_url) {
            Ok(html) => {
                let html_urls = collect_pr_check_detail_urls_from_html(pr_url, &html);
                let found = html_urls.len();
                let html_filled = fill_missing_pr_check_details_from_urls(pr, &html_urls);
                perf_log(&format!("pr_check_details_html_fallback: sha={short_sha} missing={missing_after_rest} found={found} filled={html_filled}"), Duration::ZERO);
            }
            Err(_) => {
                perf_log(&format!("pr_check_details_html_fallback: sha={short_sha} missing={missing_after_rest} html_failed=true"), Duration::ZERO);
            }
        }
    }
}

fn parse_gh_repo_arg(value: &str) -> Option<GitHubRepo> {
    let mut parts = value.split('/');
    let host = parts.next()?.trim();
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if host.is_empty() || owner.is_empty() || repo.is_empty() || parts.next().is_some() || !is_github_like_host(host) { return None; }
    Some(GitHubRepo { host: host.to_ascii_lowercase(), owner: owner.into(), repo: repo.into() })
}

// Kept as two small static documents because GraphQL arguments cannot switch
// between headRefName/baseRefName through a variable. Both return precisely
// the vocabulary already consumed by the existing gh response mapper.
const GITHUB_PRS_BY_HEAD_QUERY: &str = r#"
query PullRequestsByHead($owner: String!, $name: String!, $branch: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: 100, states: [OPEN], headRefName: $branch) {
      nodes {
        number title headRefName baseRefName state isDraft mergeable reviewDecision url
        headRepository { name }
        headRepositoryOwner { login }
        reviewRequests(first: 20) {
          nodes { requestedReviewer { ... on User { login } } }
        }
        latestReviews(first: 20) { nodes { author { login } state url } }
        commits(last: 1) { nodes { commit { oid statusCheckRollup {
          state
          contexts(first: 100) { nodes {
            ... on CheckRun { name status conclusion detailsUrl }
            ... on StatusContext { context state targetUrl }
          } }
        } } } }
      }
    }
  }
}
"#;
const GITHUB_PRS_BY_BASE_QUERY: &str = r#"
query PullRequestsByBase($owner: String!, $name: String!, $branch: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: 100, states: [OPEN], baseRefName: $branch) {
      nodes {
        number title headRefName baseRefName state isDraft mergeable reviewDecision url
        headRepository { name }
        headRepositoryOwner { login }
        reviewRequests(first: 20) {
          nodes { requestedReviewer { ... on User { login } } }
        }
        latestReviews(first: 20) { nodes { author { login } state url } }
        commits(last: 1) { nodes { commit { oid statusCheckRollup {
          state
          contexts(first: 100) { nodes {
            ... on CheckRun { name status conclusion detailsUrl }
            ... on StatusContext { context state targetUrl }
          } }
        } } } }
      }
    }
  }
}
"#;

fn normalize_graphql_pr(item: &serde_json::Value) -> serde_json::Value {
    let head_sha = item.pointer("/commits/nodes/0/commit/oid").cloned().unwrap_or(serde_json::Value::Null);
    let rollup = item.pointer("/commits/nodes/0/commit/statusCheckRollup/contexts/nodes")
        .and_then(|value| value.as_array()).cloned()
        .or_else(|| item.pointer("/commits/nodes/0/commit/statusCheckRollup/state")
            .and_then(|value| value.as_str()).map(|state| vec![serde_json::json!({ "state": state })]))
        .unwrap_or_default();
    let review_requests = item.pointer("/reviewRequests/nodes").and_then(|value| value.as_array())
        .map(|nodes| nodes.iter().filter_map(|node| node.get("requestedReviewer").cloned()).collect::<Vec<_>>())
        .unwrap_or_default();
    let latest_reviews = item.pointer("/latestReviews/nodes").and_then(|value| value.as_array()).cloned().unwrap_or_default();
    serde_json::json!({
        "number": item.get("number").cloned().unwrap_or(serde_json::Value::Null),
        "title": item.get("title").cloned().unwrap_or(serde_json::Value::Null),
        "headRefName": item.get("headRefName").cloned().unwrap_or(serde_json::Value::Null),
        "headRepository": item.get("headRepository").cloned().unwrap_or(serde_json::Value::Null),
        "headRepositoryOwner": item.get("headRepositoryOwner").cloned().unwrap_or(serde_json::Value::Null),
        "baseRefName": item.get("baseRefName").cloned().unwrap_or(serde_json::Value::Null),
        "state": item.get("state").cloned().unwrap_or(serde_json::Value::Null),
        "isDraft": item.get("isDraft").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "mergeable": item.get("mergeable").cloned().unwrap_or(serde_json::Value::Null),
        "reviewDecision": item.get("reviewDecision").cloned().unwrap_or(serde_json::Value::Null),
        "reviewRequests": review_requests,
        "latestReviews": latest_reviews,
        "headSha": head_sha,
        "statusCheckRollup": rollup,
        "url": item.get("url").cloned().unwrap_or(serde_json::Value::Null),
    })
}

// gh expands team review requests with Team.name/slug, which GitHub Enterprise
// protects with read:org. Keep the CLI path usable with the ordinary repo scope;
// submitted reviews remain available through latestReviews. The direct API path
// still reports individually requested users without querying protected fields.
const PR_GH_JSON_FIELDS: &str = "number,title,headRefName,headRepository,headRepositoryOwner,baseRefName,state,isDraft,mergeable,reviewDecision,latestReviews,statusCheckRollup,url";

fn gh_stderr_looks_like_auth(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("auth") || lower.contains("not logged") || lower.contains("credentials")
        || lower.contains("no accounts") || lower.contains("gh auth login")
}

fn run_gh_pr_list(query: &PrGhQuery) -> GhOutcome {
    let side_flag = match query.direction { PrQueryDirection::Head => "--head", PrQueryDirection::Base => "--base" };
    let mut command = Command::new("gh");
    command.args(["pr", "list", "--repo", &query.repo, side_flag, &query.branch, "--state", "open", "--json", PR_GH_JSON_FIELDS]);
    command.stdin(std::process::Stdio::null());
    match run_with_timeout_labeled(command, Duration::from_secs(20), "gh", "20 seconds") {
        Err(error) => {
            let not_installed = error.contains("Cannot start gh") || error.to_lowercase().contains("no such file");
            GhOutcome::Unavailable { not_installed, detail: error }
        }
        Ok(output) if !output.status.success() => GhOutcome::Failure { stderr: String::from_utf8_lossy(&output.stderr).trim().to_string() },
        Ok(output) => match serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) {
            Ok(items) => GhOutcome::Prs(items),
            Err(error) => GhOutcome::Failure { stderr: format!("Could not read the GitHub CLI's response: {error}") },
        },
    }
}

fn pr_summary_from_json(item: &serde_json::Value) -> PullRequestSummary {
    let rollup = item.get("statusCheckRollup").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    PullRequestSummary {
        number: item.get("number").and_then(|v| v.as_u64()).unwrap_or(0),
        title: item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        source_branch: item.get("headRefName").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        target_branch: item.get("baseRefName").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        state: map_pr_state(item.get("state").and_then(|v| v.as_str()).unwrap_or(""), item.get("isDraft").and_then(|v| v.as_bool()).unwrap_or(false)),
        mergeable: map_mergeable(item.get("mergeable").and_then(|v| v.as_str()).unwrap_or("")),
        review_summary: map_review_summary(item.get("reviewDecision").and_then(|v| v.as_str()).unwrap_or("")),
        reviewers: pr_reviewers_from_json(item),
        checks_status: map_checks_status(&rollup),
        checks: pr_checks_from_json(&rollup),
        url: item.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    }
}

// `gh pr list --head <name>` matches by head ref name across *every* fork,
// so a raw response can contain PRs from unrelated repos that happen to
// have a same-named branch. Keep only the ones that are genuinely this
// branch on this repo: exact remote-branch name, and a head repo whose
// owner (and name, when gh reports it) is where the branch actually lives.
fn select_matching_prs(raw: &[serde_json::Value], head_branch: &str, head_owner: &str, head_repo_name: &str) -> Vec<PullRequestSummary> {
    raw.iter()
        .filter(|item| item.get("headRefName").and_then(|v| v.as_str()) == Some(head_branch))
        .filter(|item| {
            let owner_ok = item.get("headRepositoryOwner").and_then(|v| v.get("login")).and_then(|v| v.as_str())
                .map(|login| login.eq_ignore_ascii_case(head_owner)).unwrap_or(false);
            let name_ok = match item.get("headRepository").and_then(|v| v.get("name")).and_then(|v| v.as_str()) {
                Some(name) => name.eq_ignore_ascii_case(head_repo_name),
                None => true,
            };
            owner_ok && name_ok
        })
        .map(pr_summary_from_json)
        .collect()
}

// `gh pr list --repo X --base Y` has no cross-fork ambiguity the way
// `--head` does: a PR's base branch always lives in the repo the PR was
// opened against, which is exactly `--repo` — there is no second repo a
// same-named base branch could belong to. So, deliberately, no
// owner/repo-name filter here at all (point 4 of the report): an incoming
// PR is expected to come from any other branch, fork, or owner, and
// filtering it the same way outgoing PRs are would silently drop
// legitimate incoming PRs from anyone but ourselves.
fn select_incoming_prs(raw: &[serde_json::Value]) -> Vec<PullRequestSummary> {
    raw.iter().map(pr_summary_from_json).collect()
}

// One query's outcome, tagged with which role it played — collected from
// the concurrent dispatch in pr_status_impl below and reduced into a
// PrStatusResult afterward, all on the calling thread, so the reduction
// logic itself stays single-threaded and easy to follow. Outgoing (current
// branch as head/source) still fans out across every candidate base, as
// before; Incoming (current branch as base/target) is always exactly one
// query, against the repo the branch actually lives in — the two have
// different validation rules (point 4) and are tracked for completeness
// completely separately (point 6).
enum PrCandidateRole { Outgoing { is_head_repo: bool }, Incoming }
struct PrCandidateOutcome { role: PrCandidateRole, base_id: String, outcome: GhOutcome }

fn pr_status_impl(
    ctx: PrQueryContext,
    repository_path: &str,
    context_label: &str,
    run: &(dyn Fn(&PrGhQuery) -> GhOutcome + Sync),
) -> PrStatusResult {
    let head_owner = ctx.head_repo.owner.clone();
    let head_name = ctx.head_repo.repo.clone();
    let queried_repo = ctx.head_repo.gh_repo_arg();

    if let Some(frontend_branch) = &ctx.frontend_branch_mismatch {
        perf_log(&format!("pr_status: [{context_label}] frontend branch '{frontend_branch}' != HEAD '{}' — using HEAD", ctx.local_branch), Duration::ZERO);
    }
    perf_log(&format!(
        "pr_status: [{}] repo={} host={} head_repo=<{}> local_branch={} remote_branch={} tracking_remote={} had_upstream={} candidate_bases={} truncated={}",
        context_label, anonymized_repository_id(repository_path), ctx.head_repo.host,
        anonymized_repository_id(&ctx.head_repo.gh_repo_arg()), ctx.local_branch, ctx.head_branch,
        ctx.tracking_remote, ctx.had_upstream, ctx.candidate_bases.len(), ctx.candidates_truncated,
    ), Duration::ZERO);

    // Every outgoing candidate base, *and* the one incoming query, are
    // dispatched together in the same concurrent batch — one OS thread
    // each — instead of one after another (which meant a worst case of N
    // sequential 20s timeouts). Since they all start together, the
    // wall-clock bound for the whole round is just the slowest single
    // query's own existing 20s timeout, not their sum. `run` must be
    // `Sync` — shared, read-only, across every spawned thread. The
    // incoming query is never fanned out across candidate_bases the way
    // outgoing is: a PR that targets this branch can only ever be opened
    // in the repo the branch actually lives in, never a different remote.
    let dispatch_started = Instant::now();
    let outcomes: Vec<PrCandidateOutcome> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(ctx.candidate_bases.len() + 1);
        for (index, base) in ctx.candidate_bases.iter().enumerate() {
            let query = PrGhQuery { repo: base.gh_repo_arg(), branch: ctx.head_branch.clone(), direction: PrQueryDirection::Head };
            handles.push(scope.spawn(move || {
                let base_id = anonymized_repository_id(&query.repo);
                PrCandidateOutcome { role: PrCandidateRole::Outgoing { is_head_repo: index == 0 }, base_id, outcome: run(&query) }
            }));
        }
        let incoming_query = PrGhQuery { repo: ctx.head_repo.gh_repo_arg(), branch: ctx.head_branch.clone(), direction: PrQueryDirection::Base };
        handles.push(scope.spawn(move || {
            let base_id = anonymized_repository_id(&incoming_query.repo);
            PrCandidateOutcome { role: PrCandidateRole::Incoming, base_id, outcome: run(&incoming_query) }
        }));
        handles.into_iter().map(|handle| handle.join().unwrap_or(PrCandidateOutcome {
            role: PrCandidateRole::Outgoing { is_head_repo: false }, base_id: "unknown".into(),
            outcome: GhOutcome::Failure { stderr: "gh query thread panicked".into() },
        })).collect()
    });
    perf_log(&format!("pr_status: [{context_label}] {} outgoing + 1 incoming query dispatched concurrently", ctx.candidate_bases.len()), dispatch_started.elapsed());

    let mut outgoing_found: Vec<PullRequestSummary> = Vec::new();
    let mut incoming_found: Vec<PullRequestSummary> = Vec::new();
    let mut raw_total = 0usize;
    let mut head_repo_query_ok = false;
    let mut head_repo_error: Option<(bool, String)> = None; // (auth-ish, detail)
    let mut head_repo_unavailable: Option<(bool, String)> = None; // gh missing / unrunnable
    // A *non*-head outgoing candidate that failed or couldn't run — the
    // search wasn't complete, so an otherwise-empty result must not read
    // as confident.
    let mut secondary_candidate_failed = false;
    // The incoming query's own completeness — deliberately never promoted
    // to a hard top-level error the way the head repo's own failure is
    // (point 6 asks for "a partial result", not for an outgoing success to
    // be overridden by an incoming-side auth/API error): it only ever
    // widens `incomplete`, same bucket as secondary_candidate_failed.
    let mut incoming_incomplete = false;

    for PrCandidateOutcome { role, base_id, outcome } in outcomes {
        match role {
            PrCandidateRole::Outgoing { is_head_repo } => match outcome {
                GhOutcome::Prs(items) => {
                    if is_head_repo { head_repo_query_ok = true; }
                    raw_total += items.len();
                    outgoing_found.extend(select_matching_prs(&items, &ctx.head_branch, &head_owner, &head_name));
                }
                GhOutcome::Failure { stderr } => {
                    let auth = gh_stderr_looks_like_auth(&stderr);
                    perf_log(&format!("pr_status: [{context_label}] outgoing base=<{base_id}> query failed (auth={auth})"), Duration::ZERO);
                    if is_head_repo { head_repo_error = Some((auth, stderr)); } else { secondary_candidate_failed = true; }
                }
                GhOutcome::Unavailable { not_installed, detail } => {
                    perf_log(&format!("pr_status: [{context_label}] outgoing base=<{base_id}> gh unavailable (not_installed={not_installed})"), Duration::ZERO);
                    if is_head_repo { head_repo_unavailable = Some((not_installed, detail)); } else { secondary_candidate_failed = true; }
                }
            },
            PrCandidateRole::Incoming => match outcome {
                GhOutcome::Prs(items) => {
                    raw_total += items.len();
                    incoming_found.extend(select_incoming_prs(&items));
                }
                GhOutcome::Failure { stderr } => {
                    perf_log(&format!("pr_status: [{context_label}] incoming base=<{base_id}> query failed: {stderr}"), Duration::ZERO);
                    incoming_incomplete = true;
                }
                GhOutcome::Unavailable { not_installed, detail } => {
                    perf_log(&format!("pr_status: [{context_label}] incoming base=<{base_id}> gh unavailable (not_installed={not_installed}): {detail}"), Duration::ZERO);
                    incoming_incomplete = true;
                }
            },
        }
    }

    // The same PR reached through two different `--repo` targets is one PR
    // — within each direction; outgoing and incoming are two genuinely
    // different relationships and are never deduplicated against each
    // other (a branch can, in principle, be both some PR's head and a
    // different PR's base at the same time).
    outgoing_found.sort_by(|a, b| a.url.cmp(&b.url));
    outgoing_found.dedup_by(|a, b| !a.url.is_empty() && a.url == b.url);
    incoming_found.sort_by(|a, b| a.url.cmp(&b.url));
    incoming_found.dedup_by(|a, b| !a.url.is_empty() && a.url == b.url);

    let incomplete = ctx.candidates_truncated || secondary_candidate_failed || incoming_incomplete;
    perf_log(&format!("pr_status: [{context_label}] raw_results={raw_total} outgoing={} incoming={} incomplete={incomplete}", outgoing_found.len(), incoming_found.len()), Duration::ZERO);

    if !outgoing_found.is_empty() || !incoming_found.is_empty() {
        // Real PRs found in either direction — worth reporting regardless
        // of whether every query could be checked; `partial` still says so.
        return PrStatusResult { state: "ok".into(), detail: String::new(), branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: incomplete, outgoing_pull_requests: outgoing_found, incoming_pull_requests: incoming_found };
    }
    if let Some((not_installed, detail)) = head_repo_unavailable {
        let message = if not_installed {
            "Pull request status needs GitHub authentication. Git DrillDown could not use GitHub CLI or obtain an HTTPS credential from Git Credential Manager. Make sure `git fetch` works over HTTPS for this host, or provide a read-only GitHub API token. You can still open the repository's pull requests in your signed-in browser.".to_string()
        } else { detail };
        return PrStatusResult { state: "auth_missing".into(), detail: message, branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: true, outgoing_pull_requests: Vec::new(), incoming_pull_requests: Vec::new() };
    }
    if let Some((auth, stderr)) = head_repo_error {
        let enterprise = ctx.head_repo.host != "github.com";
        let state = if auth { "auth_missing" } else { "api_error" };
        let detail = if auth && enterprise {
            format!("GitHub authentication failed for {}. Configure an HTTPS credential in Git Credential Manager or authenticate the optional GitHub CLI for this host.{}", ctx.head_repo.host,
                if stderr.trim().is_empty() { String::new() } else { format!(" ({})", stderr.trim()) })
        } else if stderr.trim().is_empty() {
            "The GitHub API request failed.".to_string()
        } else { stderr.trim().to_string() };
        return PrStatusResult { state: state.into(), detail, branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: true, outgoing_pull_requests: Vec::new(), incoming_pull_requests: Vec::new() };
    }
    if !head_repo_query_ok {
        // Neither a result, an error, nor "unavailable" from the head repo —
        // shouldn't happen, but never report a confident "no PR" off it.
        return PrStatusResult { state: "api_error".into(), detail: "Could not determine pull request status.".into(), branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: true, outgoing_pull_requests: Vec::new(), incoming_pull_requests: Vec::new() };
    }
    if incomplete {
        // Point 6: an empty result is only ever confident once *both*
        // relevant queries (every outgoing candidate, and the one incoming
        // query) actually completed. If either couldn't be checked, this
        // must never look like a confident "no open PR" — including when
        // the incoming query specifically is what failed, even though the
        // outgoing side came back clean.
        return PrStatusResult {
            state: "partial_result".into(),
            detail: "Some candidate repositories could not be checked, so this result may be incomplete.".into(),
            branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: true, outgoing_pull_requests: Vec::new(), incoming_pull_requests: Vec::new(),
        };
    }
    // Genuine empty: every relevant query — outgoing and incoming alike —
    // completed, and none had a match.
    let state = if ctx.had_upstream { "no_open_pr" } else { "no_upstream" };
    PrStatusResult { state: state.into(), detail: String::new(), branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: false, outgoing_pull_requests: Vec::new(), incoming_pull_requests: Vec::new() }
}

// A newer pr_status call for the same repository path tells an older,
// possibly still-in-flight one (rapid submodule/context switching, or the
// same panel polling again before the previous poll returned) that its
// answer is no longer wanted — checked right before the expensive gh round,
// so a burst of superseded calls skips straight past it instead of each
// spending a full concurrent-gh round on an answer nobody will see. This is
// a best-effort, pre-dispatch check, not true mid-flight cancellation of an
// already-running gh process; the frontend's own request-generation guard
// (see createPrStatusPanel) is what actually keeps a late response from
// ever being *applied* to the wrong context — this only saves the work.
// Keyed by the raw repository path: a fast in-memory hint, not a
// correctness-critical identity like repo_write_lock's canonicalized key.
static PR_STATUS_GENERATION: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn pr_status_generation_map() -> &'static Mutex<HashMap<String, u64>> {
    PR_STATUS_GENERATION.get_or_init(|| Mutex::new(HashMap::new()))
}

fn claim_pr_status_generation(repository_path: &str) -> u64 {
    let mut map = pr_status_generation_map().lock().unwrap();
    let next = map.get(repository_path).copied().unwrap_or(0) + 1;
    map.insert(repository_path.to_string(), next);
    next
}

fn is_latest_pr_status_generation(repository_path: &str, generation: u64) -> bool {
    pr_status_generation_map().lock().unwrap().get(repository_path).copied() == Some(generation)
}

#[tauri::command]
pub async fn pr_status(repository_path: String, branch: Option<String>, context: Option<String>) -> Result<PrStatusResult, String> {
    off_main_thread(move || pr_status_inner(repository_path, branch, context)).await
}

fn pr_status_inner(repository_path: String, branch: Option<String>, context: Option<String>) -> Result<PrStatusResult, String> {
    validate_path(&repository_path)?;
    let generation = claim_pr_status_generation(&repository_path);
    let repo = internal_repository(&repository_path)?;
    let context_label = context.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("parent").to_string();

    let ctx = match resolve_pr_query_context(&repo, branch.as_deref()) {
        PrContext::Ready(ctx) => ctx,
        PrContext::Terminal(result) => {
            perf_log(&format!("pr_status: [{}] repo={} terminal={} (no gh call)", context_label, anonymized_repository_id(&repository_path), result.state), Duration::ZERO);
            return Ok(result);
        }
    };
    drop(repo);
    if !is_latest_pr_status_generation(&repository_path, generation) {
        perf_log(&format!("pr_status: [{context_label}] repo={} superseded by a newer request — skipping the provider round", anonymized_repository_id(&repository_path)), Duration::ZERO);
        return Ok(PrStatusResult::plain("superseded", "A newer request for this repository has already superseded this one."));
    }
    if gh_cli_available() {
        perf_log(&format!("pr_status: [{context_label}] provider=gh"), Duration::ZERO);
        return Ok(pr_status_impl(ctx, &repository_path, &context_label, &run_gh_pr_list));
    }

    // No optional gh installation: reuse the HTTPS credential already held
    // by Git Credential Manager and query the same Enterprise host directly.
    // Discover once per panel refresh, before the concurrent candidate calls,
    // so no credential helper is invoked once per repository/row.
    let api = match GitHubGraphqlClient::discover(&repository_path, &ctx.head_repo) {
        Ok(api) => api,
        Err(detail) => return Ok(PrStatusResult {
            state: "auth_missing".into(),
            detail,
            branch: Some(ctx.head_branch),
            queried_repo: Some(ctx.head_repo.gh_repo_arg()),
            partial: true,
            outgoing_pull_requests: Vec::new(),
            incoming_pull_requests: Vec::new(),
        }),
    };
    perf_log(&format!("pr_status: [{context_label}] provider=github_api credential=git_or_environment"), Duration::ZERO);
    Ok(pr_status_impl(ctx, &repository_path, &context_label, &|query| api.run(query)))
}

fn parse_pull_request_target(url: &str) -> Option<(GitHubRepo, u64)> {
    if !is_generated_pull_request_url(url) { return None; }
    let rest = url.strip_prefix("https://")?.split('#').next()?;
    let (host, path) = rest.split_once('/')?;
    if !is_github_like_host(host) { return None; }
    let parts: Vec<_> = path.split('/').collect();
    let [owner, repo, "pull", number] = parts.as_slice() else { return None; };
    Some((GitHubRepo { host: host.to_ascii_lowercase(), owner: (*owner).into(), repo: (*repo).into() }, number.parse().ok()?))
}

fn github_rest_endpoint(repo: &GitHubRepo, suffix: &str) -> String {
    if repo.host.eq_ignore_ascii_case("github.com") {
        format!("https://api.github.com/repos/{}/{}/{}", repo.owner, repo.repo, suffix.trim_start_matches('/'))
    } else {
        format!("https://{}/api/v3/repos/{}/{}/{}", repo.host, repo.owner, repo.repo, suffix.trim_start_matches('/'))
    }
}

fn post_pull_request_comment_with_gh(repo: &GitHubRepo, number: u64, body: &str) -> Result<(), String> {
    let endpoint = format!("repos/{}/{}/issues/{number}/comments", repo.owner, repo.repo);
    let mut command = Command::new("gh");
    command.args(["api", "--hostname", &repo.host, "--method", "POST", &endpoint, "--raw-field"])
        .arg(format!("body={body}")).stdin(std::process::Stdio::null());
    let output = run_with_timeout_labeled(command, Duration::from_secs(20), "gh comment", "20 seconds")?;
    if output.status.success() { return Ok(()); }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() { "GitHub CLI did not accept the comment.".into() } else { detail })
}

// Explicit external mutation: called only from a PR card's Send button.
// It deliberately does not share pr_status' refresh path, so opening or
// refreshing the read-only panel can never send a comment accidentally.
#[tauri::command]
pub async fn post_pull_request_comment(repository_path: String, pull_request_url: String, body: String) -> Result<(), String> {
    off_main_thread(move || post_pull_request_comment_inner(repository_path, pull_request_url, body)).await
}

fn post_pull_request_comment_inner(repository_path: String, pull_request_url: String, body: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let body = body.trim();
    if body.is_empty() { return Err("Write a comment before sending it.".into()); }
    if body.chars().count() > 65_536 { return Err("The comment is too long (maximum 65,536 characters).".into()); }
    let (repo, number) = parse_pull_request_target(&pull_request_url).ok_or("The pull request link is invalid.")?;
    let gh_error = if gh_cli_available() {
        match post_pull_request_comment_with_gh(&repo, number, body) {
            Ok(()) => {
                perf_log(&format!("post_pull_request_comment: repo=<{}> pr={number} provider=gh sent", anonymized_repository_id(&repo.gh_repo_arg())), Duration::ZERO);
                return Ok(());
            }
            Err(error) => Some(error),
        }
    } else { None };
    let api = GitHubGraphqlClient::discover(&repository_path, &repo).map_err(|direct_error| {
        gh_error.map(|gh_error| format!("GitHub CLI could not send the comment ({gh_error}). Direct API fallback also failed: {direct_error}"))
            .unwrap_or(direct_error)
    })?;
    let endpoint = github_rest_endpoint(&repo, &format!("issues/{number}/comments"));
    let mut request = api.http.post(endpoint).json(&serde_json::json!({ "body": body }));
    if let Some(token) = &api.token { request = request.bearer_auth(token); }
    let response = request.send().map_err(|error| format!("Could not send the pull request comment: {error}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(format!("GitHub refused the comment (HTTP {status}). The saved token needs permission to write pull request comments."));
    }
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        let summary = serde_json::from_str::<serde_json::Value>(&detail).ok()
            .and_then(|value| value.get("message").and_then(|message| message.as_str()).map(String::from))
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(format!("GitHub did not accept the comment: {summary}"));
    }
    perf_log(&format!("post_pull_request_comment: repo=<{}> pr={number} provider=github_api sent", anonymized_repository_id(&repo.gh_repo_arg())), Duration::ZERO);
    Ok(())
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

// Every commit reachable from `branch` but not from any of `remote`'s own
// branches — shared by publish_status (which just lists/displays them) and
// unpushed_submodule_references (which must inspect every one of them, not
// just the tip, to catch a gitlink an *older* outgoing commit already
// carries — see that function's own doc comment). Hides everything
// reachable from ANY of the remote's branches, not only the one sharing
// `branch`'s name — a brand new local branch that descends from, or sits
// right at, a commit already on the server under a different name doesn't
// need to re-push that shared history; only what isn't reachable from
// anything already on this remote is genuinely new. Without this,
// "Publish" on any new branch showed its *entire* ancestry as pending,
// even commits from years ago already sitting on origin.
fn outgoing_commit_ids(repo: &Repository, branch: &str, remote: &str) -> Result<Vec<git2::Oid>, String> {
    let local_oid = repo.refname_to_id(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; walk.push(local_oid).map_err(|error| error.message().to_string())?;
    if let Ok(references) = repo.references_glob(&format!("refs/remotes/{remote}/*")) {
        for reference in references.flatten() { if let Some(oid) = reference.target() { let _ = walk.hide(oid); } }
    }
    Ok(walk.flatten().collect())
}

#[tauri::command]
pub fn publish_status(repository_path: String, branch: String, remote: String) -> Result<PublishStatus, String> {
    validate_path(&repository_path)?;
    let branch = branch.trim(); let remote = remote.trim();
    if branch.is_empty() || remote.is_empty() { return Err("Choose a local branch and a remote".into()); }
    let repo = internal_repository(&repository_path)?;
    let remote_branch = format!("{remote}/{branch}");
    let local_oid = repo.refname_to_id(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    let remote_oid = repo.refname_to_id(&format!("refs/remotes/{remote}/{branch}")).ok();
    let outgoing = outgoing_commit_ids(&repo, branch, remote)?;
    let mut commits = outgoing.iter().take(100).filter_map(|&oid| repo.find_commit(oid).ok().map(|commit| PublishCommit { id: oid.to_string(), subject: commit.summary().unwrap_or("No message").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()) })).collect::<Vec<_>>(); commits.reverse();
    let (ahead, behind, remote_branch_exists) = match remote_oid {
        Some(remote_oid) => {
            let (ahead, behind) = repo.graph_ahead_behind(local_oid, remote_oid).map_err(|error| error.message().to_string())?;
            (ahead, behind, true)
        }
        None => (commits.len(), 0, false),
    };
    Ok(PublishStatus { branch: branch.into(), remote: remote.into(), remote_branch, commits, ahead, behind, remote_branch_exists })
}

// One outgoing parent commit's gitlink that cannot be confirmed safe to
// publish — either it points at a submodule commit not known to exist on
// that submodule's own remote yet ("unpushed", the common case: push the
// submodule first — never overridable, since it's always fixable), its
// *effective* .gitmodules clone source is a plain filesystem path or a
// file:// URL ("local_only" — genuinely reachable only from this exact
// machine, no matter how confidently it fetches from itself), the submodule
// has no remote/URL configured at all ("no_remote"), or the submodule's own
// repo couldn't be opened here to check at all ("unverifiable"). See
// unpushed_submodule_references for how this list is built.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct UnpushedSubmoduleReference {
    relative_path: String, submodule_oid: String, commit_id: String, commit_subject: String, risk: String,
    // The URL another, fresh clone would actually use for this submodule —
    // .gitmodules' own recorded URL when there is one, else this checkout's
    // local "origin" as a best-effort fallback. None only when neither
    // exists (risk is then always "no_remote" or "unverifiable").
    configured_url: Option<String>,
    // The submodule's own checkout right now — deliberately not the same
    // question as submodule_oid above (the gitlink an *outgoing parent
    // commit* references, which can be an older, since-superseded commit).
    // Lets the frontend say so explicitly when the two differ, instead of
    // silently showing what looks like a second, disagreeing SHA with no
    // explanation — the submodule can be fully pushed and in sync *right
    // now* while an earlier outgoing commit still references a commit that
    // isn't, and that is not a contradiction.
    current_submodule_oid: Option<String>,
    // The submodule commit recorded by the final parent commit that will be
    // pushed. When this differs from submodule_oid, the unsafe gitlink belongs
    // to an older intermediate parent commit, not to the branch tip the user is
    // trying to share now.
    target_submodule_oid: Option<String>,
    // True when the final parent commit's submodule revision is a descendant
    // of this revision. In that case pushing the final submodule tip really
    // would also make this older gitlink reachable. When false, the two
    // revisions are siblings/divergent history, which is the confusing real
    // report: "but I pushed the later one" is true, just not relevant to this
    // older parent commit.
    target_contains_submodule_oid: bool,
}

// Opening Publish already performs the expensive, explicit network refresh
// of every relevant submodule so it can show an accurate warning before the
// user confirms. The old confirm path immediately repeated those same fetches
// for the same parent commit. Keep only a short proof that this exact target
// was refreshed; Publish still reruns the complete tree/ref safety scan, but
// can reuse the freshly updated remote-tracking refs instead of doing the
// network round trips twice. An "unpushed" result is never cached because the
// user may push that submodule and retry while the dialog is still open.
const PUBLISH_PREFLIGHT_REUSE_WINDOW: Duration = Duration::from_secs(120);
static PUBLISH_PREFLIGHT_REFRESH_CACHE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn publish_preflight_cache() -> &'static Mutex<HashMap<String, Instant>> {
    PUBLISH_PREFLIGHT_REFRESH_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn publish_preflight_key(repository_path: &str, branch: &str, remote: &str, target: git2::Oid) -> String {
    format!("{}\u{0}{}\u{0}{}\u{0}{}", repo_lock_key(repository_path), branch, remote, target)
}

fn remember_publish_preflight(repository_path: &str, branch: &str, remote: &str, target: git2::Oid) {
    let mut cache = publish_preflight_cache().lock().unwrap();
    cache.retain(|_, checked_at| checked_at.elapsed() < PUBLISH_PREFLIGHT_REUSE_WINDOW);
    cache.insert(publish_preflight_key(repository_path, branch, remote, target), Instant::now());
}

fn has_recent_publish_preflight(repository_path: &str, branch: &str, remote: &str, target: git2::Oid) -> bool {
    publish_preflight_cache().lock().unwrap().get(&publish_preflight_key(repository_path, branch, remote, target))
        .is_some_and(|checked_at| checked_at.elapsed() < PUBLISH_PREFLIGHT_REUSE_WINDOW)
}

// True for anything only this exact machine (or one with filesystem access
// to that exact path) could ever resolve: a bare path (absolute, relative,
// or a Windows drive path) or an explicit file:// URL. False for a real
// scheme (https://, ssh://, git://...) or scp-like shorthand
// (git@host:owner/repo.git, host:path) — see the submodule-publish-safety
// report's own point 2: "Correctly classify filesystem paths and file://
// URLs as LOCAL-ONLY, not as globally available merely because the remote
// is named origin." A remote literally named "origin" pointing at
// `../_submodule_sources/x` is exactly this case — fetching from it always
// "succeeds" (it's sitting right there), which is precisely why checking
// reachability alone was never enough.
fn is_local_only_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() { return true; }
    if url.starts_with("file://") { return true; }
    if let Some((scheme, _)) = url.split_once("://") {
        if !scheme.is_empty() && scheme.chars().all(|value| value.is_ascii_alphanumeric() || value == '+' || value == '-') { return false; }
    }
    // scp-like shorthand ("git@host:owner/repo.git", "host:path") is remote
    // too — recognized the same way git itself does: a ':' whose left side
    // isn't a single-letter Windows drive and doesn't itself look like a path.
    if let Some(colon) = url.find(':') {
        let before = &url[..colon];
        let looks_like_drive_letter = before.len() == 1 && url[colon + 1..].starts_with(['/', '\\']);
        if !looks_like_drive_letter && !before.is_empty() && !before.contains('/') && !before.contains('\\') { return false; }
    }
    // No scheme, no scp-shorthand host — an absolute/relative filesystem
    // path or a Windows drive path.
    true
}

// None = this exact submodule commit is already known to be safely
// available from the submodule's effective clone source. Best-effort
// fetches first (mirrors push_submodule_inner's own `let _ = git(...
// "fetch" ...)` — not a "hidden" network operation in the sense point 6 of
// the earlier report asks to avoid: it's a disclosed, direct part of the
// explicit Publish action the user just took, exactly like
// push_submodule_inner already fetches as part of an explicit push) so a
// commit pushed moments ago from elsewhere isn't reported as missing just
// because this app's local remote-tracking refs hadn't caught up yet. A
// fetch failure (offline, unreachable) only ever leaves the existing local
// knowledge in place — that can only make this check *more* cautious, never
// less, which is the right direction to err in for something that decides
// whether it's safe to publish.
fn submodule_reference_risks(repository_path: &str, relative_path: &str, oids: &[git2::Oid], refresh_remote: bool) -> (HashMap<git2::Oid, Option<&'static str>>, Option<String>, Option<git2::Oid>) {
    let started = Instant::now();
    let all = |risk| oids.iter().copied().map(|oid| (oid, risk)).collect();
    // The *effective* clone source: what .gitmodules itself records for this
    // path, since that's what any other, fresh clone would actually use —
    // not merely whatever this local checkout's own "origin" happens to be
    // pointed at right now (someone may have redirected it to a personal
    // mirror or cache). Only falls back to the local "origin" URL when
    // .gitmodules has no URL recorded for this path at all.
    let gitmodules_url = submodule_value(repository_path, relative_path, "url").filter(|url| !url.trim().is_empty());
    let parent_repo = internal_repository(repository_path).ok();
    let absolute = Path::new(repository_path).join(relative_path);
    let Ok(repo) = internal_submodule_repository(&absolute) else {
        return (all(Some("unverifiable")), gitmodules_url, None);
    };
    // The submodule's current checkout, purely for context in the message
    // shown to the user: an outgoing parent commit's gitlink can legitimately
    // reference an older, since-superseded commit than what's checked out
    // right now (e.g. the submodule moved on locally after that parent
    // commit was made) — this is what lets that message say so explicitly
    // instead of leaving two different SHAs to appear to silently disagree.
    let current_oid = repo.head().ok().and_then(|head| head.target());
    let local_origin_url = repo.find_remote("origin").ok().and_then(|remote| remote.url().map(String::from)).filter(|url| !url.trim().is_empty());
    let configured_url = gitmodules_url.clone().or_else(|| local_origin_url.clone());
    // A relative .gitmodules value is not inherently a local filesystem
    // path. Git resolves it against the parent repository's remote. Thus
    // `../../eng/sw-pkg-x.git` under a GitHub Enterprise parent is a network
    // source, while the same text under a local-path parent remains local.
    let effective_url = gitmodules_url.as_deref().map(|url| {
        parent_repo.as_ref().and_then(|repo| resolved_submodule_io_url(repo, url).ok()).unwrap_or_else(|| url.to_string())
    }).or_else(|| local_origin_url.clone());
    match effective_url.as_deref() {
        None => return (all(Some("no_remote")), None, current_oid),
        Some(url) if is_local_only_url(url) => return (all(Some("local_only")), configured_url, current_oid),
        Some(_) => {}
    }
    // Verify only against origin. Fetching every configured remote was both
    // slow and subtly unsafe: a commit available only from an unrelated
    // personal/backup remote does not make it restorable by a normal clone.
    // Do not require literal URL equality here: Git commonly stores an
    // equivalent rewritten/credentialed URL in the local clone, while
    // .gitmodules keeps the portable public form.
    if repo.find_remote("origin").is_err() { return (all(Some("no_remote")), configured_url, current_oid); }
    let remote = "origin";
    let sub_path = absolute.to_string_lossy().into_owned();
    // One submodule path can occur at several different gitlink revisions in
    // the outgoing parent history. Fetch origin once for the entire batch,
    // never once per revision — the old nested behavior made a single
    // Publish repeat the same network round trip many times.
    if refresh_remote {
        let fetch_started = Instant::now();
        // Tags and nested-submodule recursion are irrelevant to the question
        // being answered here (is the gitlink commit reachable from a branch on
        // this source?) and can add substantial network work on large projects.
        let result = git(&sub_path, &["fetch", "--no-tags", "--no-recurse-submodules", remote]);
        perf_log(&format!("publish_safety: submodule={} fetch {} ({})", relative_path, remote, if result.is_ok() { "ok" } else { "failed; using local refs" }), fetch_started.elapsed());
    } else {
        perf_log(&format!("publish_safety: submodule={} reused freshly refreshed refs", relative_path), Duration::ZERO);
    }
    let remote_pattern = format!("refs/remotes/{remote}/*");
    let remote_tips: Vec<git2::Oid> = repo.references_glob(&remote_pattern).ok().into_iter().flat_map(|references| references.flatten())
        .filter_map(|reference| reference.target()).collect();
    let risks = oids.iter().copied().map(|oid| {
        let reachable = remote_tips.iter().copied().any(|tip| tip == oid || repo.graph_descendant_of(tip, oid).unwrap_or(false));
        (oid, if reachable { None } else { Some("unpushed") })
    }).collect();
    perf_log(&format!("publish_safety: submodule={} checked {} revision{} after one fetch round", relative_path, oids.len(), if oids.len() == 1 { "" } else { "s" }), started.elapsed());
    (risks, configured_url, current_oid)
}

// Point 3 of the submodule-publish-safety report: inspects every *outgoing*
// commit's own tree (diffed against its first parent — same one-parent
// convention unpushed_paths already uses for merges), not just the current
// index/HEAD state. An older outgoing commit can carry a gitlink to a
// submodule commit that was only ever local, even if a *later* outgoing
// commit already moved the submodule on to a since-pushed one — git can't
// publish the later commit while holding back the earlier one it depends
// on, so that older, unsafe gitlink ships right along with it. Distinct
// (path, submodule oid) pairs are checked once each, no matter how many
// outgoing commits repeat the same value.
fn unpushed_submodule_references(repository_path: &str, branch: &str, remote: &str, upto: Option<git2::Oid>, refresh_remotes: bool) -> Result<Vec<UnpushedSubmoduleReference>, String> {
    let total_started = Instant::now();
    let repo = internal_repository(repository_path)?;
    let step = Instant::now();
    let mut outgoing = outgoing_commit_ids(&repo, branch, remote)?;
    let target_parent_oid = match upto {
        Some(oid) => oid,
        None => repo.refname_to_id(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?,
    };
    perf_log(&format!("publish_safety: outgoing history ({} commits)", outgoing.len()), step.elapsed());
    // A partial publish ("stop at an earlier commit", see publish_branch's
    // own doc comment) only ever actually pushes commits up to and
    // including that cutoff — a commit *newer* than it, deliberately held
    // back, must never block this publish over a gitlink that isn't going
    // anywhere yet either.
    if let Some(cutoff) = upto {
        outgoing.retain(|&oid| oid == cutoff || repo.graph_descendant_of(cutoff, oid).unwrap_or(false));
    }
    // A safety scan must never silently check only *some* of the outgoing
    // history — that would defeat the entire point of this function. Bail
    // out instead of truncating when there's an implausibly large amount to
    // walk (a huge first publish on a very stale local branch).
    if outgoing.len() > 5000 {
        return Err(format!("Too many outgoing commits ({}) to verify submodule safety in one pass. Fetch/pull first, or publish in smaller steps.", outgoing.len()));
    }
    let mut seen = HashSet::new();
    let mut first_reference: Vec<(String, git2::Oid, git2::Oid)> = Vec::new(); // (path, submodule oid, the oldest outgoing commit introducing it)
    let step = Instant::now();
    for &oid in outgoing.iter().rev() { // oldest outgoing commit first, so "first" below really means first
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let Ok(tree) = commit.tree() else { continue };
        let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
        let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else { continue };
        for delta in diff.deltas() {
            if delta.status() == git2::Delta::Deleted { continue; }
            let new_file = delta.new_file();
            if new_file.mode() != git2::FileMode::Commit { continue; }
            let (Some(path), sub_oid) = (new_file.path(), new_file.id()) else { continue };
            if sub_oid.is_zero() { continue; }
            let path = normalized(path);
            if seen.insert((path.clone(), sub_oid)) { first_reference.push((path, sub_oid, oid)); }
        }
    }
    perf_log(&format!("publish_safety: tree scan ({} unique gitlinks)", first_reference.len()), step.elapsed());

    // Preserve first-seen path order for stable user-facing output while
    // grouping all revisions of the same submodule into one network round.
    let mut path_order = Vec::new();
    let mut by_path: HashMap<String, Vec<(git2::Oid, git2::Oid)>> = HashMap::new();
    for (path, sub_oid, commit_oid) in first_reference {
        if !by_path.contains_key(&path) { path_order.push(path.clone()); }
        by_path.entry(path).or_default().push((sub_oid, commit_oid));
    }
    let mut violations = Vec::new();
    for path in path_order {
        let references = by_path.remove(&path).unwrap_or_default();
        let target_submodule_oid = repo.find_commit(target_parent_oid).ok()
            .and_then(|commit| commit.tree().ok())
            .and_then(|tree| tree.get_path(Path::new(&path)).ok())
            .filter(|entry| entry.filemode() == 0o160000)
            .map(|entry| entry.id());
        let mut revision_seen = HashSet::new();
        let mut revisions: Vec<git2::Oid> = references.iter()
            .filter_map(|(sub_oid, _)| revision_seen.insert(*sub_oid).then_some(*sub_oid))
            .collect();
        if let Some(target_oid) = target_submodule_oid {
            if revision_seen.insert(target_oid) { revisions.push(target_oid); }
        }
        let (mut risks, configured_url, current_oid) = submodule_reference_risks(repository_path, &path, &revisions, refresh_remotes);
        let current_submodule_oid = current_oid.map(|oid| oid.to_string());
        let target_is_known_safe = target_submodule_oid.and_then(|oid| risks.get(&oid).copied()).is_some_and(|risk| risk.is_none());
        let target_submodule_oid_string = target_submodule_oid.map(|oid| oid.to_string());
        let submodule_repo_for_graph = internal_submodule_repository(&Path::new(repository_path).join(&path)).ok();
        for (sub_oid, commit_oid) in references {
            let Some(risk) = risks.remove(&sub_oid).flatten() else { continue };
            let target_contains_submodule_oid = target_submodule_oid.is_some_and(|target_oid| {
                target_oid == sub_oid || submodule_repo_for_graph.as_ref()
                    .is_some_and(|sub_repo| sub_repo.graph_descendant_of(target_oid, sub_oid).unwrap_or(false))
            });
            let risk = if risk == "unpushed" && target_submodule_oid.is_some_and(|target_oid| target_oid != sub_oid) && target_is_known_safe {
                if target_contains_submodule_oid {
                    // If the final, safely pushed submodule tip actually
                    // contains this older commit, the older gitlink is safe
                    // too: a normal push of the final branch carries its
                    // ancestors. Keep this invariant explicit so we never
                    // recreate the "two commits in order but first is missing"
                    // false alarm.
                    continue;
                }
                "superseded_unpushed"
            } else {
                risk
            };
            let commit_subject = repo.find_commit(commit_oid).ok().and_then(|commit| commit.summary().map(str::to_string)).unwrap_or_default();
            violations.push(UnpushedSubmoduleReference { relative_path: path.clone(), submodule_oid: sub_oid.to_string(), commit_id: commit_oid.to_string(), commit_subject, risk: risk.into(), configured_url: configured_url.clone(), current_submodule_oid: current_submodule_oid.clone(), target_submodule_oid: target_submodule_oid_string.clone(), target_contains_submodule_oid });
        }
    }
    perf_log(&format!("publish_safety: TOTAL ({} violation{})", violations.len(), if violations.len() == 1 { "" } else { "s" }), total_started.elapsed());
    Ok(violations)
}

// Marks an error as override-eligible (point 4: a submodule with no remote
// at all, or one this app couldn't even open to check, can never be
// verified — the only way forward besides never publishing is an explicit,
// informed override) — never used for risk "unpushed", which is always
// fixable by pushing and is never overridable. The frontend looks for this
// exact prefix to offer that override; stripped before it's shown.
const UNPUSHED_SUBMODULE_OVERRIDABLE_PREFIX: &str = "UNPUSHED_SUBMODULE_OVERRIDABLE::";

fn submodule_publish_safety_check(repository_path: &str, branch: &str, remote: &str, upto: Option<git2::Oid>, override_unpushed_submodules: bool) -> Result<(), String> {
    let reuse_refresh = upto.is_some_and(|target| has_recent_publish_preflight(repository_path, branch, remote, target));
    let violations = unpushed_submodule_references(repository_path, branch, remote, upto, !reuse_refresh)?;
    if reuse_refresh { perf_log("publish_safety: reused dialog network preflight; full local safety scan still ran", Duration::ZERO); }
    if violations.is_empty() { return Ok(()); }
    let hard: Vec<&UnpushedSubmoduleReference> = violations.iter().filter(|v| v.risk == "unpushed").collect();
    if !hard.is_empty() {
        let lines: Vec<String> = hard.iter().map(|v| {
            let target = &v.submodule_oid[..8.min(v.submodule_oid.len())];
            // The submodule's current checkout can be fully pushed and in
            // sync *right now* while an older outgoing commit still
            // references a different, since-superseded commit that isn't —
            // say so plainly instead of leaving two SHAs looking like they
            // silently disagree.
            let currently_different = v.current_submodule_oid.as_deref().is_some_and(|current| current != v.submodule_oid);
            let context = if currently_different {
                format!(" (its current checkout has since moved on to {}, but the parent branch still publishes a commit that points at the older local-only version)", &v.current_submodule_oid.as_deref().unwrap_or_default()[..8.min(v.current_submodule_oid.as_deref().unwrap_or_default().len())])
            } else { String::new() };
            format!("Push submodule {} first. The main project references {}, which is only local{}.", v.relative_path, target, context)
        }).collect();
        return Err(format!("Cannot publish — {} submodule commit{} not yet available on {}'s own remote:\n{}", hard.len(), if hard.len() == 1 { " is" } else { "s are" }, if hard.len() == 1 { "its" } else { "their" }, lines.join("\n")));
    }
    if override_unpushed_submodules { return Ok(()); }
    let lines: Vec<String> = violations.iter().map(|v| {
        let reason = match v.risk.as_str() {
            "superseded_unpushed" => {
                let target = v.target_submodule_oid.as_deref().unwrap_or_default();
                if v.target_contains_submodule_oid {
                    format!("has an older intermediate gitlink, but the final parent commit points at descendant {}", &target[..8.min(target.len())])
                } else {
                    format!("has an older intermediate gitlink to a divergent local-only commit; the final parent commit points at {}", &target[..8.min(target.len())])
                }
            }
            "no_remote" => "has no configured remote".to_string(),
            "local_only" => format!("is only reachable from a filesystem path or file:// URL ({})", v.configured_url.as_deref().unwrap_or("?")),
            _ => "could not be checked locally".to_string(),
        };
        format!("Submodule {} {} — its commit {} may exist only on this machine. Other users will not be able to restore it after cloning.", v.relative_path, reason, &v.submodule_oid[..8.min(v.submodule_oid.len())])
    }).collect();
    Err(format!("{UNPUSHED_SUBMODULE_OVERRIDABLE_PREFIX}{}", lines.join("\n")))
}

// Exposes the same list confirmPublish's advanced override dialog needs to
// show — path, referenced SHA, configured .gitmodules URL, reason, and (per
// the report) what happens if you proceed anyway — without requiring the
// frontend to first attempt (and fail) a real publish just to see it.
// upto_commit mirrors publish_branch's own parameter exactly, so the list
// shown here can never disagree with what publish_branch would actually
// check for the same call.
#[tauri::command]
pub async fn submodule_publish_risks(repository_path: String, branch: String, remote: String, upto_commit: String) -> Result<Vec<UnpushedSubmoduleReference>, String> {
    off_main_thread(move || submodule_publish_risks_inner(repository_path, branch, remote, upto_commit)).await
}

fn submodule_publish_risks_inner(repository_path: String, branch: String, remote: String, upto_commit: String) -> Result<Vec<UnpushedSubmoduleReference>, String> {
    validate_path(&repository_path)?;
    let branch = branch.trim(); let remote = remote.trim();
    if branch.is_empty() || remote.is_empty() { return Ok(Vec::new()); }
    let repo = internal_repository(&repository_path)?;
    let (upto, target) = if upto_commit.trim().is_empty() {
        let target = repo.refname_to_id(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
        (None, target)
    } else {
        let object = repo.revparse_single(upto_commit.trim()).map_err(|error| format!("Cannot resolve {upto_commit}: {}", error.message()))?;
        let commit = object.peel_to_commit().map_err(|error| error.message().to_string())?;
        (Some(commit.id()), commit.id())
    };
    let risks = unpushed_submodule_references(&repository_path, branch, remote, upto, true)?;
    // A hard "unpushed" result is deliberately not reusable: the common
    // next action is to push that submodule and retry from the still-open
    // dialog, in which case Publish must refresh it again.
    if !risks.iter().any(|risk| risk.risk == "unpushed") {
        remember_publish_preflight(&repository_path, branch, remote, target);
    }
    Ok(risks)
}

#[tauri::command]
pub async fn publish_branch(repository_path: String, branch: String, remote: String, username: String, access_token: String, upto_commit: String, override_unpushed_submodules: bool) -> Result<(), String> {
    off_main_thread(move || publish_branch_inner(repository_path, branch, remote, username, access_token, upto_commit, override_unpushed_submodules)).await
}

fn publish_branch_inner(repository_path: String, branch: String, remote: String, username: String, access_token: String, upto_commit: String, override_unpushed_submodules: bool) -> Result<(), String> {
    let total_started = Instant::now();
    perf_log("publish_branch: START", Duration::ZERO);
    let result = (|| {
    validate_path(&repository_path)?;
    if branch.trim().is_empty() || remote.trim().is_empty() { return Err("Choose a local branch and a remote".into()); }
    // See repo_write_lock's doc comment. No credentials in this log line —
    // just like every other one here, only the repo id and durations.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "publish_branch", queue_started.elapsed());
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
    // Point 3 of the submodule-publish-safety report: never just check the
    // current working-tree/index state — every outgoing commit up to
    // push_oid gets inspected (see unpushed_submodule_references's own doc
    // comment for why an older one matters too), and this runs before any
    // network push is attempted, whether or not an explicit token was given.
    let safety_started = Instant::now();
    submodule_publish_safety_check(&repository_path, branch, remote_name, Some(push_oid), override_unpushed_submodules)?;
    perf_log("publish_branch: safety preflight", safety_started.elapsed());
    let push_started = Instant::now();
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
    perf_log("publish_branch: network push", push_started.elapsed());
    let tracking_started = Instant::now();
    repo.reference(&format!("refs/remotes/{remote_name}/{branch}"), push_oid, true, "successful publish").map_err(|error| format!("Push succeeded, but local server tracking could not be updated: {}", error.message()))?;
    let mut config = repo.config().map_err(|error| error.message().to_string())?; config.set_str(&format!("branch.{branch}.remote"), remote_name).map_err(|error| error.message().to_string())?; config.set_str(&format!("branch.{branch}.merge"), &format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    perf_log("publish_branch: tracking + cache invalidation", tracking_started.elapsed());
    Ok(())
    })();
    match &result {
        Ok(()) => perf_log("publish_branch: TOTAL", total_started.elapsed()),
        Err(error) => perf_log(&format!("publish_branch: ERROR: {error}"), total_started.elapsed()),
    }
    result
}

#[tauri::command]
pub async fn submodule_repository(repository_path: String, relative_path: String) -> Result<RepositoryData, String> {
    off_main_thread(move || submodule_repository_inner(repository_path, relative_path)).await
}

fn submodule_repository_inner(repository_path: String, relative_path: String) -> Result<RepositoryData, String> {
    perf_log(&format!("submodule_repository: requested (parent={}, relative_path={relative_path})", anonymized_repository_id(&repository_path)), Duration::ZERO);
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    // load_repository_inner has no idea it's being asked for a submodule —
    // it just opens whatever path it was given — so it always leaves
    // submodule_url as None; only this caller actually knows the parent's
    // .gitmodules entry this resolved from, so it fills that in here.
    let (submodule_url, _) = submodule_url_and_branch(&repository_path, &relative_path);
    let mut result = load_repository_inner(absolute.to_string_lossy().into_owned(), None);
    if let Ok(data) = &mut result { data.repository.submodule_url = submodule_url; }
    match &result {
        Ok(data) => {
            // Verbose diagnostic for the "does the Submodule Branch Map
            // ever show the wrong submodule's history" report — enough to
            // confirm from the log alone, without a debugger, exactly which
            // repository/gitdir this call resolved to and what it returned:
            // a real cross-contamination bug would show two different
            // relative_path requests resolving to the same gitdir/HEAD/
            // commit OIDs; two genuinely different (if superficially
            // similar-looking) submodules would not.
            let first_three: Vec<String> = data.commits.iter().take(3).map(|c| format!("{}:{}", &c.id[..8.min(c.id.len())], c.subject)).collect();
            perf_log(&format!(
                "submodule_repository: resolved to {} (gitdir={}, branch={}, head={}, url={}, branches={}, commits={}, first_commits=[{}])",
                anonymized_repository_id(&data.repository.path), anonymized_repository_id(&data.repository.gitdir), data.repository.current_branch, &data.repository.head_oid[..8.min(data.repository.head_oid.len())],
                data.repository.submodule_url.as_deref().unwrap_or("(none)"), data.branches.len(), data.commits.len(), first_three.join(" | "),
            ), Duration::ZERO);
        }
        Err(error) => perf_log(&format!("submodule_repository: ERROR: {error}"), Duration::ZERO),
    }
    result
}

// Pure filesystem read — no Git calls of any kind, not even the cheap index
// read cached_index_metadata does, so this genuinely never blocks on
// anything Git-related. A real perf log caught the actual bug this exists
// to fix: openRepositoryFast's very first render called the *normal*
// openDirectory('') for the root folder, whose scope is the empty string —
// which internal_statuses treats as "no pathspec restriction", i.e. a full,
// UNSCOPED status scan (2.67-4.58s measured), completely defeating the
// point of open_repository_fast skipping that exact cost a moment earlier.
// This never touches status at all: kind is never reported as "submodule"
// here (that needs the index), just "folder" — briefly imprecise until the
// real reload moments later corrects it, an acceptable trade for a listing
// that's instant regardless of repository size. status_known is false
// throughout; the frontend shows a loading state instead of treating
// tracked/status as real answers.
#[derive(Serialize)]
pub struct SubmoduleNavigationStatus {
    // Parent-relative path of the submodule `relative_path` is inside — lets
    // the frontend recognize "still inside the same submodule" on later
    // navigation without asking the backend again on every click.
    submodule_path: String,
    // Whether a full, current status snapshot for that submodule is already
    // sitting in full_status_cache (seeded by submodule_folder_status below,
    // or by anything else that scanned this exact submodule recently) — an
    // index-only, no-Git-calls check.
    ready: bool,
}

// Cheap (index lookup + one cache-timestamp check, no Git process, no status
// scan) — the frontend calls this on navigating into a folder to decide
// whether it can go straight to the normal load_directory (submodule not
// involved, or already warm) or must show the filesystem-only listing first
// while a background scan warms this one specific submodule.
#[tauri::command]
pub async fn submodule_navigation_status(repository_path: String, relative_path: String) -> Result<Option<SubmoduleNavigationStatus>, String> {
    off_main_thread(move || submodule_navigation_status_inner(repository_path, relative_path)).await
}

fn submodule_navigation_status_inner(repository_path: String, relative_path: String) -> Result<Option<SubmoduleNavigationStatus>, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    let (_, submodules) = cached_index_metadata(&repository_path);
    let Some(submodule_path) = submodules.iter()
        .find(|sub| relative_string == sub.as_str() || relative_string.starts_with(&format!("{sub}/")))
        .cloned() else { return Ok(None) };
    let absolute_sub = Path::new(&repository_path).join(&submodule_path).to_string_lossy().into_owned();
    let ready = full_status_cache().lock().unwrap().get(&absolute_sub).map(|(cached_at, _)| cached_at.elapsed() < GIT_METADATA_TTL).unwrap_or(false);
    Ok(Some(SubmoduleNavigationStatus { submodule_path, ready }))
}

// The one read-only, single-flight full status scan of *one specific*
// submodule's own repository — never every submodule (a repository with
// hundreds of submodules must never pay for scanning ones the user hasn't
// even opened, especially not on Windows where each Git subprocess/libgit2
// call is comparatively more expensive). Reuses recent_full_statuses, the
// same single-flight + short reuse-window machinery refresh_status already
// relies on, so a second call landing while the first is still scanning
// blocks and reuses its result instead of starting a redundant scan of its
// own. Seeds full_status_cache (keyed by the submodule's own absolute path)
// — every subsequent load_directory/entry_details call for *any* folder or
// file inside this submodule then reuses that one snapshot via the existing
// worktree_status/cached_git_metadata reuse path, instead of each running
// its own scoped scan.
#[tauri::command]
pub async fn submodule_folder_status(repository_path: String, relative_path: String) -> Result<(), String> {
    off_main_thread(move || submodule_folder_status_inner(repository_path, relative_path)).await
}

fn submodule_folder_status_inner(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let (sub_path, _inner) = resolve_submodule_boundary(&repository_path, &relative_path)
        .ok_or("This path is not inside a submodule")?;
    let repo = internal_submodule_repository(Path::new(&sub_path))?;
    let statuses = recent_full_statuses(&repo, &sub_path)?;
    let mapped = statuses.into_iter().map(|(path, status, _)| (path, status)).collect();
    replace_git_metadata(&sub_path, mapped);
    Ok(())
}

#[tauri::command]
pub async fn list_directory_fast(repository_path: String, relative_path: String) -> Result<Vec<DirectoryEntry>, String> {
    off_main_thread(move || list_directory_fast_inner(repository_path, relative_path)).await
}

fn list_directory_fast_inner(repository_path: String, relative_path: String) -> Result<Vec<DirectoryEntry>, String> {
    let started = Instant::now();
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let absolute = Path::new(&repository_path).join(&relative);
    if !absolute.is_dir() { return Err("The selected path is not a folder".into()); }
    let mut entries = Vec::new();
    for item in fs::read_dir(&absolute).map_err(|error| error.to_string())? {
        let item = item.map_err(|error| error.to_string())?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" { continue; }
        let relative_string = normalized(&relative.join(&name));
        let metadata = item.metadata().map_err(|error| error.to_string())?;
        let kind = if metadata.file_type().is_symlink() { "symlink" } else if metadata.is_dir() { "folder" } else { "file" }.to_string();
        let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|value| value.as_secs()).unwrap_or(0);
        entries.push(DirectoryEntry { name, relative_path: relative_string, kind, status: String::new(), tracked: false, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, submodule_has_unpushed_commits: false, submodule_is_dirty: false, submodule_state: String::new(), submodule_checked: false, submodule_initialized: false, submodule_current_branch: None, unpushed: false, status_known: false, stashed: false });
    }
    entries.sort_by_cached_key(|entry| (!matches!(entry.kind.as_str(), "folder" | "submodule"), entry.name.to_lowercase()));
    perf_log(&format!("list_directory_fast: TOTAL ({} entries, {relative_path})", entries.len()), started.elapsed());
    Ok(entries)
}

#[tauri::command]
pub async fn load_directory(repository_path: String, relative_path: String, force: Option<bool>) -> Result<Vec<DirectoryEntry>, String> {
    off_main_thread(move || load_directory_inner(repository_path, relative_path, force)).await
}

fn load_directory_inner(repository_path: String, relative_path: String, force: Option<bool>) -> Result<Vec<DirectoryEntry>, String> {
    let load_started = Instant::now();
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
    // The 300s TTL below is tuned for ordinary navigation, where a moment's
    // staleness is an acceptable trade for not re-scanning on every folder
    // click. An explicit refresh click is a different signal — the user just
    // told us they expect to see whatever changed on disk *right now* (e.g.
    // files edited by another program), so it must bypass that cache instead
    // of silently serving up to 5-minute-old data back at them.
    if force.unwrap_or(false) {
        invalidate_git_metadata(status_repo);
        // invalidate_git_metadata deliberately leaves submodule_state_cache
        // alone (an ordinary stage/commit elsewhere has nothing to do with
        // any submodule's own push status) — but an explicit forced reload
        // is the one signal that *should* also bypass it, same reasoning as
        // everything else force does here: the user asked for the truth
        // right now, not a cached answer from up to 5 minutes ago. Scoped to
        // just the submodules under this folder's own repository, not every
        // submodule cached anywhere.
        let prefix = format!("{status_repo}/");
        submodule_state_cache().lock().unwrap().retain(|key, _| key != status_repo && !key.starts_with(&prefix));
    }
    let step = Instant::now();
    let git_metadata = cached_git_metadata(status_repo, status_scope);
    perf_log(&format!("load_directory: cached_git_metadata ({relative_path})"), step.elapsed());
    // "Does this folder contain any tracked/unpushed file?" used to be a
    // linear scan over *every* tracked path in the whole repository, for
    // *every* entry in the folder being listed — O(entries × total tracked
    // files). On a large monorepo (hundreds of thousands of tracked files)
    // that made opening a folder with many items dramatically slower than it
    // needed to be, worse still on Windows where each string comparison in
    // that scan is itself typically a bit slower. Sorting turns "does
    // anything start with this prefix" into a binary search (O(log N))
    // instead of a full scan — and since tracked/unpushed don't depend on
    // which folder is open, the sorted lists themselves are now built once
    // per repository (cached_sorted_lookups) and reused across every folder
    // navigated to, instead of collecting-and-sorting a fresh copy on every
    // single call regardless of whether anything actually changed.
    let sorted_lookups = cached_sorted_lookups(status_repo);
    let tracked_sorted = &sorted_lookups.tracked;
    let unpushed_sorted = &sorted_lookups.unpushed;
    // One repository handle serves both stash inspection and the submodule
    // gitlink comparisons below. Opening it twice on every folder click is
    // unnecessary filesystem work, especially on Windows/network drives.
    let mut status_repository = internal_repository(status_repo).ok();
    let stashed_paths = status_repository.as_mut()
        .map(|repo| stash::all_stashed_paths(status_repo, repo))
        .transpose()
        .unwrap_or_else(|error| {
            perf_log(&format!("load_directory: could not inspect stashes ({error})"), Duration::ZERO);
            None
        })
        .unwrap_or_else(|| Arc::new(HashSet::new()));
    let mut stashed_sorted: Vec<String> = stashed_paths.iter().cloned().collect();
    stashed_sorted.sort_unstable();
    let has_prefix = has_sorted_prefix;
    // Same fix, same reason, for the third and last O(entries × something)
    // scan in this loop: status_for did up to two linear scans through every
    // *changed* file for every entry being listed. Usually small, but not
    // when there's a large uncommitted change (a big refactor, an unstaged
    // submodule bump touching many files) — sorted once, it's a binary
    // search here too.
    let mut statuses_sorted: Vec<&(String, String)> = git_metadata.statuses.iter().collect();
    statuses_sorted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let status_for_entry = |key: &str| -> String {
        if let Ok(idx) = statuses_sorted.binary_search_by(|(path, _)| path.as_str().cmp(key)) { return statuses_sorted[idx].1.clone(); }
        let prefix = format!("{key}/");
        let idx = statuses_sorted.partition_point(|(path, _)| path.as_str() < prefix.as_str());
        if statuses_sorted.get(idx).map(|(path, _)| path.starts_with(&prefix)).unwrap_or(false) { "•".to_string() } else { String::new() }
    };
    let mut entries = Vec::new();
    let mut names_on_disk = HashSet::new();
    // Open the owning repository once for all visible submodule rows. Gitlink
    // comparisons below are cheap index/tree lookups and must not rediscover
    // the parent repository once per row.
    let step = Instant::now();
    let mut submodule_count = 0usize;

    for item in fs::read_dir(&absolute).map_err(|error| error.to_string())? {
        let item = item.map_err(|error| error.to_string())?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" { continue; }
        names_on_disk.insert(name.clone());
        let relative_string = normalized(&relative.join(&name));
        let status_key = if boundary.is_some() { normalized(&Path::new(status_scope).join(&name)) } else { relative_string.clone() };
        // `entry.metadata()` (not `fs::symlink_metadata(entry.path())`) —
        // on Windows the directory enumeration itself (FindNextFile) already
        // returns each entry's basic attributes, so DirEntry::metadata()
        // is free; a separate fs::symlink_metadata call forces one full
        // extra per-file system call that Windows is, per Rust's own docs,
        // specifically slower at than macOS/Linux. For a folder with many
        // items that redundant stat, doubled for every single entry, was a
        // real and avoidable cost.
        let metadata = item.metadata().map_err(|error| error.to_string())?;
        let kind = if git_metadata.submodules.contains(&status_key) { "submodule" }
            else if metadata.file_type().is_symlink() { "symlink" }
            else if metadata.is_dir() { "folder" }
            else { "file" }.to_string();
        let tracked_prefix = format!("{status_key}/");
        let tracked = git_metadata.submodules.contains(&status_key) || git_metadata.tracked.contains(&status_key) || has_prefix(tracked_sorted, &tracked_prefix);
        let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|value| value.as_secs()).unwrap_or(0);
        if kind == "submodule" { submodule_count += 1; }
        let status = status_for_entry(&status_key);
        let unpushed = if kind == "folder" { git_metadata.unpushed.contains(&status_key) || has_prefix(unpushed_sorted, &tracked_prefix) } else { git_metadata.unpushed.contains(&status_key) };
        let stashed = stashed_paths.contains(&status_key) || has_prefix(&stashed_sorted, &tracked_prefix);
        // A clean, fully synchronized submodule needs no sub-repository scan.
        // Only a row whose parent status/unpushed information says there is
        // something to explain gets one cached, consolidated inspection.
        let submodule_snapshot = (kind == "submodule" && (!status.is_empty() || unpushed))
            .then(|| {
                let sub_path = item.path().to_string_lossy().into_owned();
                // If the parent status already reports the gitlink/worktree as
                // changed, the submodule's own cached snapshot can be stale
                // after an external editor touched files inside it. In that
                // case the expensive signal has already happened (this visible
                // submodule row is changed), so pay for one fresh submodule
                // inspection and keep the row truthful: "Changes inside" must
                // outrank any older "project push" / "synced" cached answer.
                if status.is_empty() { cached_submodule_state(&sub_path) } else { fresh_submodule_state(&sub_path) }
            });
        let submodule_has_unpushed_commits = submodule_snapshot.as_ref().is_some_and(|snapshot| snapshot.remote_relation == SubmoduleRemoteRelation::PushNeeded);
        let submodule_is_dirty = submodule_snapshot.as_ref().is_some_and(|snapshot| snapshot.dirty);
        let submodule_state = match (&status_repository, &submodule_snapshot) {
            (Some(parent), Some(snapshot)) => submodule_workflow_state(parent, &status_key, unpushed, snapshot),
            _ if kind == "submodule" && unpushed => "project_commit_push_needed".into(),
            _ if kind == "submodule" => "synced".into(),
            _ => String::new(),
        };
        let submodule_checked = submodule_snapshot.is_some();
        let submodule_initialized = kind == "submodule" && item.path().join(".git").exists();
        let submodule_current_branch = submodule_snapshot.as_ref().and_then(|snapshot| snapshot.attached_branch.clone());
        entries.push(DirectoryEntry { name, relative_path: relative_string, kind, status, tracked, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, submodule_has_unpushed_commits, submodule_is_dirty, submodule_state, submodule_checked, submodule_initialized, submodule_current_branch, unpushed, status_known: true, stashed });
    }

    // A deleted tracked path cannot be returned by read_dir: it is absent on
    // disk by definition. Previously its parent folder correctly received a
    // "modified files inside" marker from Git, but opening that folder showed
    // only the surviving (clean) files, making the marker look false. Add a
    // lightweight synthetic row for each missing direct child reported as D.
    // A whole deleted subtree is represented once by its first missing path
    // component; drilling into an existing ancestor will still expose the
    // exact deleted file at the first level where it is absent. No filesystem
    // scan or Git command is added here — this reuses the status result already
    // loaded for this directory.
    let scope_prefix = if status_scope.is_empty() { String::new() } else { format!("{status_scope}/") };
    let mut missing_names = HashSet::new();
    for (changed_path, code) in &git_metadata.statuses {
        if code != "D" { continue; }
        let scoped_path = if scope_prefix.is_empty() {
            changed_path.as_str()
        } else if let Some(value) = changed_path.strip_prefix(&scope_prefix) {
            value
        } else {
            continue;
        };
        let Some(name) = scoped_path.split('/').next().filter(|name| !name.is_empty()) else { continue; };
        if names_on_disk.contains(name) || !missing_names.insert(name.to_string()) { continue; }
        let direct_status_key = if status_scope.is_empty() { name.to_string() } else { normalized(&Path::new(status_scope).join(name)) };
        let missing_kind = if !scoped_path.contains('/') && git_metadata.submodules.contains(&direct_status_key) {
            "deleted-submodule"
        } else if scoped_path.contains('/') {
            "deleted-folder"
        } else {
            "deleted"
        };
        entries.push(DirectoryEntry {
            name: name.to_string(),
            relative_path: normalized(&relative.join(name)),
            kind: missing_kind.into(),
            status: "D".into(),
            tracked: true,
            size: 0,
            modified: 0,
            submodule_has_unpushed_commits: false,
            submodule_is_dirty: false,
            submodule_state: String::new(),
            submodule_checked: false,
            submodule_initialized: false,
            submodule_current_branch: None,
            unpushed: false,
            status_known: true,
            stashed: stashed_paths.contains(&direct_status_key) || has_prefix(&stashed_sorted, &format!("{direct_status_key}/")),
        });
    }
    perf_log(&format!("load_directory: readdir loop ({} entries, {submodule_count} submodules)", entries.len()), step.elapsed());
    let step = Instant::now();
    // sort_by_cached_key computes each entry's sort key exactly once (O(n)
    // lowercase allocations total) instead of `sort_by` with `.to_lowercase()`
    // inside the comparator, which re-allocates two new Strings on *every*
    // comparison the sort makes — O(n log n) allocations. For a folder with
    // 20,000 direct entries that's the difference between ~20,000 and
    // ~570,000 allocations just to sort the listing.
    entries.sort_by_cached_key(|entry| (!matches!(entry.kind.as_str(), "folder" | "submodule" | "deleted-folder" | "deleted-submodule"), entry.name.to_lowercase()));
    perf_log(&format!("load_directory: sort ({} entries)", entries.len()), step.elapsed());
    perf_log(&format!("load_directory: TOTAL ({relative_path})"), load_started.elapsed());
    Ok(entries)
}

#[tauri::command]
pub async fn entry_details(repository_path: String, relative_path: String) -> Result<EntryDetails, String> {
    off_main_thread(move || entry_details_inner(repository_path, relative_path)).await
}

fn entry_details_inner(repository_path: String, relative_path: String) -> Result<EntryDetails, String> {
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
    // Same fix as load_directory: these were linear scans over the *entire*
    // tracked/unpushed sets on every single selection, not just every
    // navigation — the pre-sorted, once-per-repository lookup turns each into
    // a binary search instead.
    let sorted_lookups = cached_sorted_lookups(status_repo);
    let tracked = git_metadata.tracked.contains(status_scope) || has_sorted_prefix(&sorted_lookups.tracked, &prefix);
    let status = status_for(status_scope, &git_metadata.statuses);
    let unpushed = if kind == "folder" { git_metadata.unpushed.contains(status_scope) || has_sorted_prefix(&sorted_lookups.unpushed, &prefix) } else { git_metadata.unpushed.contains(status_scope) };
    let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|value| value.as_secs()).unwrap_or(0);
    let item_count = metadata.is_dir().then(|| fs::read_dir(&absolute).map(|items| items.count()).unwrap_or(0));

    // "Last commit touching this path" walks history diffing every commit
    // against its parent until it finds a match — up to 2000 commits, each a
    // real tree-diff computation. On a large monorepo that's genuinely heavy
    // (this is the same cost `git log -- <path>` has), and it used to run
    // inline on every single selection click, making clicking around a
    // large, rarely-touched folder feel badly stuck. Left as None here —
    // entry_last_commit below computes it as a separate, later call the
    // frontend fires only after the rest of the (fast) details are already
    // showing, instead of blocking on it up front.
    let last: Option<(String, String, String, String)> = None;
    // For a submodule entry, this used to independently open/discover its
    // repository up to 5 times (url, branch, push status, unpushed commits,
    // HEAD commit) plus re-scan the parent's .gitmodules twice (url, branch)
    // — each of those was real, avoidable disk I/O for what's logically one
    // "describe this submodule" read. Open the submodule's own repo once
    // here and share it; the parent-side url/branch lookup is now a single
    // scan too (submodule_url_and_branch), not two.
    let (submodule_url, submodule_web_url, submodule_branch, submodule_push_status, submodule_unpushed_commits, submodule_commit, submodule_commit_tags, submodule_snapshot) = if kind == "submodule" {
        let (url, branch) = submodule_url_and_branch(&repository_path, &relative_string);
        let web_url = submodule_browser_base(&repository_path, &relative_string).ok();
        match internal_submodule_repository(&absolute) {
            Ok(sub_repo) => {
                let snapshot = inspect_submodule_state_in(&sub_repo);
                let push_status = submodule_push_status_in(&sub_repo);
                let unpushed_commits = submodule_unpushed_commits_in(&sub_repo);
                let commit = sub_repo.head().ok().and_then(|head| head.peel_to_commit().ok()).map(|commit| {
                    let author_name = commit.author().name().unwrap_or("Unknown").to_string();
                    (commit.id().to_string(), commit.summary().unwrap_or("No message").to_string(), author_name, short_date(commit.time().seconds()))
                });
                let commit_tags = sub_repo.head().ok().and_then(|head| head.target()).map(|oid| tags_pointing_at_commit(&sub_repo, oid)).unwrap_or_default();
                (url, web_url, branch, push_status, unpushed_commits, commit, commit_tags, Some(snapshot))
            }
            Err(_) => (url, web_url, branch, None, Vec::new(), None, Vec::new(), None),
        }
    } else { (None, None, None, None, Vec::new(), None, Vec::new(), None) };
    let submodule_is_dirty = submodule_snapshot.as_ref().is_some_and(|snapshot| snapshot.dirty);
    let submodule_initialized = kind == "submodule" && submodule_snapshot.is_some();
    let submodule_current_branch = submodule_snapshot.as_ref().and_then(|snapshot| snapshot.attached_branch.clone());
    let submodule_state = if kind == "submodule" {
        match (internal_repository(status_repo).ok(), submodule_snapshot.as_ref()) {
            (Some(parent), Some(snapshot)) => submodule_workflow_state(&parent, status_scope, unpushed, snapshot),
            _ => "unavailable".into(),
        }
    } else { String::new() };

    Ok(EntryDetails {
        name: absolute.file_name().and_then(|name| name.to_str()).unwrap_or(&relative_string).to_string(), relative_path: relative_string,
        kind, status, tracked, unpushed, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, item_count, submodule_url, submodule_web_url, submodule_branch, submodule_push_status, submodule_unpushed_commits, submodule_is_dirty, submodule_state, submodule_initialized, submodule_current_branch,
        last_commit_id: last.as_ref().map(|value| value.0.clone()),
        last_commit_subject: last.as_ref().map(|value| value.1.clone()), last_commit_author: last.as_ref().map(|value| value.2.clone()), last_commit_date: last.as_ref().map(|value| value.3.clone()),
        submodule_commit_id: submodule_commit.as_ref().map(|value| value.0.clone()),
        submodule_commit_subject: submodule_commit.as_ref().map(|value| value.1.clone()), submodule_commit_author: submodule_commit.as_ref().map(|value| value.2.clone()), submodule_commit_date: submodule_commit.as_ref().map(|value| value.3.clone()),
        submodule_commit_tags,
    })
}

// The "last commit touching this path" lookup entry_details deliberately no
// longer does inline — a separate, later call the frontend fires only after
// the fast details above are already on screen, so selecting item after
// item in a large folder doesn't sit blocked on a potentially-heavy history
// walk on every single click.
#[tauri::command]
pub async fn entry_last_commit(repository_path: String, relative_path: String) -> Result<Option<PublishCommit>, String> {
    off_main_thread(move || entry_last_commit_inner(repository_path, relative_path)).await
}

fn entry_last_commit_inner(repository_path: String, relative_path: String) -> Result<Option<PublishCommit>, String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    let boundary = resolve_submodule_boundary(&repository_path, &relative_string).filter(|(_, inner)| !inner.is_empty());
    let last = match &boundary {
        Some((sub_path, inner_relative)) => last_commit_touching_path(sub_path, Path::new(inner_relative)),
        None => last_commit_touching_path(&repository_path, &relative),
    };
    Ok(last.map(|(id, subject, author, date)| PublishCommit { id, subject, author, date }))
}

// Uses only already-known local refs (no fetch) so it's cheap enough to call every
// time a submodule is selected. Tells the user, at a glance, whether the commit
// they're looking at has actually reached the submodule's own remote yet.
#[cfg(test)]
fn submodule_push_status(sub_path: &str) -> Option<String> {
    submodule_push_status_in(&internal_submodule_repository(Path::new(sub_path)).ok()?)
}

// Same check, taking an already-open Repository — lets a caller that needs
// several pieces of a submodule's state at once (entry_details opens the
// submodule's repo for `submodule_commit` right next to this) share the one
// `internal_repository`/`Repository::discover` instead of repeating it.
fn submodule_push_status_in(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    let local_target = head.target()?;
    if repo.head_detached().unwrap_or(true) { return Some("Detached — not on a branch, so it cannot be pushed as-is. Use \"Change version\" to switch to a branch first.".into()); }
    let branch = head.shorthand()?.to_string();
    drop(head);
    // The branch's real upstream (see upstream_ref's own doc comment) — not a
    // hardcoded assumption that it's origin/<same name>. No configured
    // upstream at all reads as "can't tell", same as it always has.
    let (remote_target, upstream_label) = upstream_ref(repo, &branch)?;
    if remote_target == local_target { return None; }
    let mut walk = repo.revwalk().ok()?; walk.push(local_target).ok()?; let _ = walk.hide(remote_target);
    let ahead = walk.take(50).count();
    Some(if ahead > 0 { format!("{ahead} commit{} not yet pushed to {upstream_label} — needs push", if ahead == 1 { "" } else { "s" }) } else { format!("Diverged from {upstream_label}") })
}

// The actual commits behind `submodule_push_status`'s summary line — showing
// "3 commits not pushed" as a sentence and making the user guess which three
// isn't good enough; list them the same way the main project's "Unpublished
// commits" dialog already does.
// Path-based wrapper around `submodule_unpushed_commits_in` — production code
// now always already has the submodule's `Repository` open for something else
// by the time it needs this (entry_details shares it with submodule_push_status
// and the HEAD commit lookup) and calls the `_in` version directly instead of
// re-opening the repo here; kept for tests, which exercise it standalone.
#[cfg(test)]
fn submodule_unpushed_commits(sub_path: &str) -> Vec<PublishCommit> {
    match internal_submodule_repository(Path::new(sub_path)) { Ok(repo) => submodule_unpushed_commits_in(&repo), Err(_) => Vec::new() }
}

fn submodule_unpushed_commits_in(repo: &Repository) -> Vec<PublishCommit> {
    (|| -> Option<Vec<PublishCommit>> {
        let head = repo.head().ok()?;
        let local_target = head.target()?;
        if repo.head_detached().unwrap_or(true) { return Some(Vec::new()); }
        let branch = head.shorthand()?.to_string();
        drop(head);
        let (remote_target, _) = upstream_ref(repo, &branch)?;
        if remote_target == local_target { return Some(Vec::new()); }
        let mut walk = repo.revwalk().ok()?; walk.push(local_target).ok()?; let _ = walk.hide(remote_target);
        let mut commits: Vec<PublishCommit> = walk.take(50).flatten().filter_map(|oid| repo.find_commit(oid).ok().map(|commit| PublishCommit {
            id: oid.to_string(), subject: commit.summary().unwrap_or("No message").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()),
        })).collect();
        commits.reverse();
        Some(commits)
    })().unwrap_or_default()
}

// The shared gate essentially every submodule-targeting command calls
// first (Submodule Branch Map, commit/push/pull/reset/switch-version,
// change URL, new branch...) — using the strict opener here, and only here,
// protects all of them at once: each of those returns immediately via `?`
// on a validate_submodule error, so a submodule with missing/broken .git
// never reaches whatever `internal_repository` call it might otherwise have
// made next.
fn validate_submodule(repository_path: &str, relative_path: &str) -> Result<PathBuf, String> {
    let relative = safe_relative_path(relative_path)?;
    let normalized_path = normalized(&relative);
    if !cached_index_metadata(repository_path).1.contains(&normalized_path) { return Err("The selected folder is not a Git submodule".into()); }
    let absolute = Path::new(repository_path).join(relative);
    if !absolute.is_dir() { return Err("The submodule is not initialized".into()); }
    internal_submodule_repository(&absolute)?;
    Ok(absolute)
}

// `git submodule update --init` clones/fetches over the network — must not run
// on the webview UI thread.
#[tauri::command]
pub async fn init_submodule(repository_path: String, relative_path: String) -> Result<(), String> {
    off_main_thread(move || init_submodule_inner(repository_path, relative_path)).await
}

fn init_submodule_inner(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let normalized_path = normalized(&relative);
    if !cached_index_metadata(&repository_path).1.contains(&normalized_path) {
        return Err("The selected folder is not a Git submodule".into());
    }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "init_submodule", queue_started.elapsed());
    let step = Instant::now();
    let result = git(&repository_path, &["-c", "protocol.file.allow=always", "submodule", "update", "--init", "--recursive", "--", &normalized_path]);
    perf_log(&format!("init_submodule: update {normalized_path} {}", if result.is_ok() { "ok" } else { "ERROR" }), step.elapsed());
    result?;
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path);
    Ok(())
}

// Which of the known branch/remote tips contain `target`, closest tip first.
// Reused both for "current_containing_branches" (target = the active
// checkout) and for each row in the bounded commit-history list below
// (target = that row's own commit) — same question, just asked about a
// different commit each time. graph_ahead_behind is a pure in-memory graph
// walk over commits already loaded for this repository, not filesystem I/O,
// so repeating it once per (history commit × known tip) stays cheap even for
// the full 100-commit history window this is bounded to.
fn branches_containing_commit(repo: &Repository, tips: &[(String, git2::Oid, bool)], target: git2::Oid) -> Vec<String> {
    let mut containing: Vec<(&str, bool, usize)> = tips.iter().filter_map(|(name, tip, is_local)| {
        let (ahead, behind) = repo.graph_ahead_behind(*tip, target).ok()?;
        (behind == 0).then_some((name.as_str(), *is_local, ahead))
    }).collect();
    // Prefer the closest containing tip. At equal distance, a local branch is
    // more useful than its remote-tracking duplicate because the user can
    // attach HEAD to it directly.
    containing.sort_by(|left, right| (left.2, !left.1, left.0).cmp(&(right.2, !right.1, right.0)));
    containing.into_iter().map(|(name, _, _)| name.to_string()).collect()
}

#[tauri::command]
pub async fn submodule_versions(repository_path: String, relative_path: String) -> Result<SubmoduleVersions, String> {
    off_main_thread(move || submodule_versions_inner(repository_path, relative_path)).await
}

fn submodule_versions_inner(repository_path: String, relative_path: String) -> Result<SubmoduleVersions, String> {
    const HISTORY_LIMIT: usize = 100;
    let started = Instant::now();
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let parent = internal_repository(&repository_path)?;
    let parent_revision = parent.index().ok().and_then(|index| index.get_path(&relative, 0)).map(|entry| entry.id.to_string()).unwrap_or_default();
    let repo = internal_submodule_repository(&absolute)?;
    let current_revision = repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    // `Reference::shorthand()` returns the literal string "HEAD" for a
    // detached checkout.  That is not a branch name and made the version
    // dialog look as though the current detached commit belonged to the
    // branch rows shown below it.  Keep the same contract as RepositoryInfo:
    // an empty branch means detached, while current_revision is the exact
    // commit that is checked out.
    let current_branch = if repo.head_detached().unwrap_or(true) {
        String::new()
    } else {
        repo.head().ok().and_then(|head| head.shorthand().map(String::from)).unwrap_or_default()
    };
    let current_oid = git2::Oid::from_str(&current_revision).ok();
    let mut versions = Vec::new();
    // Local branch tips, indexed by the commit they currently point at — used
    // below to report which branch (if any) is "attached" to a given tag.
    let mut branch_tip_names: HashMap<String, String> = HashMap::new();
    // name, tip, is-local, number of commits from current checkout to tip.
    // The last value is None when this branch does not contain current HEAD.
    let mut known_branch_tips: Vec<(String, git2::Oid, bool, Option<usize>)> = Vec::new();
    for branch_type in [BranchType::Local, BranchType::Remote] {
        let Ok(iterator) = repo.branches(Some(branch_type)) else { continue };
        for item in iterator.flatten() {
            let name = item.0.name().ok().flatten().unwrap_or("").to_string();
            if name.ends_with("/HEAD") { continue; }
            let Some(oid) = item.0.get().target() else { continue };
            let commits_after_current = current_oid.and_then(|current| {
                let (ahead, behind) = repo.graph_ahead_behind(oid, current).ok()?;
                (behind == 0).then_some(ahead)
            });
            let contains_current = commits_after_current.is_some();
            known_branch_tips.push((name.clone(), oid, branch_type == BranchType::Local, commits_after_current));
            if branch_type == BranchType::Local { branch_tip_names.entry(oid.to_string()).or_insert_with(|| name.clone()); }
            let Ok(commit) = repo.find_commit(oid) else { continue };
            let kind = if branch_type == BranchType::Local { "branch" } else { "remote" };
            // Point 3: an upstream (and how far ahead/behind it) is a
            // property of a *local* branch only — never populated for a
            // remote-tracking entry itself.
            let (upstream, ahead, behind) = if branch_type == BranchType::Local {
                item.0.upstream().ok().and_then(|upstream| {
                    let upstream_oid = upstream.get().target()?;
                    let label = upstream.get().shorthand()?.to_string();
                    let (ahead, behind) = repo.graph_ahead_behind(oid, upstream_oid).unwrap_or((0, 0));
                    Some((Some(label), Some(ahead), Some(behind)))
                }).unwrap_or((None, None, None))
            } else { (None, None, None) };
            versions.push(SubmoduleVersion {
                name: name.clone(), revision: oid.to_string(), kind: kind.into(), current: kind == "branch" && name == current_branch,
                subject: commit.summary().unwrap_or("").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()),
                attached_branch: None, upstream, ahead, behind, contains_current, commits_after_current, containing_branches: Vec::new(),
            });
        }
    }
    if let Ok(tag_names) = repo.tag_names(None) {
        for name in tag_names.iter().flatten() {
            let reference = match repo.find_reference(&format!("refs/tags/{name}")) { Ok(reference) => reference, Err(_) => continue };
            let target = match reference.target() { Some(target) => target, None => continue };
            let object = match repo.find_object(target, None) { Ok(object) => object, Err(_) => continue };
            let commit = match object.peel_to_commit() { Ok(commit) => commit, Err(_) => continue };
            versions.push(SubmoduleVersion {
                name: name.to_string(), revision: commit.id().to_string(), kind: "tag".into(),
                current: commit.id().to_string() == current_revision,
                subject: commit.summary().unwrap_or("").into(), author: commit.author().name().unwrap_or("Unknown").into(),
                date: short_date(commit.time().seconds()), attached_branch: branch_tip_names.get(&commit.id().to_string()).cloned(),
                upstream: None, ahead: None, behind: None, contains_current: false, commits_after_current: None, containing_branches: Vec::new(),
            });
        }
    }
    let branch_tips_simple: Vec<(String, git2::Oid, bool)> = known_branch_tips.iter().map(|(name, oid, is_local, _)| (name.clone(), *oid, *is_local)).collect();
    let current_containing_branches = current_oid.map(|oid| branches_containing_commit(&repo, &branch_tips_simple, oid)).unwrap_or_default();
    let history_context_branch = if current_branch.is_empty() { current_containing_branches.first().cloned().unwrap_or_default() } else { current_branch.clone() };
    let history_start = known_branch_tips.iter().find(|(name, _, _, _)| name == &history_context_branch).map(|(_, oid, _, _)| *oid).or(current_oid);
    let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; let _ = walk.set_sorting(Sort::TIME); if let Some(oid) = history_start { let _ = walk.push(oid); }
    for oid in walk.flatten().take(HISTORY_LIMIT) {
        if let Ok(commit) = repo.find_commit(oid) {
            let containing_branches = branches_containing_commit(&repo, &branch_tips_simple, oid);
            versions.push(SubmoduleVersion { name: oid.to_string()[..8].into(), revision: oid.to_string(), kind: "commit".into(), current: oid.to_string() == current_revision, subject: commit.summary().unwrap_or("").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()), attached_branch: None, upstream: None, ahead: None, behind: None, contains_current: false, commits_after_current: None, containing_branches });
        }
    }
    perf_log(&format!("submodule_versions: TOTAL ({} refs/commits, {} containing branches)", versions.len(), current_containing_branches.len()), started.elapsed());
    Ok(SubmoduleVersions { path: relative_path, current_revision, current_branch, parent_revision, current_containing_branches, history_context_branch, history_limit: HISTORY_LIMIT, versions })
}

#[tauri::command]
pub async fn add_submodule(repository_path: String, parent_path: String, url: String, folder_name: String, username: String, access_token: String) -> Result<String, String> {
    off_main_thread(move || add_submodule_inner(repository_path, parent_path, url, folder_name, username, access_token)).await
}

fn add_submodule_inner(repository_path: String, parent_path: String, url: String, folder_name: String, username: String, access_token: String) -> Result<String, String> {
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

    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "add_submodule", queue_started.elapsed());
    let mut repo = internal_repository(&repository_path)?;
    let configured_url = portable_submodule_configured_url(url);
    let clone_url = resolved_submodule_io_url(&repo, &configured_url)?;
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
    let mut submodule = repo.submodule(&clone_url, &relative, true)
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
    drop(submodule);
    // All clone/finalize failures above use the shared rollback closure.
    // Failures from this point perform the same cleanup inline.
    if let Err(error) = repo.submodule_set_url(&name, &configured_url) {
        let _ = cleanup_submodule_registration(&repo, &name, &relative);
        if destination.is_dir() { let _ = fs::remove_dir_all(&destination); }
        let storage = repo.path().join("modules").join(&relative); if storage.is_dir() { let _ = fs::remove_dir_all(storage); }
        return Err(format!("Cannot save the portable submodule URL: {}", error.message()));
    }
    if let Ok(mut index) = repo.index() { if let Err(error) = index.add_path(Path::new(".gitmodules")).and_then(|_| index.write()) {
        let _ = cleanup_submodule_registration(&repo, &name, &relative);
        if destination.is_dir() { let _ = fs::remove_dir_all(&destination); }
        let storage = repo.path().join("modules").join(&relative); if storage.is_dir() { let _ = fs::remove_dir_all(storage); }
        return Err(format!("Cannot stage .gitmodules: {}", error.message()));
    } }
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path); // this app just changed a submodule's registration/version
    Ok(relative_string)
}

#[tauri::command]
pub async fn switch_submodule_version(repository_path: String, relative_path: String, revision: String, version_kind: String, name: String) -> Result<String, String> {
    off_main_thread(move || switch_submodule_version_inner(repository_path, relative_path, revision, version_kind, name)).await
}

fn switch_submodule_version_inner(repository_path: String, relative_path: String, revision: String, version_kind: String, name: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    // Preserve the user's existing staging intent. A freshly-added submodule
    // is already staged by `git submodule add`; if Change version moves its
    // checkout afterwards, leaving the old gitlink in the index creates the
    // confusing two-commit workflow (commit A, then discover B as Modified).
    // An ordinary, previously-unstaged submodule switch must remain unstaged.
    let parent = internal_repository(&repository_path)?;
    let was_parent_gitlink_staged = parent_gitlink_oid(&parent, &relative_path, true).is_some()
        && parent_gitlink_oid(&parent, &relative_path, true) != parent_gitlink_oid(&parent, &relative_path, false);
    drop(parent);
    let absolute_string = absolute.to_string_lossy().into_owned();
    // The submodule's own lock, held only for the checkout below. This
    // function never touches the parent's index (see the comment after the
    // checkout for why), so there is no second, parent-side lock to worry
    // about ordering against.
    let queue_started = Instant::now();
    let sub_lock_handle = repo_write_lock(&absolute_string);
    let sub_lock = sub_lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&absolute_string, "switch_submodule_version(submodule)", queue_started.elapsed());
    let repo = internal_submodule_repository(&absolute)?;
    // Resolve the destination first, but do not move HEAD yet. Moving HEAD
    // before checkout makes the old index/worktree look like staged reverse
    // changes against the new branch (most visibly after "Restore parent
    // version" followed by selecting the old local branch). Git's checkout
    // order is the opposite: safely update the tree/index while HEAD still
    // describes their current baseline, then attach/detach HEAD only after
    // that checkout succeeds.
    let (target_oid, attach_reference, create_local_branch, configure_upstream) = if version_kind == "branch" {
        // `name` is the actual branch name (e.g. "main"); `revision` is only the SHA
        // it currently points at and is NOT a valid ref on its own — using it here
        // produced "reference 'refs/heads/<sha>' not found" for every local branch.
        let branch_name = if name.is_empty() { revision.clone() } else { name.clone() };
        let reference = format!("refs/heads/{branch_name}");
        let target = repo.find_reference(&reference).map_err(|error| format!("Branch '{branch_name}' not found: {}", error.message()))?
            .peel_to_commit().map_err(|error| error.message().to_string())?.id();
        (target, Some(reference), None, None)
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
        let target = commit.id();
        let local_name = name.split_once('/').map(|(_, rest)| rest).unwrap_or(&name);
        match repo.find_branch(local_name, BranchType::Local) {
            Ok(existing) if existing.get().target() == Some(commit.id()) => {
                let needs_upstream = existing.upstream().is_err();
                (target, Some(format!("refs/heads/{local_name}")), None, needs_upstream.then(|| (local_name.to_string(), name.clone())))
            }
            Ok(_) => (target, None, None, None),
            Err(_) => {
                (target, Some(format!("refs/heads/{local_name}")), Some(local_name.to_string()), Some((local_name.to_string(), name.clone())))
            }
        }
    } else if version_kind == "tag" {
        // `revision` here is already the commit the tag points at (resolved by
        // submodule_versions, which peels annotated tags to their commit), but
        // resolve by tag name and peel again defensively in case a caller passes
        // the tag name as `revision` instead.
        let tag_name = if name.is_empty() { revision.clone() } else { name.clone() };
        let object = repo.find_reference(&format!("refs/tags/{tag_name}")).and_then(|reference| reference.peel(git2::ObjectType::Commit))
            .or_else(|_| repo.revparse_single(&revision).and_then(|object| object.peel(git2::ObjectType::Commit)))
            .map_err(|error| error.message().to_string())?;
        (object.id(), None, None, None)
    } else {
        let object = repo.revparse_single(&revision).map_err(|error| error.message().to_string())?;
        let commit = object.peel_to_commit().map_err(|error| error.message().to_string())?;
        (commit.id(), None, None, None)
    };
    let target = repo.find_object(target_oid, Some(ObjectType::Commit)).map_err(|error| error.message().to_string())?;
    let mut checkout = git2::build::CheckoutBuilder::new(); checkout.safe();
    repo.checkout_tree(&target, Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    drop(target);
    if let Some(branch_name) = create_local_branch {
        let commit = repo.find_commit(target_oid).map_err(|error| error.message().to_string())?;
        repo.branch(&branch_name, &commit, false).map_err(|error| error.message().to_string())?;
    }
    if let Some((local_branch, upstream_name)) = configure_upstream {
        let mut branch = repo.find_branch(&local_branch, BranchType::Local)
            .map_err(|error| format!("Local branch '{local_branch}' was selected, but could not be reopened to set its upstream: {}", error.message()))?;
        branch.set_upstream(Some(&upstream_name))
            .map_err(|error| format!("Local branch '{local_branch}' was selected, but tracking '{upstream_name}' could not be configured: {}", error.message()))?;
    }
    if let Some(reference) = attach_reference {
        repo.set_head(&reference).map_err(|error| error.message().to_string())?;
    } else {
        repo.set_head_detached(target_oid).map_err(|error| error.message().to_string())?;
    }
    let selected = repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    drop(repo);
    drop(sub_lock);
    // Do not silently stage a normal checkout. Only refresh a gitlink that
    // was already staged before this operation (notably `submodule add`).
    // The submodule lock is released first, so this follows the application's
    // single-repository-lock invariant before stage_files_inner takes the
    // parent lock.
    if was_parent_gitlink_staged {
        stage_files_inner(&repository_path, vec![relative_path.clone()])
            .map_err(|error| format!("The submodule was switched to {}, but its already-staged project reference could not be updated: {error}. Stage the submodule again before committing.", &selected[..selected.len().min(8)]))?;
    }
    invalidate_git_metadata(&absolute_string); // the submodule's own cache — its HEAD just moved
    invalidate_git_metadata_for_submodule_checkout(&repository_path, &relative_path);
    invalidate_submodule_sync(&repository_path); // this app just changed a submodule's registration/version
    Ok(selected)
}

#[derive(Serialize, Debug)]
pub struct CreateSubmoduleTagResult {
    name: String,
    target: String,
    annotated: bool,
    pushed: bool,
    // Always set when `push` was requested — the reason it wasn't pushed
    // (also_push semantics, same shape as CommitSubmoduleResult's own
    // push_detail) when `pushed` is false; the confirmation otherwise. None
    // only when the caller never asked to push at all.
    push_detail: Option<String>,
}

// Submodule-tag-creation report, point 4: a tag captures the submodule's
// *last commit* exactly as it already is — it is never built from the
// working tree, so an uncommitted edit sitting there is neither included nor
// referenced by it (see the frontend's own dirty-tree notice, shown before
// this is even called). Creating (or pushing) a tag never touches the
// parent's index/gitlink: the submodule's own current commit doesn't change
// just because a new name now also points at it.
#[tauri::command]
pub async fn create_submodule_tag(repository_path: String, relative_path: String, tag_name: String, message: String, push: bool, target_revision: Option<String>) -> Result<CreateSubmoduleTagResult, String> {
    off_main_thread(move || create_submodule_tag_at_inner(repository_path, relative_path, tag_name, message, push, target_revision)).await
}

#[cfg(test)]
fn create_submodule_tag_inner(repository_path: String, relative_path: String, tag_name: String, message: String, push: bool) -> Result<CreateSubmoduleTagResult, String> {
    create_submodule_tag_at_inner(repository_path, relative_path, tag_name, message, push, None)
}

fn create_submodule_tag_at_inner(repository_path: String, relative_path: String, tag_name: String, message: String, push: bool, target_revision: Option<String>) -> Result<CreateSubmoduleTagResult, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let tag_name = tag_name.trim().to_string();
    validate_tag_name(&tag_name)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let (target_id, annotated) = {
        let queue_started = Instant::now();
        let lock_handle = repo_write_lock(&sub_path);
        let _lock = lock_handle.lock().unwrap();
        log_repo_write_lock_acquired(&sub_path, "create_submodule_tag", queue_started.elapsed());
        let repo = internal_submodule_repository(&absolute)?;
        // Never overwrite an existing tag silently — local or (checked
        // further down, before any push) remote.
        if repo.find_reference(&format!("refs/tags/{tag_name}")).is_ok() {
            return Err(format!("Tag '{tag_name}' already exists in this submodule. Choose a different name, or delete the existing tag first."));
        }
        let target = if let Some(revision) = target_revision.as_deref().map(str::trim).filter(|revision| !revision.is_empty()) {
            repo.revparse_single(revision)
                .map_err(|_| format!("Commit '{revision}' no longer exists in this submodule. Refresh History and choose it again."))?
                .peel_to_commit().map_err(|_| format!("'{revision}' does not identify a commit in this submodule."))?
        } else {
            repo.head().map_err(|error| error.message().to_string())?.peel_to_commit().map_err(|error| error.message().to_string())?
        };
        let target_id = target.id();
        let message = message.trim();
        let annotated = !message.is_empty();
        // Prefer an annotated tag when a message was supplied (it records
        // who/when/why, same as a commit); a lightweight tag otherwise — no
        // reason to force an empty annotation object when a plain ref is all
        // that was asked for.
        if annotated {
            let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this submodule".to_string())?;
            repo.tag(&tag_name, target.as_object(), &signature, message, false).map_err(|error| error.message().to_string())?;
        } else {
            repo.reference(&format!("refs/tags/{tag_name}"), target_id, false, "created via Git DrillDown").map_err(|error| error.message().to_string())?;
        }
        (target_id, annotated)
    };
    invalidate_git_metadata(&sub_path);
    if !push {
        return Ok(CreateSubmoduleTagResult { name: tag_name, target: target_id.to_string(), annotated, pushed: false, push_detail: None });
    }
    match push_submodule_tag_inner(repository_path, relative_path, tag_name.clone()) {
        Ok(()) => Ok(CreateSubmoduleTagResult { name: tag_name, target: target_id.to_string(), annotated, pushed: true, push_detail: Some("Pushed to the submodule's remote.".into()) }),
        Err(detail) => Ok(CreateSubmoduleTagResult { name: tag_name, target: target_id.to_string(), annotated, pushed: false, push_detail: Some(detail) }),
    }
}

#[tauri::command]
pub async fn push_submodule_tag(repository_path: String, relative_path: String, tag_name: String) -> Result<(), String> {
    off_main_thread(move || push_submodule_tag_inner(repository_path, relative_path, tag_name)).await
}

// Pushes exactly `refs/tags/<tag_name>` — never the branch it happens to
// sit on, never every tag (`--tags`), and never anything else the submodule
// might have unpushed. A push_submodule-workflow-style, single-purpose
// action distinct from the branch-push commands above.
fn push_submodule_tag_inner(repository_path: String, relative_path: String, tag_name: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let tag_name = tag_name.trim().to_string();
    validate_tag_name(&tag_name)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&sub_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&sub_path, "push_submodule_tag", queue_started.elapsed());
    let repo = internal_submodule_repository(&absolute)?;
    repo.find_reference(&format!("refs/tags/{tag_name}")).map_err(|_| format!("Tag '{tag_name}' does not exist in this submodule"))?;
    repo.find_remote("origin").map_err(|_| "No 'origin' remote configured for this submodule".to_string())?;
    git(&sub_path, &["push", "origin", &format!("refs/tags/{tag_name}")]).map_err(|detail| {
        if detail.contains("already exists") || detail.contains("[rejected]") {
            format!("Push rejected — a tag named '{tag_name}' already exists on the remote. Tags are meant to stay immutable once shared; use a different name, or delete the remote tag first if you're certain.\n\nGit's message: {detail}")
        } else { format!("Push failed: {detail}") }
    })?;
    Ok(())
}

// Validates using git's own ref-name rules (git2::Reference::is_valid_name
// against the full "refs/tags/<name>" form — catches spaces, "..", a
// trailing ".lock", etc., the exact same way `git tag <name>` itself would
// reject them) rather than a hand-rolled character allowlist. A leading '-'
// is separately rejected here: it's syntactically a valid ref name to
// git-check-ref-format, but this value later reaches the `git` CLI as a
// positional argument (push_submodule_tag_inner's own `git push ... "refs/tags/{name}"`)
// where it would instead be parsed as an option.
fn validate_tag_name(name: &str) -> Result<(), String> {
    if name.is_empty() { return Err("Tag name cannot be empty".into()); }
    if name.starts_with('-') { return Err(format!("'{name}' is not a valid tag name — it cannot start with '-'.")); }
    if !git2::Reference::is_valid_name(&format!("refs/tags/{name}")) {
        return Err(format!("'{name}' is not a valid tag name."));
    }
    Ok(())
}

// Discards whatever local state a submodule has drifted into — a dirty
// working tree, local commits, or an uncommitted "switch version" done from
// this app or by hand — and forces it back to exactly the commit the parent
// repository currently has recorded for it (its index entry, so a *staged*
// version bump is respected as the target, not silently discarded too; only
// HEAD's committed tree wins when nothing is staged, since the index then
// mirrors it). Equivalent to `git submodule update --force <path>`, which is
// why the submodule ends up in detached HEAD afterward — same as ordinary
// `git submodule update` always does.
#[tauri::command]
pub async fn reset_submodule(repository_path: String, relative_path: String) -> Result<String, String> {
    off_main_thread(move || reset_submodule_inner(repository_path, relative_path)).await
}

fn reset_submodule_inner(repository_path: String, relative_path: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    let parent = internal_repository(&repository_path)?;
    let index = parent.index().map_err(|error| error.message().to_string())?;
    let entry = index.iter().find(|entry| String::from_utf8_lossy(&entry.path) == relative_string)
        .ok_or("This path is not a registered submodule in the parent index")?;
    let target_oid = entry.id;
    drop(index); drop(parent); // read-only above — only the submodule itself is mutated below
    let absolute_string = absolute.to_string_lossy().into_owned();
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&absolute_string);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&absolute_string, "reset_submodule", queue_started.elapsed());
    let sub_repo = internal_submodule_repository(&absolute)?;
    sub_repo.set_head_detached(target_oid).map_err(|error| error.message().to_string())?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    // force discards dirty working-tree edits, same as before. remove_untracked
    // + remove_ignored is the git2-native equivalent of `git clean -fdx`,
    // folded into this same checkout instead of a separate pass — this button
    // already promises "the exact submodule commit recorded by the parent
    // project", so leftover untracked/ignored cruft (notably a clone or
    // checkout interrupted mid-way by something outside this app, which can
    // leave partial files force-checkout alone never touches since they
    // aren't part of the target tree) must not survive it either.
    checkout.force().remove_untracked(true).remove_ignored(true);
    sub_repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    drop(sub_repo);
    invalidate_git_metadata(&absolute_string); // the submodule's own cache — its HEAD just moved
    invalidate_git_metadata_for_submodule_checkout(&repository_path, &relative_string);
    invalidate_submodule_sync(&repository_path); // this app just changed a submodule's registration/version
    Ok(target_oid.to_string())
}

#[derive(Serialize, Debug)]
pub struct ResetSubmoduleBranchResult { branch: String, upstream: String, revision: String }

// Explicitly destructive counterpart to the non-destructive fast-forward
// pull: discard the checked-out branch's local commits and dirty index/tree,
// then make it exactly match its configured upstream. This is intentionally
// separate from reset_submodule, whose target is the parent project's pinned
// gitlink and which correctly leaves a detached HEAD.
#[tauri::command]
pub async fn reset_submodule_branch_to_upstream(repository_path: String, relative_path: String, branch_name: String) -> Result<ResetSubmoduleBranchResult, String> {
    off_main_thread(move || reset_submodule_branch_to_upstream_inner(repository_path, relative_path, branch_name)).await
}

fn reset_submodule_branch_to_upstream_inner(repository_path: String, relative_path: String, branch_name: String) -> Result<ResetSubmoduleBranchResult, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&sub_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&sub_path, "reset_submodule_branch_to_upstream", queue_started.elapsed());
    let repo = internal_submodule_repository(&absolute)?;
    let branch = branch_name.trim().to_string();
    if branch.is_empty() { return Err("Choose a local branch to match to its upstream".into()); }
    repo.find_branch(&branch, BranchType::Local).map_err(|_| format!("Local branch '{branch}' was not found"))?;

    // A working tree is shared by every branch in this repository. Moving HEAD
    // to some *other* saved branch and then doing a hard reset would therefore
    // destroy the edits belonging to the checkout the user was actually using,
    // not edits somehow owned by the row they clicked. Only permit dirty-work
    // deletion when the selected branch is already the active checkout. A clean
    // detached checkout remains supported (the normal state after restoring the
    // parent project's recorded submodule version).
    if repo.state() != git2::RepositoryState::Clean {
        return Err("Cannot discard this branch while another Git operation or conflict resolution is in progress. Finish or abort it first.".into());
    }
    let current_branch = if repo.head_detached().unwrap_or(true) {
        None
    } else {
        repo.head().ok().and_then(|head| head.shorthand().map(String::from))
    };
    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() && current_branch.as_deref() != Some(branch.as_str()) {
        let checkout = current_branch.as_deref().map(|name| format!("branch '{name}'")).unwrap_or_else(|| "the detached checkout".into());
        return Err(format!("Cannot replace branch '{branch}' because {checkout} has uncommitted work. A Git working tree is shared between branches, so switching and hard-resetting here would delete that work. Commit or stash it first, or checkout '{branch}' before explicitly discarding its work."));
    }
    let remote = repo.config().ok().and_then(|config| config.get_string(&format!("branch.{branch}.remote")).ok())
        .filter(|name| !name.is_empty() && name != ".")
        .ok_or_else(|| format!("Branch '{branch}' has no remote upstream. Configure an upstream before matching it."))?;
    repo.find_remote(&remote).map_err(|_| format!("Configured remote '{remote}' was not found"))?;

    // Refresh the tracking ref first. The hard reset below is based on this
    // just-fetched exact target, never on a stale cached origin/* value.
    git(&sub_path, &["fetch", &remote]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    let (target_oid, upstream) = upstream_ref(&repo, &branch)
        .ok_or_else(|| format!("Could not resolve the configured upstream for branch '{branch}' after fetch"))?;
    let target = repo.find_object(target_oid, Some(ObjectType::Commit)).map_err(|error| error.message().to_string())?;
    // Make the explicitly selected local branch current first; ResetType::Hard
    // then moves that branch ref and makes both index and worktree exactly
    // match the fetched upstream. This also supports the natural flow after
    // "Restore project version", where HEAD is detached but the saved local
    // branch is visible in the dialog.
    repo.set_head(&format!("refs/heads/{branch}")).map_err(|error| error.message().to_string())?;
    repo.reset(&target, git2::ResetType::Hard, None).map_err(|error| format!("Could not reset '{branch}' to {upstream}: {}", error.message()))?;
    drop(target);
    // Same reasoning as Restore project version's own checkout: this button's
    // whole point is "match upstream exactly, discard any local divergence" —
    // a plain ResetType::Hard (like `git reset --hard`) only resets tracked
    // content, it never removes untracked or .gitignore'd leftovers, and —
    // empirically confirmed, not assumed — passing remove_untracked/
    // remove_ignored checkout options directly to reset() does not change
    // that either; libgit2's hard reset does not honor them there. A
    // separate, explicit checkout_head pass afterward (the same call Restore
    // project version uses) does honor them, checking out the exact same
    // tree reset() just landed on but this time actually sweeping the cruft.
    let mut cleanup_checkout = git2::build::CheckoutBuilder::new();
    cleanup_checkout.force().remove_untracked(true).remove_ignored(true);
    repo.checkout_head(Some(&mut cleanup_checkout)).map_err(|error| error.message().to_string())?;
    if !internal_statuses(&repo, None)?.is_empty() {
        return Err("The branch moved to its upstream, but the index or working tree is not clean. No further action was performed.".into());
    }
    drop(repo);
    invalidate_git_metadata(&sub_path);
    invalidate_git_metadata_for_submodule_checkout(&repository_path, &relative_path);
    invalidate_submodule_sync(&repository_path);
    Ok(ResetSubmoduleBranchResult { branch, upstream, revision: target_oid.to_string() })
}

#[tauri::command]
pub async fn change_submodule_url(repository_path: String, relative_path: String, url: String) -> Result<(), String> {
    off_main_thread(move || change_submodule_url_inner(repository_path, relative_path, url)).await
}

fn change_submodule_url_inner(repository_path: String, relative_path: String, url: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let relative = normalized(&safe_relative_path(&relative_path)?);
    let url = url.trim();
    if url.is_empty() || url.starts_with('-') { return Err("Enter a valid Git repository URL".into()); }
    let queue_started = Instant::now();
    let parent_lock_handle = repo_write_lock(&repository_path);
    let parent_lock = parent_lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "change_submodule_url(parent)", queue_started.elapsed());
    let mut repo = internal_repository(&repository_path)?;
    let configured_url = portable_submodule_configured_url(url);
    let fetch_url = resolved_submodule_io_url(&repo, &configured_url)?;
    let name = repo.submodules().map_err(|error| error.message().to_string())?.into_iter().find(|item| normalized(item.path()) == relative).map(|item| item.name().unwrap_or("").to_string()).ok_or("Submodule configuration was not found")?;
    repo.submodule_set_url(&name, &configured_url).map_err(|error| error.message().to_string())?;
    drop(repo); drop(parent_lock); // released before the submodule's own lock, never nested
    let absolute_string = absolute.to_string_lossy().into_owned();
    let queue_started = Instant::now();
    let sub_lock_handle = repo_write_lock(&absolute_string);
    let _sub_lock = sub_lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&absolute_string, "change_submodule_url(submodule)", queue_started.elapsed());
    let subrepo = internal_submodule_repository(&absolute)?; subrepo.remote_set_url("origin", &fetch_url).map_err(|error| error.message().to_string())?; subrepo.find_remote("origin").map_err(|error| error.message().to_string())?; git(absolute.to_str().unwrap_or_default(), &["fetch", "origin"]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path); // this app just changed a submodule's registration/version
    Ok(())
}

#[tauri::command]
pub async fn remove_git_path(repository_path: String, relative_path: String) -> Result<(), String> {
    off_main_thread(move || {
        let started = Instant::now();
        perf_log(&format!("remove_git_path: START ({relative_path})"), Duration::ZERO);
        let result = remove_git_path_inner(&repository_path, &relative_path);
        match &result {
            Ok(()) => perf_log(&format!("remove_git_path: TOTAL ({relative_path})"), started.elapsed()),
            Err(error) => perf_log(&format!("remove_git_path: ERROR ({relative_path}): {error}"), started.elapsed()),
        }
        result
    }).await
}

fn remove_git_path_inner(repository_path: &str, relative_path: &str) -> Result<(), String> {
    validate_path(repository_path)?;
    let relative = normalized(&safe_relative_path(relative_path)?);
    if relative.is_empty() { return Err("The repository root cannot be removed".into()); }
    // A file inside a submodule is tracked by the submodule's own index, not
    // by the parent project. Route the deletion to the owning repository so a
    // tracked submodule file can be removed/staged correctly instead of being
    // rejected as "not tracked" by the parent, which only knows the gitlink.
    if let Some((submodule_relative, sub_path, inner_relative)) = resolve_submodule_boundary_with_path(repository_path, &relative) {
        if !inner_relative.is_empty() {
            let result = remove_git_path_inner(&sub_path, &inner_relative);
            if result.is_ok() {
                invalidate_git_metadata(&sub_path);
                invalidate_git_metadata_for_submodule_checkout(repository_path, &submodule_relative);
                invalidate_submodule_sync(repository_path);
            }
            return result;
        }
    }
    let (tracked, _) = cached_index_metadata(repository_path);
    let prefix = format!("{relative}/");
    if !tracked.contains(&relative) && !tracked.iter().any(|path| path.starts_with(&prefix)) {
        return Err("This item is not tracked by Git. Remove it with the operating system if intended".into());
    }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(repository_path, "remove_git_path", queue_started.elapsed());
    let repo = internal_repository(repository_path)?;
    let step = Instant::now();
    let submodule_name = repo.submodules().ok().and_then(|items| items.into_iter().find(|item| normalized(item.path()) == relative).map(|item| item.name().unwrap_or(&relative).to_string()));
    perf_log("remove_git_path: repo.submodules() lookup", step.elapsed());
    // Deleting a submodule's own working copy means deleting its own real
    // .git directory too — a full, independent object database, not just a
    // gitlink pointer. That's genuinely real data (git objects, one file
    // per blob/tree/commit in the simple case) with a real disk-I/O cost to
    // remove, worse on Windows specifically (already documented elsewhere in
    // this codebase as slower at exactly this kind of per-file filesystem
    // work) — logged separately here so a real "this is just how much data
    // there was" case can be told apart from an actual bug.
    let step = Instant::now();
    let target = Path::new(repository_path).join(&relative); if target.is_dir() { fs::remove_dir_all(&target).map_err(|error| error.to_string())?; } else if target.exists() { fs::remove_file(&target).map_err(|error| error.to_string())?; }
    perf_log(&format!("remove_git_path: remove working copy from disk ({relative})"), step.elapsed());
    if let Some(name) = submodule_name {
        let step = Instant::now();
        cleanup_submodule_registration(&repo, &name, Path::new(&relative))?;
        perf_log("remove_git_path: cleanup_submodule_registration", step.elapsed());
        let step = Instant::now();
        let modules = repo.path().join("modules").join(safe_relative_path(&name)?);
        if modules.is_dir() { fs::remove_dir_all(modules).map_err(|error| format!("Cannot remove internal submodule data: {error}"))?; }
        perf_log("remove_git_path: remove .git/modules copy", step.elapsed());
    }
    let step = Instant::now();
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    let paths: Vec<PathBuf> = index.iter().filter_map(|entry| {
        let path = String::from_utf8_lossy(&entry.path);
        (path == relative || path.starts_with(&prefix)).then(|| PathBuf::from(path.as_ref()))
    }).collect();
    for path in paths { index.remove_path(&path).map_err(|error| error.message().to_string())?; }
    let gitmodules = Path::new(repository_path).join(".gitmodules");
    if gitmodules.exists() { index.add_path(Path::new(".gitmodules")).map_err(|error| error.message().to_string())?; }
    else { let _ = index.remove_path(Path::new(".gitmodules")); }
    index.write().map_err(|error| error.message().to_string())?;
    perf_log("remove_git_path: sync index", step.elapsed());
    invalidate_git_metadata(repository_path);
    invalidate_submodule_sync(repository_path); // this app just changed a submodule's registration/version
    Ok(())
}

#[tauri::command]
pub fn delete_local_path(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    let relative_string = normalized(&relative);
    if relative_string.is_empty() { return Err("The repository root cannot be deleted".into()); }
    // Same ownership rule as remove_git_path_inner: if the selected file is
    // inside a submodule, the submodule decides whether it is local-only or
    // tracked. The parent index cannot answer that question.
    if let Some((submodule_relative, sub_path, inner_relative)) = resolve_submodule_boundary_with_path(&repository_path, &relative_string) {
        if !inner_relative.is_empty() {
            let result = delete_local_path(sub_path.clone(), inner_relative);
            if result.is_ok() {
                invalidate_git_metadata(&sub_path);
                invalidate_git_metadata_for_submodule_checkout(&repository_path, &submodule_relative);
                invalidate_submodule_sync(&repository_path);
            }
            return result;
        }
    }
    // Destructive checks must reflect the current index, not an older Explorer snapshot.
    let (tracked, _) = cached_index_metadata(&repository_path);
    let prefix = format!("{relative_string}/");
    if tracked.contains(&relative_string) || tracked.iter().any(|path| path.starts_with(&prefix)) {
        return Err("This item is tracked. Use Remove from Git so the deletion can be committed".into());
    }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "delete_local_path", queue_started.elapsed());
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

// Fast path for the common case — the main Commit button, committing
// everything currently staged, not some folder-scoped subset. commit_files
// below has to rebuild a scratch index (parent tree + just the given paths)
// because it also has to support committing *part* of what's staged; that
// machinery is unnecessary work here, since "everything staged" already *is*
// exactly the tree this commit needs — the real on-disk index can be used
// directly with no rebuilding, no re-adding paths one phase then again,
// and no post-commit resync (nothing about the index needs to change: it
// already equals the new HEAD's tree, which is what "nothing staged
// anymore" after a commit means).
#[tauri::command]
pub async fn commit_staged(repository_path: String, message: String) -> Result<String, String> {
    off_main_thread(move || commit_staged_inner(repository_path, message)).await
}

fn commit_staged_inner(repository_path: String, message: String) -> Result<String, String> {
    let started = Instant::now();
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    // See repo_write_lock's doc comment — a commit right as a staging click
    // is still mid-flight would otherwise race the same index.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "commit_staged", queue_started.elapsed());
    let step = Instant::now();
    let repo = internal_repository(&repository_path)?;
    perf_log("commit_staged: internal_repository (open)", step.elapsed());
    let step = Instant::now();
    let mut index = repo.index().map_err(|error| error.message().to_string())?;
    perf_log("commit_staged: repo.index()", step.elapsed());
    // Never commit a stale staged gitlink. This can happen when an external
    // tool changes a submodule checkout after it was staged. The commit would
    // succeed but record the old SHA, then immediately show the submodule as
    // Modified and make the user commit/push a second time. Refuse that
    // misleading partial result and tell the user exactly what to restage.
    let parent_tree = repo.head().ok().and_then(|head| head.peel_to_commit().ok()).and_then(|commit| commit.tree().ok());
    let mut stale_gitlinks = Vec::new();
    for entry in index.iter().filter(|entry| entry.mode == 0o160000) {
        let path = String::from_utf8_lossy(&entry.path).into_owned();
        let committed = parent_tree.as_ref().and_then(|tree| tree.get_path(Path::new(&path)).ok()).map(|tree_entry| tree_entry.id());
        if committed == Some(entry.id) { continue; }
        let current = internal_submodule_repository(&Path::new(&repository_path).join(&path)).ok()
            .and_then(|sub_repo| sub_repo.head().ok().and_then(|head| head.target()));
        if let Some(current) = current.filter(|current| *current != entry.id) {
            stale_gitlinks.push(format!("{path} (staged {}, current {})", &entry.id.to_string()[..8], &current.to_string()[..8]));
        }
    }
    if !stale_gitlinks.is_empty() {
        return Err(format!("The staged submodule reference is older than its current checkout: {}. Stage the submodule again, then commit once; this prevents recording the wrong version and needing a second commit.", stale_gitlinks.join(", ")));
    }
    let step = Instant::now();
    let tree_id = index.write_tree_to(&repo).map_err(|error| error.message().to_string())?;
    perf_log(&format!("commit_staged: write_tree_to ({} index entries)", index.len()), step.elapsed());
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    if parent.as_ref().map(|commit| commit.tree_id()) == Some(tree_id) { return Err("There are no changes to commit in the selected files".into()); }
    let step = Instant::now();
    let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?;
    let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?;
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    let oid = repo.commit(Some("HEAD"), &signature, &signature, message.trim(), &tree, &parents).map_err(|error| error.message().to_string())?;
    perf_log("commit_staged: find_tree + signature + repo.commit", step.elapsed());
    let step = Instant::now();
    invalidate_git_metadata(&repository_path);
    perf_log("commit_staged: invalidate_git_metadata", step.elapsed());
    perf_log("commit_staged: TOTAL", started.elapsed());
    Ok(oid.to_string())
}

#[tauri::command]
pub async fn commit_files(repository_path: String, files: Vec<String>, message: String) -> Result<String, String> {
    off_main_thread(move || commit_files_inner(repository_path, files, message)).await
}

fn commit_files_inner(repository_path: String, files: Vec<String>, message: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    if files.is_empty() { return Err("Select at least one file".into()); }
    let safe_files: Vec<String> = files.iter().map(|file| safe_relative_path(file.trim_end_matches(|character| character == '/' || character == '\\')).map(|path| normalized(&path))).collect::<Result<_, _>>()?;
    commit_selected_internal(&repository_path, &safe_files, message.trim())
}

fn ensure_changed_submodule_gitlinks_are_publishable(repository_path: &str, repo: &Repository, parent_tree: Option<&git2::Tree<'_>>, new_tree: &git2::Tree<'_>) -> Result<(), String> {
    let Ok(diff) = repo.diff_tree_to_tree(parent_tree, Some(new_tree), None) else { return Ok(()); };
    let mut seen = HashSet::new();
    let mut blocked = Vec::new();
    for delta in diff.deltas() {
        if delta.status() == git2::Delta::Deleted { continue; }
        let new_file = delta.new_file();
        if new_file.mode() != git2::FileMode::Commit { continue; }
        let (Some(path), oid) = (new_file.path(), new_file.id()) else { continue };
        if oid.is_zero() { continue; }
        let path = normalized(path);
        if !seen.insert((path.clone(), oid)) { continue; }
        let (mut risks, _, _) = submodule_reference_risks(repository_path, &path, &[oid], true);
        if risks.remove(&oid).flatten() == Some("unpushed") {
            let short = oid.to_string();
            blocked.push(format!("{path} -> {}", &short[..8.min(short.len())]));
        }
    }
    if blocked.is_empty() { return Ok(()); }
    Err(format!(
        "Cannot commit the project submodule reference yet. Push the submodule commit first, then commit the project link.\n\nUnpublished submodule gitlink{}:\n{}",
        if blocked.len() == 1 { "" } else { "s" },
        blocked.join("\n")
    ))
}

fn commit_selected_internal(repository_path: &str, files: &[String], message: &str) -> Result<String, String> {
    let commit_started = Instant::now();
    // See repo_write_lock's doc comment — shared by both commit_files and
    // commit_path, both of which mutate the index.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(repository_path, "commit_selected_internal", queue_started.elapsed());
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
    // Batched instead of one `add_all`/`remove_all` call per file: each call
    // re-matches its pathspec against the whole index internally, so on a large
    // repository calling it once per file (245 calls for a 245-file commit, here
    // and again in the post-commit sync below — 490 total) scaled with both the
    // number of files *and* the size of the index, turning what should be a
    // sub-second commit into minutes. A single call with every path at once does
    // the same matching pass just once.
    // Same fix as stage_files, same reason: add_path for a known exact file
    // (a direct hash-and-insert) instead of add_all (a working-directory-wide
    // pathspec match/diff even for one literal path) — a real Windows log
    // showed a single add_all call taking 1.6-1.9 SECONDS for exactly one
    // file. add_all is kept only for an actual directory needing expansion.
    let mut files_to_add: Vec<&Path> = Vec::new();
    let mut dirs_to_add: Vec<&Path> = Vec::new();
    let mut to_remove: Vec<&Path> = Vec::new();
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
        if absolute.is_dir() {
            if embedded_git_repo_path(&absolute) { return Err(embedded_git_repo_error(file)); }
            dirs_to_add.push(path);
        } else if absolute.exists() { files_to_add.push(path); } else { to_remove.push(path); }
    }
    perf_log(&format!("commit: build scratch index ({} files)", files.len()), commit_started.elapsed());
    let step = Instant::now();
    for file in &files_to_add { index.add_path(file).map_err(|error| error.message().to_string())?; }
    if !dirs_to_add.is_empty() {
        index.add_all(&dirs_to_add, git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?;
        index.update_all(&dirs_to_add, None).map_err(|error| error.message().to_string())?;
    }
    if !to_remove.is_empty() { index.remove_all(&to_remove, None).map_err(|error| error.message().to_string())?; }
    perf_log("commit: add_path/add_all/remove_all (scratch index)", step.elapsed());
    if includes_submodule && Path::new(repository_path).join(".gitmodules").exists() { index.add_path(Path::new(".gitmodules")).map_err(|error| error.message().to_string())?; }
    let step = Instant::now();
    let tree_id = index.write_tree_to(&repo).map_err(|error| error.message().to_string())?; if parent_tree.as_ref().map(|tree| tree.id()) == Some(tree_id) { return Err("There are no changes to commit in the selected files".into()); } let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?; if includes_submodule { ensure_changed_submodule_gitlinks_are_publishable(repository_path, &repo, parent_tree.as_ref(), &tree)?; } let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?; let parents: Vec<&git2::Commit<'_>> = parent.iter().collect(); let oid = repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &parents).map_err(|error| error.message().to_string())?;
    perf_log("commit: write_tree_to + commit", step.elapsed());
    // `index` above was repurposed as an in-memory scratch copy (parent tree plus
    // only the selected files) to build the commit tree, and `repo.index()` returns
    // that same cached instance rather than a fresh read — so it must not be
    // written back to .git/index as-is, or every other pending file not part of
    // this (possibly scoped/partial) commit would silently lose its staged status.
    // Force-reload the real on-disk index first, then sync just the committed
    // files into it so they stop showing as staged, leaving every other entry
    // (which was never touched on disk) untouched.
    let step = Instant::now();
    index.read(true).map_err(|error| error.message().to_string())?;
    let mut files_to_add: Vec<&Path> = Vec::new();
    let mut dirs_to_add: Vec<&Path> = Vec::new();
    let mut to_remove: Vec<&Path> = Vec::new();
    for file in files {
        if file.is_empty() {
            index.add_all(Vec::<String>::new(), git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?;
            index.update_all(Vec::<String>::new(), None).map_err(|error| error.message().to_string())?;
            continue;
        }
        let relative = Path::new(file); let absolute = Path::new(repository_path).join(relative);
        if gitlinks.contains_key(file) && absolute.exists() { if let Ok(mut submodule) = repo.find_submodule(file) { let _ = submodule.add_to_index(true); } continue; }
        if absolute.is_dir() { dirs_to_add.push(relative); } else if absolute.exists() { files_to_add.push(relative); } else { to_remove.push(relative); }
    }
    for file in &files_to_add { index.add_path(file).map_err(|error| error.message().to_string())?; }
    if !dirs_to_add.is_empty() {
        index.add_all(&dirs_to_add, git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?;
        index.update_all(&dirs_to_add, None).map_err(|error| error.message().to_string())?;
    }
    if !to_remove.is_empty() { index.remove_all(&to_remove, None).map_err(|error| error.message().to_string())?; }
    index.write().map_err(|error| error.message().to_string())?;
    perf_log("commit: sync real index", step.elapsed());
    perf_log(&format!("commit: TOTAL ({} files)", files.len()), commit_started.elapsed());
    invalidate_git_metadata(repository_path); Ok(oid.to_string())
}

#[tauri::command]
pub fn restore_file(repository_path: String, relative_path: String, source_ref: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    if let Some((sub_path, inner_relative)) = resolve_submodule_boundary(&repository_path, &relative_path) {
        return restore_file(sub_path, inner_relative, source_ref);
    }
    let relative = safe_relative_path(&relative_path)?; if relative.as_os_str().is_empty() { return Err("Select a file".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "restore_file", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let object = repo.revparse_single(source_ref.trim()).or_else(|_| repo.revparse_single(&format!("refs/remotes/{}", source_ref.trim()))).map_err(|error| format!("Cannot resolve {source_ref}: {}", error.message()))?;
    let commit = object.peel_to_commit().map_err(|error| error.message().to_string())?; let tree = commit.tree().map_err(|error| error.message().to_string())?;
    let entry = match tree.get_path(&relative) {
        Ok(entry) => entry,
        Err(_) => {
            // Restoring a path that is new relative to the selected source is
            // a discard, not a blob checkout. The common case is: create a new
            // file, stage it (so it is "tracked" in the index), delete it from
            // disk, then choose Restore from HEAD. That path genuinely does
            // not exist in HEAD, so the only correct restore result is to
            // remove the staged add and leave no file behind — otherwise the
            // UI keeps showing a confusing tracked/new/deleted remnant.
            let mut index = repo.index().map_err(|error| error.message().to_string())?;
            let _ = index.remove_path(relative.as_path());
            index.write().map_err(|error| error.message().to_string())?;
            let destination = Path::new(&repository_path).join(&relative);
            if destination.is_dir() {
                fs::remove_dir_all(&destination).map_err(|error| error.to_string())?;
            } else if destination.exists() {
                fs::remove_file(&destination).map_err(|error| error.to_string())?;
            }
            invalidate_git_metadata(&repository_path);
            return Ok(());
        }
    };
    let blob = repo.find_blob(entry.id()).map_err(|error| error.message().to_string())?;
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

fn parse_name_status(output: &str) -> Vec<Change> {
    output.lines().filter_map(|line| {
        let mut parts = line.split('\t');
        let status = parts.next()?.trim().to_string();
        let path = parts.last().unwrap_or_default().trim().to_string();
        (!status.is_empty() && !path.is_empty()).then_some(Change { status, path, staged: false })
    }).collect()
}

fn parse_porcelain_status(output: &str) -> Vec<Change> {
    output.lines().filter_map(|line| {
        if line.len() < 4 { return None; }
        let status = line[..2].trim().to_string();
        if status == "??" { return None; }
        let path = line[3..].trim().trim_matches('"').to_string();
        (!status.is_empty() && !path.is_empty()).then_some(Change { status, path, staged: line.as_bytes().first().is_some_and(|byte| *byte != b' ') })
    }).collect()
}

fn parse_clean_dry_run(output: &str) -> Vec<String> {
    output.lines().filter_map(|line| {
        line.strip_prefix("Would remove ")
            .map(|path| path.trim().trim_matches('"').trim_end_matches('/').replace('\\', "/"))
            .filter(|path| !path.is_empty())
    }).collect()
}

fn commit_for_folder_restore<'repo>(repo: &'repo Repository, source_revision: &str) -> Result<git2::Commit<'repo>, String> {
    repo.revparse_single(source_revision.trim())
        .or_else(|_| repo.revparse_single(&format!("refs/remotes/{}", source_revision.trim())))
        .and_then(|object| object.peel(ObjectType::Commit))
        .and_then(|object| object.into_commit().map_err(|_| git2::Error::from_str("Selected revision is not a commit")))
        .map_err(|error| format!("Cannot resolve {source_revision}: {}", error.message()))
}

fn folder_restore_preview_inner(repository_path: &str, relative_path: &str, source_revision: &str, clean_untracked: bool) -> Result<FolderRestorePreview, String> {
    validate_path(repository_path)?;
    let relative = safe_relative_path(relative_path)?;
    if relative.as_os_str().is_empty() { return Err("Select a folder to restore".into()); }
    let relative_string = normalized(&relative);
    let folder_path = Path::new(repository_path).join(&relative);
    if !folder_path.is_dir() { return Err("Folder restore works only for an existing normal folder".into()); }
    let repo = internal_repository(repository_path)?;
    let commit = commit_for_folder_restore(&repo, source_revision)?;
    let tree = commit.tree().map_err(|error| error.message().to_string())?;
    let tree_entry = tree.get_path(&relative).map_err(|_| format!("{relative_string} does not exist in {}", source_revision.trim()))?;
    if tree_entry.kind() != Some(ObjectType::Tree) {
        return Err(format!("{relative_string} is not a folder in {}", source_revision.trim()));
    }
    let source_id = commit.id().to_string();
    let tracked_changes = if source_revision.trim() == "HEAD" || source_id == repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default() {
        parse_porcelain_status(&git(repository_path, &["status", "--porcelain", "--", &relative_string])?)
    } else {
        parse_name_status(&git(repository_path, &["diff", "--name-status", "HEAD", &source_id, "--", &relative_string])?)
    };
    let clean_candidates = if clean_untracked {
        parse_clean_dry_run(&git(repository_path, &["clean", "-nd", "--", &relative_string])?)
    } else { Vec::new() };
    let source_subject = commit.summary().unwrap_or("No message").to_string();
    let source_author = commit.author().name().unwrap_or("Unknown").to_string();
    let source_date = short_date(commit.time().seconds());
    Ok(FolderRestorePreview {
        folder: relative_string,
        source_revision: source_revision.trim().to_string(),
        source_id,
        source_subject,
        source_author,
        source_date,
        tracked_changes,
        clean_candidates,
    })
}

#[tauri::command]
pub fn preview_folder_restore(repository_path: String, relative_path: String, source_revision: String, clean_untracked: bool) -> Result<FolderRestorePreview, String> {
    folder_restore_preview_inner(&repository_path, &relative_path, &source_revision, clean_untracked)
}

#[tauri::command]
pub async fn restore_folder(repository_path: String, relative_path: String, source_revision: String, clean_paths: Vec<String>) -> Result<(), String> {
    off_main_thread(move || restore_folder_inner(repository_path, relative_path, source_revision, clean_paths)).await
}

fn restore_folder_inner(repository_path: String, relative_path: String, source_revision: String, clean_paths: Vec<String>) -> Result<(), String> {
    let started = Instant::now();
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    if relative.as_os_str().is_empty() { return Err("Select a folder to restore".into()); }
    let relative_string = normalized(&relative);
    let folder_path = Path::new(&repository_path).join(&relative);
    if !folder_path.is_dir() { return Err("Folder restore works only for an existing normal folder".into()); }
    let repo = internal_repository(&repository_path)?;
    let commit = commit_for_folder_restore(&repo, &source_revision)?;
    let tree = commit.tree().map_err(|error| error.message().to_string())?;
    let tree_entry = tree.get_path(&relative).map_err(|_| format!("{relative_string} does not exist in {}", source_revision.trim()))?;
    if tree_entry.kind() != Some(ObjectType::Tree) {
        return Err(format!("{relative_string} is not a folder in {}", source_revision.trim()));
    }

    let clean_relative = clean_paths.into_iter().map(|path| {
        let safe = safe_relative_path(path.trim_end_matches(['/', '\\']))?;
        let normalized_safe = normalized(&safe);
        if normalized_safe != relative_string && !normalized_safe.starts_with(&format!("{relative_string}/")) {
            return Err("Clean path escaped the selected folder".to_string());
        }
        Ok(normalized_safe)
    }).collect::<Result<Vec<_>, _>>()?;

    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "restore_folder", queue_started.elapsed());
    let source_id = commit.id().to_string();
    git(&repository_path, &["restore", "--source", &source_id, "--staged", "--worktree", "--", &relative_string])
        .map_err(|detail| format!("Folder restore failed: {detail}"))?;
    let submodule_status = git(&repository_path, &["submodule", "status", "--recursive", "--", &relative_string])
        .unwrap_or_default();
    if !submodule_status.trim().is_empty() {
        let _ = git(&repository_path, &["submodule", "sync", "--recursive", "--", &relative_string]);
        git(&repository_path, &["-c", "protocol.file.allow=always", "submodule", "update", "--init", "--recursive", "--force", "--", &relative_string])
            .map_err(|detail| format!("Folder restored, but submodule checkout failed: {detail}"))?;
    }
    if !clean_relative.is_empty() {
        let mut args = vec!["clean".to_string(), "-fd".to_string(), "--".to_string()];
        args.extend(clean_relative);
        git_owned(&repository_path, args).map_err(|detail| format!("Folder restored, but clean failed: {detail}"))?;
    }
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path);
    perf_log("restore_folder: TOTAL", started.elapsed());
    Ok(())
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
    resolve_submodule_boundary_from(&submodules, repository_path, relative_path)
}

fn resolve_submodule_boundary_with_path(repository_path: &str, relative_path: &str) -> Option<(String, String, String)> {
    let (_, submodules) = cached_index_metadata(repository_path);
    let submodule_path = submodules.iter().find(|sub| relative_path == sub.as_str() || relative_path.starts_with(&format!("{sub}/")))?.clone();
    let absolute_sub = Path::new(repository_path).join(&submodule_path);
    let sub_path_string = absolute_sub.to_string_lossy().into_owned();
    let inner_relative = if relative_path == submodule_path { String::new() } else { relative_path[submodule_path.len() + 1..].to_string() };
    Some((submodule_path, sub_path_string, inner_relative))
}

// Same lookup, taking an already-fetched submodules set — `cached_index_metadata`
// returns an owned *clone* of its (tracked, submodules) HashSets on every call
// (needed since callers mutate/hold their own copy elsewhere), and `tracked` can
// have hundreds of thousands of entries on a large repository. Calling
// `resolve_submodule_boundary` once per file in a loop (partition_by_submodule,
// for every path passed to Stage all / Unstage all) cloned that entire set once
// per file — for 245 files that's 245 full clones of the tracked-path set before
// any of the real staging work even starts, and it happens before the first
// perf_log call, so it never showed up in the timing logs either. Fetching the
// metadata once per *call* (not per file) and sharing it by reference here
// fixes both.
fn resolve_submodule_boundary_from(submodules: &HashSet<String>, repository_path: &str, relative_path: &str) -> Option<(String, String)> {
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
        // Same reasoning as load_directory: DirEntry::metadata() reuses what
        // the directory enumeration already returned instead of paying for
        // an extra per-file system call.
        let metadata = item.metadata().map_err(|error| error.to_string())?;
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

// The submodule-publish-safety report's point 1: this used to also call
// record_pushed_submodule_in_parent right here, unconditionally — an actual
// commit_selected_internal call into the *parent* — whether or not anything
// had ever been pushed anywhere. That is exactly how "Publish main project"
// could end up pushing a parent commit whose gitlink pointed at a submodule
// commit that existed only on this machine: nothing forced the safe order
// (submodule commit -> submodule push -> parent commit -> publish), so a
// habitual "commit, then immediately Publish" click could ship an
// unrecoverable parent history without ever pushing the submodule at all.
// Committing here now only ever touches the submodule itself. `also_push`
// (point 1 asks to keep this off by default; the app wires the "Commit
// submodule" action to pass true, so the common case still only takes one
// click) optionally chains an immediate push attempt via push_submodule_inner
// right after — reusing its already-tested logic rather than duplicating it —
// and, only on that push's own success, its tail stages (never commits) the
// parent's gitlink. A commit here always succeeds and returns Ok on its own
// merits; a failed or skipped push is reported in the result, never as an
// error that would make the caller think the commit itself failed.
#[derive(Serialize, Debug)]
pub struct CommitSubmoduleResult {
    revision: String,
    pushed: bool,
    // Set only when `pushed` is true.
    branch: Option<String>,
    // Always set — the human-readable reason `pushed` is what it is, so the
    // caller never has to guess (see the misleading-success-message point of
    // the report this fixes).
    push_detail: String,
}

#[tauri::command]
pub async fn commit_submodule(repository_path: String, relative_path: String, message: String, also_push: bool) -> Result<CommitSubmoduleResult, String> {
    off_main_thread(move || commit_submodule_inner(repository_path, relative_path, message, also_push)).await
}

fn commit_submodule_inner(repository_path: String, relative_path: String, message: String, also_push: bool) -> Result<CommitSubmoduleResult, String> {
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    // Held only for the submodule's own commit, released at the end of this
    // block — the parent's own lock, if this reaches push_submodule_inner
    // below, is only ever taken after that, inside its own tail, never nested
    // with this one (see stage_files_inner's comment for why that matters).
    let oid = {
        let queue_started = Instant::now();
        let lock_handle = repo_write_lock(&sub_path);
        let _lock = lock_handle.lock().unwrap();
        log_repo_write_lock_acquired(&sub_path, "commit_submodule", queue_started.elapsed());
        let repo = internal_submodule_repository(&absolute)?;
        if repo.head_detached().unwrap_or(true) {
            let short = repo.head().ok()
                .and_then(|head| head.target())
                .map(|oid| {
                    let value = oid.to_string();
                    value[..8.min(value.len())].to_string()
                })
                .unwrap_or_else(|| "unknown".into());
            return Err(format!("Commit unavailable — this submodule is detached at {short}. Create a new branch from this commit, or checkout an existing branch, then commit. This keeps the new commit pushable and avoids losing track of it."));
        }
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
        repo.commit(Some("HEAD"), &signature, &signature, message.trim(), &tree, &parents).map_err(|error| error.message().to_string())?
    };
    invalidate_git_metadata(&sub_path);
    invalidate_submodule_sync(&repository_path); // this app just changed the submodule's own commit
    if !also_push {
        return Ok(CommitSubmoduleResult {
            revision: oid.to_string(), pushed: false, branch: None,
            push_detail: "Local submodule commit — push the submodule before publishing the main project.".into(),
        });
    }
    match push_submodule_inner(repository_path, relative_path) {
        Ok(pushed) => Ok(CommitSubmoduleResult {
            revision: oid.to_string(), pushed: true, branch: Some(pushed.branch.clone()),
            push_detail: format!("Pushed to origin/{}. The parent's reference to it was staged — commit the parent when you're ready to share that.", pushed.branch),
        }),
        Err(detail) => Ok(CommitSubmoduleResult {
            revision: oid.to_string(), pushed: false, branch: None,
            push_detail: format!("Committed locally, but not pushed: {detail}\n\nPush the submodule before publishing the main project."),
        }),
    }
}

#[derive(Serialize, Debug)]
pub struct PushSubmoduleResult { revision: String, branch: String }

// A push destination must be the submodule's *currently checked-out local
// branch*.  A detached HEAD has a commit but no branch: guessing main, a
// .gitmodules branch, or origin/HEAD combines one ref's name with another
// ref's SHA and can publish the commit to the wrong branch.  Keep this single
// resolver shared by preview and push so their destination can never differ.
fn resolve_submodule_push_branch(repo: &Repository) -> Result<String, String> {
    let head = repo.head().map_err(|error| error.message().to_string())?;
    let revision = head.target().map(|oid| oid.to_string()).unwrap_or_default();
    if repo.head_detached().unwrap_or(true) {
        let short = &revision[..8.min(revision.len())];
        return Err(format!("Push unavailable — detached HEAD at {short}. A detached commit is not on a branch. Use \"Change version\" to switch to a branch, or \"New branch\" to create one from this commit, then push."));
    }
    head.shorthand().map(String::from).filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Could not determine this submodule's current branch. Switch to a local branch before pushing.".into())
}

#[derive(Debug)]
struct BranchRemoteTarget {
    remote_name: String,
    remote_branch: String,
    tracking_ref: String,
    display: String,
    remote_url: String,
    configured_upstream: bool,
}

// Resolve one authoritative destination for every submodule operation. A
// configured upstream may use a remote other than `origin` and a remote branch
// whose name differs from the local branch; preview, push, force-push and pull
// must never silently disagree about that destination. Only a branch with no
// upstream falls back to origin/<local-branch>, which a successful first push
// then persists with --set-upstream.
fn resolve_submodule_remote_target(repo: &Repository, local_branch: &str) -> Result<BranchRemoteTarget, String> {
    let config = repo.config().map_err(|error| error.message().to_string())?;
    let remote_key = format!("branch.{local_branch}.remote");
    let merge_key = format!("branch.{local_branch}.merge");
    let configured_remote = config.get_string(&remote_key).ok().filter(|value| !value.trim().is_empty());
    let configured_merge = config.get_string(&merge_key).ok().filter(|value| !value.trim().is_empty());

    let (remote_name, remote_branch, configured_upstream) = match (configured_remote, configured_merge) {
        (Some(remote), Some(merge)) => {
            if remote == "." {
                return Err(format!("Branch '{local_branch}' tracks another local branch, not a pushable remote. Configure a real remote upstream first."));
            }
            let remote_branch = merge.strip_prefix("refs/heads/")
                .ok_or_else(|| format!("Branch '{local_branch}' has an unsupported upstream ref '{merge}'"))?;
            (remote, remote_branch.to_string(), true)
        }
        (None, None) => ("origin".to_string(), local_branch.to_string(), false),
        _ => return Err(format!("Branch '{local_branch}' has an incomplete upstream configuration. Set both its remote and merge branch before synchronizing.")),
    };
    let remote_url = repo.find_remote(&remote_name).ok()
        .and_then(|remote| remote.url().map(String::from))
        .ok_or_else(|| format!("Remote '{remote_name}' is not configured with a URL for this submodule"))?;
    let display = format!("{remote_name}/{remote_branch}");
    Ok(BranchRemoteTarget {
        tracking_ref: format!("refs/remotes/{remote_name}/{remote_branch}"),
        remote_name,
        remote_branch,
        display,
        remote_url,
        configured_upstream,
    })
}

// Local test fixtures sometimes use another working checkout as `origin`.
// Git intentionally refuses to update a branch that is checked out in a
// non-bare destination because its index/worktree would no longer match the
// ref.  Detect that topology before push so the UI explains the setup error
// rather than suggesting force-push (which cannot safely fix it).  Bare local
// remotes and ordinary SSH/HTTPS remotes are unaffected.
fn local_worktree_remote_block(sub_path: &Path, remote_url: &str, remote_name: &str, branch: &str) -> Option<String> {
    let remote_path = if let Some(path) = remote_url.strip_prefix("file://") {
        PathBuf::from(path)
    } else {
        // Treat URI/scp-style values as network remotes. On Windows,
        // Path::is_absolute correctly recognizes drive-letter paths.
        if remote_url.contains("://") || (remote_url.contains(':') && !Path::new(remote_url).is_absolute()) { return None; }
        let path = PathBuf::from(remote_url);
        if path.is_absolute() { path } else { sub_path.join(path) }
    };
    let remote_repo = Repository::open(remote_path).ok()?;
    if remote_repo.is_bare() || remote_repo.head_detached().unwrap_or(true) { return None; }
    let checked_out = remote_repo.head().ok()?.shorthand()?.to_string();
    if checked_out != branch { return None; }
    Some(format!("Push blocked safely: {remote_name} is a local working repository with branch \"{branch}\" checked out. Git cannot update that branch without making the destination worktree inconsistent. Use a bare local repository as the remote, or configure this submodule's remote to its real Git server URL. Force push will not fix this setup."))
}

#[derive(Serialize, Debug)]
pub struct SubmodulePushPreview {
    branch: String,
    local_sha: String,
    remote_url: String,
    // Some("<remote>/<branch>") when the local branch has a real configured
    // upstream (branch.<name>.remote/.merge) — None otherwise, even if a
    // same-named remote branch happens to exist (see remote_sha/
    // will_create_remote_branch for that case).
    upstream: Option<String>,
    // The commit compared against — the configured upstream's tip if there is
    // one, else origin/<branch>'s tip if that ref exists at all, else None.
    remote_sha: Option<String>,
    ahead: usize,
    behind: usize,
    // True only when there is neither a configured upstream nor any
    // same-named ref on origin yet — pushing will create a brand new remote
    // branch, not update an existing one.
    will_create_remote_branch: bool,
    // Built from the same comparison target as `ahead`/`behind`. Keeping the
    // list in this response prevents the frontend from combining this preview
    // with entry_details, whose generic "unpushed" calculation intentionally
    // requires a configured upstream and therefore cannot describe a first
    // `git push -u` to an already-existing origin/<branch>.
    commits: Vec<PublishCommit>,
    // Push eligibility is a backend decision, not `commits.len() > 0`: a new
    // remote branch can be created even when every object is already present
    // through another ref, while a diverged branch must not offer normal push.
    can_push: bool,
    blocked_reason: Option<String>,
}

#[tauri::command]
pub async fn push_submodule_preview(repository_path: String, relative_path: String) -> Result<SubmodulePushPreview, String> {
    off_main_thread(move || push_submodule_preview_inner(repository_path, relative_path)).await
}

fn push_submodule_preview_inner(repository_path: String, relative_path: String) -> Result<SubmodulePushPreview, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let repo = internal_submodule_repository(&absolute)?;
    let local_target = repo.head().ok().and_then(|head| head.target()).ok_or("Could not determine this submodule's current commit")?;
    let branch = resolve_submodule_push_branch(&repo)?;
    let destination = resolve_submodule_remote_target(&repo, &branch)?;

    // Best-effort — same as the actual push's own pre-flight fetch — so the
    // comparison reflects what's really on the server right now, not
    // whatever this app last happened to know. A failure here (offline) just
    // falls back to already-known local refs instead of blocking the preview.
    let _ = git(&sub_path, &["fetch", &destination.remote_name]);

    let upstream = destination.configured_upstream.then(|| destination.display.clone());
    let remote_sha = repo.find_reference(&destination.tracking_ref).ok().and_then(|reference| reference.target());
    let (ahead, behind) = match remote_sha {
        Some(remote_oid) => repo.graph_ahead_behind(local_target, remote_oid).map_err(|error| error.message().to_string())?,
        None => (0, 0),
    };
    let commits = match remote_sha {
        Some(remote_oid) if remote_oid != local_target => {
            let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?;
            walk.push(local_target).map_err(|error| error.message().to_string())?;
            walk.hide(remote_oid).map_err(|error| error.message().to_string())?;
            let mut commits: Vec<PublishCommit> = walk.take(50).flatten().filter_map(|oid| repo.find_commit(oid).ok().map(|commit| PublishCommit {
                id: oid.to_string(), subject: commit.summary().unwrap_or("No message").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()),
            })).collect();
            commits.reverse();
            commits
        }
        _ => Vec::new(),
    };
    let will_create_remote_branch = remote_sha.is_none();
    let local_remote_block = local_worktree_remote_block(&absolute, &destination.remote_url, &destination.remote_name, &destination.remote_branch);
    let (can_push, blocked_reason) = if let Some(message) = local_remote_block {
        (false, Some(message))
    } else if will_create_remote_branch {
        (true, None)
    } else if ahead == 0 && behind == 0 {
        (false, Some(format!("Already up to date with {}.", destination.display)))
    } else if behind > 0 {
        let message = if ahead > 0 {
            format!("Local {branch} and {} have diverged ({ahead} ahead, {behind} behind). Fetch, review and merge before pushing.", destination.display)
        } else {
            format!("Local {branch} is behind {} by {behind} commit{}. Pull or merge before pushing.", destination.display, if behind == 1 { "" } else { "s" })
        };
        (false, Some(message))
    } else {
        (ahead > 0, None)
    };
    Ok(SubmodulePushPreview {
        branch, local_sha: local_target.to_string(), remote_url: destination.remote_url,
        will_create_remote_branch, upstream, remote_sha: remote_sha.map(|oid| oid.to_string()), ahead, behind,
        commits, can_push, blocked_reason,
    })
}

#[tauri::command]
pub async fn push_submodule(repository_path: String, relative_path: String) -> Result<PushSubmoduleResult, String> {
    off_main_thread(move || push_submodule_inner(repository_path, relative_path)).await
}

fn push_submodule_inner(repository_path: String, relative_path: String) -> Result<PushSubmoduleResult, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    // Held for the submodule's own fetch+push (a concurrent local write on
    // this same submodule — a commit, "Reset submodule" — must queue behind
    // it, not race it) — released below before stage_pushed_submodule_in_parent
    // takes the *parent's* lock, never nested with it.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&sub_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&sub_path, "push_submodule", queue_started.elapsed());
    let repo = internal_submodule_repository(&absolute)?;
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

    let branch = resolve_submodule_push_branch(&repo)?;
    let destination = resolve_submodule_remote_target(&repo, &branch)?;
    // Use the system `git` binary (not libgit2) for network operations here: it
    // transparently reuses the user's already-working SSH agent, credential helper,
    // and OS keychain, instead of libgit2's much narrower built-in credential search
    // — which is what produced "failed to acquire username/password" even though a
    // plain `git push` in a terminal works fine for the same repository.
    let _ = git(&sub_path, &["fetch", &destination.remote_name]);
    if let Some(message) = local_worktree_remote_block(&absolute, &destination.remote_url, &destination.remote_name, &destination.remote_branch) { return Err(message); }

    let remote_target = repo.find_reference(&destination.tracking_ref).ok().and_then(|reference| reference.target());
    if remote_target.is_some() && remote_target == local_target {
        return Err(format!("Nothing to push — this submodule has no commits ahead of {}. Commit your changes in the submodule first.", destination.display));
    }
    let push_refspec = format!("HEAD:refs/heads/{}", destination.remote_branch);
    let push_result = if destination.configured_upstream {
        git(&sub_path, &["push", &destination.remote_name, &push_refspec])
    } else {
        git(&sub_path, &["push", "--set-upstream", &destination.remote_name, &push_refspec])
    };
    push_result.map_err(|detail| {
        if detail.contains("non-fast-forward") || detail.contains("[rejected]") || detail.contains("fetch first") {
            format!("Push rejected — {} has commits you don't have locally (someone else pushed there, or it moved since the last fetch). Fetch the submodule, review/merge the new commits, then push again — or, if you're the only one using this remote, use \"Force push submodule\" to overwrite it.\n\nGit's message: {detail}", destination.display)
        } else { format!("Push failed: {detail}") }
    })?;

    // `dirty`/status checks above ran against the submodule's own cached status
    // entries (keyed by `sub_path`), separate from the parent's cache that
    // `stage_pushed_submodule_in_parent` invalidates below — without this, opening
    // the submodule as its own repository view right after a push could still show
    // its pre-push status for up to the cache's TTL.
    invalidate_git_metadata(&sub_path);
    invalidate_submodule_sync(&repository_path); // this app just changed the submodule's own commit
    drop(repo); drop(_lock); // fully released before the parent's own lock, never nested
    stage_pushed_submodule_in_parent(&repository_path, &relative_path)?;
    Ok(PushSubmoduleResult { revision: local_target.map(|oid| oid.to_string()).unwrap_or_default(), branch })
}

// Point 2 of the submodule-publish-safety report: once a commit is safely on
// the submodule's own server, the parent's gitlink is out of date — stage the
// new one so the *next* explicit parent commit picks it up, exactly like
// editing any other tracked file. Never commits the parent itself: silently
// creating a parent commit here (this used to call commit_selected_internal
// directly) is exactly what let "Publish main project" push a parent commit
// whose gitlink referenced a still-local submodule commit before, since
// nothing forced the two operations into the safe order. Reuses
// stage_files_inner's own submodule-HEAD-vs-index detection (see its comment)
// rather than reimplementing it — that mechanism already exists precisely to
// let a submodule's moved-on HEAD be staged as an explicit action.
fn stage_pushed_submodule_in_parent(repository_path: &str, relative_path: &str) -> Result<(), String> {
    match stage_files_inner(repository_path, vec![relative_path.to_string()]) {
        Ok(_) => Ok(()),
        Err(message) => Err(format!("Pushed successfully, but could not stage the parent's reference to the new commit: {message}")),
    }
}

#[tauri::command]
pub async fn force_push_submodule(repository_path: String, relative_path: String) -> Result<PushSubmoduleResult, String> {
    off_main_thread(move || force_push_submodule_inner(repository_path, relative_path)).await
}

fn force_push_submodule_inner(repository_path: String, relative_path: String) -> Result<PushSubmoduleResult, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    // See push_submodule's own comment — same submodule-then-parent, never
    // nested, lock ordering.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&sub_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&sub_path, "force_push_submodule", queue_started.elapsed());
    let repo = internal_submodule_repository(&absolute)?;
    let local_target = repo.head().ok().and_then(|head| head.target());

    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() {
        let files: Vec<String> = dirty.iter().take(5).map(|(path, status, _)| format!("{status} {path}")).collect();
        let more = if dirty.len() > 5 { format!(" (+{} more)", dirty.len() - 5) } else { String::new() };
        return Err(format!("This submodule has uncommitted changes that will NOT be pushed:\n{}{more}\n\nCommit them first, then push.", files.join("\n")));
    }
    let branch = resolve_submodule_push_branch(&repo)?;
    let destination = resolve_submodule_remote_target(&repo, &branch)?;
    // Fetch first so the lease below is checked against the freshest known
    // state of origin/<branch>, not whatever this app last happened to see —
    // matches push_submodule_inner's own pre-flight fetch.
    let _ = git(&sub_path, &["fetch", &destination.remote_name]);
    // Push-submodule-workflow report, point 1: --force-with-lease, never raw
    // --force. Raw --force overwrites origin/<branch> unconditionally, even
    // if it moved again since the last time this app looked — exactly the
    // "someone else pushed there" case this whole action exists to recover
    // from, so blindly clobbering it a second time would be the same mistake
    // it's meant to fix. --force-with-lease still refuses if the remote isn't
    // where this app last observed it (via the fetch just above), while
    // succeeding for the one case this command is actually for: nobody else
    // is using that remote and it's exactly what was just fetched.
    if let Some(message) = local_worktree_remote_block(&absolute, &destination.remote_url, &destination.remote_name, &destination.remote_branch) { return Err(message); }
    let push_refspec = format!("HEAD:refs/heads/{}", destination.remote_branch);
    let force_result = if destination.configured_upstream {
        git(&sub_path, &["push", "--force-with-lease", &destination.remote_name, &push_refspec])
    } else {
        git(&sub_path, &["push", "--force-with-lease", "--set-upstream", &destination.remote_name, &push_refspec])
    };
    force_result.map_err(|detail| format!("Force push to {} failed: {detail}", destination.display))?;

    invalidate_git_metadata(&sub_path);
    invalidate_submodule_sync(&repository_path); // this app just changed the submodule's own commit
    drop(repo); drop(_lock); // fully released before the parent's own lock, never nested
    stage_pushed_submodule_in_parent(&repository_path, &relative_path)?;
    Ok(PushSubmoduleResult { revision: local_target.map(|oid| oid.to_string()).unwrap_or_default(), branch })
}

#[tauri::command]
pub async fn fetch_submodule(repository_path: String, relative_path: String) -> Result<(), String> {
    off_main_thread(move || fetch_submodule_inner(repository_path, relative_path)).await
}

fn fetch_submodule_inner(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    let repo = internal_submodule_repository(&absolute)?;
    repo.find_remote("origin").map_err(|_| "No 'origin' remote configured for this submodule".to_string())?;
    git(&sub_path, &["fetch", "origin"])?;
    invalidate_git_metadata(&sub_path);
    invalidate_submodule_sync(&repository_path); // this app just changed the submodule's own commit
    Ok(())
}

#[derive(Serialize)]
pub struct FetchProjectResult {
    parent_fetched: bool,
    submodules_total: usize,
    submodules_fetched: usize,
    submodules_skipped: usize,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[tauri::command]
pub async fn fetch_project(repository_path: String) -> Result<FetchProjectResult, String> {
    off_main_thread(move || fetch_project_inner(repository_path)).await
}

fn fetch_project_inner(repository_path: String) -> Result<FetchProjectResult, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let submodule_paths = repo
        .submodules()
        .map_err(|error| error.message().to_string())?
        .into_iter()
        .map(|submodule| normalized(submodule.path()))
        .collect::<Vec<_>>();
    drop(repo);

    let mut result = FetchProjectResult {
        parent_fetched: false,
        submodules_total: submodule_paths.len(),
        submodules_fetched: 0,
        submodules_skipped: 0,
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    match fetch_all_remotes_inner(repository_path.clone()) {
        Ok(()) => result.parent_fetched = true,
        Err(error) => result.errors.push(format!("Parent repository: {error}")),
    }

    for relative_path in submodule_paths {
        let absolute = Path::new(&repository_path).join(&relative_path);
        let sub_path = absolute.to_string_lossy().into_owned();
        let fetch_result = internal_submodule_repository(&absolute).and_then(|repo| {
            repo.find_remote("origin")
                .map_err(|_| "No 'origin' remote configured for this submodule".to_string())?;
            git(&sub_path, &["fetch", "origin"])?;
            Ok(())
        });
        match fetch_result {
            Ok(()) => {
                result.submodules_fetched += 1;
                invalidate_git_metadata(&sub_path);
            }
            Err(error) if error.contains("not initialized") || error.contains("No 'origin' remote") => {
                result.submodules_skipped += 1;
                result.warnings.push(format!("{relative_path}: {error}"));
            }
            Err(error) => result.errors.push(format!("{relative_path}: {error}")),
        }
    }

    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path);
    Ok(result)
}

#[tauri::command]
pub async fn pull_submodule(repository_path: String, relative_path: String) -> Result<(), String> {
    off_main_thread(move || pull_submodule_inner(repository_path, relative_path)).await
}

fn pull_submodule_inner(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    // Fast-forwards the submodule's local branch ref and checks out the new
    // tree — a real local write, unlike a plain fetch.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&sub_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&sub_path, "pull_submodule", queue_started.elapsed());
    let repo = internal_submodule_repository(&absolute)?;

    let dirty = internal_statuses(&repo, None)?;
    if !dirty.is_empty() {
        return Err("This submodule has uncommitted changes. Commit or discard them before pulling, so a fast-forward can't overwrite anything.".into());
    }
    if repo.head_detached().unwrap_or(true) {
        return Err("This submodule is in detached HEAD (not on a branch), so there is nothing to pull into. Use \"Change version\" to switch to a branch first.".into());
    }
    let branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).ok_or("Could not determine the current branch")?;
    let destination = resolve_submodule_remote_target(&repo, &branch)?;
    git(&sub_path, &["fetch", &destination.remote_name])?;

    let remote_ref = repo.find_reference(&destination.tracking_ref).map_err(|error| format!("{} not found after fetch: {}", destination.display, error.message()))?;
    let target = remote_ref.target().ok_or_else(|| format!("{} has no commits", destination.display))?;
    let annotated = repo.find_annotated_commit(target).map_err(|error| error.message().to_string())?;
    let (analysis, _) = repo.merge_analysis(&[&annotated]).map_err(|error| error.message().to_string())?;
    if analysis.is_up_to_date() { return Err(format!("Already up to date with {}.", destination.display)); }
    if !analysis.is_fast_forward() {
        return Err(format!("Cannot fast-forward — your local commit(s) and {} have diverged (both have commits the other doesn't). Keep both histories with the app's \"Merge branch…\" action, or deliberately replace the local branch with the remote version via \"Change version\" → \"Replace with remote…\".", destination.display));
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
    invalidate_submodule_sync(&repository_path); // this app just changed the submodule's own commit
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

    fn run_git_capture(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git").arg("-C").arg(path).args(args).output().unwrap();
        assert!(output.status.success(), "git command failed: {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
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

    // The two tests below are the only ones that touch RECENT_GIT_COMMANDS
    // (a process-global static shared across this whole test binary's
    // threads) — serialized against *each other* only, so the tight,
    // in-memory eviction loop in the second can never race the slow,
    // real-subprocess first one out of the buffer before it gets read back.
    // Without this, that's a genuine intermittent flake: 50+ fast pushes
    // easily fit inside the time a real `git` child process takes to spawn
    // and exit. unwrap_or_else recovers from poisoning instead of letting a
    // panic in one cascade into a spurious failure in the other.
    static RECENT_GIT_COMMANDS_TEST_LOCK: Mutex<()> = Mutex::new(());

    // The status bar's own quiet command hint and its double-click history —
    // report: show the real git commands the app runs, not just its own
    // human-readable status messages. It includes the shared git() helper,
    // the Git-only command box, and Terminal/App Actions commands that start
    // with `git`; non-Git shell commands stay in the Terminal transcript.
    #[test]
    fn git_helper_calls_are_recorded_for_the_status_bars_own_command_history() {
        let _guard = RECENT_GIT_COMMANDS_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-recent-commands-{suffix}"));
        create_libgit2_repository(&repository, "file.txt");
        let repo_path = repository.to_string_lossy().into_owned();

        // A unique, unmistakable subcommand — real fixture helpers elsewhere
        // (run_git/run_git_capture) call the system git binary directly, not
        // through this app's own git() helper, so this cannot pick up noise
        // from any other concurrently-running test doing ordinary setup.
        git(&repo_path, &["rev-parse", "--is-inside-work-tree"]).unwrap();

        let recorded = recent_git_commands();
        let mine = recorded.iter().find(|entry| entry.command == "git rev-parse --is-inside-work-tree" && entry.repo_hint == repository.file_name().unwrap().to_str().unwrap())
            .expect("the git() call above must be recorded, with the repository's own directory name as its hint");
        assert!(mine.success, "a command that actually succeeded must be recorded as such");
        assert!(mine.seconds_ago >= 0.0 && mine.seconds_ago < 30.0, "should report as just having happened, got {}s ago", mine.seconds_ago);

        let command_box = run_git_command(repo_path.clone(), "rev-parse --show-toplevel".into()).unwrap();
        assert!(command_box.success);
        assert!(recent_git_commands().iter().any(|entry| entry.command == "git rev-parse --show-toplevel" && entry.repo_hint == repository.file_name().unwrap().to_str().unwrap()),
            "Git commands launched through the command box must also show up in the footer history");

        let terminal = run_terminal_command_inner(repo_path.clone(), "git rev-parse --is-bare-repository".into()).unwrap();
        assert!(terminal.success);
        assert!(recent_git_commands().iter().any(|entry| entry.command == "git rev-parse --is-bare-repository" && entry.repo_hint == repository.file_name().unwrap().to_str().unwrap()),
            "Git commands launched through Terminal/App Actions must also show up in the footer history");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn recent_git_commands_never_grows_past_its_bound() {
        let _guard = RECENT_GIT_COMMANDS_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // Exercises record_git_command directly (not real subprocesses) —
        // this only needs to prove the bound holds, not re-prove git() itself
        // works, and avoids spawning dozens of real git processes plus
        // adding that much unrelated noise to the same process-global store
        // every other test in this file also shares.
        for _ in 0..(RECENT_GIT_COMMANDS_LIMIT + 20) {
            record_git_command("/tmp/does-not-need-to-exist", &["status"], true);
        }
        assert!(recent_git_commands_store().lock().unwrap().len() <= RECENT_GIT_COMMANDS_LIMIT, "must never grow without bound across a long session");
    }

    #[test]
    fn blocked_branch_switch_keeps_head_index_and_worktree_on_the_original_branch() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-safe-switch-{suffix}"));
        create_libgit2_repository(&repository, "shared.txt");
        let original_branch = run_git_capture(&repository, &["branch", "--show-current"]);
        run_git(&repository, &["switch", "-c", "other"]);
        fs::write(repository.join("shared.txt"), "other branch\n").unwrap();
        run_git(&repository, &["commit", "-am", "Other branch version"]);
        run_git(&repository, &["switch", &original_branch]);

        fs::write(repository.join("shared.txt"), "important local edit\n").unwrap();
        let index_before = run_git_capture(&repository, &["write-tree"]);
        let result = switch_branch(repository.to_string_lossy().into_owned(), "other".into());
        assert!(result.is_err(), "a conflicting local edit must block the switch");
        assert_eq!(run_git_capture(&repository, &["branch", "--show-current"]), original_branch, "HEAD must stay attached to the original branch when checkout fails");
        assert_eq!(run_git_capture(&repository, &["write-tree"]), index_before, "the index must not be rebased against a branch that was never checked out");
        assert_eq!(fs::read_to_string(repository.join("shared.txt")).unwrap(), "important local edit\n", "the user's worktree content must remain untouched");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn folder_restore_from_old_commit_is_path_scoped_and_never_moves_head() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-folder-restore-{suffix}"));
        fs::create_dir_all(repository.join("folder")).unwrap();
        fs::write(repository.join(".gitignore"), "folder/ignored.log\n").unwrap();
        fs::write(repository.join("folder/file1.txt"), "A1\n").unwrap();
        fs::write(repository.join("folder/file2.txt"), "A2\n").unwrap();
        fs::write(repository.join("outside.txt"), "outside A\n").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "A folder base"]);
        let commit_a = run_git_capture(&repository, &["rev-parse", "HEAD"]);

        fs::write(repository.join("folder/file1.txt"), "B1\n").unwrap();
        run_git(&repository, &["commit", "-am", "B modifies file1"]);
        fs::write(repository.join("outside.txt"), "outside C\n").unwrap();
        run_git(&repository, &["commit", "-am", "C unrelated outside folder"]);
        fs::write(repository.join("folder/file2.txt"), "D2\n").unwrap();
        fs::write(repository.join("folder/file3.txt"), "D3\n").unwrap();
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "D modifies folder"]);
        let head_before = run_git_capture(&repository, &["rev-parse", "HEAD"]);
        let branch_before = run_git_capture(&repository, &["branch", "--show-current"]);

        fs::write(repository.join("folder/file1.txt"), "staged local\n").unwrap();
        run_git(&repository, &["add", "folder/file1.txt"]);
        fs::write(repository.join("folder/file2.txt"), "unstaged local\n").unwrap();
        fs::write(repository.join("folder/temp.txt"), "remove me\n").unwrap();
        fs::write(repository.join("folder/ignored.log"), "keep me\n").unwrap();

        let repo_string = repository.to_string_lossy().into_owned();
        let preview = folder_restore_preview_inner(&repo_string, "folder", &commit_a, true).unwrap();
        assert!(preview.clean_candidates.iter().any(|path| path == "folder/temp.txt"), "untracked temp file must be previewed for clean: {:?}", preview.clean_candidates);
        assert!(!preview.clean_candidates.iter().any(|path| path == "folder/ignored.log"), "ignored files must not be cleaned by default");
        assert!(!preview.tracked_changes.iter().any(|change| change.status == "??"), "untracked items belong in clean preview, not tracked changes");

        restore_folder_inner(repo_string, "folder".into(), commit_a, preview.clean_candidates).unwrap();
        assert_eq!(run_git_capture(&repository, &["rev-parse", "HEAD"]), head_before, "restore must not move HEAD");
        assert_eq!(run_git_capture(&repository, &["branch", "--show-current"]), branch_before, "restore must not switch branches");
        assert_eq!(fs::read_to_string(repository.join("folder/file1.txt")).unwrap(), "A1\n");
        assert_eq!(fs::read_to_string(repository.join("folder/file2.txt")).unwrap(), "A2\n");
        assert!(!repository.join("folder/file3.txt").exists(), "tracked files absent in the source snapshot should be removed from the folder");
        assert!(!repository.join("folder/temp.txt").exists(), "confirmed untracked clean candidate should be removed");
        assert_eq!(fs::read_to_string(repository.join("folder/ignored.log")).unwrap(), "keep me\n", "ignored file must remain untouched");
        assert_eq!(fs::read_to_string(repository.join("outside.txt")).unwrap(), "outside C\n", "unrelated paths must be untouched");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn folder_restore_updates_nested_submodule_worktrees_to_the_recorded_revision() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-folder-restore-submodule-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dependency).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dependency.join("module.txt"), "dep v1\n").unwrap();
        for path in [&repository, &dependency] {
            run_git(path, &["init", "-b", "main"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }

        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "folder/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);
        let checkpoint = run_git_capture(&repository, &["rev-parse", "HEAD"]);
        let sub_path = repository.join("folder/dep");
        let checkpoint_sub = run_git_capture(&sub_path, &["rev-parse", "HEAD"]);

        fs::write(sub_path.join("module.txt"), "dep v2\n").unwrap();
        run_git(&sub_path, &["commit", "-am", "Submodule v2"]);
        assert_ne!(run_git_capture(&sub_path, &["rev-parse", "HEAD"]), checkpoint_sub);
        assert!(!refresh_status_inner(repository.to_string_lossy().into_owned()).unwrap().is_empty(), "advancing only the submodule checkout should make the parent folder look modified");

        restore_folder_inner(repository.to_string_lossy().into_owned(), "folder".into(), checkpoint, Vec::new()).unwrap();
        assert_eq!(run_git_capture(&sub_path, &["rev-parse", "HEAD"]), checkpoint_sub, "folder restore must move nested submodule worktrees to the gitlink recorded by the selected source");
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "dep v1\n");
        assert!(refresh_status_inner(repository.to_string_lossy().into_owned()).unwrap().is_empty(), "parent repo should be clean after restoring the folder and its nested submodule");

        fs::remove_dir_all(base).unwrap();
    }

    // Test-only: rewrites .gitmodules' recorded `url =` line(s) to a fake
    // https:// address, without touching the submodule's own actual "origin"
    // remote (still whatever local bare repo the test already pushes/fetches
    // against — there's no real network in these tests). This is exactly the
    // real-world shape is_local_only_url exists to tell apart:
    // .gitmodules records the URL a *fresh* clone would use, while this one
    // checkout's own "origin" can legitimately be reconfigured to something
    // else entirely (a mirror, a cache — or, here, the local temp dir
    // standing in for "a real server" for testing purposes). Assumes exactly
    // one `[submodule]` block, matching every fixture that calls this.
    fn fake_https_gitmodules_url(repository_path: &Path) {
        let gitmodules_path = repository_path.join(".gitmodules");
        let content = fs::read_to_string(&gitmodules_path).unwrap();
        let updated: String = content.lines().map(|line| {
            if line.trim_start().starts_with("url =") { "\turl = https://example.test/repo.git".to_string() } else { line.to_string() }
        }).collect::<Vec<_>>().join("\n") + "\n";
        fs::write(&gitmodules_path, updated).unwrap();
    }

    // Test-only: deletes .gitmodules' `url =` line(s) entirely, simulating a
    // submodule with no URL recorded there at all (a hand-edited or
    // malformed .gitmodules) — distinct from fake_https_gitmodules_url above.
    // Assumes exactly one `[submodule]` block, matching every fixture that
    // calls this.
    fn remove_gitmodules_url_line(repository_path: &Path) {
        let gitmodules_path = repository_path.join(".gitmodules");
        let content = fs::read_to_string(&gitmodules_path).unwrap();
        let updated: String = content.lines().filter(|line| !line.trim_start().starts_with("url =")).collect::<Vec<_>>().join("\n") + "\n";
        fs::write(&gitmodules_path, updated).unwrap();
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
        assert!(!load_repository_inner(parent_string.clone(), Some(true)).unwrap().changes.iter().any(|change| change.path == "README.md"));
        let added = add_submodule_inner(parent_string.clone(), "components".into(), dependency.to_string_lossy().into_owned(), "engine".into(), String::new(), String::new()).unwrap();
        assert_eq!(added, "components/engine");
        assert!(parent.join(".gitmodules").exists());
        let repo = Repository::open(&parent).unwrap();
        assert_eq!(repo.index().unwrap().get_path(Path::new("components/engine"), 0).unwrap().mode, 0o160000);
        drop(repo);
        create_commit(parent_string.clone(), "P:89312 add engine".into()).unwrap();
        run_git(&parent.join("components/engine"), &["tag", "IMS.VITESCO.IO_HM_MHB01L04.00A"]);
        assert_eq!(entry_last_commit_inner(parent_string.clone(), "README.md".into()).unwrap().map(|c| c.subject), Some("Initial commit".to_string()));
        let engine_details = entry_details_inner(parent_string.clone(), "components/engine".into()).unwrap();
        assert_eq!(entry_last_commit_inner(parent_string.clone(), "components/engine".into()).unwrap().map(|c| c.subject), Some("P:89312 add engine".to_string()), "last-commit-touching-path must stay the parent's gitlink-bump commit");
        assert_eq!(engine_details.submodule_commit_subject.as_deref(), Some("Initial commit"), "submodule_commit_* must be the submodule's own HEAD commit, not the parent's");
        assert!(engine_details.submodule_commit_id.is_some());
        assert_eq!(engine_details.submodule_commit_tags, vec!["IMS.VITESCO.IO_HM_MHB01L04.00A".to_string()]);
        let versions = submodule_versions_inner(parent_string.clone(), added.clone()).unwrap();
        switch_submodule_version_inner(parent_string.clone(), added.clone(), versions.current_revision, "commit".into(), String::new()).unwrap();
        remove_git_path_inner(&parent_string, &added).unwrap();
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
        let entries = load_directory_inner(path.clone(), "".into(), None).unwrap();
        assert!(entries.iter().any(|entry| entry.relative_path == "src" && entry.kind == "folder"));
        assert!(entries.iter().any(|entry| entry.relative_path == "vendor" && entry.kind == "folder"));
        let cached_start = std::time::Instant::now();
        for _ in 0..100 { assert!(!load_directory_inner(path.clone(), "".into(), None).unwrap().is_empty()); }
        assert!(cached_start.elapsed().as_millis() < 1000, "cached navigation took {:?}", cached_start.elapsed());
        let nested = load_directory_inner(path.clone(), "vendor".into(), None).unwrap();
        assert!(nested.iter().any(|entry| entry.relative_path == "vendor/dependency" && entry.kind == "submodule"));
        let details = entry_details_inner(path, "vendor/dependency".into()).unwrap();
        assert_eq!(details.kind, "submodule");
        assert!(details.submodule_url.as_deref().unwrap_or_default().contains("dependency"));
        let versions = submodule_versions_inner(repository.to_string_lossy().into_owned(), "vendor/dependency".into()).unwrap();
        let release = versions.versions.iter().find(|version| version.name.ends_with("release/2.4")).unwrap();
        let switched = switch_submodule_version_inner(repository.to_string_lossy().into_owned(), "vendor/dependency".into(), release.revision.clone(), release.kind.clone(), release.name.clone()).unwrap();
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
        assert_eq!(submodule_repository_inner(repository.to_string_lossy().into_owned(), "vendor/dependency".into()).unwrap().repository.name, "dependency");
        assert_eq!(list_remotes(repository.to_string_lossy().into_owned()).unwrap().len(), 1);
        let cloned = clone_repository(dependency.to_string_lossy().into_owned(), base.to_string_lossy().into_owned(), "cloned-dependency".into(), None, None).unwrap();
        assert_eq!(load_repository_inner(cloned.clone(), Some(true)).unwrap().repository.name, "cloned-dependency");
        remove_git_path_inner(&cloned, "README.md").unwrap();
        assert!(!Path::new(&cloned).join("README.md").exists());
        assert!(git(&cloned, &["diff", "--cached", "--name-only"]).unwrap().lines().any(|path| path == "README.md"));
        assert_eq!(browser_repository_url("git@github.com:team/project.git").as_deref(), Some("https://github.com/team/project"));
        assert_eq!(browser_repository_url("https://gitlab.example/team/project.git").as_deref(), Some("https://gitlab.example/team/project"));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn editing_a_file_inside_a_submodule_through_the_editor_shows_up_as_modified_there() {
        // Reported bug: editing a file *inside* a submodule via the app's
        // built-in editor stopped showing that file (and the submodule
        // itself) as modified. Root cause: the editor's frontend always
        // calls write_text_file/read_text_file with the *parent's*
        // repository_path (it has no separate notion of "which repo" —
        // relative_path alone carries it into the submodule), so
        // write_text_file only ever invalidated the parent's own cached
        // status. The parent's own row for the submodule still updated
        // correctly (its cache was the one invalidated) — but the
        // submodule's *own*, separately cached view of itself (what the
        // Explorer shows once you actually navigate inside it) kept
        // serving the stale, pre-edit "clean" status for up to
        // GIT_METADATA_TTL.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-edit-in-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = parent.join(&added);
        let sub_path_string = sub_path.to_string_lossy().into_owned();

        // Warm the submodule's own cache with the pre-edit, clean status —
        // exactly what browsing into it in the Explorer would have already
        // done before the edit.
        let before = load_directory_inner(sub_path_string.clone(), "".into(), Some(true)).unwrap();
        let file_before = before.iter().find(|entry| entry.relative_path == "module.txt").expect("module.txt should be listed");
        assert_eq!(file_before.status, "", "should be clean before editing");
        let cached_before_edit = cached_submodule_state(&sub_path_string);
        assert!(!cached_before_edit.dirty, "seed the Explorer's consolidated submodule-state cache as clean too");

        // Edit it the same way the app's own editor does: repositoryPath is
        // always the *parent's*, relativePath crosses into the submodule.
        write_text_file(parent_string.clone(), format!("{added}/module.txt"), "edited content".into()).unwrap();

        // A fresh (unforced — this must not require the user to know to
        // force-refresh) listing of the submodule's own directory must now
        // show it as modified.
        let after = load_directory_inner(sub_path_string, "".into(), None).unwrap();
        let file_after = after.iter().find(|entry| entry.relative_path == "module.txt").expect("module.txt should still be listed");
        assert_eq!(file_after.status, "M", "editing the file must show up as modified when browsing inside the submodule itself, not just in the parent's own row for it");
        let parent_listing = load_directory_inner(parent_string, "".into(), None).unwrap();
        let submodule_after = parent_listing.iter().find(|entry| entry.relative_path == added).expect("submodule should still be listed in its parent");
        assert_eq!(submodule_after.submodule_state, "changes_inside", "the parent Explorer row must also drop the cached clean snapshot immediately after an in-app edit");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn externally_dirty_submodule_bypasses_stale_clean_submodule_snapshot() {
        // This is the real Explorer-row failure mode: the submodule-state
        // cache was warmed while the submodule was clean, then another tool
        // edited a file inside the submodule. A fresh parent status correctly
        // reports the gitlink row as changed, but reusing the stale clean
        // submodule snapshot made the row say "project push/synced" instead
        // of "changes inside". A changed submodule row must force one fresh
        // submodule inspection.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-external-dirty-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = parent.join(&added);
        let sub_path_string = sub_path.to_string_lossy().into_owned();

        let cached_before_edit = cached_submodule_state(&sub_path_string);
        assert!(!cached_before_edit.dirty, "sanity check: cache starts with a clean submodule snapshot");
        invalidate_git_metadata(&parent_string);
        fs::write(sub_path.join("module.txt"), "changed outside the app").unwrap();

        let parent_listing = load_directory_inner(parent_string, "".into(), None).unwrap();
        let submodule_after = parent_listing.iter().find(|entry| entry.relative_path == added).expect("submodule should be listed in parent");
        assert_eq!(submodule_after.status, "M");
        assert_eq!(submodule_after.submodule_state, "changes_inside", "a parent-visible dirty submodule must not reuse a stale clean submodule snapshot");
        assert!(submodule_after.submodule_is_dirty);

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

    // Restore project version (reset_submodule_inner) reads the target
    // commit from the parent's INDEX, on the documented assumption that a
    // clean fast-forward leaves the index matching the new HEAD tree for
    // every path, gitlinks included. Confirms merge_branch's fast-forward
    // path (checkout_head with force()) actually holds that assumption for
    // a submodule pointer specifically, not just ordinary files.
    #[test]
    fn fast_forward_merge_updates_the_index_gitlink_not_just_head() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-ff-gitlink-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep"]);
        let repo_path = repository.to_string_lossy().into_owned();
        let default_branch = Repository::open(&repository).unwrap().head().unwrap().shorthand().unwrap().to_string();
        run_git(&repository, &["switch", "-c", "feature"]);
        run_git(&repository, &["switch", &default_branch]);

        let sub_path = repository.join("vendor/dep");
        fs::write(sub_path.join("module.txt"), "bumped").unwrap();
        run_git(&sub_path, &["commit", "-am", "Bump"]);
        let bumped_oid = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap();
        run_git(&repository, &["add", "vendor/dep"]);
        run_git(&repository, &["commit", "-m", "Bump dep"]);

        run_git(&repository, &["switch", "feature"]);
        run_git(&repository, &["submodule", "update", "vendor/dep"]); // clean checkout matching feature's own pin, like a real user's working tree
        let outcome = merge_branch(repo_path.clone(), "".into(), default_branch.clone()).unwrap();
        assert_eq!(outcome.status, "fast_forwarded");

        let repo = Repository::open(&repository).unwrap();
        let head_oid = parent_gitlink_oid(&repo, "vendor/dep", false);
        let index_oid = parent_gitlink_oid(&repo, "vendor/dep", true);
        assert_eq!(head_oid, Some(bumped_oid), "HEAD's tree must show the bumped submodule after fast-forward");
        assert_eq!(index_oid, Some(bumped_oid), "the INDEX must also show the bumped submodule after fast-forward — this is what Restore project version actually reads");
        fs::remove_dir_all(base).unwrap();
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
        assert!(load_repository_inner(path, Some(true)).unwrap().changes.is_empty());

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn parent_merge_can_resolve_a_submodule_gitlink_conflict_by_choosing_one_side() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-gitlink-conflict-{suffix}"));
        let parent = base.join("parent");
        let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&parent, &["commit", "-am", "Add dep"]);
        let parent_branch = run_git_capture(&parent, &["branch", "--show-current"]);
        let sub_path = parent.join("vendor/dep");

        run_git(&sub_path, &["switch", "-c", "left"]);
        fs::write(sub_path.join("module.txt"), "left\n").unwrap();
        run_git(&sub_path, &["commit", "-am", "Left submodule pin"]);
        let left_oid = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap();
        run_git(&parent, &["add", "vendor/dep"]);
        run_git(&parent, &["commit", "-m", "Parent records left pin"]);

        run_git(&parent, &["switch", "-c", "incoming", "HEAD~1"]);
        run_git(&parent, &["submodule", "update", "--checkout", "vendor/dep"]);
        run_git(&sub_path, &["switch", "-c", "right"]);
        fs::write(sub_path.join("module.txt"), "right\n").unwrap();
        run_git(&sub_path, &["commit", "-am", "Right submodule pin"]);
        let right_oid = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap();
        run_git(&parent, &["add", "vendor/dep"]);
        run_git(&parent, &["commit", "-m", "Parent records right pin"]);

        run_git(&parent, &["switch", &parent_branch]);
        run_git(&parent, &["submodule", "update", "--checkout", "vendor/dep"]);
        assert_eq!(Repository::open(&sub_path).unwrap().head().unwrap().target(), Some(left_oid));
        let parent_string = parent.to_string_lossy().into_owned();
        let outcome = merge_branch(parent_string.clone(), "".into(), "incoming".into()).unwrap();
        assert_eq!(outcome.status, "conflicts");
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.conflicts[0].path, "vendor/dep");
        assert_eq!(outcome.conflicts[0].kind, "submodule");

        resolve_conflict(parent_string.clone(), "".into(), "vendor/dep".into(), "theirs".into()).unwrap();
        assert!(list_conflicts(parent_string.clone(), "".into()).unwrap().is_empty());
        let repo = Repository::open(&parent).unwrap();
        assert_eq!(parent_gitlink_oid(&repo, "vendor/dep", true), Some(right_oid), "the resolved index must record the incoming submodule commit");
        assert_eq!(Repository::open(&sub_path).unwrap().head().unwrap().target(), Some(right_oid), "the initialized submodule checkout should be aligned to the selected gitlink");
        let oid = complete_merge(parent_string.clone(), "".into(), "Merge incoming".into()).unwrap();
        assert!(!oid.is_empty());
        assert!(load_repository_inner(parent_string, Some(true)).unwrap().changes.is_empty());

        fs::remove_dir_all(base).unwrap();
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
        assert!(load_repository_inner(path, Some(true)).unwrap().changes.is_empty());

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn merge_and_conflict_commands_can_target_a_submodule_by_relative_path() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-merge-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
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
        assert!(load_repository_inner(path, Some(true)).unwrap().changes.is_empty(), "nothing should be left pending after committing the whole repository");

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
        fetch_all_remotes_inner(path.clone()).unwrap();
        let origin_head = git(&path, &["rev-parse", "refs/remotes/origin/main"]).unwrap().trim().to_string();
        let upstream_head = git(&path, &["rev-parse", "refs/remotes/upstream/main"]).unwrap().trim().to_string();
        let expected_origin = git(clone_origin.to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let expected_upstream = git(clone_upstream.to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        assert_eq!(origin_head, expected_origin, "origin should be up to date after fetch-all");
        assert_eq!(upstream_head, expected_upstream, "upstream should be up to date after fetch-all too, not just the first remote");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn branch_creation_context_reports_position_relative_to_origin_main() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-branch-context-{suffix}"));
        let repository = base.join("main");
        let origin_remote = base.join("origin.git");
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        run_git(&base, &["init", "--bare", "origin.git"]);
        run_git(&repository, &["remote", "add", "origin", origin_remote.to_str().unwrap()]);
        run_git(&repository, &["push", "-u", "origin", "main"]);
        let path = repository.to_string_lossy().into_owned();

        // Freshly in sync: nothing ahead, nothing behind.
        let context = branch_creation_context(path.clone(), "".into()).unwrap();
        assert_eq!(context.current_branch, "main");
        assert_eq!(context.main_remote_branch.as_deref(), Some("origin/main"));
        assert_eq!((context.ahead, context.behind), (0, 0));

        // Someone else pushes to origin directly — this checkout falls behind
        // until it fetches (branch_creation_context reads the *local* record
        // of origin/main, same as everything else in this app).
        let clone = base.join("clone");
        run_git(&base, &["clone", origin_remote.to_str().unwrap(), clone.to_str().unwrap()]);
        run_git(&clone, &["config", "user.email", "test@example.com"]);
        run_git(&clone, &["config", "user.name", "Test User"]);
        fs::write(clone.join("a.txt"), "two").unwrap();
        run_git(&clone, &["commit", "-am", "Someone else's commit"]);
        run_git(&clone, &["push", "origin", "main"]);
        git(&path, &["fetch", "origin"]).unwrap();
        let context = branch_creation_context(path.clone(), "".into()).unwrap();
        assert_eq!((context.ahead, context.behind), (0, 1), "one commit landed on origin/main that this checkout doesn't have yet");

        // Now also commit locally, without pulling first — genuinely diverged.
        fs::write(repository.join("a.txt"), "three").unwrap();
        run_git(&repository, &["commit", "-am", "Local-only commit"]);
        let context = branch_creation_context(path.clone(), "".into()).unwrap();
        assert_eq!((context.ahead, context.behind), (1, 1));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn graph_branch_divergence_reports_real_ahead_behind_and_merge_base_not_lane_position() {
        // A controlled DAG exercising exactly what the graph view's "N commits
        // ahead" annotation must be based on: real merge-base/ahead/behind per
        // branch, computed from OIDs — never from which row/lane a ref happens
        // to render on.
        //
        //   A ── B (feature) ── (feature merged into main below)
        //   └── C (main) ── M (merge: parents C, B)
        //   A ── D (old-diverged, never merged)
        //   (unrelated) — orphan root, no shared history with main at all
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-graph-divergence-{suffix}"));
        create_libgit2_repository(&base, "a.txt");
        run_git(&base, &["branch", "-M", "main"]);
        let commit_a = run_git_capture(&base, &["rev-parse", "HEAD"]);

        run_git(&base, &["checkout", "-b", "feature"]);
        fs::write(base.join("b.txt"), "b").unwrap();
        run_git(&base, &["add", "."]); run_git(&base, &["commit", "-m", "B on feature"]);
        let commit_b = run_git_capture(&base, &["rev-parse", "HEAD"]);

        run_git(&base, &["checkout", "main"]);
        fs::write(base.join("c.txt"), "c").unwrap();
        run_git(&base, &["add", "."]); run_git(&base, &["commit", "-m", "C on main"]);

        run_git(&base, &["merge", "--no-ff", "-m", "Merge feature into main", "feature"]);
        let merge_commit = run_git_capture(&base, &["rev-parse", "HEAD"]);
        let parents = run_git_capture(&base, &["log", "-1", "--pretty=%P", &merge_commit]);
        assert_eq!(parents.split_whitespace().count(), 2, "sanity check: the merge commit must have exactly two parents");

        run_git(&base, &["checkout", "-b", "old-diverged", &commit_a]);
        fs::write(base.join("d.txt"), "d").unwrap();
        run_git(&base, &["add", "."]); run_git(&base, &["commit", "-m", "D, never merged"]);

        run_git(&base, &["checkout", "--orphan", "unrelated"]);
        run_git(&base, &["reset", "--hard"]);
        fs::write(base.join("u.txt"), "u").unwrap();
        run_git(&base, &["add", "."]); run_git(&base, &["commit", "-m", "Unrelated root, no shared history"]);

        run_git(&base, &["checkout", "main"]);
        run_git(&base, &["update-ref", "refs/remotes/origin/main", "main"]);
        let repo_path = base.to_string_lossy().into_owned();

        let divergence = graph_branch_divergence(repo_path, "main".into()).unwrap();
        let by_name = |name: &str| divergence.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("branch {name} should be reported"));

        let main = by_name("main");
        assert_eq!(main.tip, merge_commit);
        assert_eq!((main.ahead, main.behind), (0, 0), "main compared against itself must be exactly in sync");
        assert_eq!(main.merge_base.as_deref(), Some(merge_commit.as_str()));

        let origin_main = by_name("origin/main");
        assert_eq!(origin_main.tip, merge_commit, "remote-tracking origin/main must be reported too so the graph can mark the common ancestor with main");
        assert_eq!(origin_main.merge_base.as_deref(), Some(merge_commit.as_str()));

        let feature = by_name("feature");
        assert_eq!(feature.tip, commit_b);
        assert_eq!(feature.ahead, 0, "feature's only commit (B) is reachable from main after the merge — it must not be reported as still ahead");
        assert_eq!(feature.behind, 2, "main has C and the merge commit that feature's tip doesn't — real count, not a lane-distance guess");
        assert_eq!(feature.merge_base.as_deref(), Some(commit_b.as_str()), "B is itself an ancestor of the merge commit, so it IS the real merge-base");

        let old_diverged = by_name("old-diverged");
        assert_eq!(old_diverged.ahead, 1, "exactly one commit (D) exists only on old-diverged");
        assert_eq!(old_diverged.behind, 3, "main has C, B and the merge commit that old-diverged doesn't");
        assert_eq!(old_diverged.merge_base.as_deref(), Some(commit_a.as_str()), "the real divergence point is A, not whatever commit happens to share a lane in the rendered graph");

        let unrelated = by_name("unrelated");
        assert!(unrelated.merge_base.is_none(), "two branches with genuinely disconnected histories must be reported as having no merge-base, never a fabricated one");
        assert!(unrelated.ahead >= 1, "an orphan branch's own commit(s) must count as ahead");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn branch_creation_context_can_target_a_submodule_by_relative_path() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-branch-context-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        // Addressed exactly like every other submodule-targeting command in
        // this app: the parent's path plus the submodule's relative path,
        // never a raw absolute path.
        let context = branch_creation_context(parent_string, added).unwrap();
        assert_eq!(context.current_branch, "master");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn graph_commit_actions_create_branch_from_commit_and_checkout_detached_safely() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-graph-commit-actions-{suffix}"));
        create_libgit2_repository(&repository, "a.txt");
        run_git(&repository, &["branch", "-M", "main"]);
        let first = run_git_capture(&repository, &["rev-parse", "HEAD"]);
        fs::write(repository.join("a.txt"), "second\n").unwrap();
        run_git(&repository, &["commit", "-am", "second"]);
        let second = run_git_capture(&repository, &["rev-parse", "HEAD"]);
        let path = repository.to_string_lossy().into_owned();

        create_branch_at_commit(path.clone(), "from-first".into(), first.clone()).unwrap();
        assert_eq!(run_git_capture(&repository, &["branch", "--show-current"]), "from-first");
        assert_eq!(run_git_capture(&repository, &["rev-parse", "HEAD"]), first);

        checkout_commit(path, second.clone()).unwrap();
        assert!(run_git_capture(&repository, &["branch", "--show-current"]).is_empty(), "checking out a raw commit must detach HEAD rather than moving a branch");
        assert_eq!(run_git_capture(&repository, &["rev-parse", "HEAD"]), second);

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn restore_exact_checkpoint_cleans_parent_and_submodule_leftovers() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-exact-checkpoint-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        let extra_dependency = base.join("extra-dependency");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dependency).unwrap();
        fs::create_dir_all(&extra_dependency).unwrap();
        fs::write(repository.join("README.md"), "root v1").unwrap();
        fs::write(dependency.join("module.txt"), "dep v1").unwrap();
        fs::write(extra_dependency.join("extra.txt"), "extra v1").unwrap();
        for path in [&repository, &dependency, &extra_dependency] {
            run_git(path, &["init", "-b", "main"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);
        let checkpoint = run_git_capture(&repository, &["rev-parse", "HEAD"]);
        let sub_path = repository.join("vendor/dep");
        let checkpoint_sub = run_git_capture(&sub_path, &["rev-parse", "HEAD"]);

        fs::write(repository.join("README.md"), "root v2").unwrap();
        fs::write(sub_path.join("module.txt"), "dep v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "Submodule v2"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Move parent and submodule"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", extra_dependency.to_str().unwrap(), "vendor/extra"]);
        run_git(&repository, &["commit", "-m", "Add extra submodule"]);
        fs::write(repository.join("leftover.tmp"), "parent leftover").unwrap();
        fs::write(sub_path.join("sub-leftover.tmp"), "submodule leftover").unwrap();
        fs::write(repository.join("vendor/extra/extra-leftover.tmp"), "extra submodule leftover").unwrap();

        restore_exact_checkpoint_inner(repository.to_string_lossy().into_owned(), checkpoint.clone()).unwrap();
        assert!(run_git_capture(&repository, &["branch", "--show-current"]).is_empty(), "exact checkpoint restore intentionally leaves detached HEAD");
        assert_eq!(run_git_capture(&repository, &["rev-parse", "HEAD"]), checkpoint);
        assert_eq!(run_git_capture(&sub_path, &["rev-parse", "HEAD"]), checkpoint_sub);
        assert!(!repository.join("leftover.tmp").exists(), "parent untracked leftovers from the previous checkpoint must be removed");
        assert!(!sub_path.join("sub-leftover.tmp").exists(), "submodule untracked leftovers from the previous checkpoint must be removed too");
        assert!(!repository.join("vendor/extra").exists(), "submodules that only existed in the previous checkpoint must be removed");
        assert!(refresh_status_inner(repository.to_string_lossy().into_owned()).unwrap().is_empty(), "workspace should be clean after exact checkpoint restore");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn init_submodule_populates_a_registered_empty_submodule() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-init-submodule-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&dependency).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(dependency.join("module.txt"), "dep").unwrap();
        for path in [&repository, &dependency] {
            run_git(path, &["init", "-b", "main"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);
        let recorded = run_git_capture(&repository.join("vendor/dep"), &["rev-parse", "HEAD"]);
        run_git(&repository, &["submodule", "deinit", "--all", "--force"]);
        fs::remove_dir_all(repository.join("vendor/dep")).unwrap();
        fs::create_dir_all(repository.join("vendor/dep")).unwrap();

        let listing = load_directory_inner(repository.to_string_lossy().into_owned(), "vendor".into(), None).unwrap();
        let row = listing.iter().find(|entry| entry.name == "dep").unwrap();
        assert_eq!(row.kind, "submodule");
        assert!(!row.submodule_initialized);

        init_submodule_inner(repository.to_string_lossy().into_owned(), "vendor/dep".into()).unwrap();
        assert_eq!(run_git_capture(&repository.join("vendor/dep"), &["rev-parse", "HEAD"]), recorded);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn entry_details_stays_fast_by_leaving_last_commit_lookup_to_a_separate_call() {
        // Reported: selecting items in a large folder was extremely slow.
        // entry_details used to walk up to 2000 commits, diffing each
        // against its parent, on *every* selection — the same cost as
        // `git log -- <path>`, run inline on every click. It must no
        // longer compute that at all; entry_last_commit does it instead,
        // as a separate call the frontend fires after the fast details are
        // already showing.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-entry-details-fast-{suffix}"));
        create_libgit2_repository(&repository, "a.txt");
        let path = repository.to_string_lossy().into_owned();

        let details = entry_details_inner(path.clone(), "a.txt".into()).unwrap();
        assert!(details.last_commit_id.is_none(), "entry_details must not populate last-commit-touching-path fields itself");
        assert!(details.last_commit_subject.is_none());

        let last = entry_last_commit_inner(path.clone(), "a.txt".into()).unwrap();
        assert_eq!(last.unwrap().subject, "Initial commit", "entry_last_commit must still find it correctly when actually asked");

        fs::remove_dir_all(repository).unwrap();
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
        assert!(load_repository_inner(path, Some(true)).unwrap().changes.is_empty(), "nothing should be left pending after committing the folder");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn committing_the_whole_repository_picks_up_a_submodule_advanced_outside_the_app() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-root-commit-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
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
        add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "test".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add test submodule".into()).unwrap();

        // Simulate the user deleting the submodule folder outside the app (Finder/
        // terminal) instead of using the app's own removal flow — the working
        // directory (and its .git) is gone, but the gitlink is still registered.
        fs::remove_dir_all(parent.join("test")).unwrap();
        assert!(!parent.join("test").exists());

        let result = commit_files_inner(parent_string.clone(), vec!["test".into()], "Remove deleted submodule".into());
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

        let data = load_repository_inner(path.clone(), Some(true)).unwrap();
        assert!(!data.commits.iter().any(|commit| commit.parents.len() > 1), "the WIP stash commit (with its index/untracked parents) must never appear as a graph commit");
        assert!(!data.commits.iter().any(|commit| commit.refs.iter().any(|r| r.name == "stash")), "refs/stash must not be attached as a label on any commit");
        assert_eq!(data.stashes.len(), 1);
        assert_eq!(data.stashes[0].base_commit, base);

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn parent_and_submodule_stash_lists_are_independent_and_explicitly_scoped() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-stash-scope-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_path = parent.to_string_lossy().into_owned();
        add_submodule_inner(parent_path.clone(), "".into(), dependency.to_string_lossy().into_owned(), "test".into(), String::new(), String::new()).unwrap();
        create_commit(parent_path.clone(), "Add test submodule".into()).unwrap();

        fs::write(parent.join("test/module.txt"), "submodule edit").unwrap();
        stash_changes(parent.join("test").to_string_lossy().into_owned()).unwrap();

        let parent_scope = list_stashes(parent_path.clone()).unwrap();
        let sub_scope = list_submodule_stashes(parent_path, "test".into()).unwrap();
        assert!(!parent_scope.is_submodule);
        assert!(parent_scope.stashes.is_empty(), "a stash created inside a submodule must never appear in the parent repository's list");
        assert!(sub_scope.is_submodule);
        assert_eq!(sub_scope.relative_path.as_deref(), Some("test"));
        assert_eq!(sub_scope.stashes.len(), 1);
        assert_eq!(sub_scope.repository_path, parent.join("test").to_string_lossy());

        fs::remove_dir_all(base).unwrap();
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

        let data = load_repository_inner(path.clone(), Some(true)).unwrap();
        assert!(!data.changes.iter().any(|change| change.path == "a.txt"), "a.txt should be set aside by the stash, not showing as a pending change");
        assert!(data.changes.iter().any(|change| change.path == "b.txt"), "b.txt was never selected — it must stay modified, untouched by the scoped stash");
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "one", "a.txt on disk should be back to the committed version once stashed");
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "two", "b.txt's edit must be left alone on disk");
        assert_eq!(data.stashes.len(), 1);

        let blocked = pop_stash(path.clone(), 0).expect_err("a full pop over unrelated live work must be blocked");
        assert!(blocked.contains("already has uncommitted work"), "the error should explain why the operation was refused: {blocked}");
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "one", "a blocked pop must not apply any stash content");
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "two", "a blocked pop must preserve unrelated live work");
        assert_eq!(list_stashes(path.clone()).unwrap().stashes.len(), 1, "a blocked pop must keep the stash unchanged");

        run_git(&repository, &["restore", "b.txt"]);
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
        assert!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len() == 1, "the stash entry must survive a conflicted pop, not be silently dropped");

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
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "the stash itself must still be there to try again");

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

        let entries = load_directory_inner(path.clone(), String::new(), Some(true)).unwrap();
        assert!(entries.iter().find(|entry| entry.name == "a.txt").unwrap().stashed, "the Explorer row must show that a.txt is preserved in a stash");
        assert!(!entries.iter().find(|entry| entry.name == "b.txt").unwrap().stashed, "an unrelated clean file must not receive the stash badge");

        drop_stash(path.clone(), 0).unwrap();
        let entries_after_drop = load_directory_inner(path.clone(), String::new(), Some(true)).unwrap();
        assert!(!entries_after_drop.iter().find(|entry| entry.name == "a.txt").unwrap().stashed, "the badge must disappear as soon as the stash is dropped");

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
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "dropping index 1 should leave only the b.txt stash");
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "one", "dropping never applies the change — a.txt stays at its committed content");

        pop_stash(path.clone(), 0).unwrap();
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "b changed second", "popping should bring the change back");
        assert!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.is_empty(), "the only remaining stash should be gone after a clean pop");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn restore_stash_paths_restores_only_the_chosen_file_and_keeps_the_stash_as_backup() {
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
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "the original stash must remain as a safety backup");
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["a.txt".to_string(), "b.txt".to_string()], "a partial restore must never rewrite the stash");

        restore_stash_paths(path.clone(), 0, vec!["b.txt".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "b changed", "b.txt should now be restored too");
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "restoring every file still keeps the backup until Drop is explicit");

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
    fn restoring_a_file_keeps_the_original_stash_immutable_as_a_backup() {
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
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "the backup stash should remain");
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["OrdersFromSite/ordersForm.css".to_string(), "README.md".to_string()], "restoring one file must not rewrite the stash or its other files");

        restore_stash_paths(path.clone(), 0, vec!["README.md".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("README.md")).unwrap(), "changed");
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "only an explicit Drop may delete the backup stash");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn restoring_one_stash_file_never_touches_live_work_in_an_unselected_file() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-unselected-live-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "base a").unwrap();
        fs::write(repository.join("b.txt"), "base b").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::write(repository.join("a.txt"), "stashed a").unwrap();
        fs::write(repository.join("b.txt"), "stashed b").unwrap();
        stash_changes(path.clone()).unwrap();
        fs::write(repository.join("b.txt"), "new live b — must survive byte for byte").unwrap();

        restore_stash_paths(path.clone(), 0, vec!["a.txt".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "stashed a");
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "new live b — must survive byte for byte");
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["a.txt".to_string(), "b.txt".to_string()], "the original backup remains immutable");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn restoring_a_stash_file_refuses_to_overwrite_current_work_on_that_file() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-selected-live-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("a.txt"), "base").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::write(repository.join("a.txt"), "stashed version").unwrap();
        stash_changes(path.clone()).unwrap();
        fs::write(repository.join("a.txt"), "new live version").unwrap();

        let error = restore_stash_paths(path.clone(), 0, vec!["a.txt".into()]).unwrap_err();
        assert!(error.contains("already has uncommitted work"));
        assert_eq!(fs::read_to_string(repository.join("a.txt")).unwrap(), "new live version", "the current edit must never be overwritten");
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "the stash must remain available after refusal");

        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn restoring_a_stashed_deletion_removes_only_that_clean_tracked_file() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-stash-deletion-{suffix}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("delete-me.txt"), "tracked").unwrap();
        fs::write(repository.join("leave-me.txt"), "tracked").unwrap();
        run_git(&repository, &["init"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial commit"]);
        let path = repository.to_string_lossy().into_owned();

        fs::remove_file(repository.join("delete-me.txt")).unwrap();
        fs::write(repository.join("leave-me.txt"), "stashed but not selected").unwrap();
        stash_changes(path.clone()).unwrap();

        restore_stash_paths(path.clone(), 0, vec!["delete-me.txt".into()]).unwrap();
        assert!(!repository.join("delete-me.txt").exists(), "the selected stashed deletion should be restored");
        assert_eq!(fs::read_to_string(repository.join("leave-me.txt")).unwrap(), "tracked", "an unselected path must remain untouched");
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["delete-me.txt".to_string(), "leave-me.txt".to_string()], "the original backup remains intact");

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
        assert!(load_repository_inner(path.clone(), Some(true)).unwrap().changes.iter().any(|change| change.path == "intro.css" && change.staged));

        commit_files_inner(path.clone(), vec!["intro.css".into()], "Update intro.css".into()).unwrap();
        assert!(!load_repository_inner(path.clone(), Some(true)).unwrap().changes.iter().any(|change| change.path == "intro.css"), "intro.css should no longer appear as a pending change right after commit");

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
        let staged = load_repository_inner(path.clone(), Some(true)).unwrap();
        let change = staged.changes.iter().find(|change| change.path == "src/main.rs").expect("src/main.rs should be a pending change");
        assert!(change.staged, "src/main.rs should be staged after stage_files");

        unstage_files(path.clone(), vec!["src/main.rs".into()]).unwrap();
        let unstaged = load_repository_inner(path.clone(), Some(true)).unwrap();
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
        let entries = load_directory_inner(path.clone(), "".into(), None).unwrap();
        let folder_entry = entries.iter().find(|entry| entry.relative_path == "brand-new-folder").expect("the new folder should be listed");
        assert!(!folder_entry.tracked, "a wholly new folder should not be marked as tracked");
        assert!(!folder_entry.status.is_empty(), "load_directory should flag the new folder with a status (e.g. untracked/changed), got empty status");

        let details = entry_details_inner(path, "brand-new-folder".into()).unwrap();
        assert!(!details.tracked, "entry_details should also report the new folder as untracked");
        assert!(!details.status.is_empty(), "entry_details should flag the new folder with a status too, got empty status");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn deleted_tracked_items_remain_visible_inside_a_changed_folder() {
        // A deleted path is absent from read_dir by definition. The Explorer
        // used to mark `src` as changed, then show only clean surviving files
        // after opening it, with no clue which tracked item Git considered
        // deleted. Both a direct deleted file and a whole missing tracked
        // subtree must therefore be represented by synthetic rows sourced
        // from the status result already loaded for this folder.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-deleted-explorer-items-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        fs::create_dir_all(base.join("src/old")).unwrap();
        fs::write(base.join("src/keep.txt"), "still here").unwrap();
        fs::write(base.join("src/gone.txt"), "delete me").unwrap();
        fs::write(base.join("src/old/nested.txt"), "delete this folder").unwrap();
        run_git(&base, &["add", "."]);
        run_git(&base, &["commit", "-m", "Add tracked source files"]);

        fs::remove_file(base.join("src/gone.txt")).unwrap();
        fs::remove_dir_all(base.join("src/old")).unwrap();
        let path = base.to_string_lossy().into_owned();

        let root = load_directory_inner(path.clone(), "".into(), Some(true)).unwrap();
        let src = root.iter().find(|entry| entry.relative_path == "src").expect("src should remain visible");
        assert_eq!(src.status, "•", "the parent folder should advertise nested changes");

        let entries = load_directory_inner(path, "src".into(), Some(true)).unwrap();
        let keep = entries.iter().find(|entry| entry.relative_path == "src/keep.txt").expect("surviving file should remain visible");
        assert!(keep.status.is_empty(), "the surviving file itself is unchanged");
        let gone = entries.iter().find(|entry| entry.relative_path == "src/gone.txt").expect("deleted file should remain visible as a Git row");
        assert_eq!((gone.kind.as_str(), gone.status.as_str(), gone.tracked), ("deleted", "D", true));
        let old = entries.iter().find(|entry| entry.relative_path == "src/old").expect("deleted tracked subtree should remain visible as one Git row");
        assert_eq!((old.kind.as_str(), old.status.as_str(), old.tracked), ("deleted-folder", "D", true));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn forced_reload_bypasses_the_status_cache_to_show_an_external_edit_immediately() {
        // Reproduces the report: editing a file with an external program (not
        // through this app), then clicking Refresh, used to still show the old
        // status — because load_directory's status cache has a 300s TTL tuned for
        // ordinary navigation, and an explicit refresh click used to have no way
        // to bypass it. `force: true` must see the change right away, without
        // waiting out the TTL.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-forced-reload-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        let path = base.to_string_lossy().into_owned();

        // Warm the status cache with a clean tree (no `force`, like ordinary navigation).
        let clean = load_directory_inner(path.clone(), "".into(), None).unwrap();
        let readme = clean.iter().find(|entry| entry.relative_path == "README.md").expect("README.md should be listed");
        assert!(readme.status.is_empty(), "a freshly committed file should start with no status");

        // Simulate an external program editing the file on disk, bypassing this app entirely.
        fs::write(base.join("README.md"), "edited externally, not through this app").unwrap();

        // Without force, the cache is still fresh (TTL is 300s) and unaware of the edit.
        let stale = load_directory_inner(path.clone(), "".into(), None).unwrap();
        let stale_readme = stale.iter().find(|entry| entry.relative_path == "README.md").unwrap();
        assert!(stale_readme.status.is_empty(), "sanity check: without force, the pre-existing cache should still be serving the stale, clean status");

        // A forced reload (what the Refresh button now sends) must reflect the edit immediately.
        let refreshed = load_directory_inner(path.clone(), "".into(), Some(true)).unwrap();
        let refreshed_readme = refreshed.iter().find(|entry| entry.relative_path == "README.md").unwrap();
        assert!(!refreshed_readme.status.is_empty(), "force:true must bypass the status cache and show the external edit immediately, got status={:?}", refreshed_readme.status);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn detached_head_reports_empty_branch_name_and_the_real_head_oid_not_the_string_head() {
        // git2's own Reference::shorthand() returns the literal string "HEAD"
        // for a detached checkout — this app used to pass that straight
        // through as current_branch, so a detached submodule (extremely
        // common right after `git submodule update` / "Reset submodule",
        // both of which always leave it detached) looked like it was on an
        // actual branch named "HEAD". Both open_repository_fast and
        // load_repository must instead report current_branch: "" and
        // head_detached: true, with head_oid holding the real commit.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-detached-head-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        let head_commit = run_git_capture(&base, &["rev-parse", "HEAD"]);
        run_git(&base, &["checkout", "--detach", "HEAD"]);
        let path = base.to_string_lossy().into_owned();

        let fast = open_repository_fast_inner(path.clone()).unwrap();
        assert!(fast.repository.head_detached, "open_repository_fast must report a detached checkout as such");
        assert_eq!(fast.repository.current_branch, "", "current_branch must never be the literal string \"HEAD\"");
        assert_eq!(fast.repository.head_oid, head_commit);

        let full = load_repository_inner(path, None).unwrap();
        assert!(full.repository.head_detached);
        assert_eq!(full.repository.current_branch, "");
        assert_eq!(full.repository.head_oid, head_commit);

        // Sanity check the non-detached case is unaffected: a normal
        // checkout must still report its real branch name and head_detached: false.
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn load_older_commits_paginates_correctly_and_reports_has_more() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-load-older-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        // 4 more commits on top of the initial one — 5 total: c0 (oldest) .. c4 (HEAD, newest).
        let mut ids = Vec::new();
        {
            let repo = Repository::open(&base).unwrap();
            ids.push(repo.head().unwrap().target().unwrap().to_string());
        }
        for i in 1..5 {
            fs::write(base.join("README.md"), format!("v{i}")).unwrap();
            run_git(&base, &["commit", "-am", &format!("commit {i}")]);
            let repo = Repository::open(&base).unwrap();
            ids.push(repo.head().unwrap().target().unwrap().to_string());
        }
        let repo_path = base.to_string_lossy().into_owned();

        // First page: the 2 newest (c4, c3) — matches what load_repository
        // itself would show first, oldest-last.
        let first_page = load_repository_inner(repo_path.clone(), None).unwrap();
        assert_eq!(first_page.commits.len(), 5, "small repository — nothing should be truncated");
        assert!(!first_page.commits_truncated);

        // "Load older" after the oldest commit currently on screen (c0, the
        // very first commit) — nothing further back exists.
        let after_oldest = load_older_commits(repo_path.clone(), ids[0].clone(), Some(2)).unwrap();
        assert!(after_oldest.commits.is_empty(), "there is nothing older than the very first commit");
        assert!(!after_oldest.has_more);

        // Paginate with a small page size from the newest commit (c4): must
        // return exactly [c3, c2] in that order, and report more remain.
        let page1 = load_older_commits(repo_path.clone(), ids[4].clone(), Some(2)).unwrap();
        assert_eq!(page1.commits.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![ids[3].clone(), ids[2].clone()]);
        assert!(page1.has_more, "c1 and c0 still remain after this page");

        // Continuing from the last commit of page1 must yield the rest ([c1, c0]) with no more left.
        let page2 = load_older_commits(repo_path.clone(), ids[2].clone(), Some(2)).unwrap();
        assert_eq!(page2.commits.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![ids[1].clone(), ids[0].clone()]);
        assert!(!page2.has_more);

        // A marker commit that isn't actually in this repository's history must fail clearly, not silently return an empty/wrong page.
        assert!(load_older_commits(repo_path, "0".repeat(40), Some(2)).is_err());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn load_repository_truncates_at_the_graph_commit_window_and_load_older_continues_past_it() {
        // Real truncation, not a small-repo simulation: builds a genuine
        // linear chain of GRAPH_COMMIT_WINDOW + 5 commits directly through
        // git2 (in-process tree/commit writes — fast; no per-commit `git`
        // subprocess) and confirms load_repository stops at exactly the
        // window, flags commits_truncated, and load_older_commits picks up
        // the real remainder from there.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-truncation-{suffix}"));
        fs::create_dir_all(&base).unwrap();
        let repo = Repository::init(&base).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        let total = GRAPH_COMMIT_WINDOW + 5;
        let mut last_commit: Option<git2::Oid> = None;
        let mut all_ids = Vec::with_capacity(total);
        for i in 0..total {
            fs::write(base.join("file.txt"), format!("{i}")).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("file.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let parents: Vec<git2::Commit> = last_commit.map(|oid| repo.find_commit(oid).unwrap()).into_iter().collect();
            let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
            let oid = repo.commit(Some("HEAD"), &signature, &signature, &format!("commit {i}"), &tree, &parent_refs).unwrap();
            last_commit = Some(oid);
            all_ids.push(oid.to_string());
        }
        drop(repo);
        let repo_path = base.to_string_lossy().into_owned();

        let data = load_repository_inner(repo_path.clone(), None).unwrap();
        assert_eq!(data.commits.len(), GRAPH_COMMIT_WINDOW, "must stop at exactly the window, not the repository's real total");
        assert!(data.commits_truncated, "a repository with more history than the window must say so");
        // Newest-first: the window's oldest (last) entry is commit total-window.
        let oldest_in_window = data.commits.last().unwrap().id.clone();
        assert_eq!(oldest_in_window, all_ids[total - GRAPH_COMMIT_WINDOW]);

        let older = load_older_commits(repo_path, oldest_in_window, Some(500)).unwrap();
        assert_eq!(older.commits.len(), 5, "exactly the 5 real commits older than the window must come back");
        assert!(!older.has_more, "that really is the entire rest of the history");
        assert_eq!(older.commits.last().unwrap().id, all_ids[0], "must end at the true root commit");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_repaint_after_load_repository_reuses_the_fresh_scan_instead_of_rescanning() {
        // Reproduces the redundant-scan report: load_repository (or
        // refresh_status) already computes a full, current status scan and
        // seeds the cache with it via replace_git_metadata. The frontend then
        // repaints the folder on screen — that repaint must reuse the scan
        // that was *just* paid for, for the root folder and for a child folder
        // alike, not immediately invalidate and redo it. Only an explicit
        // force:true (the "Reload folder" button) should ask for a rescan.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-repaint-reuse-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join("sub/file.txt"), "a").unwrap();
        run_git(&base, &["add", "sub/file.txt"]);
        run_git(&base, &["commit", "-m", "Add sub/file.txt"]);
        let path = base.to_string_lossy().into_owned();

        // The one full scan: what "opening the repository" does.
        load_repository_inner(path.clone(), None).unwrap();

        // An external program edits files after that scan — a repaint must
        // not see this, since seeing it would mean it rescanned instead of
        // reusing what load_repository just computed.
        fs::write(base.join("README.md"), "edited after the scan").unwrap();
        fs::write(base.join("sub/file.txt"), "edited after the scan too").unwrap();

        let root_repaint = load_directory_inner(path.clone(), "".into(), None).unwrap();
        let readme = root_repaint.iter().find(|e| e.relative_path == "README.md").unwrap();
        assert!(readme.status.is_empty(), "a root repaint without force must reuse load_repository's fresh scan, not rescan and see the external edit");

        let child_repaint = load_directory_inner(path.clone(), "sub".into(), None).unwrap();
        let child_file = child_repaint.iter().find(|e| e.relative_path == "sub/file.txt").unwrap();
        assert!(child_file.status.is_empty(), "a child-folder repaint without force must also reuse the same fresh scan (via the full-scan reuse path), not run its own scoped rescan");

        // Only the explicit "Reload folder" action (force:true) should invalidate and rescan.
        let forced = load_directory_inner(path, "".into(), Some(true)).unwrap();
        let forced_readme = forced.iter().find(|e| e.relative_path == "README.md").unwrap();
        assert!(!forced_readme.status.is_empty(), "an explicit forced reload must see the external edit");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_navigation_status_reports_none_outside_a_submodule_and_tracks_readiness() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-subnav-status-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);
        let repo_path = repository.to_string_lossy().into_owned();

        assert!(submodule_navigation_status_inner(repo_path.clone(), "".into()).unwrap().is_none(), "the repository root is not inside a submodule");
        assert!(submodule_navigation_status_inner(repo_path.clone(), "README.md".into()).unwrap().is_none(), "an ordinary parent-repo file is not inside a submodule");

        let before = submodule_navigation_status_inner(repo_path.clone(), "vendor/dep".into()).unwrap().expect("vendor/dep should be recognized as a submodule");
        assert_eq!(before.submodule_path, "vendor/dep");
        assert!(!before.ready, "no scan has happened yet — must not be reported ready");

        submodule_folder_status_inner(repo_path.clone(), "vendor/dep".into()).unwrap();

        let after = submodule_navigation_status_inner(repo_path, "vendor/dep".into()).unwrap().unwrap();
        assert!(after.ready, "after submodule_folder_status scans it, the submodule must be reported ready");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_folder_status_scans_only_the_requested_submodule_not_every_submodule() {
        // "Nu pre-scana toate submodulele" — a repository with several
        // submodules must never have entering one of them trigger a scan of
        // the others too (an I/O storm on a project with hundreds, especially
        // on Windows). Proven the same way as the other cache-reuse tests: an
        // external edit made right after scanning submodule A must still be
        // invisible through A's own snapshot reuse, while submodule B (never
        // scanned) must show its own real, current status when read directly.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-subnav-isolated-{suffix}"));
        let repository = base.join("main");
        let dep_a = base.join("dep-a");
        let dep_b = base.join("dep-b");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dep_a, "a.txt");
        create_libgit2_repository(&dep_b, "b.txt");
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dep_a.to_str().unwrap(), "vendor/a"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dep_b.to_str().unwrap(), "vendor/b"]);
        run_git(&repository, &["commit", "-am", "Add both submodules"]);
        let repo_path = repository.to_string_lossy().into_owned();

        submodule_folder_status_inner(repo_path.clone(), "vendor/a".into()).unwrap();
        assert!(submodule_navigation_status_inner(repo_path.clone(), "vendor/a".into()).unwrap().unwrap().ready, "the requested submodule should now be ready");
        assert!(!submodule_navigation_status_inner(repo_path.clone(), "vendor/b".into()).unwrap().unwrap().ready, "a sibling submodule that was never entered must not have been scanned too");

        // b's real, current status must still be answered correctly on demand
        // (just not pre-emptively) — add an untracked file and confirm load_directory sees it.
        fs::write(repository.join("vendor/b/new.txt"), "new").unwrap();
        let listing = load_directory_inner(repo_path, "vendor/b".into(), None).unwrap();
        let new_entry = listing.iter().find(|entry| entry.name == "new.txt").expect("the new file in the never-prescanned submodule should still be listed with real status");
        assert!(!new_entry.status.is_empty(), "vendor/b must still get its own correct, current status when actually read, despite never being pre-scanned");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn one_submodule_folder_status_scan_serves_every_subsequent_folder_and_entry_details_in_that_submodule() {
        // The core acceptance criteria: one scan per submodule, then cache
        // HIT for its folders (load_directory) *and* for entry_details on
        // files inside it — reproduced the same way as the parent-repository
        // version of this test: an external edit right after the one scan
        // must not show up in any of the reads that follow, since seeing it
        // would mean one of them rescanned instead of reusing the snapshot.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-subnav-reuse-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        fs::create_dir_all(dependency.join("nested")).unwrap();
        fs::write(dependency.join("top.txt"), "a").unwrap();
        fs::write(dependency.join("nested/deep.txt"), "b").unwrap();
        run_git(&dependency, &["init"]);
        run_git(&dependency, &["config", "user.email", "test@example.com"]);
        run_git(&dependency, &["config", "user.name", "Test User"]);
        run_git(&dependency, &["add", "."]);
        run_git(&dependency, &["commit", "-m", "Initial"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);
        let repo_path = repository.to_string_lossy().into_owned();

        // The one full scan for this submodule.
        submodule_folder_status_inner(repo_path.clone(), "vendor/dep".into()).unwrap();

        // External edits, made right after that one scan, to a file in the
        // submodule's root listing, one in a nested folder, and one that will
        // be looked up individually via entry_details.
        fs::write(repository.join("vendor/dep/top.txt"), "edited after the scan").unwrap();
        fs::write(repository.join("vendor/dep/nested/deep.txt"), "edited after the scan too").unwrap();

        let root_listing = load_directory_inner(repo_path.clone(), "vendor/dep".into(), None).unwrap();
        let top = root_listing.iter().find(|e| e.name == "top.txt").unwrap();
        assert!(top.status.is_empty(), "listing the submodule's own root must reuse the one scan, not rescan");

        let nested_listing = load_directory_inner(repo_path.clone(), "vendor/dep/nested".into(), None).unwrap();
        let deep = nested_listing.iter().find(|e| e.name == "deep.txt").unwrap();
        assert!(deep.status.is_empty(), "a nested folder inside the submodule must also reuse the same scan, not run its own scoped scan");

        let details = entry_details_inner(repo_path, "vendor/dep/nested/deep.txt".into()).unwrap();
        assert!(details.status.is_empty(), "entry_details for a file inside the submodule must reuse the same snapshot too, not trigger a duplicate scan");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn parse_github_repo_accepts_github_com_and_enterprise_and_rejects_other_hosts() {
        let gi = GitHubRepo { host: "github.com".into(), owner: "AndreiRomanC".into(), repo: "git-integrity".into() };
        assert_eq!(parse_github_repo("git@github.com:AndreiRomanC/git-integrity.git"), Some(gi.clone()));
        assert_eq!(parse_github_repo("https://github.com/AndreiRomanC/git-integrity.git"), Some(gi.clone()));
        assert_eq!(parse_github_repo("https://github.com/AndreiRomanC/git-integrity"), Some(gi.clone()));
        assert_eq!(parse_github_repo("ssh://git@github.com/AndreiRomanC/git-integrity.git"), Some(gi));

        // GitHub Enterprise — host kept verbatim, never rewritten to github.com.
        let ent = GitHubRepo { host: "github.vitesco.io".into(), owner: "eng".into(), repo: "sw-prj-OMBMS_000U0".into() };
        assert_eq!(parse_github_repo("git@github.vitesco.io:eng/sw-prj-OMBMS_000U0.git"), Some(ent.clone()));
        assert_eq!(parse_github_repo("https://github.vitesco.io/eng/sw-prj-OMBMS_000U0.git"), Some(ent.clone()));
        assert_eq!(parse_github_repo("ssh://git@github.vitesco.io/eng/sw-prj-OMBMS_000U0.git"), Some(ent.clone()));
        assert_eq!(parse_github_repo("ssh://git@github.vitesco.io:22/eng/sw-prj-OMBMS_000U0.git"), Some(ent.clone()));
        assert_eq!(ent.gh_repo_arg(), "github.vitesco.io/eng/sw-prj-OMBMS_000U0");

        // Non-GitHub hosts and GitLab-style subgroup paths stay unrecognized.
        assert_eq!(parse_github_repo("https://gitlab.com/AndreiRomanC/git-integrity.git"), None);
        assert_eq!(parse_github_repo("https://git.internal.example.com/team/repo.git"), None);
        assert_eq!(parse_github_repo("git@mygithub.com:team/repo.git"), None, "only a real `github.*` host counts, not any host with 'github' in it");
        assert_eq!(parse_github_repo("https://github.vitesco.io/eng/group/sub/repo.git"), None, "a GitHub repo path is exactly owner/repo");
        assert_eq!(parse_github_repo(""), None);
    }

    #[test]
    fn pr_status_mapping_helpers_translate_gh_json_vocabulary_correctly() {
        assert_eq!(map_pr_state("OPEN", false), "open");
        assert_eq!(map_pr_state("OPEN", true), "draft", "a draft PR must be reported as draft even though gh's own `state` field still says OPEN");
        assert_eq!(map_pr_state("MERGED", false), "merged");
        assert_eq!(map_pr_state("CLOSED", false), "closed");

        assert_eq!(map_mergeable("MERGEABLE"), "mergeable");
        assert_eq!(map_mergeable("CONFLICTING"), "conflicting");
        assert_eq!(map_mergeable("UNKNOWN"), "calculating", "GitHub reports UNKNOWN while it's still computing mergeability — must read as calculating, not as a hard unknown/error");

        assert_eq!(map_review_summary("APPROVED"), "approved");
        assert_eq!(map_review_summary("CHANGES_REQUESTED"), "changes_requested");
        assert_eq!(map_review_summary("REVIEW_REQUIRED"), "review_required");
        assert_eq!(map_review_summary(""), "none");
        assert_eq!(map_reviewer_state("APPROVED"), "approved");
        assert_eq!(map_reviewer_state("CHANGES_REQUESTED"), "changes_requested");
        assert_eq!(map_reviewer_state("COMMENTED"), "commented");

        let failing = vec![serde_json::json!({"conclusion": "SUCCESS"}), serde_json::json!({"conclusion": "FAILURE"})];
        assert_eq!(map_checks_status(&failing), "failing", "any real failure must win over other passing/pending checks");
        let pending = vec![serde_json::json!({"conclusion": "SUCCESS"}), serde_json::json!({"state": "PENDING"})];
        assert_eq!(map_checks_status(&pending), "pending");
        let passing = vec![serde_json::json!({"conclusion": "SUCCESS"}), serde_json::json!({"conclusion": "NEUTRAL"})];
        assert_eq!(map_checks_status(&passing), "passing");
        assert_eq!(map_checks_status(&[]), "none", "no checks configured at all must read as none, not as passing");
    }

    #[test]
    fn cli_pr_response_maps_requested_and_latest_reviewers_without_duplicate_people() {
        let item = serde_json::json!({
            "reviewRequests": [
                { "__typename": "User", "login": "alice" },
                { "__typename": "User", "login": "Alice" },
                { "__typename": "Team", "name": "Core maintainers", "slug": "core" }
            ],
            "latestReviews": [
                { "author": { "login": "alice" }, "state": "APPROVED" },
                { "author": { "login": "copilot-pull-request-reviewer" }, "state": "COMMENTED" }
            ]
        });
        assert_eq!(pr_reviewers_from_json(&item), vec![
            PullRequestReviewerSummary { login: "alice".into(), state: "approved".into(), review_url: String::new() },
            PullRequestReviewerSummary { login: "Core maintainers".into(), state: "requested".into(), review_url: String::new() },
            PullRequestReviewerSummary { login: "copilot-pull-request-reviewer".into(), state: "commented".into(), review_url: String::new() },
        ]);
    }

    #[test]
    fn github_api_fallback_uses_the_correct_public_and_enterprise_endpoints() {
        assert_eq!(github_graphql_endpoint("github.com"), "https://api.github.com/graphql");
        assert_eq!(github_graphql_endpoint("github.vitesco.io"), "https://github.vitesco.io/api/graphql");
        assert_eq!(parse_gh_repo_arg("github.vitesco.io/eng/sw-prj-VWAQ4_000U0"), Some(GitHubRepo {
            host: "github.vitesco.io".into(), owner: "eng".into(), repo: "sw-prj-VWAQ4_000U0".into(),
        }));
        assert_eq!(parse_gh_repo_arg("github.vitesco.io/eng/group/repo"), None);
    }

    #[test]
    fn pr_queries_do_not_require_enterprise_organization_scope() {
        for query in [GITHUB_PRS_BY_HEAD_QUERY, GITHUB_PRS_BY_BASE_QUERY] {
            assert!(query.contains("... on User { login }"));
            assert!(!query.contains("... on Team"), "team fields require read:org on GitHub Enterprise");
            assert!(!query.contains(" slug"), "team slug requires read:org on GitHub Enterprise");
        }
        assert!(!PR_GH_JSON_FIELDS.split(',').any(|field| field == "reviewRequests"),
            "gh expands team review requests and can require read:org");
        assert!(PR_GH_JSON_FIELDS.split(',').any(|field| field == "latestReviews"),
            "submitted reviewer activity should remain available");
    }

    #[test]
    fn git_credential_parser_extracts_only_the_password_field() {
        let output = b"protocol=https\nhost=github.vitesco.io\nusername=employee\npassword=secret-token\n";
        assert_eq!(parse_git_credential_password(output).as_deref(), Some("secret-token"));
        assert_eq!(parse_git_credential_password(b"username=employee\n"), None);
    }

    #[test]
    fn graphql_pr_response_normalizes_to_the_existing_rich_pr_card_shape() {
        let graphql = serde_json::json!({
            "number": 282,
            "title": "Implementation of LAH",
            "headRefName": "feature/lah",
            "headRepository": { "name": "sw-prj-VWAQ4_000U0" },
            "headRepositoryOwner": { "login": "eng" },
            "baseRefName": "main",
            "state": "OPEN",
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "reviewRequests": { "nodes": [{ "requestedReviewer": { "login": "waiting-reviewer" } }] },
            "latestReviews": { "nodes": [{
                "author": { "login": "approved-reviewer" },
                "state": "APPROVED",
                "url": "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282#pullrequestreview-987"
            }] },
            "url": "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282",
            "commits": { "nodes": [{ "commit": { "oid": "abc1234567890def", "statusCheckRollup": {
                "state": "PENDING",
                "contexts": { "nodes": [
                    { "context": "Collaborator", "state": "PENDING", "targetUrl": "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/runs/123" },
                    { "name": "Submodule status", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/actions/runs/456" }
                ] }
            } } }] }
        });
        let normalized = normalize_graphql_pr(&graphql);
        let summary = pr_summary_from_json(&normalized);
        assert_eq!(summary.number, 282);
        assert_eq!(summary.source_branch, "feature/lah");
        assert_eq!(summary.target_branch, "main");
        assert_eq!(summary.mergeable, "mergeable");
        assert_eq!(summary.review_summary, "approved");
        assert_eq!(summary.reviewers, vec![
            PullRequestReviewerSummary { login: "waiting-reviewer".into(), state: "requested".into(), review_url: String::new() },
            PullRequestReviewerSummary {
                login: "approved-reviewer".into(), state: "approved".into(),
                review_url: "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282#pullrequestreview-987".into(),
            },
        ]);
        assert_eq!(summary.checks_status, "pending");
        assert_eq!(summary.checks, vec![
            PullRequestCheckSummary { name: "Collaborator".into(), status: "pending".into(), details_url: "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/runs/123".into() },
            PullRequestCheckSummary { name: "Submodule status".into(), status: "passing".into(), details_url: "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/actions/runs/456".into() },
        ]);
        assert_eq!(summary.url, "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282");
    }

    #[test]
    fn pr_check_details_rest_fallback_fills_missing_links_without_overwriting_existing_ones() {
        let mut pr = serde_json::json!({
            "headSha": "abc1234567890def",
            "statusCheckRollup": [
                { "name": "build-status", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "https://existing.example/build" },
                { "context": "Collaborator", "state": "PENDING" },
                { "context": "Polarion Link", "state": "SUCCESS" },
                { "name": "Submodule status", "status": "COMPLETED", "conclusion": "SUCCESS" }
            ]
        });
        let statuses = serde_json::json!({
            "statuses": [
                { "context": "Collaborator", "target_url": "https://github.vitesco.io/eng/repo/status/collaborator" },
                { "context": "Polarion Link", "target_url": "https://github.vitesco.io/eng/repo/status/polarion" }
            ]
        });
        let check_runs = serde_json::json!({
            "check_runs": [
                { "name": "Submodule status", "details_url": "https://github.vitesco.io/eng/repo/actions/runs/123" },
                { "name": "build-status", "details_url": "https://github.vitesco.io/eng/repo/actions/runs/should-not-overwrite" }
            ]
        });
        let urls = collect_rest_check_detail_urls(Some(&statuses), Some(&check_runs));
        assert_eq!(fill_missing_pr_check_details_from_urls(&mut pr, &urls), 3);
        let checks = pr.get("statusCheckRollup").and_then(|value| value.as_array()).unwrap();
        assert_eq!(checks[0].get("detailsUrl").and_then(|value| value.as_str()), Some("https://existing.example/build"));
        assert_eq!(checks[1].get("targetUrl").and_then(|value| value.as_str()), Some("https://github.vitesco.io/eng/repo/status/collaborator"));
        assert_eq!(checks[2].get("targetUrl").and_then(|value| value.as_str()), Some("https://github.vitesco.io/eng/repo/status/polarion"));
        assert_eq!(checks[3].get("detailsUrl").and_then(|value| value.as_str()), Some("https://github.vitesco.io/eng/repo/actions/runs/123"));
    }

    #[test]
    fn pr_check_details_html_fallback_extracts_status_action_links_from_github_page() {
        let html = r#"
          <div class="merge-status-item">
            <strong class="text-emphasized mr-2">Collaborator</strong>
            <a class="status-actions" href="https://collaborator.vitesco.io/ui#review:id=601412" aria-label="Details for Collaborator.">Details</a>
          </div>
          <div class="merge-status-item">
            <strong class="text-emphasized mr-2">Polarion Link</strong>
            <a class="status-actions" href="/eng/sw-prj-VWAQ4_000U0/pull/282/checks?check_run_id=2765725&amp;foo=bar" aria-label="Details for Polarion Link.">Details</a>
          </div>
          <div class="merge-status-item">
            <strong class="text-emphasized mr-2">Submodule status</strong>
            <a class="status-actions" href="/eng/sw-prj-VWAQ4_000U0/pull/282/checks?check_run_id=2765726" aria-label="Details for Submodule status.">Details</a>
          </div>
        "#;
        let urls = collect_pr_check_detail_urls_from_html("https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282", html);
        assert_eq!(urls.get("collaborator").map(String::as_str), Some("https://collaborator.vitesco.io/ui#review:id=601412"));
        assert_eq!(urls.get("polarion link").map(String::as_str), Some("https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282/checks?check_run_id=2765725&foo=bar"));
        assert_eq!(urls.get("submodule status").map(String::as_str), Some("https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282/checks?check_run_id=2765726"));
    }

    #[test]
    fn pr_check_details_html_fallback_extracts_status_links_from_enterprise_blocks_without_aria() {
        let html = r#"
          <div class="merge-status-item d-flex flex-items-baseline">
            <div class="color-fg-muted col-10 css-truncate css-truncate-target">
              <strong class="text-emphasized mr-2">
                Collaborator
              </strong>
              <span class="text-italic">Pending</span>
              —
              <span class="text-italic">Review still in progress</span>
            </div>
            <div class="d-flex col-2 flex-shrink-0">
              <span class="label Label--primary">Required</span>
              <a class="status-actions" href="[https://collaborator.vitesco.io/ui#review:id=603480](https://collaborator.vitesco.io/ui#review:id=603480)">Details</a>
            </div>
          </div>
          <div class="merge-status-item d-flex flex-items-baseline">
            <div class="color-fg-muted col-10 css-truncate css-truncate-target">
              <strong class="text-emphasized mr-2">
                Polarion Link
              </strong>
              Successful in 1s
            </div>
            <div class="d-flex col-2 flex-shrink-0">
              <a class="status-actions" href="/eng/sw-prj-VWAQ4_000U0/pull/282/checks?check_run_id=2781386">Details</a>
            </div>
          </div>
          <div class="merge-status-item d-flex flex-items-baseline">
            <div class="color-fg-muted col-10 css-truncate css-truncate-target">
              <strong class="text-emphasized mr-2">
                Submodule status
              </strong>
              Successful in 2m
            </div>
            <div class="d-flex col-2 flex-shrink-0">
              <a class="status-actions" href="/eng/sw-prj-VWAQ4_000U0/pull/282/checks?check_run_id=2781387">Details</a>
            </div>
          </div>
        "#;
        let urls = collect_pr_check_detail_urls_from_html("https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282", html);
        assert_eq!(urls.get("collaborator").map(String::as_str), Some("https://collaborator.vitesco.io/ui#review:id=603480"));
        assert_eq!(urls.get("polarion link").map(String::as_str), Some("https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282/checks?check_run_id=2781386"));
        assert_eq!(urls.get("submodule status").map(String::as_str), Some("https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282/checks?check_run_id=2781387"));
    }

    #[test]
    fn pr_status_reports_no_remote_when_the_repository_has_none() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-pr-status-no-remote-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        let result = pr_status_inner(base.to_string_lossy().into_owned(), None, None).unwrap();
        assert_eq!(result.state, "no_remote");
        assert!(result.outgoing_pull_requests.is_empty());
        assert!(result.incoming_pull_requests.is_empty());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_reports_unsupported_provider_for_a_non_github_remote() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-pr-status-unsupported-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        run_git(&base, &["remote", "add", "origin", "https://gitlab.com/team/repo.git"]);
        let result = pr_status_inner(base.to_string_lossy().into_owned(), None, None).unwrap();
        assert_eq!(result.state, "unsupported_provider");
        assert!(result.outgoing_pull_requests.is_empty());
        assert!(result.incoming_pull_requests.is_empty());
        fs::remove_dir_all(base).unwrap();
    }

    // Shared setup: a real repo with one commit, a checked-out branch, and a
    // GitHub Enterprise `origin`. Returns (base_dir, repo_path_string).
    fn pr_repo(tag: &str) -> (PathBuf, String) {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-prctx-{tag}-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        let path = base.to_string_lossy().into_owned();
        (base, path)
    }

    fn ready_ctx(repo: &Repository, frontend_branch: Option<&str>) -> PrQueryContext {
        match resolve_pr_query_context(repo, frontend_branch) {
            PrContext::Ready(ctx) => ctx,
            PrContext::Terminal(result) => panic!("expected a resolvable context, got terminal state {}", result.state),
        }
    }

    fn terminal_state(repo: &Repository, frontend_branch: Option<&str>) -> String {
        match resolve_pr_query_context(repo, frontend_branch) {
            PrContext::Terminal(result) => result.state,
            PrContext::Ready(ctx) => panic!("expected a terminal result, got a resolvable context for {}/{}", ctx.head_repo.gh_repo_arg(), ctx.head_branch),
        }
    }

    fn base_slugs(ctx: &PrQueryContext) -> Vec<String> {
        ctx.candidate_bases.iter().map(|b| b.gh_repo_arg()).collect()
    }

    // A gh-JSON PR object shaped like `gh pr list --json ...` returns.
    fn pr_json(number: u64, head_ref: &str, head_owner: &str, head_repo: &str) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "title": format!("Change {number}"),
            "headRefName": head_ref,
            "headRepository": { "name": head_repo },
            "headRepositoryOwner": { "login": head_owner },
            "baseRefName": "main",
            "state": "OPEN",
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "statusCheckRollup": [{ "conclusion": "SUCCESS" }],
            "url": format!("https://github.example/{head_owner}/{head_repo}/pull/{number}"),
        })
    }

    #[test]
    fn resolve_pr_query_context_reads_head_from_git_not_the_stale_frontend_branch() {
        let (base, path) = pr_repo("head-truth");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "really-on-this"]);
        let repo = internal_repository(&path).unwrap();

        // Frontend still believes an old branch is checked out.
        let ctx = ready_ctx(&repo, Some("frontend-thinks-this"));
        assert_eq!(ctx.local_branch, "really-on-this", "must use the branch Git actually has checked out");
        assert_eq!(ctx.head_branch, "really-on-this");
        assert_eq!(ctx.frontend_branch_mismatch.as_deref(), Some("frontend-thinks-this"), "the disagreement is recorded (for logging), not acted on");
        assert_eq!(ctx.head_repo.gh_repo_arg(), "github.vitesco.io/eng/demo");
        assert_eq!(base_slugs(&ctx), vec!["github.vitesco.io/eng/demo"]);
        assert!(!ctx.had_upstream);

        // A matching frontend branch is not flagged.
        assert!(ready_ctx(&repo, Some("really-on-this")).frontend_branch_mismatch.is_none());
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn resolve_pr_query_context_queries_the_remote_branch_name_not_the_local_one() {
        let (base, path) = pr_repo("renamed-upstream");
        run_git(&base, &["remote", "add", "origin", "https://github.vitesco.io/eng/demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "local-name"]);
        run_git(&base, &["config", "branch.local-name.remote", "origin"]);
        run_git(&base, &["config", "branch.local-name.merge", "refs/heads/name-on-the-server"]);
        let repo = internal_repository(&path).unwrap();

        let ctx = ready_ctx(&repo, None);
        assert_eq!(ctx.local_branch, "local-name");
        assert_eq!(ctx.head_branch, "name-on-the-server", "gh --head must get the branch's name on the remote, not the local name");
        assert_eq!(ctx.tracking_remote, "origin");
        assert!(ctx.had_upstream);
        assert_eq!(base_slugs(&ctx), vec!["github.vitesco.io/eng/demo"]);
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn resolve_pr_query_context_follows_the_upstream_to_a_non_origin_remote() {
        let (base, path) = pr_repo("non-origin-upstream");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/local-fork.git"]);
        run_git(&base, &["remote", "add", "mirror", "https://github.vitesco.io/eng/canonical.git"]);
        run_git(&base, &["checkout", "-q", "-b", "work"]);
        run_git(&base, &["config", "branch.work.remote", "mirror"]);
        run_git(&base, &["config", "branch.work.merge", "refs/heads/work"]);
        let repo = internal_repository(&path).unwrap();

        let ctx = ready_ctx(&repo, None);
        assert_eq!(ctx.tracking_remote, "mirror", "the branch's own upstream remote wins over origin");
        assert_eq!(ctx.head_repo.host, "github.vitesco.io");
        assert_eq!((ctx.head_repo.owner.as_str(), ctx.head_repo.repo.as_str()), ("eng", "canonical"),
            "the branch lives on its tracking remote, not origin");
        // origin is a *different GitHub host* (github.com vs github.vitesco.io)
        // than the head repo — never a candidate, regardless of remote name;
        // see resolve_pr_query_context_only_considers_remotes_on_the_same_github_host.
        assert_eq!(base_slugs(&ctx), vec!["github.vitesco.io/eng/canonical"], "the head repo is tried first, and origin's different host excludes it");
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn resolve_pr_query_context_treats_every_github_remote_as_a_candidate_base_for_a_fork() {
        let (base, path) = pr_repo("fork");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/local-fork.git"]);
        run_git(&base, &["remote", "add", "upstream", "git@github.com:acme/product.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();

        let ctx = ready_ctx(&repo, None);
        assert_eq!(ctx.tracking_remote, "origin");
        assert_eq!((ctx.head_repo.owner.as_str(), ctx.head_repo.repo.as_str()), ("me", "local-fork"),
            "the branch lives on the fork");
        // Both the fork and the upstream are queried; the upstream is not
        // trusted just because it's *named* `upstream`, it's queried because
        // it's one of the repo's GitHub remotes.
        assert_eq!(base_slugs(&ctx), vec!["github.com/me/local-fork", "github.com/acme/product"]);
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn resolve_pr_query_context_reports_detached_head_and_unsupported_remote_as_terminal() {
        let (base, path) = pr_repo("terminal");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/demo.git"]);
        run_git(&base, &["checkout", "-q", "--detach"]);
        let repo = internal_repository(&path).unwrap();
        assert_eq!(terminal_state(&repo, None), "detached_head");
        drop(repo);

        run_git(&base, &["checkout", "-q", "-b", "back-on-a-branch"]);
        run_git(&base, &["remote", "set-url", "origin", "https://bitbucket.org/team/demo.git"]);
        let repo = internal_repository(&path).unwrap();
        assert_eq!(terminal_state(&repo, None), "unsupported_provider");
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_on_a_detached_head_returns_a_clear_state_without_calling_gh() {
        let (base, path) = pr_repo("detached-cmd");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/demo.git"]);
        run_git(&base, &["checkout", "-q", "--detach"]);
        let result = pr_status_inner(path, None, None).unwrap();
        assert_eq!(result.state, "detached_head");
        assert!(result.queried_repo.is_none());
        assert!(result.outgoing_pull_requests.is_empty());
        assert!(result.incoming_pull_requests.is_empty());
        fs::remove_dir_all(base).unwrap();
    }

    // ---- select_matching_prs: pure filtering of a gh response ----

    #[test]
    fn select_matching_prs_keeps_a_pr_in_the_same_repo() {
        let raw = vec![pr_json(7, "feature/x", "eng", "demo")];
        let prs = select_matching_prs(&raw, "feature/x", "eng", "demo");
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 7);
        assert_eq!(prs[0].source_branch, "feature/x");
        assert_eq!(prs[0].url, "https://github.example/eng/demo/pull/7");
    }

    #[test]
    fn select_matching_prs_returns_nothing_for_a_genuinely_empty_response() {
        assert!(select_matching_prs(&[], "feature/x", "eng", "demo").is_empty());
    }

    #[test]
    fn select_matching_prs_drops_a_pr_whose_head_ref_is_the_local_name_not_the_remote_name() {
        // The response carries the *local* branch name; we asked about the
        // remote name — it must not match.
        let raw = vec![pr_json(9, "local-name", "eng", "demo")];
        assert!(select_matching_prs(&raw, "name-on-the-server", "eng", "demo").is_empty());
    }

    #[test]
    fn select_matching_prs_drops_a_fork_pr_from_a_different_head_owner() {
        let raw = vec![
            pr_json(1, "feature/x", "eng", "demo"),        // ours
            pr_json(2, "feature/x", "someone-else", "demo"), // a same-named branch in an unrelated fork
        ];
        let prs = select_matching_prs(&raw, "feature/x", "eng", "demo");
        assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn select_matching_prs_with_two_forks_sharing_a_branch_name_keeps_only_ours() {
        let raw = vec![
            pr_json(10, "shared-name", "fork-a", "product"),
            pr_json(11, "shared-name", "fork-b", "product"),
            pr_json(12, "shared-name", "fork-b", "unrelated-repo"),
        ];
        let prs = select_matching_prs(&raw, "shared-name", "fork-b", "product");
        assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), vec![11]);
    }

    // ---- pr_status_impl: candidate-base iteration + response handling,
    //      with an injected gh runner (no real `gh` process) ----

    fn ctx_for(repo: &Repository) -> PrQueryContext {
        ready_ctx(repo, None)
    }

    #[test]
    fn pr_status_impl_forms_the_query_and_parses_a_pr_into_state_ok() {
        let (base, path) = pr_repo("impl-ok");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/sw-prj-OMBMS_000U0.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature/local"]);
        run_git(&base, &["config", "branch.feature/local.remote", "origin"]);
        run_git(&base, &["config", "branch.feature/local.merge", "refs/heads/feature/on-server"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);

        // Mutex, not RefCell: pr_status_impl now dispatches candidates
        // concurrently, so `run` must be Sync. Discriminates by direction —
        // real `gh pr list --head` and `--base` are different, real calls;
        // a fake here that answered every query identically would make an
        // outgoing PR show up as incoming too, which is exactly the
        // muddling this whole mechanism exists to keep apart.
        let seen = Mutex::new(Vec::<PrGhQuery>::new());
        let run = |q: &PrGhQuery| {
            seen.lock().unwrap().push(q.clone());
            match q.direction {
                PrQueryDirection::Head => GhOutcome::Prs(vec![pr_json(42, "feature/on-server", "eng", "sw-prj-OMBMS_000U0")]),
                PrQueryDirection::Base => GhOutcome::Prs(Vec::new()),
            }
        };
        let result = pr_status_impl(ctx, &path, "parent", &run);

        {
            let queries = seen.lock().unwrap();
            assert_eq!(queries.len(), 2, "the enterprise head repo, outgoing and incoming");
            assert!(queries.contains(&PrGhQuery {
                repo: "github.vitesco.io/eng/sw-prj-OMBMS_000U0".into(),
                branch: "feature/on-server".into(), direction: PrQueryDirection::Head,
            }), "outgoing query against the enterprise head repo, for the remote branch name");
            assert!(queries.contains(&PrGhQuery {
                repo: "github.vitesco.io/eng/sw-prj-OMBMS_000U0".into(),
                branch: "feature/on-server".into(), direction: PrQueryDirection::Base,
            }), "incoming query against the same repo and branch");
        }
        assert_eq!(result.state, "ok");
        assert_eq!(result.queried_repo.as_deref(), Some("github.vitesco.io/eng/sw-prj-OMBMS_000U0"));
        assert_eq!(result.branch.as_deref(), Some("feature/on-server"));
        assert_eq!(result.outgoing_pull_requests.len(), 1);
        assert_eq!(result.outgoing_pull_requests[0].number, 42);
        assert_eq!(result.outgoing_pull_requests[0].review_summary, "approved");
        assert_eq!(result.outgoing_pull_requests[0].checks_status, "passing");
        assert!(result.incoming_pull_requests.is_empty());
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_reports_no_open_pr_on_a_real_empty_response() {
        let (base, path) = pr_repo("impl-empty");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feat"]);
        run_git(&base, &["config", "branch.feat.remote", "origin"]);
        run_git(&base, &["config", "branch.feat.merge", "refs/heads/feat"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        let run = |_: &PrGhQuery| GhOutcome::Prs(Vec::new());
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "no_open_pr");
        assert_eq!(result.branch.as_deref(), Some("feat"));
        assert!(result.outgoing_pull_requests.is_empty());
        assert!(result.incoming_pull_requests.is_empty());
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_command_wires_context_through_for_an_enterprise_remote() {
        // The #[tauri::command] wrapper itself: it must resolve the enterprise
        // context and hand it to the real gh runner. gh isn't authenticated
        // for this host here, so the state is auth_missing — but the context
        // fields the UI renders ("... in <host/owner/repo>") must be right.
        let (base, path) = pr_repo("cmd-enterprise");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/sw-prj-OMBMS_000U0.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature/local"]);
        run_git(&base, &["config", "branch.feature/local.remote", "origin"]);
        run_git(&base, &["config", "branch.feature/local.merge", "refs/heads/feature/on-server"]);
        let result = pr_status_inner(path, Some("some-stale-branch".into()), Some("parent".into())).unwrap();
        assert_eq!(result.queried_repo.as_deref(), Some("github.vitesco.io/eng/sw-prj-OMBMS_000U0"));
        assert_eq!(result.branch.as_deref(), Some("feature/on-server"));
        assert_ne!(result.state, "unsupported_provider");
        assert_ne!(result.state, "no_remote");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_surfaces_an_api_error_instead_of_reporting_no_open_pr() {
        let (base, path) = pr_repo("impl-err");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feat"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        let run = |_: &PrGhQuery| GhOutcome::Failure { stderr: "HTTP 500: something broke".into() };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "api_error", "a failed head-repo query must never read as a confident 'no PR'");
        assert!(result.detail.contains("500"));
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_auth_failure_on_enterprise_explains_both_supported_credentials() {
        let (base, path) = pr_repo("impl-auth");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feat"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        let run = |_: &PrGhQuery| GhOutcome::Failure { stderr: "You are not logged into any GitHub hosts. Run gh auth login".into() };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "auth_missing");
        assert!(result.detail.contains("github.vitesco.io"), "detail was: {}", result.detail);
        assert!(result.detail.contains("Git Credential Manager"), "detail was: {}", result.detail);
        assert!(result.detail.contains("optional GitHub CLI"), "detail was: {}", result.detail);
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_tries_every_candidate_base_not_just_a_remote_named_upstream() {
        // A fork checkout with a WRONG `upstream` remote and the real base
        // under a third name. A single guess at `upstream` would report "no
        // PR"; iterating every candidate finds it.
        let (base, path) = pr_repo("impl-candidates");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/fork.git"]);
        run_git(&base, &["remote", "add", "upstream", "git@github.com:wrong/place.git"]);
        run_git(&base, &["remote", "add", "canonical", "git@github.com:acme/product.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);

        let seen = Mutex::new(Vec::<String>::new());
        let run = |q: &PrGhQuery| {
            seen.lock().unwrap().push(q.repo.clone());
            if q.repo == "github.com/acme/product" {
                GhOutcome::Prs(vec![pr_json(5, "feature", "me", "fork")]) // head repo is our fork
            } else {
                GhOutcome::Prs(Vec::new())
            }
        };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "ok");
        assert_eq!(result.outgoing_pull_requests.iter().map(|p| p.number).collect::<Vec<_>>(), vec![5]);
        let queried = seen.lock().unwrap().clone();
        assert!(queried.contains(&"github.com/me/fork".to_string()));
        assert!(queried.contains(&"github.com/wrong/place".to_string()));
        assert!(queried.contains(&"github.com/acme/product".to_string()));
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_dedupes_a_pr_reached_through_two_candidate_bases() {
        let (base, path) = pr_repo("impl-dedupe");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/fork.git"]);
        run_git(&base, &["remote", "add", "upstream", "git@github.com:acme/product.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        // Both `--repo` targets return the same PR (same canonical url) for
        // the outgoing (--head) direction only — this test is specifically
        // about deduping *across candidate bases*, not about the incoming
        // side, which is kept empty so it can't muddy that assertion.
        let run = |q: &PrGhQuery| match q.direction {
            PrQueryDirection::Head => GhOutcome::Prs(vec![pr_json(88, "feature", "me", "fork")]),
            PrQueryDirection::Base => GhOutcome::Prs(Vec::new()),
        };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "ok");
        assert_eq!(result.outgoing_pull_requests.len(), 1, "one PR, not one per candidate base");
        assert!(result.incoming_pull_requests.is_empty());
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_never_reports_no_open_pr_when_a_relevant_candidate_failed() {
        // The head repo itself comes back genuinely empty, but a second
        // relevant candidate (the real upstream) could not be checked — this
        // must read as "the search was incomplete", never as a confident
        // "no open PR".
        let (base, path) = pr_repo("impl-partial");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/fork.git"]);
        run_git(&base, &["remote", "add", "upstream", "git@github.com:acme/product.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        let run = |q: &PrGhQuery| {
            if q.repo == "github.com/me/fork" { GhOutcome::Prs(Vec::new()) }
            else { GhOutcome::Failure { stderr: "HTTP 502".into() } }
        };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "partial_result", "an empty head repo plus a failed secondary candidate must not read as no_open_pr");
        assert!(result.partial);
        assert!(result.outgoing_pull_requests.is_empty());
        assert!(result.incoming_pull_requests.is_empty());
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn resolve_pr_query_context_only_considers_remotes_on_the_same_github_host() {
        // A remote on a *different* GitHub host (github.com vs an
        // enterprise instance) can never legitimately hold this branch's PR
        // — gh's auth context is per-host, and a fork/upstream pair is never
        // split across two different GitHub instances.
        let (base, path) = pr_repo("cross-host");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/demo.git"]);
        run_git(&base, &["remote", "add", "mirror", "git@github.com:eng/demo-mirror.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        assert_eq!(base_slugs(&ctx), vec!["github.vitesco.io/eng/demo"], "the github.com remote must be excluded — different host than the head repo");
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn resolve_pr_query_context_caps_candidates_and_flags_the_result_as_truncated() {
        let (base, path) = pr_repo("many-remotes");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/fork.git"]);
        for (name, owner) in [("r1", "org1"), ("r2", "org2"), ("r3", "org3"), ("r4", "org4"), ("r5", "org5")] {
            run_git(&base, &["remote", "add", name, &format!("git@github.com:{owner}/repo.git")]);
        }
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        assert_eq!(ctx.candidate_bases.len(), PR_MAX_CANDIDATE_BASES, "must cap at the concurrency limit, not query all 6 remotes");
        assert!(ctx.candidates_truncated, "6 same-host GitHub remotes exceeds the cap of {PR_MAX_CANDIDATE_BASES} — must be flagged truncated");

        // And the truncation alone (even with every *queried* candidate
        // succeeding empty) must still mark the result partial, not a
        // confident empty.
        let run = |_: &PrGhQuery| GhOutcome::Prs(Vec::new());
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "partial_result");
        assert!(result.partial);
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn resolve_pr_query_context_adds_a_remotes_push_url_as_a_candidate_when_it_differs() {
        let (base, path) = pr_repo("pushurl");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/fetch-side.git"]);
        run_git(&base, &["remote", "set-url", "--push", "origin", "git@github.com:me/push-side.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        assert_eq!(base_slugs(&ctx), vec!["github.com/me/fetch-side", "github.com/me/push-side"], "a distinct push URL must be queried too — where commits actually land can differ from the fetch URL");
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_queries_candidates_concurrently_not_sequentially() {
        // Three candidates, each artificially slow — sequential would take
        // roughly 3x as long as the slowest one; concurrent takes roughly 1x.
        let (base, path) = pr_repo("concurrency");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/fork.git"]);
        run_git(&base, &["remote", "add", "upstream", "git@github.com:acme/product.git"]);
        run_git(&base, &["remote", "add", "third", "git@github.com:third/party.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        assert_eq!(ctx.candidate_bases.len(), 3, "sanity check: three candidates to race");

        const PER_CALL: Duration = Duration::from_millis(200);
        let run = |_: &PrGhQuery| { std::thread::sleep(PER_CALL); GhOutcome::Prs(Vec::new()) };
        let started = Instant::now();
        let _ = pr_status_impl(ctx, &path, "parent", &run);
        let elapsed = started.elapsed();
        assert!(elapsed < PER_CALL * 2, "3 candidates dispatched concurrently should take roughly 1x the per-call delay ({PER_CALL:?}), not the sequential ~3x; took {elapsed:?}");
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_generation_tracks_the_latest_request_per_repository() {
        let path = "some/fake/repo/path/for/generation/tracking/only";
        let first = claim_pr_status_generation(path);
        let second = claim_pr_status_generation(path);
        assert_ne!(first, second);
        assert!(!is_latest_pr_status_generation(path, first), "an older claimed generation must no longer read as latest once a newer one exists");
        assert!(is_latest_pr_status_generation(path, second));

        // A different repository path has its own, independent counter.
        let other_path = "some/other/fake/repo/path";
        let other_first = claim_pr_status_generation(other_path);
        assert!(is_latest_pr_status_generation(other_path, other_first));
        assert!(is_latest_pr_status_generation(path, second), "unrelated to another repository's generation counter");
    }

    #[test]
    fn pr_status_impl_resolves_a_submodules_own_context_and_produces_a_pr_card() {
        // The mandatory positive test: the active context is a *submodule*.
        // pr_status must resolve HEAD/upstream from the submodule's own repo
        // (its own branch, its own enterprise remote) and a real PR response
        // must produce a card for that repo and branch — never the parent's.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-pr-submodule-{suffix}"));
        let parent = base.join("parent");
        let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "lib.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        run_git(&parent, &["remote", "add", "origin", "git@github.vitesco.io:eng/the-parent.git"]);
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = parent.join(&added);
        let sub_path_str = sub_path.to_string_lossy().into_owned();

        // The submodule is on its own branch, with its own enterprise remote
        // and its own (differently named) remote branch.
        run_git(&sub_path, &["remote", "set-url", "origin", "git@github.vitesco.io:eng/the-dependency.git"]);
        run_git(&sub_path, &["checkout", "-q", "-b", "dep-feature"]);
        run_git(&sub_path, &["config", "branch.dep-feature.remote", "origin"]);
        run_git(&sub_path, &["config", "branch.dep-feature.merge", "refs/heads/dep-feature-remote"]);

        let sub_repo = internal_repository(&sub_path_str).unwrap();
        let ctx = ready_ctx(&sub_repo, Some("stale-parent-branch"));
        assert_eq!(ctx.head_repo.gh_repo_arg(), "github.vitesco.io/eng/the-dependency", "must be the submodule's own repo, not the parent's");
        assert_eq!(ctx.head_branch, "dep-feature-remote");

        let run = |q: &PrGhQuery| {
            assert_eq!(q.repo, "github.vitesco.io/eng/the-dependency");
            assert_eq!(q.branch, "dep-feature-remote");
            match q.direction {
                PrQueryDirection::Head => GhOutcome::Prs(vec![pr_json(3, "dep-feature-remote", "eng", "the-dependency")]),
                PrQueryDirection::Base => GhOutcome::Prs(Vec::new()),
            }
        };
        let result = pr_status_impl(ctx, &sub_path_str, "submodule:dep", &run);
        assert_eq!(result.state, "ok");
        assert_eq!(result.queried_repo.as_deref(), Some("github.vitesco.io/eng/the-dependency"));
        assert_eq!(result.branch.as_deref(), Some("dep-feature-remote"));
        assert_eq!(result.outgoing_pull_requests.len(), 1);
        assert_eq!(result.outgoing_pull_requests[0].number, 3);
        assert!(result.incoming_pull_requests.is_empty());
        drop(sub_repo);
        fs::remove_dir_all(base).unwrap();
    }

    // ---- Incoming PRs (current branch as the PR's base/target) ----

    #[test]
    fn pr_status_impl_detects_an_incoming_pr_whose_head_is_a_different_branch_than_ours() {
        // The exact reported case: current branch is feature/status-cache,
        // and the open PR has head=main, base=feature/status-cache — a PR
        // proposing to merge main *into* this branch, not out of it.
        // `gh pr list --head feature/status-cache` (the old, only query)
        // finds nothing, exactly as reported; it must now be found via the
        // new --base query and reported as incoming — and specifically
        // never as outgoing, whose own head-owner validation has no reason
        // to accept a PR whose real head is a completely different branch.
        let (base, path) = pr_repo("incoming-repro");
        run_git(&base, &["remote", "add", "origin", "git@github.com:AndreiRomanC/git-stress-small-demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature/status-cache"]);
        run_git(&base, &["config", "branch.feature/status-cache.remote", "origin"]);
        run_git(&base, &["config", "branch.feature/status-cache.merge", "refs/heads/feature/status-cache"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        assert_eq!(ctx.head_branch, "feature/status-cache");

        let incoming_pr = serde_json::json!({
            "number": 1, "title": "Merge main into feature/status-cache",
            "headRefName": "main", "headRepository": { "name": "git-stress-small-demo" }, "headRepositoryOwner": { "login": "AndreiRomanC" },
            "baseRefName": "feature/status-cache", "state": "OPEN", "isDraft": false, "mergeable": "MERGEABLE",
            "reviewDecision": "REVIEW_REQUIRED", "statusCheckRollup": [],
            "url": "https://github.com/AndreiRomanC/git-stress-small-demo/pull/1",
        });
        let run = |q: &PrGhQuery| {
            assert_eq!(q.repo, "github.com/AndreiRomanC/git-stress-small-demo");
            match q.direction {
                PrQueryDirection::Head => GhOutcome::Prs(Vec::new()), // reproduces `gh pr list --head ... => []`
                PrQueryDirection::Base => GhOutcome::Prs(vec![incoming_pr.clone()]), // reproduces `gh pr list --base ... => PR #1`
            }
        };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "ok");
        assert!(result.outgoing_pull_requests.is_empty(), "PR #1's real head is main, not this branch — it must never appear as outgoing");
        assert_eq!(result.incoming_pull_requests.len(), 1, "PR #1 targets this branch as its base — it must appear as incoming");
        assert_eq!(result.incoming_pull_requests[0].number, 1);
        assert_eq!(result.incoming_pull_requests[0].source_branch, "main");
        assert_eq!(result.incoming_pull_requests[0].target_branch, "feature/status-cache");
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_detects_incoming_prs_even_without_an_upstream_configured() {
        // Point 5 of the report: a missing upstream must not prevent
        // incoming-PR detection — head_repo/head_branch already fall back
        // to the first remote and the local branch name in this case (see
        // resolve_pr_query_context), and the incoming query must use
        // exactly that, unconditionally, the same as the outgoing query
        // already did before this change.
        let (base, path) = pr_repo("incoming-no-upstream");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature/no-upstream"]); // deliberately no branch.<name>.remote/.merge
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        assert!(!ctx.had_upstream, "sanity check: this branch genuinely has no configured upstream");
        assert_eq!(ctx.head_branch, "feature/no-upstream", "falls back to the local branch name");

        let run = |q: &PrGhQuery| match q.direction {
            PrQueryDirection::Head => GhOutcome::Prs(Vec::new()),
            PrQueryDirection::Base => GhOutcome::Prs(vec![pr_json(9, "someone-elses-branch", "them", "demo")]),
        };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "ok");
        assert_eq!(result.incoming_pull_requests.len(), 1, "a missing upstream must not prevent incoming PR detection");
        drop(repo);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pr_status_impl_reports_partial_when_only_the_incoming_query_fails() {
        // Point 6 of the report: an empty result is only ever confident
        // once *both* relevant queries succeeded. Here the outgoing side
        // comes back genuinely empty and successful, but the incoming
        // query itself fails — this must never read as a confident "no
        // open PR" just because the outgoing half happened to be fine.
        let (base, path) = pr_repo("incoming-failed");
        run_git(&base, &["remote", "add", "origin", "git@github.com:me/demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feature"]);
        run_git(&base, &["config", "branch.feature.remote", "origin"]);
        run_git(&base, &["config", "branch.feature.merge", "refs/heads/feature"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        let run = |q: &PrGhQuery| match q.direction {
            PrQueryDirection::Head => GhOutcome::Prs(Vec::new()),
            PrQueryDirection::Base => GhOutcome::Failure { stderr: "HTTP 503".into() },
        };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "partial_result", "an incoming-query failure must not be swallowed into a confident no_open_pr");
        assert!(result.partial);
        drop(repo);
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
        let push_before_commit = push_submodule_inner(repo_path.clone(), "vendor/dep".into());
        assert!(push_before_commit.is_err(), "expected an error when pushing with nothing new, got Ok");
        let message = push_before_commit.unwrap_err();
        assert!(message.to_lowercase().contains("commit") || message.to_lowercase().contains("nothing") || message.to_lowercase().contains("up to date") || message.to_lowercase().contains("up-to-date"), "message should explain there is nothing to push / not committed yet, got: {message}");

        // Modifying the file but NOT committing must block push with a clear,
        // specific warning — silently pushing an older commit while leaving fresh
        // edits behind would be worse than doing nothing.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        let push_with_uncommitted = push_submodule_inner(repo_path.clone(), "vendor/dep".into());
        assert!(push_with_uncommitted.is_err(), "expected an error when pushing with uncommitted changes, got Ok");
        let uncommitted_message = push_with_uncommitted.unwrap_err();
        assert!(uncommitted_message.to_lowercase().contains("uncommitted") || uncommitted_message.to_lowercase().contains("commit"), "message should warn about uncommitted changes, got: {uncommitted_message}");

        // Commit through our command only (not pushing yet), then push must succeed.
        let head_before = Repository::open(&repository).unwrap().head().unwrap().target().unwrap();
        let committed = commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into(), false).expect("commit_submodule should succeed");
        assert!(!committed.pushed, "also_push was false — nothing should have been pushed");

        // Submodule-publish-safety report, point 1: committing inside the
        // submodule must NEVER touch the parent on its own — the parent still
        // records the OLD commit (an ordinary unstaged modification, exactly
        // like editing any other tracked file) until the submodule is
        // actually pushed.
        let parent_index_oid = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new("vendor/dep"), 0).unwrap().id };
        let submodule_head_oid = { let repo = Repository::open(&sub_path).unwrap(); let oid = repo.head().unwrap().target().unwrap(); oid };
        assert_ne!(parent_index_oid, submodule_head_oid, "the parent must NOT record the submodule's new commit merely because it was committed locally — that is exactly the unsafe auto-commit this fixes");
        assert_eq!(Repository::open(&repository).unwrap().head().unwrap().target().unwrap(), head_before, "committing inside the submodule must never create a new commit in the parent");

        let parent_changes = load_repository_inner(repo_path.clone(), Some(true)).unwrap().changes;
        let dep_change = parent_changes.iter().find(|change| change.path == "vendor/dep").expect("the submodule must show as an ordinary unstaged modification before any push");
        assert!(!dep_change.staged, "must not be staged either — nothing was pushed yet");

        // Now push should succeed, and — since the commit is now safely on the
        // submodule's own server — the parent's gitlink should be staged (never
        // committed on its own) so the submodule stops showing as an ordinary
        // unstaged modification.
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).expect("push_submodule should succeed after a commit");
        let parent_index_oid_after_push = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new("vendor/dep"), 0).unwrap().id };
        let submodule_head_oid_after_push = { let repo = Repository::open(&sub_path).unwrap(); let oid = repo.head().unwrap().target().unwrap(); oid };
        assert_eq!(parent_index_oid_after_push, submodule_head_oid_after_push, "after a successful push, the parent's gitlink should be staged to the new submodule commit");
        assert_eq!(Repository::open(&repository).unwrap().head().unwrap().target().unwrap(), head_before, "pushing the submodule must still never create a commit in the parent on its own — only staging");
        let parent_changes_after_push = load_repository_inner(repo_path.clone(), Some(true)).unwrap().changes;
        let staged_change = parent_changes_after_push.iter().find(|change| change.path == "vendor/dep").expect("the submodule's gitlink change should still be listed, now staged");
        assert!(staged_change.staged, "after push, the parent's reference should be staged, ready for the user's own next parent commit");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn detached_submodule_commit_must_be_attached_to_a_branch_before_push() {
        // Submodules are checked out detached by default. A commit made there
        // must never be silently sent to a guessed default branch: the user
        // first has to preserve it on an explicitly chosen local branch.
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
        let detached_at_known_tip = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert!(detached_at_known_tip.current_containing_branches.iter().any(|name| name == "main"),
            "detached HEAD may still be reachable from a known branch, but must remain presented as detached");
        assert_eq!(detached_at_known_tip.history_context_branch, "main", "a known containing local branch should provide the history context without attaching HEAD");
        let main_context = detached_at_known_tip.versions.iter().find(|version| version.kind == "branch" && version.name == "main").unwrap();
        assert!(main_context.contains_current, "the branch row should identify that its history contains detached HEAD");
        assert_eq!(main_context.commits_after_current, Some(0), "the active commit is exactly this branch tip");

        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        let commit_error = commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Detached commit".into(), false).unwrap_err();
        assert!(commit_error.contains("detached") && commit_error.contains("branch"), "the app must require a branch before creating a submodule commit: {commit_error}");

        // A commit can still be created externally while detached. The app
        // must present and protect that state correctly, because users can
        // arrive here through tools outside Git Drill Down.
        run_git(&sub_path, &["commit", "-am", "Detached commit"]);
        assert!(Repository::open(&sub_path).unwrap().head_detached().unwrap(), "external Git can still create a detached commit");

        let versions = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert_eq!(versions.current_branch, "", "the version dialog must not expose Git's synthetic HEAD shorthand as a branch name");
        assert_eq!(versions.current_revision, git(&sub_path.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim());
        assert!(versions.current_containing_branches.is_empty(), "a detached commit created beyond every known branch tip must not be presented as belonging to main or origin/main");
        assert_eq!(versions.history_context_branch, "", "without a containing branch, history must start at detached HEAD instead of guessing main");
        assert!(versions.versions.iter().filter(|version| version.kind == "branch" || version.kind == "remote").all(|version| !version.contains_current && version.commits_after_current.is_none()),
            "no inactive branch row may claim ancestry for an unreachable detached commit");
        assert_eq!(versions.history_limit, 100);

        let preview_error = push_submodule_preview_inner(repo_path.clone(), "vendor/dep".into()).unwrap_err();
        assert!(preview_error.contains("detached HEAD"), "preview must identify the real state: {preview_error}");
        assert!(preview_error.contains("New branch") && preview_error.contains("switch to a branch"), "preview must explain the safe recovery: {preview_error}");

        let push_error = push_submodule_inner(repo_path.clone(), "vendor/dep".into()).unwrap_err();
        assert!(push_error.contains("detached HEAD"), "actual push must enforce the same rule as preview: {push_error}");

        let remote_main = git(&dep_remote.to_string_lossy(), &["log", "-1", "--format=%s", "main"]).unwrap();
        assert!(!remote_main.contains("Detached commit"), "the detached commit must not be published to a guessed main branch");

        // Once the user explicitly names the branch, normal push works.
        run_git(&sub_path, &["switch", "-c", "feature/detached-work"]);
        let result = push_submodule_inner(repo_path.clone(), "vendor/dep".into()).expect("an explicitly attached branch should be pushable");
        assert_eq!(result.branch, "feature/detached-work");
        let published = git(&dep_remote.to_string_lossy(), &["log", "-1", "--format=%s", "feature/detached-work"]).unwrap();
        assert!(published.contains("Detached commit"));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn push_preflight_blocks_a_checked_out_branch_in_a_non_bare_local_origin() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-non-bare-origin-{suffix}"));
        let repository = base.join("main");
        let working_origin = base.join("working-origin");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&working_origin).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(working_origin.join("module.txt"), "v1").unwrap();
        for path in [&repository, &working_origin] {
            run_git(path, &["init", "-b", "main"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", working_origin.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);
        // Be explicit: the local clone is attached to the same branch that is
        // currently checked out in the non-bare destination.
        run_git(&sub_path, &["switch", "main"]);
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "Local update"]);
        let remote_before = git(&working_origin.to_string_lossy(), &["rev-parse", "main"]).unwrap();

        let preview = push_submodule_preview_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert_eq!((preview.ahead, preview.behind), (1, 0), "comparison itself must remain correct");
        assert!(!preview.can_push, "preview must disable a push Git will safely refuse");
        let reason = preview.blocked_reason.as_deref().unwrap_or_default();
        assert!(reason.contains("local working repository") && reason.contains("main") && reason.contains("bare"), "the setup problem and safe alternatives must be clear: {reason}");

        let error = push_submodule_inner(repo_path, "vendor/dep".into()).unwrap_err();
        assert!(error.contains("local working repository"), "actual push must enforce the same preflight: {error}");
        let remote_after = git(&working_origin.to_string_lossy(), &["rev-parse", "main"]).unwrap();
        assert_eq!(remote_before, remote_after, "blocked push must not move the destination branch");

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

        let versions = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        let feature_branch = versions.versions.iter().find(|version| version.kind == "branch" && version.name == "feature-x").expect("feature-x should be listed as a local branch");

        let switched = switch_submodule_version_inner(repo_path.clone(), "vendor/dep".into(), feature_branch.revision.clone(), feature_branch.kind.clone(), feature_branch.name.clone());
        assert!(switched.is_ok(), "switching to a local branch by name should succeed, got: {:?}", switched);
        assert!(!Repository::open(&sub_path).unwrap().head_detached().unwrap(), "switching to a branch must leave HEAD attached to it, not detached");
        assert_eq!(Repository::open(&sub_path).unwrap().head().unwrap().shorthand(), Some("feature-x"));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn switch_submodule_version_to_a_remote_branch_creates_a_tracking_local_branch() {
        // A remote-tracking row like "origin/feature-x" is not a real branch
        // a worktree can attach to. The user-facing operation should mirror
        // `git switch --track origin/feature-x`: create a local branch,
        // attach HEAD to it, and remember the upstream so the UI does not
        // show a confusing duplicate "REMOTE ONLY" entry afterwards.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-switch-remote-branch-{suffix}"));
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

        run_git(&dependency, &["switch", "-c", "feature-x"]);
        fs::write(dependency.join("feature.txt"), "remote feature").unwrap();
        run_git(&dependency, &["add", "."]);
        run_git(&dependency, &["commit", "-m", "Remote feature"]);
        run_git(&sub_path, &["fetch", "origin"]);

        let versions = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        let remote_branch = versions.versions.iter().find(|version| version.kind == "remote" && version.name == "origin/feature-x").expect("origin/feature-x should be listed as a remote branch");

        let switched = switch_submodule_version_inner(repo_path, "vendor/dep".into(), remote_branch.revision.clone(), remote_branch.kind.clone(), remote_branch.name.clone());
        assert!(switched.is_ok(), "switching to a remote branch should create a local tracking branch, got: {:?}", switched);
        let sub_repo = Repository::open(&sub_path).unwrap();
        assert!(!sub_repo.head_detached().unwrap(), "remote checkout must leave HEAD attached to a local branch, not detached");
        assert_eq!(sub_repo.head().unwrap().shorthand(), Some("feature-x"));
        let upstream = sub_repo.find_branch("feature-x", BranchType::Local).unwrap().upstream().unwrap();
        assert_eq!(upstream.name().unwrap(), Some("origin/feature-x"));
        assert_eq!(upstream.get().target(), sub_repo.head().unwrap().target());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_repository_never_falls_back_to_the_parent_when_the_submodules_git_is_missing() {
        // Reproduces the report exactly, before any fix: a submodule
        // registered in the index, its directory present on disk, but its
        // own .git missing (a common real state — an interrupted clone, a
        // manually deleted .git, a submodule checked out some other way).
        // Repository::discover (what internal_repository used everywhere)
        // walks *up* from a path with no .git of its own until it finds one
        // — which is the parent's .git, right above vendor/dep — and opens
        // that instead of failing. submodule_repository (what "Submodule
        // Branch Map" calls) must error clearly instead, never silently
        // return the parent's own branches/commits under the submodule's
        // name.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-git-missing-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = parent.join(&added);

        // Confirm the parent really does have its own distinct history the
        // bug could leak — a second commit only the parent has.
        fs::write(parent.join("README.md"), "parent-only change").unwrap();
        create_commit(parent_string.clone(), "Parent-only commit".into()).unwrap();
        let parent_head = Repository::open(&parent).unwrap().head().unwrap().target().unwrap().to_string();

        // Delete the submodule's own .git (directory or gitlink file, matches
        // what a real "the submodule's Git metadata went missing" looks like)
        // while its working directory and index registration stay exactly as
        // they were.
        let sub_git = sub_path.join(".git");
        if sub_git.is_dir() { fs::remove_dir_all(&sub_git).unwrap(); } else { fs::remove_file(&sub_git).unwrap(); }
        assert!(sub_path.is_dir(), "sanity check: the submodule's working directory must still exist");
        assert!(!sub_git.exists(), "sanity check: its own .git must genuinely be gone");

        let result = submodule_repository_inner(parent_string, added);
        match result {
            Err(message) => assert!(message.contains("not initialized") || message.contains("Git metadata"), "expected a clear 'not initialized' error, got: {message}"),
            Ok(data) => panic!("submodule_repository must never succeed here — it silently returned the PARENT's own repository instead of erroring (head commit: {}, which {} the parent's real HEAD {parent_head})", data.repository.head_oid, if data.repository.head_oid == parent_head { "IS" } else { "is not" }),
        }

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_repository_returns_only_the_submodules_own_data_even_when_both_are_named_main() {
        // Both the parent and the submodule use the conventional "main"
        // branch name — the case most likely to look "the same" if the two
        // ever got mixed up. A real, valid submodule (its own .git present
        // and correct, the case Repository::open must succeed for, not just
        // reject) must return exactly its own history, never the parent's.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-main-vs-main-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&parent, &["branch", "-M", "main"]);
        run_git(&dependency, &["branch", "-M", "main"]);
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = parent.join(&added);
        // A second commit each, so their histories genuinely diverge, not
        // just their tips.
        fs::write(parent.join("README.md"), "parent v2").unwrap();
        create_commit(parent_string.clone(), "Parent second commit".into()).unwrap();
        fs::write(sub_path.join("module.txt"), "dep v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "Submodule second commit"]);

        let parent_head = Repository::open(&parent).unwrap().head().unwrap().target().unwrap().to_string();
        let sub_head = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap().to_string();
        assert_ne!(parent_head, sub_head, "sanity check: the two must genuinely have different HEADs for this test to mean anything");

        let data = submodule_repository_inner(parent_string, added).unwrap();
        assert_eq!(data.repository.current_branch, "main");
        assert_eq!(data.repository.head_oid, sub_head, "must be the submodule's own HEAD, never the parent's");
        assert!(data.commits.iter().any(|c| c.subject == "Submodule second commit"), "the submodule's own commit must be present");
        assert!(!data.commits.iter().any(|c| c.subject == "Parent second commit"), "the parent's commit must NOT leak into the submodule's history");
        assert!(!data.commits.iter().any(|c| c.subject == "Add dep submodule"), "the parent's own commit that merely references the submodule must not appear either — this is the submodule's history, not the parent's view of it");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_repository_never_mixes_two_sibling_submodules_with_the_same_branch_name() {
        // The report's own point: a parent-vs-submodule comparison isn't
        // enough to catch cross-contamination *between two submodules* — the
        // one actual reported symptom ("Submodule Branch Map looks the same
        // for every submodule"). Two real sibling submodules under the same
        // parent, both on a branch literally named "main" (so a bug that
        // mixed them up by branch name, not by repository, would also be
        // caught), each with its own unique commit subjects, OIDs, and
        // topology (A is a straight 3-commit line; B has a real merge with
        // two parents) — deliberately not just "different text", but a
        // different *shape* too, so a bug that mixed up the DAG structure
        // itself (not just labels) would also show up.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-sibling-submodules-{suffix}"));
        let parent = base.join("parent");
        let dep_a = base.join("dep-a");
        let dep_b = base.join("dep-b");
        create_libgit2_repository(&parent, "README.md");
        run_git(&parent, &["branch", "-M", "main"]);

        // A: straight line of 3 commits, all named "main".
        create_libgit2_repository(&dep_a, "a.txt");
        run_git(&dep_a, &["branch", "-M", "main"]);
        for i in 1..3 { fs::write(dep_a.join("a.txt"), format!("A v{i}")).unwrap(); run_git(&dep_a, &["commit", "-am", &format!("A-only commit {i}")]); }

        // B: a real merge — a feature branch merged back into main, giving B
        // a genuinely different topology from A's straight line, not just
        // different commit text.
        create_libgit2_repository(&dep_b, "b.txt");
        run_git(&dep_b, &["branch", "-M", "main"]);
        run_git(&dep_b, &["checkout", "-b", "feature"]);
        fs::write(dep_b.join("feature.txt"), "B feature work").unwrap();
        run_git(&dep_b, &["add", "."]); run_git(&dep_b, &["commit", "-m", "B-only feature commit"]);
        run_git(&dep_b, &["checkout", "main"]);
        fs::write(dep_b.join("b.txt"), "B main work").unwrap();
        run_git(&dep_b, &["commit", "-am", "B-only main commit"]);
        run_git(&dep_b, &["merge", "--no-ff", "-m", "B-only merge commit", "feature"]);

        let parent_string = parent.to_string_lossy().into_owned();
        let added_a = add_submodule_inner(parent_string.clone(), "".into(), dep_a.to_string_lossy().into_owned(), "dep-a".into(), String::new(), String::new()).unwrap();
        let added_b = add_submodule_inner(parent_string.clone(), "".into(), dep_b.to_string_lossy().into_owned(), "dep-b".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add both sibling submodules".into()).unwrap();

        let sub_a_head = Repository::open(parent.join(&added_a)).unwrap().head().unwrap().target().unwrap().to_string();
        let sub_b_head = Repository::open(parent.join(&added_b)).unwrap().head().unwrap().target().unwrap().to_string();
        assert_ne!(sub_a_head, sub_b_head, "sanity check");

        let data_a = submodule_repository_inner(parent_string.clone(), added_a).unwrap();
        let data_b = submodule_repository_inner(parent_string, added_b).unwrap();

        // Each resolved to its own, distinct repository and HEAD.
        assert_eq!(data_a.repository.head_oid, sub_a_head);
        assert_eq!(data_b.repository.head_oid, sub_b_head);
        assert_ne!(data_a.repository.path, data_b.repository.path, "A and B must resolve to two different repository paths");
        assert_eq!(data_a.repository.current_branch, "main");
        assert_eq!(data_b.repository.current_branch, "main");

        // A's history: only A's commits, never B's, and no merge (a straight line).
        let a_subjects: Vec<&str> = data_a.commits.iter().map(|c| c.subject.as_str()).collect();
        assert!(a_subjects.iter().any(|s| s.starts_with("A-only")), "A's own commits must be present: {a_subjects:?}");
        assert!(!a_subjects.iter().any(|s| s.starts_with("B-only")), "B's commits must never appear in A's history: {a_subjects:?}");
        assert!(data_a.commits.iter().all(|c| c.parents.len() <= 1), "A has no merge commit — none of its commits should show 2 parents");

        // B's history: only B's commits, never A's, and its real merge commit
        // (two parents) must be present.
        let b_subjects: Vec<&str> = data_b.commits.iter().map(|c| c.subject.as_str()).collect();
        assert!(b_subjects.iter().any(|s| s.starts_with("B-only")), "B's own commits must be present: {b_subjects:?}");
        assert!(!b_subjects.iter().any(|s| s.starts_with("A-only")), "A's commits must never appear in B's history: {b_subjects:?}");
        let merge_commit = data_b.commits.iter().find(|c| c.subject == "B-only merge commit").expect("B's merge commit must be present");
        assert_eq!(merge_commit.parents.len(), 2, "B's merge commit must show both real parents");

        // No OID overlap at all between the two histories.
        let a_ids: std::collections::HashSet<&str> = data_a.commits.iter().map(|c| c.id.as_str()).collect();
        let b_ids: std::collections::HashSet<&str> = data_b.commits.iter().map(|c| c.id.as_str()).collect();
        assert!(a_ids.is_disjoint(&b_ids), "A and B must share zero commit OIDs — any overlap here is real cross-contamination, not a fixture-similarity artifact");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_versions_lists_tags_with_attached_branch_and_switch_lands_on_the_commit() {
        // Reproduces two things at once: (1) tags weren't listed at all by
        // submodule_versions, so there was no way to browse/checkout them from
        // "Change version"; (2) an ANNOTATED tag's ref target is the tag object,
        // not the commit — naively detaching at that id used to be wrong (it must
        // resolve to the commit the tag points at, exactly like `git checkout <tag>`).
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-switch-tag-{suffix}"));
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
        // Lightweight tag on the tip commit (main branch is attached there too),
        // and an annotated tag — both should be listed, and the annotated one is
        // the case that used to be handled wrong.
        run_git(&sub_path, &["tag", "v1.0-light"]);
        run_git(&sub_path, &["tag", "-a", "v1.0-annotated", "-m", "Release 1.0"]);
        let tip_commit = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap().to_string();

        let versions = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        let light = versions.versions.iter().find(|v| v.kind == "tag" && v.name == "v1.0-light").expect("lightweight tag should be listed");
        let annotated = versions.versions.iter().find(|v| v.kind == "tag" && v.name == "v1.0-annotated").expect("annotated tag should be listed");
        assert_eq!(light.revision, tip_commit, "a lightweight tag should resolve to the commit it points at");
        assert_eq!(annotated.revision, tip_commit, "an annotated tag must resolve to the commit it points at, not the tag object id");
        assert_eq!(annotated.attached_branch.as_deref(), Some("main"), "the tag sits on the same commit as the 'main' branch tip, so main should be reported as attached");

        let switched = switch_submodule_version_inner(repo_path.clone(), "vendor/dep".into(), annotated.revision.clone(), annotated.kind.clone(), annotated.name.clone());
        assert!(switched.is_ok(), "switching to an annotated tag should succeed, got: {:?}", switched);
        let sub_repo = Repository::open(&sub_path).unwrap();
        assert!(sub_repo.head_detached().unwrap(), "checking out a tag must detach HEAD, exactly like `git checkout <tag>`");
        assert_eq!(sub_repo.head().unwrap().target().unwrap().to_string(), tip_commit, "HEAD must land on the commit the tag points at, not on the tag object itself");

        fs::remove_dir_all(base).unwrap();
    }

    // Report: containing_branches used to be computed only for the active
    // checkout (current_containing_branches, a single value for the whole
    // response) — every other row in the History list had no way to answer
    // "what branch is this old commit even on?". Now computed per commit row.
    #[test]
    fn submodule_versions_reports_which_branches_contain_each_history_commit_not_only_the_current_one() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-containing-branches-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);
        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        let default_branch = Repository::open(&sub_path).unwrap().head().unwrap().shorthand().unwrap().to_string();
        let shared_ancestor = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap().to_string();

        // "feature" branches off the shared ancestor and never advances
        // further — the default branch alone gets a second, newer commit.
        run_git(&sub_path, &["branch", "feature"]);
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "Only on the default branch"]);
        let default_only_commit = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap().to_string();

        let versions = submodule_versions_inner(repo_path, "vendor/dep".into()).unwrap();
        let shared_row = versions.versions.iter().find(|v| v.kind == "commit" && v.revision == shared_ancestor).expect("the shared ancestor must be in the bounded history");
        let mut shared_branches = shared_row.containing_branches.clone();
        shared_branches.sort();
        // Also contains the remote-tracking branch git submodule add's own
        // clone step creates (origin/master, still parked at this same
        // commit since nothing has fetched or pushed since).
        let mut expected = vec![default_branch.clone(), "feature".to_string(), format!("origin/{default_branch}")];
        expected.sort();
        assert_eq!(shared_branches, expected, "a commit reachable from multiple branch tips must list all of them, not only whichever happens to be current");

        let newer_row = versions.versions.iter().find(|v| v.kind == "commit" && v.revision == default_only_commit).expect("the newer, default-branch-only commit must be in history too");
        assert_eq!(newer_row.containing_branches, vec![default_branch], "a commit only 'feature' never advanced to must not claim feature contains it");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_versions_exposes_upstream_and_ahead_behind_for_local_branches_only() {
        // Submodule-branch-selector report, point 3: every local branch row
        // must expose its own upstream and ahead/behind counts (a branch
        // with no configured upstream reports all three as None, not
        // zeroes, which would misleadingly read as "perfectly in sync") —
        // and this is never populated for a remote-tracking, tag, or commit
        // row, since an "upstream" isn't a property any of those have.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-version-upstream-{suffix}"));
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
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:develop"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", "-b", "develop", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);
        // A tracked branch, 1 ahead of its upstream (a local commit not
        // pushed), and an untracked one with no upstream at all.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "local ahead"]);
        run_git(&sub_path, &["branch", "untracked-branch"]);

        let versions = submodule_versions_inner(repo_path, "vendor/dep".into()).unwrap();
        let develop = versions.versions.iter().find(|v| v.kind == "branch" && v.name == "develop").expect("develop should be listed");
        assert_eq!(develop.upstream.as_deref(), Some("origin/develop"));
        assert_eq!(develop.ahead, Some(1));
        assert_eq!(develop.behind, Some(0));

        let untracked = versions.versions.iter().find(|v| v.kind == "branch" && v.name == "untracked-branch").expect("untracked-branch should be listed");
        assert_eq!(untracked.upstream, None, "no upstream configured — must be None, not a misleadingly-in-sync 0/0");
        assert_eq!(untracked.ahead, None);
        assert_eq!(untracked.behind, None);

        let remote_entry = versions.versions.iter().find(|v| v.kind == "remote" && v.name == "origin/develop").expect("origin/develop should be listed as a remote-tracking entry too");
        assert_eq!(remote_entry.upstream, None, "upstream is a property of a local branch, never of a remote-tracking entry itself");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn push_submodule_preview_reports_the_attached_branch_never_main_or_a_stale_selection() {
        // The exact reproduction case from the push-submodule-workflow
        // report: current branch "develop", HEAD a5cf4383-shaped, an
        // existing upstream origin/develop — the preview must say
        // "<sha> -> origin/develop", never silently defaulting to the
        // parent's own branch, .gitmodules' default, or "main".
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-push-preview-{suffix}"));
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
            run_git(path, &["init", "-b", "main"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:develop"]);
        // The parent stays on "main" throughout — if the destination logic
        // ever accidentally reused the *parent's* branch, this would catch it.
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", "-b", "develop", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "local ahead"]);
        let local_sha = git(&sub_path.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();

        let preview = push_submodule_preview_inner(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(preview.branch, "develop", "must read the submodule's own checked-out branch, never the parent's ('main') or a default");
        assert_eq!(preview.local_sha, local_sha);
        assert_eq!(preview.upstream.as_deref(), Some("origin/develop"));
        assert_eq!(preview.ahead, 1);
        assert_eq!(preview.behind, 0);
        assert!(!preview.will_create_remote_branch);
        assert!(preview.remote_sha.is_some(), "the upstream's tip must be resolved");
        assert_ne!(preview.remote_sha.as_deref(), Some(local_sha.as_str()), "the remote tip must be the old commit, not the new local-only one");
        assert!(preview.can_push);
        assert!(preview.blocked_reason.is_none());
        assert_eq!(preview.commits.len(), 1);
        assert_eq!(preview.commits[0].subject, "local ahead");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn push_submodule_preview_lists_existing_origin_branch_commits_without_configured_upstream() {
        // Exact regression: the preview used origin/develop as a fallback and
        // correctly reported "2 ahead", while entry_details required an
        // upstream, returned no commits, and made the frontend disable Push.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-push-preview-no-upstream-{suffix}"));
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
            run_git(path, &["init", "-b", "main"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:develop"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", "-b", "develop", dep_remote.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);
        run_git(&sub_path, &["branch", "--unset-upstream"]);
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "first local commit"]);
        fs::write(sub_path.join("module.txt"), "v3").unwrap();
        run_git(&sub_path, &["commit", "-am", "second local commit"]);

        let preview = push_submodule_preview_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert_eq!(preview.branch, "develop");
        assert_eq!(preview.upstream, None, "the fixture deliberately has no configured upstream");
        assert!(preview.remote_sha.is_some(), "origin/develop is still a valid comparison target");
        assert_eq!((preview.ahead, preview.behind), (2, 0));
        assert_eq!(preview.commits.iter().map(|commit| commit.subject.as_str()).collect::<Vec<_>>(), vec!["first local commit", "second local commit"]);
        assert!(preview.can_push, "a fast-forward push must be offered even before -u configures the upstream");
        assert!(preview.blocked_reason.is_none());

        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).expect("the normal push should publish both commits and set the upstream");
        let after = push_submodule_preview_inner(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(after.upstream.as_deref(), Some("origin/develop"));
        assert_eq!((after.ahead, after.behind), (0, 0));
        assert!(after.commits.is_empty());
        assert!(!after.can_push);
        assert!(after.blocked_reason.as_deref().is_some_and(|message| message.contains("Already up to date")));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn push_submodule_preview_reports_a_new_remote_branch_will_be_created_when_there_is_no_upstream_or_matching_remote_ref() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-push-preview-new-branch-{suffix}"));
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
        // A brand new local branch, never pushed, no upstream, and no
        // same-named ref on origin either.
        run_git(&sub_path, &["switch", "-c", "feature/never-pushed"]);
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "new feature work"]);

        let preview = push_submodule_preview_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert_eq!(preview.branch, "feature/never-pushed");
        assert_eq!(preview.upstream, None);
        assert_eq!(preview.remote_sha, None);
        assert!(preview.will_create_remote_branch);
        assert_eq!(preview.ahead, 0, "with nothing to compare against, ahead/behind default to 0, not a misleading guess");
        assert_eq!(preview.behind, 0);
        assert!(preview.commits.is_empty(), "there is no remote tip to compare against; branch creation is still independently allowed");
        assert!(preview.can_push, "creating a remote branch must not depend on a non-empty comparison list");
        assert!(preview.blocked_reason.is_none());

        // Push-submodule-workflow report, point 1: after a successful push,
        // the upstream must be persisted (equivalent to `git push
        // --set-upstream`) — a second preview afterward must show it, and
        // ahead/behind must now read as fully in sync.
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).expect("pushing a brand new branch should succeed");
        let after = push_submodule_preview_inner(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(after.upstream.as_deref(), Some("origin/feature/never-pushed"), "the upstream must be persisted after a successful push, equivalent to --set-upstream");
        assert_eq!((after.ahead, after.behind), (0, 0));
        assert!(!after.can_push);
        assert!(after.blocked_reason.as_deref().is_some_and(|message| message.contains("Already up to date")));
        let sub_repo = Repository::open(&sub_path).unwrap();
        assert_eq!(upstream_ref(&sub_repo, "feature/never-pushed").map(|(_, label)| label), Some("origin/feature/never-pushed".to_string()), "a successful push must persist the upstream, equivalent to --set-upstream");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_preview_push_pull_and_force_push_share_a_differently_named_upstream() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-real-upstream-{suffix}"));
        let repository = base.join("main");
        let origin = base.join("origin.git");
        let mirror = base.join("mirror.git");
        let seed = base.join("seed");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&origin).unwrap();
        fs::create_dir_all(&mirror).unwrap();
        fs::create_dir_all(&seed).unwrap();
        fs::write(repository.join("README.md"), "root").unwrap();
        fs::write(seed.join("module.txt"), "v1").unwrap();
        run_git(&origin, &["init", "--bare"]);
        run_git(&mirror, &["init", "--bare"]);
        for path in [&repository, &seed] {
            run_git(path, &["init", "-b", "main"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&seed, &["remote", "add", "origin", origin.to_str().unwrap()]);
        run_git(&seed, &["push", "origin", "HEAD:main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", origin.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep"]);

        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);
        run_git(&sub_path, &["remote", "add", "mirror", mirror.to_str().unwrap()]);
        run_git(&sub_path, &["push", "mirror", "HEAD:release"]);
        run_git(&sub_path, &["switch", "-c", "work"]);
        run_git(&sub_path, &["config", "branch.work.remote", "mirror"]);
        run_git(&sub_path, &["config", "branch.work.merge", "refs/heads/release"]);
        fs::write(sub_path.join("module.txt"), "local v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "Local work for release"]);

        let preview = push_submodule_preview_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert_eq!(preview.branch, "work");
        assert_eq!(preview.upstream.as_deref(), Some("mirror/release"));
        assert_eq!(preview.remote_url, mirror.to_string_lossy());
        assert_eq!((preview.ahead, preview.behind), (1, 0));
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).expect("push must use mirror/release, exactly as preview advertised");
        assert_eq!(run_git_capture(&mirror, &["log", "-1", "--format=%s", "release"]), "Local work for release");
        assert!(Repository::open_bare(&origin).unwrap().find_reference("refs/heads/work").is_err(), "push must not silently create origin/work");

        let other = base.join("other");
        run_git(&base, &["clone", mirror.to_str().unwrap(), "other"]);
        run_git(&other, &["switch", "release"]);
        run_git(&other, &["config", "user.email", "other@example.com"]);
        run_git(&other, &["config", "user.name", "Other User"]);
        fs::write(other.join("module.txt"), "remote v3").unwrap();
        run_git(&other, &["commit", "-am", "Remote release work"]);
        run_git(&other, &["push", "origin", "release"]);

        pull_submodule_inner(repo_path, "vendor/dep".into()).expect("pull must fetch and fast-forward from mirror/release");
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "remote v3");
        assert_eq!(run_git_capture(&sub_path, &["branch", "--show-current"]), "work", "pull must keep the differently named local branch attached");

        // Deliberately diverge both sides. Force-push must still overwrite the
        // same configured mirror/release destination, never origin/work.
        fs::write(sub_path.join("module.txt"), "local force version").unwrap();
        run_git(&sub_path, &["commit", "-am", "Local force candidate"]);
        fs::write(other.join("module.txt"), "remote divergent version").unwrap();
        run_git(&other, &["commit", "-am", "Remote divergent release"]);
        run_git(&other, &["push", "origin", "release"]);
        force_push_submodule_inner(repository.to_string_lossy().into_owned(), "vendor/dep".into())
            .expect("force-push must use mirror/release too");
        assert_eq!(run_git_capture(&mirror, &["log", "-1", "--format=%s", "release"]), "Local force candidate");
        assert!(Repository::open_bare(&origin).unwrap().find_reference("refs/heads/work").is_err(), "force-push must not silently create origin/work either");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn create_submodule_tag_creates_at_the_intended_commit_and_never_touches_the_parent() {
        // Point 4 of the submodule-tag-workflow report: an annotated tag when
        // a message is supplied, a lightweight one otherwise, always at the
        // submodule's current HEAD — and never stages or commits the
        // parent's gitlink, since a tag doesn't change the submodule's
        // commit at all.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-create-tag-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        let repo_path = repository.to_string_lossy().into_owned();
        let added = add_submodule_inner(repo_path.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(repo_path.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = repository.join(&added);
        let head_sha = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap().to_string();
        let parent_index_before = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id };
        let parent_head_before = Repository::open(&repository).unwrap().head().unwrap().target().unwrap();

        let lightweight = create_submodule_tag_inner(repo_path.clone(), added.clone(), "v1.0-light".into(), String::new(), false).unwrap();
        assert_eq!(lightweight.target, head_sha);
        assert!(!lightweight.annotated, "no message was given — must be lightweight");
        assert!(!lightweight.pushed);

        let annotated = create_submodule_tag_inner(repo_path.clone(), added.clone(), "v1.0".into(), "Release 1.0".into(), false).unwrap();
        assert_eq!(annotated.target, head_sha);
        assert!(annotated.annotated, "a message was given — must be annotated");

        {
            let sub_repo = Repository::open(&sub_path).unwrap();
            let light_ref = sub_repo.find_reference("refs/tags/v1.0-light").unwrap();
            assert_eq!(light_ref.target().unwrap().to_string(), head_sha, "a lightweight tag points directly at the commit");
            let annotated_tag = sub_repo.find_reference("refs/tags/v1.0").unwrap().peel_to_tag().unwrap();
            assert_eq!(annotated_tag.message(), Some("Release 1.0"));
            assert_eq!(annotated_tag.target_id(), git2::Oid::from_str(&head_sha).unwrap());
        }

        // History can intentionally tag an older selected commit even when
        // the submodule has since moved on; creating that name must not move
        // HEAD back to the selected commit.
        fs::write(sub_path.join("module.txt"), "newer commit").unwrap();
        create_commit(sub_path.to_string_lossy().into_owned(), "Move past release".into()).unwrap();
        let newer_head = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap();
        let selected = create_submodule_tag_at_inner(repo_path.clone(), added.clone(), "v1.0-selected".into(), String::new(), false, Some(head_sha.clone())).unwrap();
        assert_eq!(selected.target, head_sha);
        assert_eq!(Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap(), newer_head, "tagging a selected historical commit must not checkout or move HEAD");

        // Neither tag may have staged or committed anything in the parent.
        let parent_index_after = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id };
        assert_eq!(parent_index_after, parent_index_before, "creating a tag must never stage the parent's gitlink");
        assert_eq!(Repository::open(&repository).unwrap().head().unwrap().target().unwrap(), parent_head_before, "creating a tag must never commit anything in the parent");

        // Never silently overwrite an existing tag.
        let duplicate = create_submodule_tag_inner(repo_path, added, "v1.0-light".into(), String::new(), false);
        assert!(duplicate.is_err(), "creating a tag with a name that already exists must fail, not overwrite it");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn create_submodule_tag_rejects_an_invalid_name() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-tag-invalid-name-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        let repo_path = repository.to_string_lossy().into_owned();
        let added = add_submodule_inner(repo_path.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(repo_path.clone(), "Add dep submodule".into()).unwrap();

        assert!(create_submodule_tag_inner(repo_path.clone(), added.clone(), "".into(), String::new(), false).is_err(), "an empty name must be rejected");
        assert!(create_submodule_tag_inner(repo_path.clone(), added.clone(), "has a space".into(), String::new(), false).is_err(), "a name with a space must be rejected");
        assert!(create_submodule_tag_inner(repo_path.clone(), added.clone(), "..".into(), String::new(), false).is_err(), "'..' must be rejected");
        assert!(create_submodule_tag_inner(repo_path, added, "-leading-dash".into(), String::new(), false).is_err(), "a leading dash must be rejected");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn push_submodule_tag_pushes_only_that_tag_not_the_branch_or_other_tags() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-push-tag-{suffix}"));
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
        // A second, un-pushed local commit — the branch itself must stay
        // exactly where the remote already has it after pushing only a tag.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "local only, must not be pushed by a tag push"]);

        create_submodule_tag_inner(repo_path.clone(), "vendor/dep".into(), "released".into(), "Release".into(), false).unwrap();
        create_submodule_tag_inner(repo_path.clone(), "vendor/dep".into(), "also-not-pushed".into(), String::new(), false).unwrap();

        push_submodule_tag_inner(repo_path.clone(), "vendor/dep".into(), "released".into()).expect("pushing an existing tag should succeed");

        assert!(git(&dep_remote.to_string_lossy(), &["rev-parse", "refs/tags/released"]).is_ok(), "the pushed tag must exist on the remote");
        assert!(git(&dep_remote.to_string_lossy(), &["rev-parse", "refs/tags/also-not-pushed"]).is_err(), "an unrelated tag must never be pushed alongside it");
        let remote_main = git(&dep_remote.to_string_lossy(), &["rev-parse", "refs/heads/main"]).unwrap().trim().to_string();
        let local_head = git(&sub_path.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        assert_ne!(remote_main, local_head, "the local-only branch commit must never have been pushed by pushing a tag");

        // Pushing a name that doesn't exist locally must fail clearly, not silently no-op.
        assert!(push_submodule_tag_inner(repo_path, "vendor/dep".into(), "missing".into()).is_err());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn reset_submodule_discards_local_commits_dirty_edits_and_an_uncommitted_version_switch() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-reset-submodule-{suffix}"));
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
        let recorded_commit = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap().to_string();
        let local_branch = git(&sub_path.to_string_lossy(), &["branch", "--show-current"]).unwrap().trim().to_string();

        // Drift the submodule: a local commit ahead of what the parent has
        // recorded (like an uncommitted "switch version"), plus a dirty,
        // uncommitted edit on top of that — both must be discarded by reset.
        fs::write(sub_path.join("module.txt"), "v2 (local commit)").unwrap();
        run_git(&sub_path, &["commit", "-am", "Local-only change, never recorded by the parent"]);
        let local_commit = git(&sub_path.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        fs::write(sub_path.join("module.txt"), "v3 (dirty, uncommitted)").unwrap();
        assert!(!Repository::open(&sub_path).unwrap().statuses(None).unwrap().is_empty(), "sanity check: the submodule should be dirty before reset");

        let reset_to = reset_submodule_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert_eq!(reset_to, recorded_commit, "reset should land on the commit the parent has recorded, not wherever the submodule had drifted to");

        let sub_repo = Repository::open(&sub_path).unwrap();
        assert_eq!(sub_repo.head().unwrap().target().unwrap().to_string(), recorded_commit, "HEAD must be back at the parent-recorded commit");
        assert!(sub_repo.statuses(None).unwrap().is_empty(), "the dirty edit must be discarded — reset means overwritten, not merged or preserved");
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "v1", "working tree content must match the recorded commit exactly");
        drop(sub_repo);

        // Exact field regression from the real UI: after restore, merely
        // selecting the old local branch used to set HEAD before checkout.
        // That made the restored tree appear as staged reverse changes
        // against the branch, even though the user had edited nothing.
        let versions = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        let branch = versions.versions.iter().find(|version| version.kind == "branch" && version.name == local_branch).unwrap();
        switch_submodule_version_inner(repo_path, "vendor/dep".into(), branch.revision.clone(), branch.kind.clone(), branch.name.clone()).unwrap();
        let switched = Repository::open(&sub_path).unwrap();
        assert!(!switched.head_detached().unwrap(), "selecting a local branch must attach HEAD");
        assert_eq!(switched.head().unwrap().target().unwrap().to_string(), local_commit);
        assert!(switched.statuses(None).unwrap().is_empty(), "restore then checkout must not manufacture staged reverse changes");
        assert_eq!(switched.index().unwrap().write_tree().unwrap(), switched.head().unwrap().peel_to_tree().unwrap().id(), "index must exactly match the selected branch tip");

        fs::remove_dir_all(base).unwrap();
    }

    // A process killed mid-operation (a timed-out Terminal command, a crash,
    // an interrupted clone) can leave a submodule with extra untracked or
    // even .gitignore'd files that were never part of any commit. force()
    // alone only overwrites tracked paths — it never deletes something that
    // isn't in the target tree — so those leftovers survived a plain reset
    // and kept surprising the user afterward. Restore project version's own
    // stated purpose is "the exact commit recorded by the parent project",
    // so it must clear that cruft too.
    #[test]
    fn reset_submodule_removes_untracked_and_ignored_leftovers_not_just_tracked_drift() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-reset-submodule-cruft-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);
        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");

        // Simulate what an interrupted clone/checkout or a stray build can
        // leave behind: an untracked file, an untracked directory, and a
        // file matching a fresh .gitignore rule.
        fs::write(sub_path.join("orphaned.tmp"), "leftover from an interrupted process").unwrap();
        fs::create_dir_all(sub_path.join("partial_clone_dir")).unwrap();
        fs::write(sub_path.join("partial_clone_dir/incomplete.bin"), "half-written").unwrap();
        fs::write(sub_path.join(".gitignore"), "*.ignored\n").unwrap();
        fs::write(sub_path.join("build.ignored"), "stray build output").unwrap();

        let reset_to = reset_submodule_inner(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(reset_to, Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap().to_string());
        assert!(!sub_path.join("orphaned.tmp").exists(), "an untracked leftover file must be removed");
        assert!(!sub_path.join("partial_clone_dir").exists(), "an untracked leftover directory must be removed");
        assert!(!sub_path.join("build.ignored").exists(), "a .gitignore'd leftover must be removed too — this action's own promise is the exact recorded commit, nothing else");
        assert!(!sub_path.join(".gitignore").exists(), ".gitignore itself was untracked here (never committed) — it is cruft like anything else, not special-cased");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn reset_submodule_branch_to_upstream_discards_a_divergence_and_leaves_a_clean_attached_branch() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-reset-submodule-upstream-{suffix}"));
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
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        fs::write(dependency.join("remote.txt"), "remote only").unwrap();
        run_git(&dependency, &["add", "."]);
        run_git(&dependency, &["commit", "-m", "Remote moves"]);
        let remote_tip = git(&dependency.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        fs::write(sub_path.join("local.txt"), "local only").unwrap();
        run_git(&sub_path, &["add", "."]);
        run_git(&sub_path, &["commit", "-m", "Local moves"]);
        fs::write(sub_path.join("dirty.txt"), "discard me").unwrap();
        run_git(&sub_path, &["add", "dirty.txt"]);

        let result = reset_submodule_branch_to_upstream_inner(repository.to_string_lossy().into_owned(), "vendor/dep".into(), "main".into()).unwrap();
        assert_eq!(result.branch, "main");
        assert_eq!(result.upstream, "origin/main");
        assert_eq!(result.revision, remote_tip);
        let sub_repo = Repository::open(&sub_path).unwrap();
        assert!(!sub_repo.head_detached().unwrap(), "matching upstream must keep the user on the local branch");
        assert_eq!(sub_repo.head().unwrap().shorthand(), Some("main"));
        assert_eq!(sub_repo.head().unwrap().target().unwrap().to_string(), remote_tip);
        assert!(sub_repo.statuses(None).unwrap().is_empty(), "hard match must leave no staged or unstaged leftovers");
        let upstream = sub_repo.find_branch("main", BranchType::Local).unwrap().upstream().unwrap();
        assert_eq!(upstream.get().target(), sub_repo.head().unwrap().target());

        fs::remove_dir_all(base).unwrap();
    }

    // Same reasoning, same gap as reset_submodule's own equivalent test: a
    // plain hard reset only overwrites tracked content, it never removes an
    // untracked or .gitignore'd leftover (e.g. from a process interrupted
    // mid-operation). "Reset to upstream" is the button for "force this
    // submodule to exactly match origin, discard everything local" — that
    // promise must include cruft, not just tracked drift.
    #[test]
    fn reset_submodule_branch_to_upstream_removes_untracked_and_ignored_leftovers_too() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-reset-upstream-cruft-{suffix}"));
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
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        fs::write(dependency.join("remote.txt"), "remote only").unwrap();
        run_git(&dependency, &["add", "."]);
        run_git(&dependency, &["commit", "-m", "Remote moves"]);

        // Leftovers an interrupted process (or a stray build) could have left
        // behind, never part of any commit.
        fs::write(sub_path.join("orphaned.tmp"), "leftover").unwrap();
        fs::create_dir_all(sub_path.join("partial_clone_dir")).unwrap();
        fs::write(sub_path.join("partial_clone_dir/incomplete.bin"), "half-written").unwrap();
        fs::write(sub_path.join(".gitignore"), "*.ignored\n").unwrap();
        fs::write(sub_path.join("build.ignored"), "stray build output").unwrap();

        reset_submodule_branch_to_upstream_inner(repository.to_string_lossy().into_owned(), "vendor/dep".into(), "main".into()).unwrap();
        assert!(!sub_path.join("orphaned.tmp").exists(), "an untracked leftover file must be removed");
        assert!(!sub_path.join("partial_clone_dir").exists(), "an untracked leftover directory must be removed");
        assert!(!sub_path.join("build.ignored").exists(), "a .gitignore'd leftover must be removed too");
        assert!(!sub_path.join(".gitignore").exists(), ".gitignore itself was untracked here and is cruft like anything else");
        assert!(Repository::open(&sub_path).unwrap().statuses(None).unwrap().is_empty(), "must land fully clean, not just tracked-content-clean");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn reset_submodule_branch_to_upstream_refuses_to_delete_dirty_work_from_another_checkout() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-reset-submodule-other-branch-{suffix}"));
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
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);
        run_git(&sub_path, &["switch", "-c", "develop"]);
        fs::write(sub_path.join("module.txt"), "develop work that must survive").unwrap();
        let main_before = git(&sub_path.to_string_lossy(), &["rev-parse", "main"]).unwrap();

        let error = reset_submodule_branch_to_upstream_inner(repository.to_string_lossy().into_owned(), "vendor/dep".into(), "main".into()).unwrap_err();
        assert!(error.contains("has uncommitted work"));
        assert_eq!(git(&sub_path.to_string_lossy(), &["branch", "--show-current"]).unwrap().trim(), "develop", "the active checkout must not change");
        assert_eq!(git(&sub_path.to_string_lossy(), &["rev-parse", "main"]).unwrap(), main_before, "the selected inactive branch must not move");
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "develop work that must survive", "dirty work must survive byte for byte");

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
        let versions = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        let origin_main = versions.versions.iter().find(|v| v.kind == "remote" && v.name == "origin/main").expect("origin/main should be listed");
        switch_submodule_version_inner(repo_path.clone(), "vendor/dep".into(), origin_main.revision.clone(), origin_main.kind.clone(), origin_main.name.clone()).unwrap();
        let sub_repo = Repository::open(&sub_path).unwrap();
        assert!(!sub_repo.head_detached().unwrap(), "selecting origin/main with no local main should attach, not detach");
        assert_eq!(sub_repo.head().unwrap().shorthand(), Some("main"));
        drop(sub_repo);

        // Case 2: a local branch of the same name already exists and already
        // points at that exact commit — selecting the remote entry should just
        // attach to the existing local branch, not error or duplicate it.
        run_git(&sub_path, &["checkout", "--detach", "HEAD"]);
        switch_submodule_version_inner(repo_path.clone(), "vendor/dep".into(), origin_main.revision.clone(), origin_main.kind.clone(), origin_main.name.clone()).unwrap();
        let sub_repo = Repository::open(&sub_path).unwrap();
        assert!(!sub_repo.head_detached().unwrap());
        assert_eq!(sub_repo.head().unwrap().shorthand(), Some("main"));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn switching_a_submodules_branch_leaves_the_parent_gitlink_unstaged_until_explicitly_staged() {
        // Message-E point 8: "Change version" on a submodule used to call
        // add_to_index(true) on the parent unconditionally, silently staging the
        // gitlink the instant the submodule's checkout moved — before the user had
        // asked to stage anything. A mere branch switch must show up as an
        // ordinary *unstaged* modification, exactly like editing any other file;
        // staging it is still the user's own explicit action via the normal
        // Stage flow.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-switch-unstaged-{suffix}"));
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
        // A second commit on a new local branch inside the submodule's own
        // checkout, so switching to it actually moves the submodule's HEAD to a
        // different commit than what the parent has recorded.
        run_git(&sub_path, &["switch", "-c", "feature-x"]);
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "v2 on feature-x"]);
        run_git(&sub_path, &["switch", "main"]);

        let versions = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        let feature_branch = versions.versions.iter().find(|v| v.kind == "branch" && v.name == "feature-x").expect("feature-x should be listed");

        switch_submodule_version_inner(repo_path.clone(), "vendor/dep".into(), feature_branch.revision.clone(), feature_branch.kind.clone(), feature_branch.name.clone()).unwrap();

        let after_switch = load_repository_inner(repo_path.clone(), Some(true)).unwrap();
        let gitlink_change = after_switch.changes.iter().find(|change| change.path == "vendor/dep").expect("the moved submodule must show up as a change");
        assert!(!gitlink_change.staged, "switching a submodule's branch must never silently stage the parent's gitlink");
        assert_eq!(gitlink_change.status, "M");

        // Staging is still available as the user's own explicit action, and it
        // must actually work: the existing submodule-HEAD-vs-index detection in
        // stage_files_inner (see its own comment there) is what now carries this,
        // once switch_submodule_version_inner stopped doing it automatically.
        stage_files(repo_path.clone(), vec!["vendor/dep".into()]).unwrap();
        let after_stage = load_repository_inner(repo_path.clone(), Some(true)).unwrap();
        let staged_change = after_stage.changes.iter().find(|change| change.path == "vendor/dep").expect("the submodule change must still be present after staging");
        assert!(staged_change.staged, "explicit Stage must still be able to stage the moved submodule");

        // Once the user has explicitly staged the gitlink, a later Change
        // version must keep that staging intent but refresh the staged SHA.
        // This is the exact add -> change version -> commit workflow that
        // previously recorded the first SHA and left a second Modified item.
        run_git(&sub_path, &["switch", "-c", "feature-y"]);
        fs::write(sub_path.join("module.txt"), "v3").unwrap();
        run_git(&sub_path, &["commit", "-am", "v3 on feature-y"]);
        let feature_y_oid = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap();
        run_git(&sub_path, &["switch", "feature-x"]);
        switch_submodule_version_inner(repo_path.clone(), "vendor/dep".into(), feature_y_oid.to_string(), "branch".into(), "feature-y".into()).unwrap();

        let parent = Repository::open(&repository).unwrap();
        assert_eq!(parent_gitlink_oid(&parent, "vendor/dep", true), Some(feature_y_oid), "an already-staged gitlink must follow the selected version");
        drop(parent);
        commit_staged_inner(repo_path.clone(), "Record feature-y once".into()).unwrap();
        let after_commit = load_repository_inner(repo_path.clone(), Some(true)).unwrap();
        assert!(after_commit.changes.iter().all(|change| change.path != "vendor/dep"), "one parent commit must fully record the selected submodule version");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn switching_a_submodule_version_patches_the_cached_full_status_instead_of_forcing_a_full_rescan() {
        // On a large repository, a full unscoped status rescan was measured at
        // 5-40+ seconds; switching a submodule's version only ever changes that
        // one gitlink's own status entry. Seeds full_status_cache directly with
        // a recognizable fake entry for an unrelated path (one that could never
        // come from a real scan) so surviving it proves no full rescan
        // happened, rather than inferring that from timing.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-switch-cache-patch-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep submodule"]);
        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        let sub_default_branch = Repository::open(&sub_path).unwrap().head().unwrap().shorthand().unwrap().to_string();
        run_git(&sub_path, &["switch", "-c", "feature-x"]);
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "v2 on feature-x"]);
        run_git(&sub_path, &["switch", &sub_default_branch]);
        let versions = submodule_versions_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        let feature_branch = versions.versions.iter().find(|v| v.kind == "branch" && v.name == "feature-x").unwrap();

        // Seed a fresh, cached "everything is clean" full scan, as if
        // load_repository had already run once before this operation.
        let fake_marker = "__unmistakably_fake_marker__.txt".to_string();
        full_status_cache().lock().unwrap().insert(repo_path.clone(), (Instant::now(), vec![(fake_marker.clone(), "??".into(), false)]));

        switch_submodule_version_inner(repo_path.clone(), "vendor/dep".into(), feature_branch.revision.clone(), feature_branch.kind.clone(), feature_branch.name.clone()).unwrap();

        let cache = full_status_cache().lock().unwrap();
        let (_, patched) = cache.get(&repo_path).expect("the cache entry must still exist — patched in place, not thrown away");
        assert!(patched.iter().any(|(path, _, _)| path == &fake_marker), "an unrelated cached path must survive untouched — proves no full rescan discarded it");
        let submodule_entry = patched.iter().find(|(path, _, _)| path == "vendor/dep");
        assert!(submodule_entry.is_some(), "the submodule's own entry must be present and correctly patched — it really did move");
        assert_eq!(submodule_entry.unwrap().1, "M");
        drop(cache);

        // The patch must be real, not just copied over: refresh_status_inner
        // reading through the same cache must see the correct, current
        // submodule state, not the fake marker's information for that path.
        let changes = refresh_status_inner(repo_path.clone()).unwrap();
        assert!(changes.iter().any(|change| change.path == "vendor/dep" && !change.staged), "the live status must reflect the real, current submodule drift");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn commit_staged_refuses_a_submodule_gitlink_that_became_stale_outside_the_app() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-stale-gitlink-{suffix}"));
        let repository = base.join("main");
        let dependency = base.join("dependency");
        create_libgit2_repository(&repository, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", dependency.to_str().unwrap(), "vendor/dep"]);
        run_git(&repository, &["commit", "-am", "Add dep"]);
        let repo_path = repository.to_string_lossy().into_owned();
        let sub_path = repository.join("vendor/dep");
        run_git(&sub_path, &["switch", "-c", "first"]);
        fs::write(sub_path.join("module.txt"), "first").unwrap();
        run_git(&sub_path, &["commit", "-am", "First"]);
        stage_files(repo_path.clone(), vec!["vendor/dep".into()]).unwrap();
        run_git(&sub_path, &["switch", "-c", "second"]);
        fs::write(sub_path.join("module.txt"), "second").unwrap();
        run_git(&sub_path, &["commit", "-am", "Second"]);

        let error = commit_staged_inner(repo_path.clone(), "Must not record stale SHA".into()).unwrap_err();
        assert!(error.contains("staged submodule reference is older"));
        assert!(error.contains("vendor/dep"));
        assert_eq!(run_git_capture(&repository, &["log", "-1", "--pretty=%s"]), "Add dep", "the parent must not create a misleading partial commit");

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
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        create_submodule_branch(parent_string.clone(), added.clone(), "feature-x".into()).unwrap();

        let sub_repo = Repository::open(parent.join(&added)).unwrap();
        assert!(!sub_repo.head_detached().unwrap());
        assert_eq!(sub_repo.head().unwrap().shorthand(), Some("feature-x"));
        drop(sub_repo);

        // Same commit, so nothing should look "modified" in the parent afterward.
        assert!(!load_repository_inner(parent_string.clone(), Some(true)).unwrap().changes.iter().any(|change| change.path == added));

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
        let main_status = publish_status(path.clone(), "main".into(), "origin".into()).unwrap();
        assert_eq!(main_status.commits.len(), 0);
        assert_eq!((main_status.ahead, main_status.behind, main_status.remote_branch_exists), (0, 0, true));

        // A brand new branch created right at main's tip, with no new work of
        // its own yet, shares 100% of its history with origin/main — it should
        // have nothing new to publish, not its whole 2-commit ancestry.
        create_branch(path.clone(), "feature-x".into()).unwrap();
        let unpublished = publish_status(path.clone(), "feature-x".into(), "origin".into()).unwrap();
        assert_eq!(unpublished.commits.len(), 0, "a new branch with no commits of its own should have nothing new to publish, even though origin/feature-x doesn't exist yet");
        assert_eq!((unpublished.ahead, unpublished.behind, unpublished.remote_branch_exists), (0, 0, false));

        // Now make one genuinely new commit on it — only *that* should show up.
        fs::write(repository.join("a.txt"), "three").unwrap();
        run_git(&repository, &["commit", "-am", "Commit 3 on feature-x"]);
        let unpublished = publish_status(path.clone(), "feature-x".into(), "origin".into()).unwrap();
        assert_eq!(unpublished.commits.len(), 1);
        assert_eq!(unpublished.commits[0].subject, "Commit 3 on feature-x");
        assert_eq!((unpublished.ahead, unpublished.behind, unpublished.remote_branch_exists), (1, 0, false));

        run_git(&repository, &["checkout", "main"]);
        let other = std::env::temp_dir().join(format!("git-integrity-new-branch-other-{suffix}"));
        run_git(std::env::temp_dir().as_path(), &["-c", "protocol.file.allow=always", "clone", remote.to_str().unwrap(), other.file_name().unwrap().to_str().unwrap()]);
        run_git(&other, &["config", "user.email", "test@example.com"]);
        run_git(&other, &["config", "user.name", "Someone Else"]);
        fs::write(other.join("a.txt"), "remote-three").unwrap();
        run_git(&other, &["commit", "-am", "Remote commit on main"]);
        run_git(&other, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["fetch", "origin"]);
        let behind_status = publish_status(path.clone(), "main".into(), "origin".into()).unwrap();
        assert_eq!(behind_status.commits.len(), 0, "being behind is not the same as having local commits to publish");
        assert_eq!((behind_status.ahead, behind_status.behind, behind_status.remote_branch_exists), (0, 1, true));

        fs::remove_dir_all(repository).unwrap();
        fs::remove_dir_all(remote).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn clone_repository_can_checkout_an_explicit_branch() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-clone-branch-{suffix}"));
        let source = base.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file.txt"), "main").unwrap();
        run_git(&source, &["init", "-b", "main"]);
        run_git(&source, &["config", "user.email", "test@example.com"]);
        run_git(&source, &["config", "user.name", "Test User"]);
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-m", "main"]);
        run_git(&source, &["checkout", "-b", "release/test"]);
        fs::write(source.join("file.txt"), "release").unwrap();
        run_git(&source, &["commit", "-am", "release"]);

        let cloned = clone_repository(source.to_string_lossy().into_owned(), base.to_string_lossy().into_owned(), "clone".into(), Some("release/test".into()), Some(false)).unwrap();
        assert_eq!(git(&cloned, &["branch", "--show-current"]).unwrap().trim(), "release/test");
        assert_eq!(fs::read_to_string(Path::new(&cloned).join("file.txt")).unwrap(), "release");

        fs::remove_dir_all(base).unwrap();
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
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let file_path = format!("{added}/module.txt");

        fs::write(parent.join(&file_path), "changed content").unwrap();

        // The file must be correctly reported as modified — sourced from the
        // submodule's own status, not silently invisible to the parent.
        let listing = load_directory_inner(parent_string.clone(), added.clone(), None).unwrap();
        let file_entry = listing.iter().find(|entry| entry.relative_path == file_path).expect("module.txt should be listed");
        assert_eq!(file_entry.status, "M", "a modified file inside a submodule must show its real status, not look permanently untracked");
        assert!(file_entry.tracked);

        let details = entry_details_inner(parent_string.clone(), file_path.clone()).unwrap();
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
    fn deleting_a_tracked_file_inside_a_submodule_uses_the_submodule_index() {
        // Reproduces the errm_common shape: the Explorer is opened through the
        // parent project, but the selected file lives inside a submodule. The
        // parent index only knows the submodule gitlink, so Remove from Git
        // must be routed to the submodule's own index.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-remove-file-inside-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let file_path = format!("{added}/module.txt");

        remove_git_path_inner(&parent_string, &file_path).unwrap();

        assert!(!parent.join(&file_path).exists(), "the working file should be removed from the submodule checkout");
        let sub_repo = Repository::open(parent.join(&added)).unwrap();
        let staged_delete = sub_repo.statuses(None).unwrap().iter().any(|entry| {
            entry.path() == Some("module.txt") && entry.status().contains(git2::Status::INDEX_DELETED)
        });
        assert!(staged_delete, "the deletion must be staged in the submodule's own index, not rejected by the parent");
        drop(sub_repo);

        let parent_repo = Repository::open(&parent).unwrap();
        assert!(parent_repo.index().unwrap().get_path(Path::new(&file_path), 0).is_none(), "the parent index must not get a bogus nested file entry");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn local_delete_inside_a_submodule_refuses_tracked_files_but_deletes_untracked_ones() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-delete-local-inside-submodule-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let tracked_path = format!("{added}/module.txt");
        let untracked_path = format!("{added}/scratch.tmp");
        fs::write(parent.join(&untracked_path), "temporary").unwrap();

        let tracked_error = delete_local_path(parent_string.clone(), tracked_path.clone()).unwrap_err();
        assert!(tracked_error.contains("tracked"), "a tracked submodule file must not be removed through local-only delete: {tracked_error}");
        assert!(parent.join(&tracked_path).exists(), "tracked file must be preserved after refused local delete");

        delete_local_path(parent_string, untracked_path.clone()).unwrap();
        assert!(!parent.join(&untracked_path).exists(), "untracked file inside the submodule can be locally deleted");

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
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let file_path = format!("{added}/nou.py");

        fs::write(parent.join(&file_path), "print('hello')\n").unwrap();

        let listing = load_directory_inner(parent_string.clone(), added.clone(), None).unwrap();
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
        let listing_after = load_directory_inner(parent_string, added, None).unwrap();
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
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "test".into(), String::new(), String::new()).unwrap();
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
        let staged_changes = load_repository_inner(parent_string.clone(), Some(true)).unwrap().changes;
        assert!(staged_changes.iter().any(|c| c.path == added && c.staged), "the submodule should be staged after checking it, not silently ignored");

        commit_files_inner(parent_string.clone(), vec![added.clone()], "Bump test submodule".into()).unwrap();

        let repo = Repository::open(&parent).unwrap();
        let recorded_oid = repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id;
        assert_eq!(recorded_oid, expected_oid, "the parent's index should now record the submodule's new commit");
        assert!(!load_repository_inner(parent_string, Some(true)).unwrap().changes.iter().any(|c| c.path == added), "the submodule should no longer show as changed after being committed");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn repo_lock_key_folds_separators_and_canonicalizes_so_the_same_repo_always_gets_the_same_lock() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-lock-key-{suffix}"));
        create_libgit2_repository(&base, "a.txt");
        let canonical = base.to_string_lossy().into_owned();
        // As if pasted from a Windows Explorer address bar (backslash
        // separators) with a trailing separator too.
        let mixed_spelling = format!("{}\\", canonical.replace('/', "\\"));

        assert_eq!(repo_lock_key(&canonical), repo_lock_key(&mixed_spelling), "backslash separators and a trailing separator must resolve to the same lock key");
        assert!(Arc::ptr_eq(&repo_write_lock(&canonical), &repo_write_lock(&mixed_spelling)), "repo_write_lock must hand out the exact same lock instance for equivalent spellings of the same repository");

        #[cfg(windows)]
        {
            // NTFS is case-insensitive — this fold only applies on Windows
            // (a real Unix filesystem is normally case-*sensitive*, where
            // folding case would wrongly merge two different repositories).
            let different_case = canonical.to_uppercase();
            assert_eq!(repo_lock_key(&canonical), repo_lock_key(&different_case), "on Windows, letter case must not change the lock key");
        }

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn concurrent_parent_and_submodule_writes_never_deadlock() {
        // The specific hazard this guards against: one call that touches both
        // the parent's index and a submodule's own index in a single command
        // (stage_files spanning both — the parent's lock, then, nested, the
        // submodule's) racing another that pushes a submodule commit and lets
        // that stage the parent's gitlink (push_submodule_inner, via
        // stage_pushed_submodule_in_parent — the submodule's lock first, then,
        // only after releasing it, the parent's — never nested). If any code
        // path ever reversed that ordering while the other still held its own
        // lock, two threads doing these concurrently would deadlock on unlucky
        // timing. Run many iterations on two genuinely concurrent threads,
        // under a bounded wait — a real deadlock hangs past the timeout
        // instead of finishing; this test failing (rather than hanging
        // forever) is itself the point. Needs a real, pushable remote (not
        // just create_libgit2_repository's bare local dependency) so the
        // racer thread's push actually reaches the parent-touching tail on
        // every iteration, exactly like the pre-fix auto-commit used to.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-deadlock-race-{suffix}"));
        let parent = base.join("parent");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        create_libgit2_repository(&parent, "root.txt");
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::write(dep_seed.join("module.txt"), "v0").unwrap();
        run_git(&dep_remote, &["init", "--bare"]);
        run_git(&dep_seed, &["init"]);
        run_git(&dep_seed, &["config", "user.email", "test@example.com"]);
        run_git(&dep_seed, &["config", "user.name", "Test User"]);
        run_git(&dep_seed, &["add", "."]);
        run_git(&dep_seed, &["commit", "-m", "Initial"]);
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "dep"]);
        run_git(&parent, &["commit", "-am", "Add dep submodule"]);
        let parent_string = parent.to_string_lossy().into_owned();
        let added = "dep".to_string();
        let sub_path = parent.join(&added);
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);

        const ITERATIONS: usize = 25;
        let (tx, rx) = std::sync::mpsc::channel();

        let (p1, s1, a1) = (parent_string.clone(), sub_path.clone(), added.clone());
        let tx1 = tx.clone();
        std::thread::spawn(move || {
            for i in 0..ITERATIONS {
                fs::write(Path::new(&p1).join("root.txt"), format!("parent v{i}")).unwrap();
                fs::write(s1.join("module.txt"), format!("sub v{i}")).unwrap();
                let _ = stage_files(p1.clone(), vec!["root.txt".into(), format!("{a1}/module.txt")]);
            }
            let _ = tx1.send("stage_files racer done");
        });

        let (p2, s2, a2) = (parent_string.clone(), sub_path.clone(), added.clone());
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            for i in 0..ITERATIONS {
                fs::write(s2.join("module.txt"), format!("committed v{i}")).unwrap();
                if commit_submodule_inner(p2.clone(), a2.clone(), format!("iteration {i}"), false).is_ok() {
                    let _ = push_submodule_inner(p2.clone(), a2.clone());
                }
            }
            let _ = tx2.send("commit_submodule racer done");
        });
        drop(tx);

        for _ in 0..2 {
            rx.recv_timeout(Duration::from_secs(30)).expect("concurrent parent/submodule writes must not deadlock");
        }

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn commit_submodule_never_touches_the_parent_when_there_is_nothing_to_push_to() {
        // Submodule-publish-safety report, points 1 and 4: a submodule with no
        // remote at all must remain fully usable locally, and committing inside
        // it must never touch the parent on its own — regardless of whether a
        // push was even attempted. The old behavior ("commit_submodule now
        // records the new commit in the parent right away") is exactly the
        // unsafe auto-commit this fixes: it let "Publish main project" push a
        // parent commit whose gitlink referenced a commit that could never
        // exist anywhere else. The submodule itself must show as an ordinary
        // unstaged modification instead — exactly like editing any other file
        // — until the user explicitly stages/commits that in the parent.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-commit-submodule-auto-bump-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let head_before = Repository::open(&parent).unwrap().head().unwrap().target().unwrap();
        let recorded_before = { let repo = Repository::open(&parent).unwrap(); repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id };

        fs::write(parent.join(&added).join("module.txt"), "v2").unwrap();
        let committed = commit_submodule_inner(parent_string.clone(), added.clone(), "Update module".into(), true).unwrap();
        // also_push was true, but this submodule has no remote configured at
        // all (create_libgit2_repository never adds one) — never attempting a
        // push there is the whole point of point 4, so this must report a
        // clean skip, not an error, and the commit itself must still succeed.
        assert!(!committed.pushed, "there is no remote to push to, so nothing should have been pushed");

        let repo = Repository::open(&parent).unwrap();
        let recorded = repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id;
        assert_eq!(recorded, recorded_before, "the parent's index must NOT change just because the submodule was committed locally");
        assert_eq!(repo.head().unwrap().target().unwrap(), head_before, "committing inside the submodule must never create a commit in the parent on its own");
        drop(repo);

        let changes = load_repository_inner(parent_string, Some(true)).unwrap().changes;
        let dep_change = changes.iter().find(|c| c.path == added).expect("the submodule must show as an ordinary unstaged modification: {:?}");
        assert!(!dep_change.staged, "must not be staged either — nothing was pushed, and nothing was staged on its own");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn load_repository_never_auto_commits_a_submodule_move_regardless_of_force() {
        // Reproduces and fixes a real safety issue: load_repository used to
        // silently create a real commit in the *parent* repository's history
        // whenever it noticed a submodule had moved (by any means: a raw git
        // command, another tool, `git pull` run directly inside it) — for an
        // ordinary reload without hesitation, and even a forced Refresh
        // still did (see the git history of this test — it used to assert
        // the opposite for force:true). A user could open or refresh a
        // repository and find a new commit had appeared in their history
        // that they never asked for. Refresh, navigation, and opening a
        // repository must be completely read-only toward the parent's
        // history — a moved submodule is only ever *reported* as modified
        // (the ordinary status scan already does that, unrelated to this),
        // never silently committed. Only the dedicated commit/push submodule
        // commands may still record it, as the direct, explicit result of
        // that specific user action (see commit_submodule_auto_updates_the_parent_even_without_a_push).
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-no-silent-commit-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "test".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add test submodule".into()).unwrap();
        let head_before = Repository::open(&parent).unwrap().head().unwrap().target().unwrap();

        // Two commits made directly with git inside the submodule — nothing
        // in this app's own commands touched it.
        let sub_path = parent.join(&added);
        run_git(&sub_path, &["commit", "--allow-empty", "-m", "Test update"]);
        run_git(&sub_path, &["commit", "--allow-empty", "-m", "Test update"]);
        let sub_head = Repository::open(&sub_path).unwrap().head().unwrap().target().unwrap();

        // Every shape of reload this app actually sends — ordinary,
        // explicit non-forced, and an explicit forced Refresh — must all
        // leave the parent's history and index completely untouched.
        load_repository_inner(parent_string.clone(), None).unwrap();
        load_repository_inner(parent_string.clone(), Some(false)).unwrap();
        let changes = load_repository_inner(parent_string.clone(), Some(true)).unwrap().changes;

        let repo = Repository::open(&parent).unwrap();
        assert_eq!(repo.head().unwrap().target().unwrap(), head_before, "no reload, forced or not, may ever create a commit in the parent");
        let recorded = repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id;
        assert_ne!(recorded, sub_head, "and must not even silently stage the updated gitlink — the parent's recorded pointer must stay exactly as it was");

        // The move must still be visible — just as an ordinary uncommitted
        // change, for the user to explicitly Stage/Commit, not silently
        // reconciled away.
        assert!(changes.iter().any(|c| c.path == added), "the submodule must show as modified so the user can decide to stage/commit it, not disappear as if nothing happened: {:?}", changes.iter().map(|c| (&c.path, &c.status)).collect::<Vec<_>>());

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
        let listing = load_directory_inner(path.clone(), "src".into(), None).unwrap();
        assert!(!listing.iter().find(|e| e.name == "a.txt").unwrap().unpushed);

        // Commit a change but don't push it.
        fs::write(repository.join("src/a.txt"), "two").unwrap();
        commit_path(path.clone(), "src/a.txt".into(), "Update a.txt".into()).unwrap();

        let listing = load_directory_inner(path.clone(), "src".into(), None).unwrap();
        let entry = listing.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(entry.status, "", "the file is fully committed, so it must not show any working-tree status");
        assert!(entry.unpushed, "a committed-but-unpushed file must be flagged unpushed");

        // The containing folder should reflect it too.
        let root_listing = load_directory_inner(path.clone(), "".into(), None).unwrap();
        let src_entry = root_listing.iter().find(|e| e.name == "src").unwrap();
        assert!(src_entry.unpushed, "a folder containing an unpushed file should be flagged too");

        let details = entry_details_inner(path.clone(), "src/a.txt".into()).unwrap();
        assert!(details.unpushed);

        // After pushing (through the app's own command, which invalidates the
        // cache — a plain external `git push` wouldn't know to), it must clear.
        sync_repository_inner(path.clone(), "push".into()).unwrap();
        let after_push = load_directory_inner(path, "src".into(), None).unwrap();
        assert!(!after_push.iter().find(|e| e.name == "a.txt").unwrap().unpushed, "after push, the file must no longer be flagged unpushed");

        fs::remove_dir_all(repository).unwrap();
        fs::remove_dir_all(remote).unwrap();
    }

    #[test]
    fn unpushed_detection_uses_the_real_upstream_not_a_hardcoded_origin_same_name_branch() {
        // The "unpushed" flag used to hardcode refs/remotes/origin/<local branch
        // name> — wrong for a repository with no `origin` at all, and wrong for
        // a branch tracking a differently-named branch on its remote (a
        // "release"/mirror remote, say). Both together, here.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-unpushed-real-upstream-{suffix}"));
        let remote = std::env::temp_dir().join(format!("git-integrity-unpushed-real-upstream-remote-{suffix}.git"));
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare"]);
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["init", "-b", "work"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial"]);
        // No `origin` at all — the only remote is named "release", and the
        // local branch "work" tracks a remote branch named "main" (a
        // different name than the local branch).
        run_git(&repository, &["remote", "add", "release", remote.to_str().unwrap()]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "push", "-u", "release", "work:main"]);
        let path = repository.to_string_lossy().into_owned();

        let listing = load_directory_inner(path.clone(), "".into(), None).unwrap();
        assert!(!listing.iter().find(|e| e.name == "a.txt").unwrap().unpushed, "freshly pushed to release/main — nothing should be flagged");

        fs::write(repository.join("a.txt"), "two").unwrap();
        commit_path(path.clone(), "a.txt".into(), "Update a.txt".into()).unwrap();
        let listing = load_directory_inner(path.clone(), "".into(), None).unwrap();
        assert!(listing.iter().find(|e| e.name == "a.txt").unwrap().unpushed, "a commit not yet on release/main must be flagged unpushed, found via the branch's real upstream");

        fs::remove_dir_all(repository).unwrap();
        fs::remove_dir_all(remote).unwrap();
    }

    #[test]
    fn submodule_unpushed_status_uses_the_real_upstream_not_a_hardcoded_origin() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-sub-unpushed-real-upstream-{suffix}"));
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
        // The submodule's only remote is "mirror" (not "origin"), tracking a
        // remote branch named "release" (not "main", the local branch name).
        run_git(&sub_path, &["remote", "rename", "origin", "mirror"]);
        run_git(&sub_path, &["branch", "-M", "main"]);
        run_git(&sub_path, &["-c", "protocol.file.allow=always", "push", "-u", "mirror", "main:release"]);

        assert_eq!(submodule_push_status(&sub_path.to_string_lossy()), None, "freshly synced against mirror/release — nothing to report");

        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        commit_submodule_inner(repo_path, "vendor/dep".into(), "Local only".into(), false).unwrap();
        let status = submodule_push_status(&sub_path.to_string_lossy());
        assert!(status.as_deref().is_some_and(|message| message.contains("1 commit") && message.contains("mirror/release")), "expected an unpushed-commit message naming the real upstream mirror/release, got: {status:?}");
        let commits = submodule_unpushed_commits(&sub_path.to_string_lossy());
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "Local only");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn publish_preflight_reuse_is_scoped_to_the_exact_target_branch_and_remote() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = format!("/tmp/git-integrity-publish-preflight-{suffix}");
        let target: git2::Oid = "1111111111111111111111111111111111111111".parse().unwrap();
        let other_target: git2::Oid = "2222222222222222222222222222222222222222".parse().unwrap();

        assert!(!has_recent_publish_preflight(&repository, "main", "origin", target));
        remember_publish_preflight(&repository, "main", "origin", target);
        assert!(has_recent_publish_preflight(&repository, "main", "origin", target));
        assert!(!has_recent_publish_preflight(&repository, "develop", "origin", target));
        assert!(!has_recent_publish_preflight(&repository, "main", "upstream", target));
        assert!(!has_recent_publish_preflight(&repository, "main", "origin", other_target));
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

        publish_branch_inner(path.clone(), "main".into(), "origin".into(), String::new(), String::new(), commit1.clone(), false).unwrap();

        let remote_head = git(&remote.to_string_lossy(), &["rev-parse", "refs/heads/main"]).unwrap().trim().to_string();
        assert_eq!(remote_head, commit1, "the server should be at exactly the chosen commit, not the branch tip");

        // The two newer commits must still show as unpublished locally.
        let status = publish_status(path.clone(), "main".into(), "origin".into()).unwrap();
        assert_eq!(status.commits.len(), 2, "commits 2 and 3 should still be pending, since only commit 1 was published");
        assert_eq!(status.commits[0].subject, "Commit 2");
        assert_eq!(status.commits[1].subject, "Commit 3");

        // Publishing the rest afterward (a normal full push) must succeed cleanly.
        publish_branch_inner(path.clone(), "main".into(), "origin".into(), String::new(), String::new(), String::new(), false).unwrap();
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
    fn terminal_command_runs_shell_and_git_commands_in_the_selected_scope() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-terminal-{suffix}"));
        let nested = repository.join("folder with spaces");
        fs::create_dir_all(&nested).unwrap();
        fs::write(repository.join("README.md"), "terminal test").unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Initial"]);

        let scoped = run_terminal_command_inner(
            nested.to_string_lossy().into_owned(),
            "git rev-parse --show-prefix".into(),
        ).unwrap();
        assert!(scoped.success, "stderr={}", scoped.stderr);
        assert_eq!(scoped.stdout.trim().replace('\\', "/"), "folder with spaces/");

        let arbitrary = run_terminal_command_inner(
            repository.to_string_lossy().into_owned(),
            "echo terminal-ok".into(),
        ).unwrap();
        assert!(arbitrary.success, "stderr={}", arbitrary.stderr);
        assert!(arbitrary.stdout.contains("terminal-ok"));

        let add_remote = run_terminal_command_inner(
            repository.to_string_lossy().into_owned(),
            "git remote add origin https://github.example.test/team/project.git".into(),
        ).unwrap();
        assert!(add_remote.success, "stderr={}", add_remote.stderr);
        assert!(!add_remote.read_only);
        assert_eq!(git(&repository.to_string_lossy(), &["remote", "get-url", "origin"]).unwrap().trim(), "https://github.example.test/team/project.git");

        let status = run_terminal_command_inner(
            repository.to_string_lossy().into_owned(),
            "git status --short".into(),
        ).unwrap();
        assert!(status.success);
        assert!(status.read_only);

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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into(), false).unwrap();
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).unwrap();

        let comparison = compare_remote_directory(repo_path.clone(), "vendor/dep".into(), "origin/main".into()).unwrap();
        let module_row = comparison.rows.iter().find(|row| row.name == "module.txt").expect("module.txt should be listed when browsing inside the submodule");
        assert_eq!(module_row.status, "same", "after commit+push inside the submodule, the file should compare as in sync with the submodule's own remote, got status: {}", module_row.status);

        let file_comparison = compare_file_contents(repo_path, "vendor/dep/module.txt".into(), "origin/main".into()).unwrap();
        assert_eq!(file_comparison.local_content, file_comparison.remote_content, "local and remote content should match after push");
        assert_eq!(file_comparison.local_content, "v2");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_status_is_staged_everywhere_after_push_including_explorer_and_details() {
        // Checks every status source the UI actually reads (load_repository's change
        // list, load_directory's per-row status, and entry_details), not just one of
        // them, to catch any inconsistency between them after a push stages (never
        // commits on its own — see the submodule-publish-safety report) the parent's
        // pointer.
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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into(), false).unwrap();
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).unwrap();

        // Staged, not committed, and not clean either — a real, visible change
        // is exactly what must keep showing until the user explicitly commits
        // the parent themselves.
        let changes = load_repository_inner(repo_path.clone(), Some(true)).unwrap().changes;
        let dep_change = changes.iter().find(|change| change.path == "vendor/dep").expect("load_repository must still list the submodule as a pending (now staged) change: {:?}");
        assert!(dep_change.staged, "the gitlink should be staged after a successful push: {:?}", changes.iter().map(|c| (&c.path, &c.status, c.staged)).collect::<Vec<_>>());

        let entries = load_directory_inner(repo_path.clone(), "vendor".into(), None).unwrap();
        let dep_entry = entries.iter().find(|entry| entry.relative_path == "vendor/dep").expect("submodule entry should be listed");
        assert_eq!(dep_entry.status, "M", "load_directory should still report a status for the staged-but-uncommitted submodule: {:?}", dep_entry.status);

        let details = entry_details_inner(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(details.status, "M", "entry_details should still report a status for the staged-but-uncommitted submodule: {:?}", details.status);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn pushing_a_submodule_stages_the_gitlink_and_the_parent_commit_publishes_cleanly() {
        // The safe end-to-end sequence the submodule-publish-safety report asks
        // for: modify a submodule, commit it, push it — the parent's gitlink is
        // staged, but publish_status must show NOTHING new to push yet (there is
        // no parent commit at all). Only once the user explicitly commits the
        // parent does it show up as one ready-to-push commit, and only then can
        // "Publish main project" actually succeed.
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
        // The report's own point 2: classify by .gitmodules' *effective*
        // clone source, not by whatever this checkout's local "origin"
        // happens to be — a real https:// URL here (unlike dep_remote,
        // fetched via the unchanged local "origin" below) is what makes this
        // test exercise the genuinely-safe case rather than local_only.
        fake_https_gitmodules_url(&repository);
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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into(), false).unwrap();
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).unwrap();

        // The parent's working copy of the submodule must already be at the new revision.
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "v2");

        // Staged, not committed — publish_status walks real commits, and there
        // still isn't one in the parent yet, so it must show nothing new.
        let staged_only = publish_status(repo_path.clone(), "main".into(), "origin".into()).unwrap();
        assert_eq!(staged_only.commits.len(), 0, "the parent must show nothing new to push until the user actually commits the staged gitlink themselves: {:?}", staged_only.commits.iter().map(|c| &c.subject).collect::<Vec<_>>());

        // The user's own explicit parent commit — only now does the bump become
        // a real, ready-to-push commit.
        commit_selected_internal(&repo_path, &["vendor/dep".to_string()], "Bump vendor/dep").expect("committing the staged gitlink should succeed");
        let after = publish_status(repo_path.clone(), "main".into(), "origin".into()).unwrap();
        assert_eq!(after.commits.len(), 1, "parent should show exactly one new commit ready to push (the submodule bump), now that it was actually committed: {:?}", after.commits.iter().map(|c| &c.subject).collect::<Vec<_>>());
        assert!(after.commits[0].subject.contains("vendor/dep"), "the ready-to-push commit should be the submodule bump: {:?}", after.commits[0].subject);

        // And publishing it must actually succeed — the submodule commit it
        // references is already safely on the submodule's own remote.
        publish_branch_inner(repo_path.clone(), "main".into(), "origin".into(), String::new(), String::new(), String::new(), false).expect("publishing should succeed once the submodule was pushed first");
        let after_publish = publish_status(repo_path, "main".into(), "origin".into()).unwrap();
        assert_eq!(after_publish.commits.len(), 0, "nothing should be left to publish after a successful push");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn publish_branch_warns_but_can_override_when_only_an_older_outgoing_commit_carries_an_unpushed_submodule_gitlink() {
        // Point 3 of the submodule-publish-safety report: the CURRENT tip's
        // gitlink can be perfectly fine while an OLDER outgoing commit still
        // carries one that only ever existed locally — git can't push the
        // newer commit while holding the older one back, so that bad gitlink
        // ships right along with it. Built directly (bypassing
        // commit_submodule/push_submodule) so the parent gets two real commits
        // whose gitlinks are known precisely: the first references a
        // submodule commit that is deliberately never reachable from the
        // remote (a sibling of what's actually pushed, not an ancestor of
        // it), the second references one that genuinely is.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-old-outgoing-bad-gitlink-{suffix}"));
        let parent = base.join("main");
        let parent_remote = base.join("main-remote.git");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&parent).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::create_dir_all(&parent_remote).unwrap();
        fs::write(parent.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v0").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        run_git(&parent_remote, &["init", "--bare"]);
        for path in [&parent, &dep_seed] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "vendor/dep"]);
        // A real https:// .gitmodules URL — X2 (fetched via the unchanged
        // local "origin" below) must come back genuinely safe, not
        // local_only, so this test actually exercises "unpushed", the risk
        // it's named for.
        fake_https_gitmodules_url(&parent);
        run_git(&parent, &["commit", "-am", "Add dep submodule"]);
        run_git(&parent, &["remote", "add", "origin", parent_remote.to_str().unwrap()]);

        let parent_path = parent.to_string_lossy().into_owned();
        let sub_path = parent.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);
        let init_sha = git(&sub_path.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();

        // Commit A: bumps the submodule to a commit that will never be pushed.
        fs::write(sub_path.join("module.txt"), "x1").unwrap();
        run_git(&sub_path, &["commit", "-am", "X1"]);
        let x1 = git(&sub_path.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        // This test deliberately constructs a historical bad parent commit.
        // The app's normal commit path now blocks this exact mistake, so use
        // raw Git here to keep the publish-safety regression fixture possible.
        run_git(&parent, &["add", "vendor/dep"]);
        run_git(&parent, &["commit", "-m", "Bump to X1"]);

        // Discard X1 from the submodule's own checkout (its own remote never
        // sees it) and commit a genuinely different, sibling commit instead —
        // X1 stays permanently unreachable from origin, not merely "not yet".
        run_git(&sub_path, &["reset", "--hard", &init_sha]);
        fs::write(sub_path.join("module.txt"), "x2").unwrap();
        run_git(&sub_path, &["commit", "-am", "X2"]);
        run_git(&sub_path, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);

        // Commit B: bumps the submodule again, to the commit that IS pushed.
        run_git(&parent, &["add", "vendor/dep"]);
        run_git(&parent, &["commit", "-m", "Bump to X2"]);

        let x2 = git(&sub_path.to_string_lossy(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let violations = unpushed_submodule_references(&parent_path, "main", "origin", None, true).unwrap();
        assert_eq!(violations.len(), 1, "only X1 (carried by the older, already-superseded commit A) should be flagged, not X2: {violations:?}");
        assert_eq!(violations[0].risk, "superseded_unpushed");
        assert_eq!(violations[0].submodule_oid, x1, "the flagged reference must be the older, unreachable one");
        assert_eq!(violations[0].target_submodule_oid.as_deref(), Some(x2.as_str()), "the branch tip records the pushed replacement gitlink, so the stale X1 pointer is only an intermediate-history risk");
        assert_eq!(violations[0].commit_subject, "Bump to X1");
        // The exact report this reproduces: the submodule's current checkout
        // (X2) is fully pushed and in sync — Push submodule would correctly
        // say "nothing to do" — while this older, already-superseded outgoing
        // commit still references X1, which genuinely isn't on the remote.
        // Both facts are true at once; this is not a contradiction.
        assert_eq!(violations[0].current_submodule_oid.as_deref(), Some(x2.as_str()), "must report the submodule's actual current checkout for context, not merely the flagged (older) oid again");

        let warned = publish_branch_inner(parent_path.clone(), "main".into(), "origin".into(), String::new(), String::new(), String::new(), false);
        assert!(warned.is_err(), "the user must still explicitly acknowledge that an intermediate parent commit is not restorable");
        let message = warned.unwrap_err();
        assert!(message.contains("vendor/dep"), "message should name the affected submodule, got: {message}");
        assert!(message.contains(&x2[..8]), "message should explain that the final parent commit points to X2, so this does not look like it contradicts an in-sync Push submodule preview: {message}");
        assert!(message.starts_with("UNPUSHED_SUBMODULE_OVERRIDABLE::"), "a superseded intermediate gitlink should be an explicit override, not a hard block: {message}");
        // Confirm the override can publish this branch: the branch tip is
        // restorable because it records X2. Only someone checking out the
        // intermediate parent commit "Bump to X1" would hit the missing gitlink.
        publish_branch_inner(parent_path, "main".into(), "origin".into(), String::new(), String::new(), String::new(), true)
            .expect("override should allow publishing a branch whose final gitlink is already safely pushed");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn parent_gitlink_commit_requires_the_submodule_commit_to_be_on_its_remote_first() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-block-local-gitlink-{suffix}"));
        let parent = base.join("main");
        let dep_remote = base.join("dep-remote.git");
        let dep_seed = base.join("dep-seed");
        fs::create_dir_all(&parent).unwrap();
        fs::create_dir_all(&dep_seed).unwrap();
        fs::create_dir_all(&dep_remote).unwrap();
        fs::write(parent.join("README.md"), "root").unwrap();
        fs::write(dep_seed.join("module.txt"), "v0").unwrap();

        run_git(&dep_remote, &["init", "--bare"]);
        for path in [&parent, &dep_seed] {
            run_git(path, &["init"]);
            run_git(path, &["config", "user.email", "test@example.com"]);
            run_git(path, &["config", "user.name", "Test User"]);
            run_git(path, &["add", "."]);
            run_git(path, &["commit", "-m", "Initial"]);
        }
        run_git(&dep_seed, &["remote", "add", "origin", dep_remote.to_str().unwrap()]);
        run_git(&dep_seed, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", dep_remote.to_str().unwrap(), "vendor/dep"]);
        fake_https_gitmodules_url(&parent);
        run_git(&parent, &["commit", "-am", "Add dep submodule"]);

        let parent_path = parent.to_string_lossy().into_owned();
        let sub_path = parent.join("vendor/dep");
        run_git(&sub_path, &["config", "user.email", "test@example.com"]);
        run_git(&sub_path, &["config", "user.name", "Test User"]);
        fs::write(sub_path.join("module.txt"), "v1").unwrap();
        commit_submodule_inner(parent_path.clone(), "vendor/dep".into(), "Submodule v1".into(), false).unwrap();

        let blocked = commit_selected_internal(&parent_path, &["vendor/dep".to_string()], "Record dep v1");
        assert!(blocked.is_err(), "a parent gitlink commit must not record a remote-unreachable submodule revision");
        let message = blocked.unwrap_err();
        assert!(message.contains("Push the submodule commit first"), "message should explain the safe order: {message}");
        assert!(message.contains("vendor/dep"), "message should name the submodule path: {message}");

        push_submodule_inner(parent_path.clone(), "vendor/dep".into()).unwrap();
        commit_selected_internal(&parent_path, &["vendor/dep".to_string()], "Record dep v1")
            .expect("once the submodule commit is on origin, recording the project gitlink is safe");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn is_local_only_url_classifies_filesystem_paths_and_file_urls_correctly() {
        // Genuine, network-reachable-in-principle URLs — real schemes and
        // scp-like shorthand.
        assert!(!is_local_only_url("https://github.com/AndreiRomanC/git-stress-small-demo.git"));
        assert!(!is_local_only_url("http://internal.example/repo.git"));
        assert!(!is_local_only_url("ssh://git@github.com/owner/repo.git"));
        assert!(!is_local_only_url("git://example.com/repo.git"));
        assert!(!is_local_only_url("git@github.com:owner/repo.git"), "scp-like shorthand is remote");
        assert!(!is_local_only_url("gituser@internal-host:team/repo.git"));

        // Only ever resolvable from this exact machine (or one with access
        // to that exact path) — the report's own point 2.
        assert!(is_local_only_url("/Users/andrei/repos/case-github-small/_submodule_sources/git-engine"));
        assert!(is_local_only_url("../_submodule_sources/git-engine"), "a relative path is still a path");
        assert!(is_local_only_url("./sibling-repo"));
        assert!(is_local_only_url("file:///Users/andrei/repos/some-repo"));
        assert!(is_local_only_url("file://localhost/repos/some-repo"));
        assert!(is_local_only_url(r"C:\Users\andrei\repos\some-repo"), "a Windows drive path is local");
        assert!(is_local_only_url("C:/Users/andrei/repos/some-repo"));
        assert!(is_local_only_url(""), "no URL at all can't be reached by anyone");
        assert!(is_local_only_url("   "));
    }

    #[test]
    fn publish_branch_requires_an_explicit_override_for_a_submodule_with_no_remote_at_all() {
        // Point 4 of the submodule-publish-safety report: a submodule that has
        // no remote configured at all can never be verified as safe to
        // reference from a published parent commit — default behavior must
        // still be safe (blocked), but since this genuinely can never be
        // fixed by pushing, an explicit, informed override must be able to
        // proceed anyway.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-no-remote-override-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        let parent_remote = base.join("main-remote.git");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        fs::create_dir_all(&parent_remote).unwrap();
        run_git(&parent_remote, &["init", "--bare"]);
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        run_git(&parent, &["remote", "add", "origin", parent_remote.to_str().unwrap()]);
        // create_libgit2_repository (git2's own Repository::init) picks its own
        // default branch name, independent of this machine's `git init`
        // config — read back whatever it actually is instead of assuming "main".
        let branch = Repository::open(&parent).unwrap().head().unwrap().shorthand().unwrap().to_string();
        // Publish "Add dep submodule" now, before touching anything else, so
        // it's no longer outgoing — only the one commit made below is,
        // keeping this test's own single (path, oid) reference unambiguous.
        run_git(&parent, &["-c", "protocol.file.allow=always", "push", "origin", &format!("HEAD:{branch}")]);

        // add_submodule_inner's clone auto-configures "origin" pointing back
        // at `dependency` itself (ordinary clone behavior), and .gitmodules
        // records that same local path as this submodule's URL — remove
        // BOTH so the submodule genuinely has no URL anywhere, matching
        // "no_remote" specifically (see the separate _local_filesystem_path
        // test right below for a URL that exists but is local-only).
        run_git(&parent.join(&added), &["remote", "remove", "origin"]);
        remove_gitmodules_url_line(&parent);
        fs::write(parent.join(&added).join("module.txt"), "v2").unwrap();
        commit_submodule_inner(parent_string.clone(), added.clone(), "Update module".into(), false).unwrap();
        stage_files_inner(&parent_string, vec![added.clone()]).unwrap();
        commit_selected_internal(&parent_string, &[added.clone()], "Bump dep").unwrap();

        let violations = unpushed_submodule_references(&parent_string, &branch, "origin", None, true).unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].risk, "no_remote");

        let blocked = publish_branch_inner(parent_string.clone(), branch.clone(), "origin".into(), String::new(), String::new(), String::new(), false);
        assert!(blocked.is_err(), "must be blocked by default — this can never be verified as safe");
        assert!(blocked.unwrap_err().starts_with("UNPUSHED_SUBMODULE_OVERRIDABLE::"), "a no-remote submodule must be override-eligible, since pushing it is never an option");

        let overridden = publish_branch_inner(parent_string, branch, "origin".into(), String::new(), String::new(), String::new(), true);
        assert!(overridden.is_ok(), "an explicit override must be able to proceed: {overridden:?}");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn publish_branch_requires_an_explicit_override_for_a_submodule_whose_source_is_a_local_filesystem_path() {
        // Point 2 of the submodule-publish-safety report, the exact reported
        // scenario: a submodule whose .gitmodules URL (and, here, its local
        // "origin" too — add_submodule_inner's ordinary clone behavior) is a
        // plain filesystem path. The commit IS genuinely reachable from that
        // path — fetching from it always "succeeds", it's sitting right
        // there — which is precisely why reachability alone was never
        // enough: "Correctly classify filesystem paths and file:// URLs as
        // LOCAL-ONLY, not as globally available merely because the remote is
        // named origin." Must still block by default and remain
        // override-eligible, exactly like "no_remote" — distinct from that
        // test only in *why* (a real, reachable, but unshareable path,
        // rather than no URL anywhere).
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-local-only-path-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        let parent_remote = base.join("main-remote.git");
        create_libgit2_repository(&parent, "README.md"); create_libgit2_repository(&dependency, "module.txt");
        fs::create_dir_all(&parent_remote).unwrap();
        run_git(&parent_remote, &["init", "--bare"]);
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        // Keep the local-path source non-bare, but move its worktree away
        // from the branch the cloned submodule will push. This is the safe
        // non-bare case Git itself permits; the separate preflight test
        // covers and blocks a destination whose target branch is checked out.
        run_git(&dependency, &["switch", "-c", "fixture-worktree"]);
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        run_git(&parent, &["remote", "add", "origin", parent_remote.to_str().unwrap()]);
        let branch = Repository::open(&parent).unwrap().head().unwrap().shorthand().unwrap().to_string();
        run_git(&parent, &["-c", "protocol.file.allow=always", "push", "origin", &format!("HEAD:{branch}")]);

        // Deliberately left as-is: .gitmodules' url (recorded by
        // add_submodule_inner at clone time) and the submodule's own local
        // "origin" are both still `dependency`'s own filesystem path —
        // exactly the reported "origin is a local filesystem repository"
        // shape. Commit and push the submodule to that very path, so the
        // commit really is present there.
        fs::write(parent.join(&added).join("module.txt"), "v2").unwrap();
        let new_sha = commit_submodule_inner(parent_string.clone(), added.clone(), "Update module".into(), false).unwrap().revision;
        push_submodule_inner(parent_string.clone(), added.clone()).expect("pushing to the local-path remote should succeed, since it's genuinely reachable");
        commit_selected_internal(&parent_string, &[added.clone()], "Bump dep").unwrap();

        let violations = unpushed_submodule_references(&parent_string, &branch, "origin", None, true).unwrap();
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].risk, "local_only", "reachable or not, a filesystem-path source is never globally available");
        assert_eq!(violations[0].submodule_oid, new_sha);
        assert!(violations[0].configured_url.as_deref().is_some_and(|url| url == dependency.to_string_lossy()), "the reported URL should be the actual local path, for display: {:?}", violations[0].configured_url);

        let blocked = publish_branch_inner(parent_string.clone(), branch.clone(), "origin".into(), String::new(), String::new(), String::new(), false);
        assert!(blocked.is_err(), "must be blocked by default even though the commit is genuinely reachable from that path");
        assert!(blocked.unwrap_err().starts_with("UNPUSHED_SUBMODULE_OVERRIDABLE::"), "a local-only submodule must be override-eligible, since pushing it anywhere else isn't this app's decision to make");

        let overridden = publish_branch_inner(parent_string, branch, "origin".into(), String::new(), String::new(), String::new(), true);
        assert!(overridden.is_ok(), "an explicit override must be able to proceed: {overridden:?}");

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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Local only".into(), false).unwrap();
        let status = submodule_push_status(&sub_path.to_string_lossy());
        assert!(status.as_deref().is_some_and(|message| message.contains("1 commit") && message.contains("needs push")), "expected an unpushed-commit message, got: {status:?}");

        // The actual commit(s) behind that summary must be listed, not just counted.
        let unpushed = submodule_unpushed_commits(&sub_path.to_string_lossy());
        assert_eq!(unpushed.len(), 1);
        assert_eq!(unpushed[0].subject, "Local only");

        // After a successful push, the warning must clear.
        push_submodule_inner(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(submodule_push_status(&sub_path.to_string_lossy()), None, "after push, the submodule should no longer report anything unpushed");
        assert!(submodule_unpushed_commits(&sub_path.to_string_lossy()).is_empty(), "after push, the unpushed commit list should be empty too");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn load_directory_caches_submodule_unpushed_status_instead_of_rescanning_every_call() {
        // Reproduces and fixes the confirmed critical-path cost: load_directory
        // used to call submodule_push_status — opening a whole separate
        // Repository and walking commits — synchronously, once per submodule
        // entry, on *every single* folder listing. Proven the same way as the
        // other cache-reuse tests in this file: after the folder is listed
        // once (seeding the cache), a change to the submodule's push status
        // made without going through this app must NOT show up on a second,
        // unforced listing — that would only happen if it reused the cache
        // rather than rescanning. An explicit force:true must still see it.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-load-directory-submodule-cache-{suffix}"));
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

        // First listing: freshly in sync, nothing unpushed — also seeds the cache.
        let listing = load_directory_inner(repo_path.clone(), "vendor".into(), None).unwrap();
        let entry = listing.iter().find(|e| e.name == "dep").unwrap();
        assert!(!entry.submodule_has_unpushed_commits, "freshly synced submodule should not be flagged");

        // A commit lands in the submodule without going through load_directory again.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "Local only, not pushed"]);

        // Without force, the cache seeded above is still fresh (TTL is 300s) and unaware of it.
        let stale = load_directory_inner(repo_path.clone(), "vendor".into(), None).unwrap();
        let stale_entry = stale.iter().find(|e| e.name == "dep").unwrap();
        assert!(!stale_entry.submodule_has_unpushed_commits, "sanity check: without force, the cached (clean) submodule-unpushed set must still be reused, proving load_directory didn't rescan on its own");

        // A forced reload (Reload folder) must invalidate and see the real, current state.
        let forced = load_directory_inner(repo_path, "vendor".into(), Some(true)).unwrap();
        let forced_entry = forced.iter().find(|e| e.name == "dep").unwrap();
        assert!(forced_entry.submodule_has_unpushed_commits, "force:true must invalidate the cache and report the real, current unpushed commit");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_is_dirty_distinguishes_a_pushed_version_bump_from_genuinely_dirty_content() {
        // Reported bug: a submodule that was committed and pushed to origin,
        // but not yet committed into the parent project, showed the same
        // plain "Modified" label as an ordinary dirty file — because the
        // Explorer's "New version" gate checked submodule_has_unpushed_commits
        // (a hard "no" once it's actually pushed) instead of whether the
        // submodule itself has any dirty content at all. Walks through every
        // relevant state and checks both load_directory and entry_details
        // agree on submodule_is_dirty for each.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-is-dirty-{suffix}"));
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

        let default_branch = Repository::open(&sub_path).unwrap().head().unwrap().shorthand().unwrap().to_string();

        // State 1: freshly synced, nothing to report anywhere.
        let clean = load_directory_inner(repo_path.clone(), "vendor".into(), Some(true)).unwrap();
        let clean_entry = clean.iter().find(|e| e.name == "dep").unwrap();
        assert_eq!(clean_entry.status, "");
        assert_eq!(clean_entry.submodule_state, "synced");
        assert!(!clean_entry.submodule_is_dirty);
        assert!(!clean_entry.submodule_has_unpushed_commits);
        assert!(!clean_entry.submodule_checked, "a clean, synced submodule must not have its own repository opened just to report attached/detached — that is exactly the per-row cost avoided for the common case");
        assert_eq!(clean_entry.submodule_current_branch, None);

        // State 2: genuinely dirty content inside the submodule, HEAD unchanged.
        fs::write(sub_path.join("module.txt"), "uncommitted edit").unwrap();
        let dirty = load_directory_inner(repo_path.clone(), "vendor".into(), Some(true)).unwrap();
        let dirty_entry = dirty.iter().find(|e| e.name == "dep").unwrap();
        assert_eq!(dirty_entry.status, "M");
        assert_eq!(dirty_entry.submodule_state, "changes_inside");
        assert!(dirty_entry.submodule_is_dirty, "an uncommitted edit inside the submodule, with its own HEAD unchanged, must be flagged dirty");
        let dirty_details = entry_details_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert!(dirty_details.submodule_is_dirty, "entry_details must agree with load_directory");
        run_git(&sub_path, &["checkout", "--", "module.txt"]); // back to clean

        // State 3: a new commit, deliberately NOT pushed yet.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into(), false).unwrap();
        let unpushed = load_directory_inner(repo_path.clone(), "vendor".into(), Some(true)).unwrap();
        let unpushed_entry = unpushed.iter().find(|e| e.name == "dep").unwrap();
        assert_eq!(unpushed_entry.status, "M");
        assert_eq!(unpushed_entry.submodule_state, "local_commit_push_needed");
        assert!(!unpushed_entry.submodule_is_dirty, "a clean version bump is never 'dirty', pushed or not");
        assert!(unpushed_entry.submodule_has_unpushed_commits);
        assert!(unpushed_entry.submodule_checked, "this row already had a reason to be inspected — riding along on that same check is free");
        assert_eq!(unpushed_entry.submodule_current_branch, Some(default_branch.clone()));

        // State 4: pushed to origin, but not staged in the project yet.
        run_git(&sub_path, &["push", "-u", "origin", "HEAD:main"]);
        invalidate_submodule_sync(&repo_path);
        let pushed = load_directory_inner(repo_path.clone(), "vendor".into(), Some(true)).unwrap();
        let pushed_entry = pushed.iter().find(|e| e.name == "dep").unwrap();
        assert_eq!(pushed_entry.status, "M");
        assert_eq!(pushed_entry.submodule_state, "on_origin_stage_project");
        assert!(!pushed_entry.submodule_is_dirty, "pushing must not make the submodule 'dirty' — it's the safest state a version bump can be in");
        assert!(!pushed_entry.submodule_has_unpushed_commits, "it really was pushed");

        // State 5: the pushed gitlink is staged, but not committed in parent.
        stage_files_inner(&repo_path, vec!["vendor/dep".into()]).unwrap();
        let staged = load_directory_inner(repo_path.clone(), "vendor".into(), Some(true)).unwrap();
        let staged_entry = staged.iter().find(|e| e.name == "dep").unwrap();
        assert_eq!(staged_entry.submodule_state, "on_origin_commit_project");
        let pushed_details = entry_details_inner(repo_path.clone(), "vendor/dep".into()).unwrap();
        assert!(!pushed_details.submodule_is_dirty);
        assert_eq!(pushed_details.submodule_state, "on_origin_commit_project");
        assert!(pushed_details.submodule_push_status.is_none(), "submodule_push_status must be None once pushed — this is exactly what the frontend used to (wrongly) rely on alone to decide 'New version'");

        // State 6: parent committed the gitlink. Whether that parent commit
        // still needs pushing is a parent-repository fact, not a submodule
        // working-tree guess.
        commit_selected_internal(&repo_path, &["vendor/dep".into()], "Record dep v2").unwrap();
        let parent = internal_repository(&repo_path).unwrap();
        let snapshot = inspect_submodule_state(sub_path.to_str().unwrap());
        assert_eq!(submodule_workflow_state(&parent, "vendor/dep", true, &snapshot), "project_commit_push_needed");
        assert_eq!(submodule_workflow_state(&parent, "vendor/dep", false, &snapshot), "synced");
        drop(parent);

        // State 7: a clean local-only commit made while detached cannot be
        // described as a normal branch commit that is ready to push.
        run_git(&sub_path, &["checkout", "--detach"]);
        fs::write(sub_path.join("module.txt"), "v3-local-detached").unwrap();
        run_git(&sub_path, &["commit", "-am", "Detached local work"]);
        let detached = load_directory_inner(repo_path, "vendor".into(), Some(true)).unwrap();
        let detached_entry = detached.iter().find(|e| e.name == "dep").unwrap();
        assert_eq!(detached_entry.submodule_state, "detached_choose_branch");
        assert!(detached_entry.submodule_checked);
        assert_eq!(detached_entry.submodule_current_branch, None, "detached HEAD must report as None, not silently reuse a stale branch name");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn load_directory_only_scans_visible_submodules_with_state_to_explain() {
        // A real counter check, not a timing threshold: synchronized rows do
        // not open/scan their sub-repositories at all. Only a visible row
        // whose parent status says something changed gets one consolidated,
        // cached inspection; hidden and clean siblings stay untouched.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-scan-scope-{suffix}"));
        let repository = base.join("main");
        create_libgit2_repository(&repository, "README.md");
        fs::create_dir_all(repository.join("groupA")).unwrap();
        fs::create_dir_all(repository.join("groupB")).unwrap();
        let mut group_a = Vec::new();
        for name in ["subA1", "subA2"] {
            let seed = base.join(format!("seed-{name}"));
            create_libgit2_repository(&seed, "module.txt");
            run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", seed.to_str().unwrap(), &format!("groupA/{name}")]);
            group_a.push(repository.join("groupA").join(name));
        }
        let mut group_b = Vec::new();
        for name in ["subB1", "subB2", "subB3"] {
            let seed = base.join(format!("seed-{name}"));
            create_libgit2_repository(&seed, "module.txt");
            run_git(&repository, &["-c", "protocol.file.allow=always", "submodule", "add", seed.to_str().unwrap(), &format!("groupB/{name}")]);
            group_b.push(repository.join("groupB").join(name));
        }
        run_git(&repository, &["commit", "-am", "Add 5 submodules across two folders"]);
        let repo_path = repository.to_string_lossy().into_owned();

        // Only subA1 has a state that needs explaining in the Explorer.
        fs::write(group_a[0].join("module.txt"), "dirty").unwrap();

        // Only groupA is ever listed.
        let listing = load_directory_inner(repo_path, "groupA".into(), Some(true)).unwrap();
        assert_eq!(listing.iter().filter(|e| e.kind == "submodule").count(), 2);
        assert!(listing.iter().find(|e| e.name == "subA1").unwrap().submodule_checked, "the dirty sibling has a reason to be inspected");
        assert!(!listing.iter().find(|e| e.name == "subA2").unwrap().submodule_checked, "the clean sibling, in the same listing, must not be");

        // Filtered to this test's own repository prefix — the cache is a
        // process-global static shared with every other test in this suite
        // (which run concurrently), so asserting its *total* size would be
        // flaky by construction; scoping to paths under this repository is
        // what actually proves the property under test.
        let repo_prefix = repository.to_string_lossy().into_owned();
        let scanned: std::collections::HashSet<String> = submodule_state_cache().lock().unwrap().keys()
            .filter(|key| key.starts_with(&repo_prefix)).cloned().collect();
        assert!(scanned.contains(group_a[0].to_str().unwrap()), "the changed visible submodule must be scanned");
        assert!(!scanned.contains(group_a[1].to_str().unwrap()), "a clean visible sibling must not be scanned");
        for sub in &group_b { assert!(!scanned.contains(sub.to_str().unwrap()), "a submodule in a folder that was never listed must NOT have been scanned: {sub:?}"); }
        assert_eq!(scanned.len(), 1, "exactly the one changed visible submodule, not every visible or registered submodule");

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
        pull_submodule_inner(repo_path.clone(), "vendor/dep".into()).expect("a clean fast-forward pull should succeed");
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "from someone else", "pull should have brought in the other clone's content");

        // Now create a REAL divergence: local commits something new, and the remote
        // (via the other clone) also moves again — neither is an ancestor of the other.
        fs::write(sub_path.join("module.txt"), "local edit").unwrap();
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Local divergent commit".into(), false).unwrap();
        fs::write(other_clone.join("module.txt"), "remote diverges too").unwrap();
        run_git(&other_clone, &["commit", "-am", "Remote diverges too"]);
        run_git(&other_clone, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);

        let diverged = pull_submodule_inner(repo_path.clone(), "vendor/dep".into());
        assert!(diverged.is_err(), "a real divergence must not be silently resolved, got Ok");
        let message = diverged.unwrap_err();
        assert!(message.to_lowercase().contains("diverged") || message.to_lowercase().contains("manual"), "expected a message explaining manual resolution is needed, got: {message}");

        // force_push_submodule must resolve exactly this stuck situation by
        // overwriting the remote with the local history.
        let forced = force_push_submodule_inner(repo_path.clone(), "vendor/dep".into());
        assert!(forced.is_ok(), "force_push_submodule should succeed even when diverged, got: {:?}", forced);
        let remote_content = git(&dep_remote.to_string_lossy(), &["show", "main:module.txt"]).unwrap();
        assert_eq!(remote_content.trim(), "local edit", "the remote should now match the local (forced) content");
        // Staged (not auto-committed — see the submodule-publish-safety
        // report) after a force push too, exactly like an ordinary push.
        let parent_changes = load_repository_inner(repo_path, Some(true)).unwrap().changes;
        let dep_change = parent_changes.iter().find(|change| change.path == "vendor/dep").expect("the submodule's gitlink change should be listed, staged, after a force push");
        assert!(dep_change.staged, "the parent's reference should be staged after a force push too");

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
    fn pull_request_url_validation_accepts_generated_links_and_rejects_everything_else() {
        // Reproduces the reported bug directly: clicking "Open pull request" on
        // a real PR card always failed with "Only generated Polarion links can
        // be opened" — open_external_url only ever allowlisted Polarion's own
        // fixed host, never the shape `gh pr list --json url` actually returns.
        assert!(is_generated_pull_request_url("https://github.com/AndreiRomanC/git-stress-small-demo/pull/1"));
        assert!(is_generated_pull_request_url("https://github.example/eng/sw-prj-OMBMS_000U0/pull/42"), "an enterprise GitHub host must work too, not just github.com");
        assert!(is_generated_pull_request_url("https://github.example/eng/sw-prj-OMBMS_000U0/pull/42#pullrequestreview-987"), "an exact GitHub review permalink must be accepted");
        assert!(is_generated_pull_request_url("https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pulls"), "the generated signed-in-browser fallback must be accepted too");

        // Wrong scheme, wrong shape, or an attempt to smuggle a different
        // destination or extra shell arguments must all be rejected — this
        // reaches a shell command exactly like the Polarion link does.
        assert!(!is_generated_pull_request_url("http://github.com/owner/repo/pull/1"), "must require https");
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo/pulls/1"), "must be the singular /pull/ path GitHub actually uses");
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo/pulls?q=is%3Aopen"), "the fallback is an exact generated path, never an arbitrary query string");
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo/pull/1#discussion_r123"), "only exact review permalinks are accepted, not arbitrary fragments");
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo/pull/1#pullrequestreview-12x"), "review identifiers must be numeric");
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo/pull/"), "PR number must not be empty");
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo/pull/1x"), "PR number must be all digits");
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo/pull/1/files"), "no trailing path beyond the PR number");
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo"), "a bare repo URL is not a PR link");
        assert!(!is_generated_pull_request_url("javascript:alert(1)"));
        assert!(!is_generated_pull_request_url("https://github.com/owner/repo/pull/1\" & calc.exe"));
    }

    #[test]
    fn pull_request_comment_target_is_derived_only_from_a_valid_pr_url() {
        let (repo, number) = parse_pull_request_target("https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0/pull/282").unwrap();
        assert_eq!(repo.gh_repo_arg(), "github.vitesco.io/eng/sw-prj-VWAQ4_000U0");
        assert_eq!(number, 282);
        assert_eq!(github_rest_endpoint(&repo, "issues/282/comments"), "https://github.vitesco.io/api/v3/repos/eng/sw-prj-VWAQ4_000U0/issues/282/comments");
        assert!(parse_pull_request_target("https://evil.example/eng/repo/pull/282").is_none());
        assert!(parse_pull_request_target("https://github.vitesco.io/eng/repo/issues/282").is_none());
    }

    #[test]
    fn browser_repository_url_builds_a_commit_link_for_enterprise_github_too() {
        let base = browser_repository_url("git@github.vitesco.io:eng/sw-prj-OMBMS_000U0.git").expect("should parse an SSH enterprise GitHub URL");
        assert_eq!(base, "https://github.vitesco.io/eng/sw-prj-OMBMS_000U0");
        assert_eq!(format!("{base}/commit/49032750e188cfb56b0c72834feef071a4d9cc13"), "https://github.vitesco.io/eng/sw-prj-OMBMS_000U0/commit/49032750e188cfb56b0c72834feef071a4d9cc13");
    }

    #[test]
    fn resolve_relative_git_url_lands_on_the_sibling_the_way_git_does() {
        // Every URL shape a `.gitmodules` `url = ../dep` can be resolved
        // against — a `../` strips one path segment off the *parent's* remote,
        // so the submodule is a sibling of the parent, never a child under it.
        // (Each case was cross-checked against `git submodule sync`.)
        assert_eq!(resolve_relative_git_url("git@github.vitesco.io:eng/parent.git", "../submodul.git"), "git@github.vitesco.io:eng/submodul.git");
        assert_eq!(resolve_relative_git_url("ssh://git@github.vitesco.io/eng/parent.git", "../submodul.git"), "ssh://git@github.vitesco.io/eng/submodul.git");
        assert_eq!(resolve_relative_git_url("https://github.vitesco.io/eng/parent.git", "../submodul.git"), "https://github.vitesco.io/eng/submodul.git");
        assert_eq!(resolve_relative_git_url("git@github.vitesco.io:eng/grp/parent.git", "../submodul.git"), "git@github.vitesco.io:eng/grp/submodul.git");
        // Two levels up, then back down a different path.
        assert_eq!(resolve_relative_git_url("https://github.vitesco.io/eng/grp/parent.git", "../../other/dep.git"), "https://github.vitesco.io/eng/other/dep.git");
        // Only one segment left after the host: git truncates at the ':'.
        assert_eq!(resolve_relative_git_url("git@github.vitesco.io:parent.git", "../submodul.git"), "git@github.vitesco.io:submodul.git");
        // A bare filesystem path (local bare-repo remotes, used in tests).
        assert_eq!(resolve_relative_git_url("/srv/git/eng/parent.git", "../submodul.git"), "/srv/git/eng/submodul.git");
        // `./` is a no-op; a non-relative URL is returned untouched.
        assert_eq!(resolve_relative_git_url("https://host/eng/parent.git", "./submodul.git"), "https://host/eng/parent.git/submodul.git");
        assert_eq!(resolve_relative_git_url("https://host/eng/parent.git", "https://elsewhere/x.git"), "https://elsewhere/x.git");
    }

    #[test]
    fn vitesco_submodule_urls_are_stored_in_the_portable_enterprise_form() {
        assert_eq!(portable_submodule_configured_url("https://github.vitesco.io/eng/sw-pkg-0G-errm_statis"), "../../eng/sw-pkg-0G-errm_statis");
        assert_eq!(portable_submodule_configured_url("git@github.vitesco.io:eng/sw-pkg-0G-errm_common.git"), "../../eng/sw-pkg-0G-errm_common.git");
        assert_eq!(portable_submodule_configured_url("../../eng/sw-pkg-0G-errm_common.git"), "../../eng/sw-pkg-0G-errm_common.git");
        assert_eq!(portable_submodule_configured_url("https://github.com/example/public.git"), "https://github.com/example/public.git", "other Git hosts must keep their chosen URL");
    }

    #[test]
    fn relative_gitmodules_url_is_classified_after_resolving_the_parent_remote() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-relative-source-{suffix}"));
        let network_parent = base.join("network-parent");
        let local_parent = base.join("local-parent");
        create_libgit2_repository(&network_parent, "README.md");
        create_libgit2_repository(&local_parent, "README.md");

        let network_repo = Repository::open(&network_parent).unwrap();
        network_repo.remote("origin", "https://github.vitesco.io/eng/sw-prj-VWAQ4_000U0.git").unwrap();
        let network_source = resolved_submodule_io_url(&network_repo, "../../eng/sw-pkg-0G-errm_common.git").unwrap();
        assert_eq!(network_source, "https://github.vitesco.io/eng/sw-pkg-0G-errm_common.git");
        assert!(!is_local_only_url(&network_source), "errm_common is network-restorable through the parent origin, not local-only");

        let local_remote = base.join("server/eng/parent.git");
        let local_repo = Repository::open(&local_parent).unwrap();
        local_repo.remote("origin", local_remote.to_str().unwrap()).unwrap();
        let local_source = resolved_submodule_io_url(&local_repo, "../../eng/sw-pkg-0G-errm_common.git").unwrap();
        assert!(is_local_only_url(&local_source), "a relative URL resolved against a filesystem parent remote must remain local-only");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn add_submodule_keeps_gitmodules_relative_but_uses_a_resolved_origin() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-portable-add-{suffix}"));
        let server = base.join("server/eng");
        let parent = base.join("work/parent");
        let dependency_seed = base.join("dependency-seed");
        fs::create_dir_all(&server).unwrap();
        run_git(&server, &["init", "-q", "--bare", "parent.git"]);
        run_git(&server, &["init", "-q", "--bare", "dependency.git"]);
        create_libgit2_repository(&dependency_seed, "module.txt");
        run_git(&dependency_seed, &["branch", "-M", "main"]);
        run_git(&dependency_seed, &["push", "-q", server.join("dependency.git").to_str().unwrap(), "main"]);

        create_libgit2_repository(&parent, "README.md");
        run_git(&parent, &["remote", "add", "origin", server.join("parent.git").to_str().unwrap()]);
        let parent_string = parent.to_string_lossy().into_owned();
        let relative_url = "../../eng/dependency.git";
        let added = add_submodule_inner(parent_string, "".into(), relative_url.into(), "dependency".into(), String::new(), String::new()).unwrap();

        let parent_repo = Repository::open(&parent).unwrap();
        let stored = parent_repo.submodules().unwrap().into_iter().find(|submodule| normalized(submodule.path()) == added).unwrap().url().unwrap().to_string();
        assert_eq!(stored, relative_url, ".gitmodules must retain the portable value");
        let submodule_repo = internal_submodule_repository(&parent.join(&added)).unwrap();
        assert_eq!(first_remote_url(&submodule_repo).as_deref(), Some(server.join("dependency.git").to_str().unwrap()), "local origin must use the resolved source for fetch/push");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_browser_base_resolves_a_relative_gitmodules_url_to_the_sibling_not_a_child() {
        // The report: right-clicking a submodule -> "Open on server" opened
        // `<parent-url>/tree/<branch>/<path>` — the parent's own gitlink page —
        // instead of the submodule's own project, which for a relative
        // `url = ../submodul.git` lives *next to* the parent on the server.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-relurl-{suffix}"));
        let server = base.join("server/eng");
        fs::create_dir_all(&server).unwrap();
        run_git(&base, &["init", "-q", "--bare", "server/eng/parent.git"]);
        run_git(&base, &["init", "-q", "--bare", "server/eng/submodul.git"]);

        // Seed the submodule's bare "server" repo with one commit.
        let sub_src = base.join("sub-src");
        create_libgit2_repository(&sub_src, "lib.txt");
        run_git(&sub_src, &["branch", "-M", "main"]);
        run_git(&sub_src, &["push", "-q", server.join("submodul.git").to_str().unwrap(), "main"]);

        // A real parent working copy with a real remote, and the submodule
        // added by the *relative* URL (`../submodul.git`) exactly as the
        // real repositories do.
        let parent = base.join("work/parent");
        create_libgit2_repository(&parent, "root.txt");
        run_git(&parent, &["branch", "-M", "main"]);
        run_git(&parent, &["config", "protocol.file.allow", "always"]);
        run_git(&parent, &["remote", "add", "origin", server.join("parent.git").to_str().unwrap()]);
        run_git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", "../submodul.git", "eng/submodul"]);
        run_git(&parent, &["commit", "-qm", "add submodule"]);
        let parent_string = parent.to_string_lossy().into_owned();

        // Sanity: the submodule's own .git is present and valid (so the
        // "prefer the submodule's absolute origin" path is what runs here).
        assert!(parent.join("eng/submodul/lib.txt").exists());

        // Now the parent points at an enterprise SSH remote (what a developer
        // actually has after cloning), and the submodule is synced against it.
        run_git(&parent, &["remote", "set-url", "origin", "git@github.vitesco.io:eng/parent.git"]);
        run_git(&parent, &["submodule", "sync", "-q", "--", "eng/submodul"]);

        let resolved = submodule_browser_base(&parent_string, "eng/submodul").unwrap();
        assert_eq!(resolved, "https://github.vitesco.io/eng/submodul",
            "must be the sibling of the parent on the server, not github.vitesco.io/eng/parent/... ");

        // And the case where nothing resolved the submodule's own origin —
        // it's still the raw relative `../submodul.git`, and the parent's
        // `.git/config` no longer carries a resolved `submodule.<name>.url`
        // either, so only `.gitmodules` (relative) is left to go on. The
        // parent's remote must still be used to reach the same sibling.
        run_git(&parent.join("eng/submodul"), &["config", "remote.origin.url", "../submodul.git"]);
        run_git(&parent, &["config", "--unset", "submodule.eng/submodul.url"]);
        let resolved_from_gitmodules = submodule_browser_base(&parent_string, "eng/submodul").unwrap();
        assert_eq!(resolved_from_gitmodules, "https://github.vitesco.io/eng/submodul",
            "a relative URL that was never resolved into .git/config must still resolve against the parent's remote");

        // Even with no `.gitmodules` entry left at all (an orphaned submodule
        // working dir whose own .git is still valid but carries only a
        // relative origin), it must resolve against the parent — never fall
        // back to opening the parent's own URL.
        run_git(&parent, &["config", "-f", ".gitmodules", "--remove-section", "submodule.eng/submodul"]);
        let resolved_orphan = submodule_browser_base(&parent_string, "eng/submodul").unwrap();
        assert_eq!(resolved_orphan, "https://github.vitesco.io/eng/submodul",
            "a relative-only submodule origin with no .gitmodules entry must still resolve to the sibling, not the parent");

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn submodule_browser_base_uses_an_absolute_submodule_origin_verbatim() {
        // When the submodule already has a fully-qualified origin of its own
        // (the common case once it's been cloned/updated), that is
        // authoritative — no parent-relative resolution, even if the parent's
        // remote points somewhere unrelated.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-abs-suburl-{suffix}"));
        let parent = base.join("parent");
        let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        run_git(&parent, &["remote", "add", "origin", "git@github.vitesco.io:eng/unrelated-parent.git"]);
        run_git(&parent.join(&added), &["remote", "set-url", "origin", "git@github.vitesco.io:tools/dependency.git"]);

        let resolved = submodule_browser_base(&parent_string, &added).unwrap();
        assert_eq!(resolved, "https://github.vitesco.io/tools/dependency");

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn stage_all_resyncs_a_staged_file_that_was_since_deleted_from_disk() {
        // Reproduces the report: copy many files in from outside, stage all of
        // them, then delete a few before committing — "Stage all" (which used
        // to only ever send still-*unstaged* paths) never touched the ones
        // already marked staged, so a deleted-after-staging file stayed
        // staged in the index forever, no matter how many times "Stage all"
        // was pressed. Fixed by having the frontend include already-staged
        // paths too; this test exercises the backend half directly: calling
        // stage_files again with a path that's staged but no longer on disk
        // must actually remove it from the index, not leave the stale entry.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-stage-resync-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        // Simulate "245 files copied in from outside": several new files at once.
        let names: Vec<String> = (0..5).map(|i| format!("new_{i}.txt")).collect();
        for name in &names { fs::write(repo_path.join(name), "content").unwrap(); }
        stage_files(repo_string.clone(), names.clone()).unwrap();
        {
            let repo = internal_repository(&repo_string).unwrap();
            let index = repo.index().unwrap();
            for name in &names { assert!(index.get_path(Path::new(name), 0).is_some(), "{name} should be staged"); }
        }

        // Delete two of the now-staged files straight from disk, then ask
        // stage_files to resync the *same* full set again — as "Stage all"
        // now does, instead of skipping paths it already thinks are staged.
        fs::remove_file(repo_path.join("new_0.txt")).unwrap();
        fs::remove_file(repo_path.join("new_2.txt")).unwrap();
        stage_files(repo_string.clone(), names.clone()).unwrap();

        let repo = internal_repository(&repo_string).unwrap();
        let index = repo.index().unwrap();
        assert!(index.get_path(Path::new("new_0.txt"), 0).is_none(), "deleted file should be gone from the index after resyncing");
        assert!(index.get_path(Path::new("new_2.txt"), 0).is_none(), "deleted file should be gone from the index after resyncing");
        assert!(index.get_path(Path::new("new_1.txt"), 0).is_some(), "untouched file should remain staged");
        assert!(index.get_path(Path::new("new_3.txt"), 0).is_some(), "untouched file should remain staged");
        assert!(index.get_path(Path::new("new_4.txt"), 0).is_some(), "untouched file should remain staged");
    }

    #[test]
    fn commit_staged_commits_exactly_the_real_index_and_clears_it() {
        // commit_staged is the fast path for the main Commit button: instead
        // of rebuilding a scratch index from the parent tree and re-adding
        // every path (what commit_files does, needed there since it also
        // supports committing part of what's staged), it should just use the
        // real on-disk index directly.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-commit-staged-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        // Nothing staged yet — should refuse, same as commit_files would.
        assert!(commit_staged_inner(repo_string.clone(), "Empty".into()).is_err());

        let names: Vec<String> = (0..5).map(|i| format!("new_{i}.txt")).collect();
        for name in &names { fs::write(repo_path.join(name), "content").unwrap(); }
        stage_files(repo_string.clone(), names.clone()).unwrap();

        let oid = commit_staged_inner(repo_string.clone(), "Add five files".into()).unwrap();
        let repo = internal_repository(&repo_string).unwrap();
        let commit = repo.find_commit(git2::Oid::from_str(&oid).unwrap()).unwrap();
        for name in &names { assert!(commit.tree().unwrap().get_path(Path::new(name)).is_ok(), "{name} should be in the new commit"); }

        // The index shouldn't need any post-commit resync — it already equals
        // the new HEAD's tree, so nothing should show as staged anymore.
        let head_tree = repo.head().unwrap().peel_to_commit().unwrap().tree().unwrap();
        let mut index = repo.index().unwrap();
        assert_eq!(index.write_tree().unwrap(), head_tree.id(), "index should already match the new HEAD tree with no resync needed");

        // Nothing staged again now — a second call should refuse too.
        assert!(commit_staged_inner(repo_string.clone(), "Nothing to commit".into()).is_err());
    }

    // Ignored by default (`cargo test` skips it; run explicitly with
    // `cargo test -- --ignored large_index_reproduces_the_245_file_report`)
    // — building a 50k+-entry index takes real time and would otherwise slow
    // down the normal test suite on every run. This is the actual reported
    // scenario at a scale close to the real repository: a large pre-existing
    // index, 245 new files copied in from outside, staged, a few deleted
    // afterward, staged again, then committed — with per-phase timings
    // printed so a real slowdown shows exactly which phase it's in.
    #[test]
    #[ignore]
    fn large_index_reproduces_the_245_file_report() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-large-index-{suffix}"));
        fs::create_dir_all(&repo_path).unwrap();
        let repo = Repository::init(&repo_path).unwrap();

        let setup_started = Instant::now();
        {
            let mut index = repo.index().unwrap();
            for dir in 0..500 {
                let dir_path = repo_path.join(format!("existing_{dir}"));
                fs::create_dir_all(&dir_path).unwrap();
                for file in 0..100 {
                    fs::write(dir_path.join(format!("f{file}.txt")), b"x").unwrap();
                }
                index.add_all([format!("existing_{dir}").as_str()], git2::IndexAddOption::DEFAULT, None).unwrap();
            }
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "Large initial import", &tree, &[]).unwrap();
        }
        println!("PERF setup: {} entries built in {:?}", repo.index().unwrap().len(), setup_started.elapsed());
        let repo_string = repo_path.to_string_lossy().into_owned();

        // "Copied in from outside": 245 new files, not yet known to Git at all.
        let names: Vec<String> = (0..245).map(|i| format!("incoming_{i}.txt")).collect();
        for name in &names { fs::write(repo_path.join(name), "new content").unwrap(); }

        let stage_started = Instant::now();
        stage_files(repo_string.clone(), names.clone()).unwrap();
        println!("PERF stage_files (245 new): {:?}", stage_started.elapsed());

        // Delete 5 of the just-staged files straight from disk, then resync —
        // exactly what "Stage all" now does (sends the full set again, not
        // just the ones still marked unstaged).
        for name in &names[..5] { fs::remove_file(repo_path.join(name)).unwrap(); }
        let restage_started = Instant::now();
        stage_files(repo_string.clone(), names.clone()).unwrap();
        println!("PERF stage_files (resync after 5 deletions): {:?}", restage_started.elapsed());

        let commit_started = Instant::now();
        let oid = commit_staged_inner(repo_string.clone(), "Add 240 incoming files".into()).unwrap();
        println!("PERF commit_staged: {:?}", commit_started.elapsed());

        let repo = internal_repository(&repo_string).unwrap();
        let commit = repo.find_commit(git2::Oid::from_str(&oid).unwrap()).unwrap();
        let tree = commit.tree().unwrap();
        for name in &names[..5] { assert!(tree.get_path(Path::new(name)).is_err(), "{name} was deleted before commit and must not be in it"); }
        for name in &names[5..] { assert!(tree.get_path(Path::new(name)).is_ok(), "{name} should be in the commit"); }

        let head_tree = repo.head().unwrap().peel_to_commit().unwrap().tree().unwrap();
        let mut index = repo.index().unwrap();
        assert_eq!(index.write_tree().unwrap(), head_tree.id(), "index should equal HEAD with no leftover staged state");
        for name in &names[..5] { assert!(!repo_path.join(name).exists(), "deleted file should still be absent from the working tree"); }
    }

    // Isolates exactly the regression that was found and fixed: partition_by_submodule
    // used to call resolve_submodule_boundary once per file, which cloned the
    // *entire* tracked-paths HashSet (via cached_index_metadata) every single
    // time — for 245 files that's 245 full clones of the tracked set before any
    // real staging work even starts. This seeds the cache directly with a large
    // synthetic tracked set (no need to write 300,000 real files to disk just to
    // prove the point) and asserts partitioning 245 paths against it stays fast
    // — if the per-file clone ever comes back, this test's timing catches it
    // immediately instead of only showing up as "the real repo is slow" again.
    #[test]
    fn partition_by_submodule_does_not_reclone_metadata_per_file() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-partition-perf-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        let tracked: HashSet<String> = (0..300_000).map(|i| format!("synthetic_{i}.txt")).collect();
        index_metadata_cache().lock().unwrap().insert(repo_string.clone(), (Instant::now(), (Arc::new(tracked), Arc::new(HashSet::new()))));

        let files: Vec<String> = (0..245).map(|i| format!("incoming_{i}.txt")).collect();
        let started = Instant::now();
        let (own, grouped) = partition_by_submodule(&repo_string, files);
        let elapsed = started.elapsed();
        println!("PERF partition_by_submodule (300k tracked, 245 files): {elapsed:?}");

        assert_eq!(own.len(), 245, "none of these paths are inside a submodule, so all 245 should pass through unchanged");
        assert!(grouped.is_empty());
        assert!(elapsed.as_millis() < 500, "partition_by_submodule took {elapsed:?} for 245 files against a 300k-entry tracked set — looks like metadata is being cloned per file again, not once per call");
    }

    #[test]
    fn commit_path_accepts_a_folder_path_with_a_trailing_separator() {
        // Reported: committing a folder failed with libgit2's "invalid path"
        // even though the folder genuinely existed. Cause: a trailing '/' or
        // '\' (e.g. pasted from Windows Explorer's address bar) survives
        // untouched through Rust's Path into the pathspec string handed to
        // libgit2, which rejects a pathspec ending in a separator outright.
        for variant in ["work/asw/aggr/errm/agf/errm_fctdg_test/", "work/asw/aggr/errm/agf/errm_fctdg_test", "work/asw/aggr/errm/agf/errm_fctdg_test//", "work/asw/aggr/errm/agf/errm_fctdg_test\\"] {
            let repo_path = std::env::temp_dir().join(format!("git-integrity-trailing-slash-variant-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
            fs::create_dir_all(repo_path.join("work/asw/aggr/errm/agf/errm_fctdg_test")).unwrap();
            create_libgit2_repository(&repo_path, "README.md");
            fs::write(repo_path.join("work/asw/aggr/errm/agf/errm_fctdg_test/file.c"), "content").unwrap();
            fs::write(repo_path.join("outside_the_folder.txt"), "should not be committed").unwrap();
            let repo_string = repo_path.to_string_lossy().into_owned();
            stage_files(repo_string.clone(), vec!["work/asw/aggr/errm/agf/errm_fctdg_test/file.c".into(), "outside_the_folder.txt".into()]).unwrap();

            let oid = commit_path(repo_string.clone(), variant.into(), "Commit the test folder".into())
                .unwrap_or_else(|error| panic!("commit_path failed for variant {variant:?}: {error}"));
            let repo = internal_repository(&repo_string).unwrap();
            let commit = repo.find_commit(git2::Oid::from_str(&oid).unwrap()).unwrap();
            let tree = commit.tree().unwrap();
            assert!(tree.get_path(Path::new("work/asw/aggr/errm/agf/errm_fctdg_test/file.c")).is_ok(), "variant {variant:?}: the folder's own file should be committed");
            assert!(tree.get_path(Path::new("outside_the_folder.txt")).is_err(), "variant {variant:?}: a file outside the selected folder must not be committed alongside it");

            // The other staged file (outside the folder) must still be
            // staged afterward — a scoped commit must not touch it.
            let index = repo.index().unwrap();
            assert!(index.get_path(Path::new("outside_the_folder.txt"), 0).is_some(), "variant {variant:?}: the untouched staged file should remain staged");
        }
    }

    #[test]
    fn scoped_commit_consumes_only_selected_scope_and_preserves_other_staged_files() {
        let repo_path = std::env::temp_dir().join(format!("git-integrity-scoped-commit-stage-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(repo_path.join("scope")).unwrap();
        fs::write(repo_path.join("scope/a.txt"), "scope v1\n").unwrap();
        fs::write(repo_path.join("scope/deleted.txt"), "delete me\n").unwrap();
        fs::write(repo_path.join("other.txt"), "other v1\n").unwrap();
        create_libgit2_repository(&repo_path, "README.md");
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["add", "scope/a.txt", "scope/deleted.txt", "other.txt"]);
        run_git(&repo_path, &["commit", "-m", "Seed files"]);

        fs::write(repo_path.join("scope/a.txt"), "scope v2\n").unwrap();
        fs::write(repo_path.join("other.txt"), "other v2\n").unwrap();
        run_git(&repo_path, &["add", "scope/a.txt", "other.txt"]);

        // The selected folder changed again after staging; "Commit this item"
        // must behave like staging that folder for the scoped commit, while
        // temporarily keeping unrelated staged work out of the commit.
        fs::write(repo_path.join("scope/a.txt"), "scope v3\n").unwrap();
        fs::remove_file(repo_path.join("scope/deleted.txt")).unwrap();

        let repo_string = repo_path.to_string_lossy().into_owned();
        let oid = commit_path(repo_string.clone(), "scope".into(), "Commit only scope".into()).unwrap();
        let repo = internal_repository(&repo_string).unwrap();
        let commit = repo.find_commit(git2::Oid::from_str(&oid).unwrap()).unwrap();
        let tree = commit.tree().unwrap();
        let scoped_blob = repo.find_blob(tree.get_path(Path::new("scope/a.txt")).unwrap().id()).unwrap();
        let outside_blob = repo.find_blob(tree.get_path(Path::new("other.txt")).unwrap().id()).unwrap();
        assert_eq!(std::str::from_utf8(scoped_blob.content()).unwrap(), "scope v3\n", "the scoped commit should include the selected folder's current working-tree content");
        assert!(tree.get_path(Path::new("scope/deleted.txt")).is_err(), "tracked deletions inside the selected folder must be included in the scoped commit");
        assert_eq!(std::str::from_utf8(outside_blob.content()).unwrap(), "other v1\n", "unrelated staged work must not be included in the scoped commit");

        let cached = run_git_capture(&repo_path, &["diff", "--cached", "--name-only"]);
        assert_eq!(cached, "other.txt", "only the unrelated pre-existing staged file should remain staged after the scoped commit; selected-scope staged entries were consumed");
        assert_eq!(run_git_capture(&repo_path, &["diff", "--", "scope"]), "", "the selected scope should be clean after it was committed");

        fs::remove_dir_all(repo_path).unwrap();
    }

    // The real-world shape this is meant to catch: a huge monorepo (here,
    // simulated with a 300,000-entry tracked set seeded directly into the
    // cache rather than writing 300,000 real files to disk, which would make
    // this test itself impractically slow) where any *one* folder you
    // actually navigate into is small. Before the fix, tracked/unpushed were
    // collected into a fresh Vec and sorted from scratch on every single
    // load_directory call regardless of which folder — successive navigation
    // through different folders paid that O(total tracked) cost every time.
    #[test]
    fn load_directory_stays_fast_across_folders_with_a_300k_entry_index() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-nav-perf-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        let mut tracked: HashSet<String> = (0..300_000).map(|i| format!("elsewhere_{i}.txt")).collect();
        for folder in 0..20 {
            let dir = format!("folder_{folder}");
            fs::create_dir_all(repo_path.join(&dir)).unwrap();
            for file in 0..10 {
                let name = format!("{dir}/file_{file}.txt");
                fs::write(repo_path.join(&name), "x").unwrap();
                tracked.insert(name);
            }
        }
        index_metadata_cache().lock().unwrap().insert(repo_string.clone(), (Instant::now(), (Arc::new(tracked), Arc::new(HashSet::new()))));

        let mut total = Duration::ZERO;
        for folder in 0..20 {
            let started = Instant::now();
            let entries = load_directory_inner(repo_string.clone(), format!("folder_{folder}"), None).unwrap();
            total += started.elapsed();
            assert_eq!(entries.len(), 10, "folder_{folder} should list exactly the 10 real files created in it");
        }
        println!("PERF load_directory x20 folders (300k tracked): {total:?} total, {:?} avg", total / 20);
        // 2000ms flaked twice under full-suite parallel contention (this
        // machine's CPU/disk shared with every other test running at the
        // same time) despite the real number always being well under 700ms
        // in isolation — the actual regression this catches showed 3.3s+,
        // so a more generous bound still catches it without being flaky.
        assert!(total.as_millis() < 5000, "navigating 20 folders took {total:?} against a 300k-entry tracked set — looks like the sorted lookups are being rebuilt per navigation again instead of cached per repository");
    }

    // Ignored by default (writing 20,000 real files makes setup itself slow)
    // — the earlier navigation benchmark used 20 folders of 10 files each,
    // which never exercises a single *large* folder listing on its own.
    // This one folder has 20,000 direct entries, and the third call forces
    // the status-scan cache to look expired (rather than waiting the real
    // 4-second TTL) to measure the worst case: a fresh scoped status scan
    // plus the full readdir+sort pass, for the folder size this was
    // actually reported slow on.
    #[test]
    #[ignore]
    fn load_directory_handles_a_folder_with_20000_direct_entries() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-big-folder-{suffix}"));
        fs::create_dir_all(repo_path.join("big_folder")).unwrap();
        let repo = Repository::init(&repo_path).unwrap();

        let setup_started = Instant::now();
        for i in 0..20_000 { fs::write(repo_path.join("big_folder").join(format!("file_{i:05}.txt")), b"x").unwrap(); }
        {
            let mut index = repo.index().unwrap();
            index.add_all(["big_folder"], git2::IndexAddOption::DEFAULT, None).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "Add 20000 files", &tree, &[]).unwrap();
        }
        println!("PERF setup (20,000 files, committed): {:?}", setup_started.elapsed());
        let repo_string = repo_path.to_string_lossy().into_owned();

        let first = Instant::now();
        let entries = load_directory_inner(repo_string.clone(), "big_folder".into(), None).unwrap();
        println!("PERF load_directory first call (cold caches): {:?}", first.elapsed());
        assert_eq!(entries.len(), 20_000);
        assert!(entries.iter().all(|entry| entry.kind == "file" && entry.tracked));

        let second = Instant::now();
        let entries = load_directory_inner(repo_string.clone(), "big_folder".into(), None).unwrap();
        println!("PERF load_directory second call (warm caches): {:?}", second.elapsed());
        assert_eq!(entries.len(), 20_000);

        // Simulate the status-scan cache having expired (GIT_METADATA_TTL,
        // normally 4s) without actually waiting for it in this test.
        let key = metadata_cache_key(&repo_string, "big_folder");
        if let Some(entry) = metadata_cache().lock().unwrap().get_mut(&key) {
            entry.0 = Instant::now() - GIT_METADATA_TTL - Duration::from_secs(1);
        }
        let third = Instant::now();
        let entries = load_directory_inner(repo_string.clone(), "big_folder".into(), None).unwrap();
        println!("PERF load_directory third call (expired status cache, fresh scoped scan): {:?}", third.elapsed());
        assert_eq!(entries.len(), 20_000);
    }

    #[test]
    fn staging_a_folder_with_an_embedded_git_repo_fails_clearly_instead_of_invalid_path() {
        // Reproduces the report exactly: staging a directory that contains
        // its own .git (an unregistered embedded repository — copied in from
        // elsewhere, or created by another tool) used to fail deep inside
        // libgit2 with a bare "invalid path: '<dir>/'", with nothing telling
        // the user what was actually wrong or how to fix it.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-embedded-git-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        fs::create_dir_all(repo_path.join("embedded")).unwrap();
        run_git(&repo_path.join("embedded"), &["init"]);
        fs::write(repo_path.join("embedded/file.txt"), "content").unwrap();

        let error = stage_files(repo_string.clone(), vec!["embedded".into()]).unwrap_err();
        assert!(error.contains("embedded"), "error should name the problem path, got: {error}");
        assert!(error.contains(".git"), "error should explain *why*, not just fail, got: {error}");
        assert!(!error.contains("invalid path"), "should never surface libgit2's bare, unexplained error to the user, got: {error}");
    }

    #[test]
    fn stage_all_with_a_mix_of_ordinary_files_and_an_embedded_git_folder() {
        // The exact real-world shape: "Stage all" sends every changed path in
        // one call, most of them ordinary files, one of them a problem
        // folder — that one folder must produce a clear, actionable error,
        // not silently abort or corrupt the staging of everything else.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-mixed-stage-all-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        fs::write(repo_path.join("a.txt"), "a").unwrap();
        fs::write(repo_path.join("b.txt"), "b").unwrap();
        fs::create_dir_all(repo_path.join("embedded")).unwrap();
        run_git(&repo_path.join("embedded"), &["init"]);
        fs::write(repo_path.join("embedded/file.txt"), "content").unwrap();

        let error = stage_files(repo_string.clone(), vec!["a.txt".into(), "b.txt".into(), "embedded".into()]).unwrap_err();
        assert!(error.contains("embedded") && error.contains(".git"), "got: {error}");

        // Since this whole call errored out before ever writing the index,
        // the two ordinary files must be exactly as unstaged as before —
        // no partial, half-applied state.
        let repo = internal_repository(&repo_string).unwrap();
        let index = repo.index().unwrap();
        assert!(index.get_path(Path::new("a.txt"), 0).is_none(), "a.txt should not have been staged by a call that errored");
        assert!(index.get_path(Path::new("b.txt"), 0).is_none(), "b.txt should not have been staged by a call that errored");
    }

    #[test]
    fn utrud_command_uses_the_parent_as_cwd_and_the_selected_folder_as_the_argument() {
        // UTRUD is launched like Explorer's "Send to": cwd is the parent of
        // whatever folder the user clicked, while the argument is the full
        // selected folder path. This must work for any folder name, not only
        // a folder literally named "r"; UTRUD itself decides whether the
        // selected folder is meaningful for its workflow.
        let absolute = Path::new("/int_opm/sw-prj-OMBMS_000U0/work/asw/aggr/errm/agf/errm_envd1/any_folder");
        let (cwd, argument) = utrud_command_parts(absolute);
        assert_eq!(cwd, Path::new("/int_opm/sw-prj-OMBMS_000U0/work/asw/aggr/errm/agf/errm_envd1"));
        assert_eq!(argument, absolute);
        assert_ne!(cwd.file_name(), argument.file_name(), "cwd must not itself be the selected folder — that's what caused doubled folder paths");
    }

    #[test]
    fn stage_all_reflects_external_changes_without_any_prior_frontend_state() {
        // The full real-world scenario: open a repository, then — entirely
        // outside anything the app was told about — 245 files appear and a
        // few of them disappear again. refresh_status must see all of it
        // fresh (no reliance on any previously loaded state), and stage_all
        // must stage exactly what's really on disk right now, not whatever
        // an earlier snapshot said. Named for the specific bug: "Stage all"
        // used to be driven by the frontend's own (possibly stale)
        // state.changes; stage_all instead re-derives everything from a
        // fresh status scan every time.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-stage-all-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        // Nothing has ever staged/loaded anything for this repository yet —
        // no prior invoke of any kind — before the 245 files show up.
        let names: Vec<String> = (0..245).map(|i| format!("incoming_{i}.txt")).collect();
        for name in &names { fs::write(repo_path.join(name), "content").unwrap(); }
        for name in &names[..7] { fs::remove_file(repo_path.join(name)).unwrap(); }

        let statuses = refresh_status_inner(repo_string.clone()).unwrap();
        let untracked_new: Vec<&Change> = statuses.iter().filter(|c| names[7..].contains(&c.path) && c.status == "??").collect();
        assert_eq!(untracked_new.len(), 238, "the 238 files that still exist on disk should all show up as untracked (??)");
        assert!(statuses.iter().all(|c| !names[..7].contains(&c.path)), "a file that was created and then deleted before ever being staged shouldn't show up as a change at all — git never knew about it");

        let staged_count = stage_all_inner(&repo_string, "").unwrap();
        assert_eq!(staged_count.staged_paths.len(), 238, "stage_all should have processed exactly the 238 real, current files");
        assert!(staged_count.skipped_dirty_submodules.is_empty());

        let repo = internal_repository(&repo_string).unwrap();
        let index = repo.index().unwrap();
        for name in &names[7..] { assert!(index.get_path(Path::new(name), 0).is_some(), "{name} should be staged"); }
        for name in &names[..7] { assert!(index.get_path(Path::new(name), 0).is_none(), "{name} was deleted before ever being staged and must not appear in the index"); }

        let oid = commit_staged_inner(repo_string.clone(), "Add 238 incoming files".into()).unwrap();
        let commit = repo.find_commit(git2::Oid::from_str(&oid).unwrap()).unwrap();
        let tree = commit.tree().unwrap();
        for name in &names[7..] { assert!(tree.get_path(Path::new(name)).is_ok(), "{name} should be in the commit"); }
    }

    #[test]
    fn stage_all_ignores_stale_recent_status_and_catches_new_external_files() {
        // Reproduces the correctness side of the Windows report: a passive
        // refresh may have populated the short reuse cache, then a file is
        // added from outside the app, and Stage all must still discover and
        // stage that new file. Stage all is a mutating command, so it must
        // take a fresh status snapshot instead of trusting the recent-cache
        // optimization used by passive refreshes.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-status-reuse-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        let fake_marker = "__unmistakably_fake_marker__.txt".to_string();
        full_status_cache().lock().unwrap().insert(repo_string.clone(), (Instant::now(), vec![(fake_marker.clone(), "??".into(), false)]));

        let changes = refresh_status_inner(repo_string.clone()).unwrap();
        assert_eq!(changes.len(), 1, "should have reused the seeded entry, not scanned the (actually empty) real repository");
        assert_eq!(changes[0].path, fake_marker);

        fs::write(repo_path.join("external-new-file.txt"), "created outside the cached status").unwrap();

        let staged = stage_all_inner(&repo_string, "").unwrap();
        assert_eq!(staged.staged_paths, vec!["external-new-file.txt".to_string()], "stage_all must ignore stale recent status and stage the real new file");

        let changes_after_expiry = refresh_status_inner(repo_string.clone()).unwrap();
        assert_eq!(changes_after_expiry.len(), 1);
        assert_eq!(changes_after_expiry[0].path, "external-new-file.txt");
        assert_eq!(changes_after_expiry[0].status, "A");
        assert!(changes_after_expiry[0].staged);
    }

    #[test]
    fn restoring_a_staged_new_file_absent_from_head_discards_it_completely() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-restore-new-file-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        fs::write(repo_path.join("new-file.txt"), "local only").unwrap();
        stage_files_inner(&repo_string, vec!["new-file.txt".into()]).unwrap();
        fs::remove_file(repo_path.join("new-file.txt")).unwrap();
        assert!(refresh_status_inner(repo_string.clone()).unwrap().iter().any(|change| change.path == "new-file.txt"));

        restore_file(repo_string.clone(), "new-file.txt".into(), "HEAD".into()).unwrap();

        assert!(!repo_path.join("new-file.txt").exists(), "a file absent from HEAD must not be recreated");
        let status = refresh_status_inner(repo_string.clone()).unwrap();
        assert!(!status.iter().any(|change| change.path == "new-file.txt"), "restoring a staged new file absent from HEAD must remove both the worktree file and the staged add");
        let cached = load_directory_inner(repo_string, "".into(), Some(true)).unwrap();
        assert!(!cached.iter().any(|entry| entry.relative_path == "new-file.txt"), "Explorer must not keep showing the discarded local-only file as tracked");
    }

    fn setup_parent_with_two_submodules(base: &Path) -> (String, PathBuf, PathBuf) {
        let parent = base.join("parent"); let dep_a = base.join("dep-a"); let dep_b = base.join("dep-b");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dep_a, "a.txt");
        create_libgit2_repository(&dep_b, "b.txt");
        run_git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", dep_a.to_str().unwrap(), "vendor/a"]);
        run_git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", dep_b.to_str().unwrap(), "vendor/b"]);
        run_git(&parent, &["commit", "-am", "Add two submodules"]);
        let parent_string = parent.to_string_lossy().into_owned();
        (parent_string, parent.join("vendor/a"), parent.join("vendor/b"))
    }

    #[test]
    fn stage_files_skips_a_submodule_that_is_only_dirty_inside_head_unchanged() {
        // Reproduces the confirmed Mac log exactly: a submodule with
        // uncommitted internal changes (its own working tree dirty) but a
        // HEAD that still matches what the parent already has recorded.
        // add_to_index used to write back the exact same SHA that was
        // already there — a real no-op that git2 doesn't error on — and the
        // caller counted it as staged anyway. Now it must be reported as
        // skipped, with nothing added to the index, and the parent's own
        // "modified" status for that submodule (from its dirty content, not
        // from a pointer change) must stay exactly as it was.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-stage-dirty-submodule-{suffix}"));
        let (parent_string, sub_a, _sub_b) = setup_parent_with_two_submodules(&base);

        // Dirty the submodule's own working tree WITHOUT committing — HEAD stays put.
        fs::write(sub_a.join("a.txt"), "uncommitted local edit").unwrap();
        let recorded_before = Repository::open(&parent_string).unwrap().index().unwrap().get_path(Path::new("vendor/a"), 0).unwrap().id;

        let result = stage_files(parent_string.clone(), vec!["vendor/a".into()]).unwrap();
        assert!(result.staged_paths.is_empty(), "a submodule with an unchanged HEAD must not be reported as staged, got: {result:?}");
        assert_eq!(result.skipped_dirty_submodules, vec!["vendor/a".to_string()], "must be reported as skipped instead");

        let recorded_after = Repository::open(&parent_string).unwrap().index().unwrap().get_path(Path::new("vendor/a"), 0).unwrap().id;
        assert_eq!(recorded_before, recorded_after, "the parent's recorded gitlink pointer must not have changed at all");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn stage_files_stages_a_submodule_whose_head_actually_moved() {
        // The real, meaningful case: the submodule's HEAD genuinely differs
        // from what the parent has recorded (a real local commit inside it,
        // clean working tree) — this IS something to stage, and must be
        // counted as such.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-stage-moved-submodule-{suffix}"));
        let (parent_string, sub_a, _sub_b) = setup_parent_with_two_submodules(&base);

        fs::write(sub_a.join("a.txt"), "v2, committed").unwrap();
        run_git(&sub_a, &["commit", "-am", "Advance the submodule"]);
        let new_head = Repository::open(&sub_a).unwrap().head().unwrap().target().unwrap();

        let result = stage_files(parent_string.clone(), vec!["vendor/a".into()]).unwrap();
        assert_eq!(result.staged_paths, vec!["vendor/a".to_string()], "a submodule whose HEAD genuinely moved must be reported as staged, got: {result:?}");
        assert!(result.skipped_dirty_submodules.is_empty());

        let recorded = Repository::open(&parent_string).unwrap().index().unwrap().get_path(Path::new("vendor/a"), 0).unwrap().id;
        assert_eq!(recorded, new_head, "the parent's index must now record the submodule's new HEAD");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn stage_all_reports_a_real_mixed_count_not_paths_requested() {
        // Stage All across a mix: one ordinary file, one submodule that
        // genuinely advanced (real stage), and one submodule that's only
        // dirty inside with an unchanged HEAD (must be skipped, not staged).
        // The old behavior reported paths.len() — every path stage_all
        // *asked about* — regardless of what actually landed in the index;
        // this must report the real, true count instead.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-stage-all-mixed-{suffix}"));
        let (parent_string, sub_a, sub_b) = setup_parent_with_two_submodules(&base);
        let parent = Path::new(&parent_string);

        fs::write(parent.join("plain.txt"), "new file").unwrap();
        fs::write(sub_a.join("a.txt"), "v2, committed").unwrap();
        run_git(&sub_a, &["commit", "-am", "Advance submodule a"]);
        fs::write(sub_b.join("b.txt"), "uncommitted, HEAD unchanged").unwrap();

        let result = stage_all_inner(&parent_string, "").unwrap();
        let staged: std::collections::HashSet<_> = result.staged_paths.iter().cloned().collect();
        assert_eq!(staged, ["plain.txt".to_string(), "vendor/a".to_string()].into_iter().collect(), "exactly the real file and the submodule that actually advanced, got: {result:?}");
        assert_eq!(result.skipped_dirty_submodules, vec!["vendor/b".to_string()], "the merely-dirty submodule must be reported as skipped, not silently counted as staged");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn open_directory_works_before_any_load_repository_call_at_all() {
        // The core guarantee open_repository_fast's whole design depends on:
        // load_directory is fully self-sufficient and doesn't need
        // load_repository (or open_repository_fast) to have run first at
        // all — navigating a repository that's had *no* prior load of any
        // kind must just work, with correct tracked/status information,
        // exactly as if a full load_repository had already primed the
        // caches. This is what makes "show structure, fetch status in the
        // background" safe: the frontend can let the user click into a
        // folder before the background status fetch finishes and still see
        // correct data, not placeholder/stale data.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-fast-open-nav-{suffix}"));
        fs::create_dir_all(repo_path.join("folder")).unwrap();
        create_libgit2_repository(&repo_path, "README.md");
        fs::write(repo_path.join("folder/new_file.txt"), "content").unwrap();
        let repo_string = repo_path.to_string_lossy().into_owned();

        let entries = load_directory_inner(repo_string.clone(), "folder".into(), None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "??", "a brand-new file must show as untracked even though nothing has loaded this repository before");
        assert!(!entries[0].tracked);
    }

    // Ignored by default (creating many real submodules is itself slow) — a
    // "combined stress" case: many submodules plus a real, moderately large
    // tracked file set, measuring open_repository_fast against the existing
    // load_repository on the *same*, unmutated repository. Run explicitly
    // with `cargo test --release -- --ignored open_repository_fast_is_much_faster`.
    #[test]
    #[ignore]
    fn open_repository_fast_is_much_faster_than_load_repository_on_a_combined_stress_repo() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-combined-stress-{suffix}"));
        let parent = base.join("parent");
        create_libgit2_repository(&parent, "README.md");
        let parent_string = parent.to_string_lossy().into_owned();

        let setup_started = Instant::now();
        for dir in 0..50 {
            let dir_path = parent.join(format!("existing_{dir}"));
            fs::create_dir_all(&dir_path).unwrap();
            for file in 0..100 { fs::write(dir_path.join(format!("f{file}.txt")), b"x").unwrap(); }
        }
        stage_all_inner(&parent_string, "").unwrap();
        commit_staged_inner(parent_string.clone(), "Add 5000 tracked files".into()).unwrap();
        for i in 0..30 {
            let dependency = base.join(format!("dep_{i}"));
            create_libgit2_repository(&dependency, "module.txt");
            add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), format!("sub_{i}"), String::new(), String::new()).unwrap();
        }
        commit_staged_inner(parent_string.clone(), "Add 30 submodules".into()).unwrap();
        println!("PERF combined-stress setup (5000 files, 30 submodules): {:?}", setup_started.elapsed());

        // Neither call below mutates the repository — pure reads, back to back.
        let fast_started = Instant::now();
        let fast = open_repository_fast_inner(parent_string.clone()).unwrap();
        let fast_elapsed = fast_started.elapsed();
        println!("PERF open_repository_fast: {fast_elapsed:?} ({} branches, {} commits)", fast.branches.len(), fast.commits.len());

        let full_started = Instant::now();
        let full = load_repository_inner(parent_string.clone(), Some(true)).unwrap();
        let full_elapsed = full_started.elapsed();
        println!("PERF load_repository (force, same repo, no mutation between calls): {full_elapsed:?} ({} branches, {} commits, {} changes)", full.branches.len(), full.commits.len(), full.changes.len());

        println!("PERF speedup: open_repository_fast was {:.1}x faster", full_elapsed.as_secs_f64() / fast_elapsed.as_secs_f64().max(0.0001));
        assert!(fast_elapsed < full_elapsed, "open_repository_fast ({fast_elapsed:?}) should be faster than load_repository ({full_elapsed:?}) by skipping the full status scan");
    }

    // Isolated from the test above deliberately (a fresh repository, not
    // reusing its cache state) — this specifically verifies the actual
    // frontend flow's acceptance criteria: the root is visible near-
    // instantly via list_directory_fast (no Git calls at all), and the
    // *single* real status scan that follows (refresh_status) is the only
    // one — a subsequent load_directory for the same repository must reuse
    // it, not run a second one. Read-only throughout; the combined-stress
    // repository itself is never mutated by this test.
    #[test]
    #[ignore]
    fn combined_stress_root_visible_fast_with_exactly_one_status_scan() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-combined-stress-flow-{suffix}"));
        let parent = base.join("parent");
        create_libgit2_repository(&parent, "README.md");
        let parent_string = parent.to_string_lossy().into_owned();

        for dir in 0..50 {
            let dir_path = parent.join(format!("existing_{dir}"));
            fs::create_dir_all(&dir_path).unwrap();
            for file in 0..100 { fs::write(dir_path.join(format!("f{file}.txt")), b"x").unwrap(); }
        }
        stage_all_inner(&parent_string, "").unwrap();
        commit_staged_inner(parent_string.clone(), "Add 5000 tracked files".into()).unwrap();
        for i in 0..30 {
            let dependency = base.join(format!("dep_{i}"));
            create_libgit2_repository(&dependency, "module.txt");
            add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), format!("sub_{i}"), String::new(), String::new()).unwrap();
        }
        commit_staged_inner(parent_string.clone(), "Add 30 submodules".into()).unwrap();

        // 1. Root visible near-instantly, zero Git calls.
        let fast_started = Instant::now();
        let fast = open_repository_fast_inner(parent_string.clone()).unwrap();
        let fast_list_started = Instant::now();
        let root_entries = list_directory_fast_inner(fast.repository.path.clone(), String::new()).unwrap();
        let fast_total = fast_started.elapsed();
        println!("PERF open_repository_fast + list_directory_fast (root): {fast_total:?} ({} entries)", root_entries.len());
        assert!(root_entries.iter().all(|entry| !entry.status_known), "list_directory_fast entries must be marked status-unknown, never clean/untracked");
        assert!(fast_total.as_millis() < 200, "root should be visible in under 200ms, took {fast_total:?}");
        let _ = fast_list_started;

        // 2. The one and only real full status scan.
        let scan_started = Instant::now();
        let changes = refresh_status_inner(fast.repository.path.clone()).unwrap();
        let scan_elapsed = scan_started.elapsed();
        println!("PERF refresh_status (the one real scan): {scan_elapsed:?} ({} changes)", changes.len());
        assert!(changes.is_empty(), "a freshly committed combined-stress repo should be clean");

        // 3. Reloading the same (root) folder now must reuse that scan, not
        // run a second one — proven the same way as the earlier reuse test:
        // if it reused the cache, this stays fast; a real second scan on
        // 5000 files/30 submodules would be measurably slower.
        let reload_started = Instant::now();
        let real_entries = load_directory_inner(fast.repository.path.clone(), String::new(), None).unwrap();
        let reload_elapsed = reload_started.elapsed();
        println!("PERF load_directory (root, reusing refresh_status's scan): {reload_elapsed:?} ({} entries)", real_entries.len());
        assert!(real_entries.iter().all(|entry| entry.status_known), "the real load_directory must always report status_known");
        assert!(reload_elapsed < scan_elapsed, "reloading the same folder right after refresh_status ({reload_elapsed:?}) should be much faster than the real scan ({scan_elapsed:?}) it reuses, not run a second one");
    }

    #[test]
    fn tokenize_git_args_respects_quotes_with_spaces() {
        // The actual reported bug: split_whitespace on `commit -m "fix bug
        // in parser"` produced 6 bogus arguments instead of the 3 a real
        // shell would.
        assert_eq!(
            tokenize_git_args(r#"commit -m "fix bug in parser""#).unwrap(),
            vec!["commit", "-m", "fix bug in parser"]
        );
        assert_eq!(tokenize_git_args("status").unwrap(), vec!["status"]);
        assert_eq!(tokenize_git_args("log --oneline -10").unwrap(), vec!["log", "--oneline", "-10"]);
        assert_eq!(tokenize_git_args("checkout 'my branch'").unwrap(), vec!["checkout", "my branch"]);
        assert_eq!(tokenize_git_args(r#"commit -m "say \"hi\"""#).unwrap(), vec!["commit", "-m", "say \"hi\""]);
        assert_eq!(tokenize_git_args("  status   --short  ").unwrap(), vec!["status", "--short"]);
        assert_eq!(tokenize_git_args("").unwrap(), Vec::<String>::new());
        assert!(tokenize_git_args(r#"commit -m "unclosed"#).is_err(), "an unclosed quote should be a clear error, not silently mis-split");
    }

    #[test]
    fn read_only_git_subcommand_allowlist_is_strict() {
        for allowed in ["status", "log", "diff", "show", "blame", "ls-files"] {
            assert!(is_read_only_git_subcommand(allowed), "{allowed} should be read-only");
        }
        // Conservative by design: anything not explicitly listed is treated
        // as possibly mutating, even a command that's usually read-only in
        // practice (e.g. `remote -v`) — a missed reload is a worse bug than
        // an unnecessary one.
        for not_allowed in ["remote", "commit", "checkout", "reset", "push", "pull", "branch", "fetch", "stash", "rebase", "merge", "tag"] {
            assert!(!is_read_only_git_subcommand(not_allowed), "{not_allowed} should NOT be treated as read-only");
        }
    }

    #[test]
    fn terminal_read_only_classification_rejects_compound_or_redirected_commands() {
        for allowed in ["git status --short", "git log --oneline -3", "pwd", "ls", "dir"] {
            assert!(is_definitely_read_only_terminal_command(allowed), "{allowed} should be safely read-only");
        }
        for mutating_or_ambiguous in [
            "git remote add origin somewhere",
            "git status && touch changed.txt",
            "ls > listing.txt",
            "pwd; rm file.txt",
            "echo $(touch changed.txt)",
            "gh pr status",
        ] {
            assert!(!is_definitely_read_only_terminal_command(mutating_or_ambiguous), "{mutating_or_ambiguous} must trigger a refresh");
        }
    }

    #[test]
    fn run_git_command_reports_exit_code_and_read_only_classification() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-console-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        let status_result = run_git_command(repo_string.clone(), "status".into()).unwrap();
        assert!(status_result.success);
        assert_eq!(status_result.exit_code, Some(0));
        assert!(status_result.read_only, "status must be classified read-only");

        // A quoted commit message with spaces — the exact reported bug —
        // must reach git as one argument, not get mangled into several.
        fs::write(repo_path.join("new_file.txt"), "content").unwrap();
        stage_files(repo_string.clone(), vec!["new_file.txt".into()]).unwrap();
        let commit_result = run_git_command(repo_string.clone(), r#"commit -m "a message with spaces""#.into()).unwrap();
        assert!(commit_result.success, "stdout={} stderr={}", commit_result.stdout, commit_result.stderr);
        assert_eq!(commit_result.exit_code, Some(0));
        assert!(!commit_result.read_only, "commit must NOT be classified read-only");

        let repo = internal_repository(&repo_string).unwrap();
        let subject = repo.head().unwrap().peel_to_commit().unwrap().summary().unwrap_or("").to_string();
        assert_eq!(subject, "a message with spaces", "the quoted message must have reached git intact, not split into separate bogus arguments");

        // A subcommand that fails should still report its real exit code, not error out.
        let bad_result = run_git_command(repo_string.clone(), "log --this-flag-does-not-exist".into()).unwrap();
        assert!(!bad_result.success);
        assert_ne!(bad_result.exit_code, Some(0));
        assert!(bad_result.read_only, "log must be classified read-only regardless of whether the specific invocation succeeded");
    }

    // ---- Structured refs / graph correctness (History & Branch Map rework) ----

    #[test]
    fn lightweight_and_annotated_tags_both_resolve_to_the_commit_they_actually_point_at() {
        // The exact bug this was rewritten for: reference.target() on an
        // annotated tag's ref returns the tag *object's* own oid, not the
        // commit's — collect_ref_seeds_and_badges must use .peel(Commit)
        // instead, so both tag kinds land on the real commit's row.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-tag-peel-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        run_git(&base, &["commit", "--allow-empty", "-m", "second"]);
        let head = run_git_capture(&base, &["rev-parse", "HEAD"]);
        run_git(&base, &["tag", "v1-lightweight"]);
        run_git(&base, &["tag", "-a", "v1-annotated", "-m", "release notes"]);
        let path = base.to_string_lossy().into_owned();

        let data = load_repository_inner(path, None).unwrap();
        let head_commit = data.commits.iter().find(|commit| commit.id == head).expect("HEAD's own commit must be in the loaded page");
        let tag_names: Vec<&str> = head_commit.refs.iter().filter(|r| r.kind == "tag").map(|r| r.name.as_str()).collect();
        assert!(tag_names.contains(&"v1-lightweight"), "lightweight tag missing from its own commit, got {tag_names:?}");
        assert!(tag_names.contains(&"v1-annotated"), "annotated tag missing from its own commit (likely still keyed under the tag object's own oid, not the commit's) — got {tag_names:?}");
        assert!(!data.commits.iter().any(|commit| commit.id != head && commit.refs.iter().any(|r| r.kind == "tag")), "neither tag must appear on any other commit");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn several_tags_on_the_same_commit_all_appear_together() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-multi-tag-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        let head = run_git_capture(&base, &["rev-parse", "HEAD"]);
        for name in ["v1.0.0", "v1.0.1-rc1", "release/2024-01"] { run_git(&base, &["tag", "-a", name, "-m", "note"]); }
        let path = base.to_string_lossy().into_owned();

        let data = load_repository_inner(path, None).unwrap();
        let head_commit = data.commits.iter().find(|commit| commit.id == head).unwrap();
        let tag_names: HashSet<&str> = head_commit.refs.iter().filter(|r| r.kind == "tag").map(|r| r.name.as_str()).collect();
        assert_eq!(tag_names, HashSet::from(["v1.0.0", "v1.0.1-rc1", "release/2024-01"]), "all three tags on the same commit must all be attached, none dropped");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_tag_on_a_commit_outside_the_first_page_still_attaches_once_that_page_loads() {
        // Same real-truncation technique as load_repository_truncates_at_the_
        // graph_commit_window_and_load_older_continues_past_it: a genuine
        // chain longer than GRAPH_COMMIT_WINDOW, tagging the very oldest
        // (definitely-not-on-the-first-page) commit.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-tag-older-page-{suffix}"));
        fs::create_dir_all(&base).unwrap();
        let repo = Repository::init(&base).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        let total = GRAPH_COMMIT_WINDOW + 5;
        let mut last_commit: Option<git2::Oid> = None;
        let mut root_oid = None;
        for i in 0..total {
            fs::write(base.join("file.txt"), format!("{i}")).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("file.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let parents: Vec<git2::Commit> = last_commit.map(|oid| repo.find_commit(oid).unwrap()).into_iter().collect();
            let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
            let oid = repo.commit(Some("HEAD"), &signature, &signature, &format!("commit {i}"), &tree, &parent_refs).unwrap();
            if i == 0 { root_oid = Some(oid); }
            last_commit = Some(oid);
        }
        let root_oid = root_oid.unwrap();
        repo.tag_lightweight("root-tag", &repo.find_object(root_oid, None).unwrap(), false).unwrap();
        drop(repo);
        let repo_path = base.to_string_lossy().into_owned();

        let first_page = load_repository_inner(repo_path.clone(), None).unwrap();
        assert!(!first_page.commits.iter().any(|commit| commit.id == root_oid.to_string()), "sanity check: the tagged root must genuinely be outside the first page");
        assert!(!first_page.commits.iter().any(|commit| commit.refs.iter().any(|r| r.name == "root-tag")), "the tag must not appear anywhere on the first page — it isn't loaded yet");

        let oldest_in_window = first_page.commits.last().unwrap().id.clone();
        let older = load_older_commits(repo_path, oldest_in_window, Some(500)).unwrap();
        let root_row = older.commits.iter().find(|commit| commit.id == root_oid.to_string()).expect("the root commit must be on the older page");
        assert!(root_row.refs.iter().any(|r| r.name == "root-tag" && r.kind == "tag"), "the tag must attach correctly once its own page actually loads");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_local_branch_and_its_remote_tracking_branch_on_the_same_commit_both_appear() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repository = std::env::temp_dir().join(format!("git-integrity-local-remote-samecommit-{suffix}"));
        let remote = std::env::temp_dir().join(format!("git-integrity-local-remote-samecommit-remote-{suffix}.git"));
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare"]);
        run_git(&repository, &["init", "-b", "main"]);
        run_git(&repository, &["config", "user.email", "test@example.com"]);
        run_git(&repository, &["config", "user.name", "Test User"]);
        fs::write(repository.join("a.txt"), "one").unwrap();
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-m", "Commit 1"]);
        run_git(&repository, &["remote", "add", "origin", remote.to_str().unwrap()]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "push", "origin", "HEAD:main"]);
        run_git(&repository, &["-c", "protocol.file.allow=always", "fetch", "origin"]);
        let head = run_git_capture(&repository, &["rev-parse", "HEAD"]);
        let path = repository.to_string_lossy().into_owned();

        let data = load_repository_inner(path, None).unwrap();
        let head_commit = data.commits.iter().find(|commit| commit.id == head).unwrap();
        let labels: Vec<(&str, &str)> = head_commit.refs.iter().map(|r| (r.name.as_str(), r.kind.as_str())).collect();
        assert!(head_commit.refs.iter().any(|r| r.kind == "local_branch" && r.name == "main"), "local branch missing, got {labels:?}");
        assert!(head_commit.refs.iter().any(|r| r.kind == "remote_branch" && r.name == "origin/main"), "remote-tracking branch missing, got {labels:?}");

        fs::remove_dir_all(&repository).unwrap();
        fs::remove_dir_all(&remote).unwrap();
    }

    #[test]
    fn a_merge_commits_parents_are_exactly_gits_own_recorded_list_in_order() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-merge-parents-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        run_git(&base, &["branch", "-M", "main"]);
        run_git(&base, &["checkout", "-b", "feature"]);
        fs::write(base.join("feature.txt"), "x").unwrap();
        run_git(&base, &["add", "feature.txt"]);
        run_git(&base, &["commit", "-m", "feature work"]);
        run_git(&base, &["checkout", "main"]);
        run_git(&base, &["merge", "--no-ff", "-m", "Merge feature", "feature"]);
        let merge_oid = run_git_capture(&base, &["rev-parse", "HEAD"]);
        let expected_parents: Vec<String> = run_git_capture(&base, &["log", "-1", "--format=%P", "HEAD"]).split_whitespace().map(str::to_string).collect();
        assert_eq!(expected_parents.len(), 2, "sanity check on the fixture itself — this must really be a two-parent merge");
        let path = base.to_string_lossy().into_owned();

        let data = load_repository_inner(path, None).unwrap();
        let merge_commit = data.commits.iter().find(|commit| commit.id == merge_oid).unwrap();
        assert_eq!(&merge_commit.parents, &expected_parents, "a merge's parents must be exactly git's own recorded list, in the same (first-parent-first) order");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn independent_orphan_histories_appear_with_no_fabricated_relationship_between_them() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-orphan-roots-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        run_git(&base, &["branch", "-M", "main"]);
        let first_root = run_git_capture(&base, &["rev-parse", "HEAD"]);
        run_git(&base, &["checkout", "--orphan", "second-root"]);
        run_git(&base, &["rm", "-rf", "--cached", "."]);
        fs::remove_file(base.join("README.md")).ok();
        fs::write(base.join("other.txt"), "x").unwrap();
        run_git(&base, &["add", "other.txt"]);
        run_git(&base, &["commit", "-m", "second root"]);
        let second_root = run_git_capture(&base, &["rev-parse", "HEAD"]);
        assert_ne!(first_root, second_root, "sanity check on the fixture itself");
        let path = base.to_string_lossy().into_owned();

        let data = load_repository_inner(path, None).unwrap();
        let a = data.commits.iter().find(|commit| commit.id == first_root).expect("first root must be loaded (still reachable via the 'main' branch ref)");
        let b = data.commits.iter().find(|commit| commit.id == second_root).expect("second, unrelated root must be loaded too (reachable via 'second-root')");
        assert!(a.parents.is_empty(), "the first root truly has no parent");
        assert!(b.parents.is_empty(), "the second, independent root truly has no parent either");
        assert!(!b.parents.contains(&a.id) && !a.parents.contains(&b.id), "two unrelated histories must never be linked to each other");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_commits_real_parent_id_is_preserved_even_when_that_parent_lands_on_the_next_page() {
        // The backend must never rewrite or drop a parent id just because
        // that parent hasn't been paginated into view yet — showing a
        // "continues in older history" stub for it is the *frontend's* job
        // (buildGraphModel/graph-model.js), never something the backend
        // should pre-empt by lying about parentage.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-parent-next-page-{suffix}"));
        fs::create_dir_all(&base).unwrap();
        let repo = Repository::init(&base).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        let total = GRAPH_COMMIT_WINDOW + 5;
        let mut last_commit: Option<git2::Oid> = None;
        for i in 0..total {
            fs::write(base.join("file.txt"), format!("{i}")).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("file.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let parents: Vec<git2::Commit> = last_commit.map(|oid| repo.find_commit(oid).unwrap()).into_iter().collect();
            let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
            let oid = repo.commit(Some("HEAD"), &signature, &signature, &format!("commit {i}"), &tree, &parent_refs).unwrap();
            last_commit = Some(oid);
        }
        drop(repo);
        let repo_path = base.to_string_lossy().into_owned();

        let first_page = load_repository_inner(repo_path.clone(), None).unwrap();
        let oldest = first_page.commits.last().unwrap();
        assert_eq!(oldest.parents.len(), 1, "sanity check on the fixture — this is a plain linear chain");
        let missing_parent = oldest.parents[0].clone();
        assert!(!first_page.commits.iter().any(|commit| commit.id == missing_parent), "sanity check: the parent really is outside this page");
        let real_parent = run_git_capture(&base, &["log", "-1", "--format=%P", &oldest.id]);
        assert_eq!(missing_parent, real_parent, "the parent id must be git's own real parent, unchanged, even though it is not loaded yet");

        let older = load_older_commits(repo_path, oldest.id.clone(), Some(500)).unwrap();
        assert!(older.commits.iter().any(|commit| commit.id == missing_parent), "the referenced parent must actually exist once its own page loads");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn two_submodules_with_identically_named_branches_and_tags_never_mix_their_ref_data() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-ref-collision-{suffix}"));
        let parent = base.join("parent"); let dep_a = base.join("dep_a"); let dep_b = base.join("dep_b");
        create_libgit2_repository(&parent, "README.md");
        // Different filenames, not just "two separate directories" — same
        // filename + same content + signatures minted in the same wall-clock
        // second (very likely, two calls apart) would hash to the exact
        // same root commit oid, which would make dep_b's *own real* root
        // commit collide with dep_a's, defeating the "never mix" assertions
        // below for a reason that has nothing to do with the mechanism
        // actually under test.
        create_libgit2_repository(&dep_a, "module_a.txt");
        create_libgit2_repository(&dep_b, "module_b.txt");
        for dep in [&dep_a, &dep_b] { run_git(dep, &["branch", "-M", "main"]); }

        // Same branch name ("release") and same tag name ("v1.0") in both
        // submodules — deliberately pointing at *different* commits.
        run_git(&dep_a, &["checkout", "-b", "release"]);
        run_git(&dep_a, &["tag", "-a", "v1.0", "-m", "a's release"]);
        let dep_a_release_commit = run_git_capture(&dep_a, &["rev-parse", "release"]);
        run_git(&dep_a, &["checkout", "main"]);

        fs::write(dep_b.join("module_b.txt"), "second").unwrap();
        run_git(&dep_b, &["commit", "-am", "second commit"]);
        run_git(&dep_b, &["checkout", "-b", "release"]);
        run_git(&dep_b, &["tag", "-a", "v1.0", "-m", "b's release"]);
        let dep_b_release_commit = run_git_capture(&dep_b, &["rev-parse", "release"]);
        run_git(&dep_b, &["checkout", "main"]);
        assert_ne!(dep_a_release_commit, dep_b_release_commit, "sanity check: the two same-named tags/branches must genuinely point at different commits");

        let parent_string = parent.to_string_lossy().into_owned();
        let added_a = add_submodule_inner(parent_string.clone(), "".into(), dep_a.to_string_lossy().into_owned(), "dep-a".into(), String::new(), String::new()).unwrap();
        let added_b = add_submodule_inner(parent_string.clone(), "".into(), dep_b.to_string_lossy().into_owned(), "dep-b".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add both sibling submodules".into()).unwrap();

        let data_a = submodule_repository_inner(parent_string.clone(), added_a).unwrap();
        let data_b = submodule_repository_inner(parent_string, added_b).unwrap();

        // add_submodule clones the dependency the same way a plain `git
        // submodule add` does — the branch checked out at clone time
        // ("main") becomes a local branch, but any *other* branch (like
        // "release") only survives as that clone's own remote-tracking ref
        // ("origin/release"), never as a same-named local branch. Tags
        // always survive as plain local tags either way, which is the
        // actual point of this fixture — that same-named branch/tag pair
        // still never gets mixed up between the two sibling submodules.
        let a_release = data_a.commits.iter().find(|commit| commit.id == dep_a_release_commit).expect("dep-a's own release commit must be present");
        assert!(a_release.refs.iter().any(|r| r.name == "origin/release" && r.kind == "remote_branch"), "got {:?}", a_release.refs.iter().map(|r| (&r.name, &r.kind)).collect::<Vec<_>>());
        assert!(a_release.refs.iter().any(|r| r.name == "v1.0" && r.kind == "tag"));
        assert!(!data_a.commits.iter().any(|commit| commit.id == dep_b_release_commit), "dep-a's own commit list must never contain dep-b's commit at all");

        let b_release = data_b.commits.iter().find(|commit| commit.id == dep_b_release_commit).expect("dep-b's own release commit must be present");
        assert!(b_release.refs.iter().any(|r| r.name == "origin/release" && r.kind == "remote_branch"), "got {:?}", b_release.refs.iter().map(|r| (&r.name, &r.kind)).collect::<Vec<_>>());
        assert!(b_release.refs.iter().any(|r| r.name == "v1.0" && r.kind == "tag"));
        assert!(!data_b.commits.iter().any(|commit| commit.id == dep_a_release_commit), "dep-b's own commit list must never contain dep-a's commit at all");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn every_returned_commits_parents_are_exactly_its_real_git2_parent_ids_no_more_no_fewer() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-edge-fidelity-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        run_git(&base, &["branch", "-M", "main"]);
        run_git(&base, &["checkout", "-b", "feature-1"]);
        fs::write(base.join("f1.txt"), "x").unwrap(); run_git(&base, &["add", "f1.txt"]); run_git(&base, &["commit", "-m", "f1"]);
        run_git(&base, &["checkout", "main"]);
        run_git(&base, &["checkout", "-b", "feature-2"]);
        fs::write(base.join("f2.txt"), "x").unwrap(); run_git(&base, &["add", "f2.txt"]); run_git(&base, &["commit", "-m", "f2"]);
        run_git(&base, &["checkout", "main"]);
        run_git(&base, &["merge", "--no-ff", "-m", "merge f1", "feature-1"]);
        run_git(&base, &["merge", "--no-ff", "-m", "merge f2", "feature-2"]);
        let path = base.to_string_lossy().into_owned();
        let ground_truth_repo = Repository::open(&base).unwrap();

        let data = load_repository_inner(path, None).unwrap();
        assert!(data.commits.len() >= 5, "sanity check: the fixture must actually be this dense");
        for commit in &data.commits {
            let oid = git2::Oid::from_str(&commit.id).unwrap();
            let real_parents: Vec<String> = ground_truth_repo.find_commit(oid).unwrap().parent_ids().map(|id| id.to_string()).collect();
            assert_eq!(&commit.parents, &real_parents, "commit {}: parents must be exactly git2's own parent_ids, in the same order — no invented edge, no dropped edge", commit.id);
        }

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn load_repository_matches_real_git_rev_list_and_show_ref_exactly() {
        // Ground truth from the real `git` binary itself, independent of
        // this app's own git2 usage: `git rev-list --parents --topo-order
        // --all` for the complete, real parent graph, and `git show-ref
        // --dereference` for every ref, peeled to a real commit (the
        // "^{}" line it emits specifically for an annotated tag, whose own
        // line otherwise shows the tag *object's* oid — exactly the
        // distinction collect_ref_seeds_and_badges exists to get right).
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-cli-ground-truth-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        run_git(&base, &["branch", "-M", "main"]);
        run_git(&base, &["checkout", "-b", "feature"]);
        fs::write(base.join("feature.txt"), "x").unwrap();
        run_git(&base, &["add", "feature.txt"]);
        run_git(&base, &["commit", "-m", "feature work"]);
        run_git(&base, &["checkout", "main"]);
        run_git(&base, &["merge", "--no-ff", "-m", "Merge feature", "feature"]);
        run_git(&base, &["tag", "v1-lightweight"]);
        run_git(&base, &["tag", "-a", "v1-annotated", "-m", "release notes"]);
        let path = base.to_string_lossy().into_owned();

        let rev_list_output = run_git_capture(&base, &["rev-list", "--parents", "--topo-order", "--all"]);
        let mut expected_parents: HashMap<String, Vec<String>> = HashMap::new();
        for line in rev_list_output.lines() {
            let mut tokens = line.split_whitespace();
            let oid = tokens.next().unwrap().to_string();
            expected_parents.insert(oid, tokens.map(str::to_string).collect());
        }

        let show_ref_output = run_git_capture(&base, &["show-ref", "--dereference"]);
        let mut expected_tag_commits: HashMap<String, String> = HashMap::new();
        for line in show_ref_output.lines() {
            let Some((oid, name)) = line.split_once(' ') else { continue };
            let Some(tag_name) = name.strip_prefix("refs/tags/") else { continue };
            if let Some(base_name) = tag_name.strip_suffix("^{}") {
                // The dereferenced line for an annotated tag — always the
                // real commit oid, and always wins over the tag object's
                // own oid on that tag's other (non-dereferenced) line.
                expected_tag_commits.insert(base_name.to_string(), oid.to_string());
            } else {
                expected_tag_commits.entry(tag_name.to_string()).or_insert_with(|| oid.to_string());
            }
        }

        let data = load_repository_inner(path, None).unwrap();

        // Same commit set, exactly. Order is deliberately not asserted here:
        // topological tiebreaking may legitimately differ between git's own
        // --topo-order and this app's revwalk(TOPOLOGICAL | TIME) — the
        // brief only requires topological order with date as a tiebreaker,
        // never bit-for-bit agreement with git log's own tiebreak.
        let actual_ids: HashSet<String> = data.commits.iter().map(|commit| commit.id.clone()).collect();
        let expected_ids: HashSet<String> = expected_parents.keys().cloned().collect();
        assert_eq!(actual_ids, expected_ids, "the exact same set of commits git itself reports must be loaded — none invented, none missing");

        for commit in &data.commits {
            let expected = expected_parents.get(&commit.id).expect("already asserted the id sets match above");
            assert_eq!(&commit.parents, expected, "commit {}: parents must match `git rev-list --parents` exactly, same order", commit.id);
        }

        for (tag_name, expected_commit) in &expected_tag_commits {
            let commit = data.commits.iter().find(|commit| &commit.id == expected_commit).unwrap_or_else(|| panic!("git show-ref says {tag_name} points at {expected_commit}, but that commit was not loaded at all"));
            assert!(commit.refs.iter().any(|r| r.kind == "tag" && &r.name == tag_name), "git show-ref says {tag_name} belongs on {expected_commit}, but this app's own data does not have it there — got {:?}", commit.refs.iter().map(|r| &r.name).collect::<Vec<_>>());
        }

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn tag_details_reports_lightweight_vs_annotated_correctly() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-tag-details-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        run_git(&base, &["tag", "v1-lightweight"]);
        run_git(&base, &["tag", "-a", "v1-annotated", "-m", "release notes here"]);
        let head = run_git_capture(&base, &["rev-parse", "HEAD"]);
        let path = base.to_string_lossy().into_owned();

        let lightweight = tag_details(path.clone(), "v1-lightweight".into()).unwrap();
        assert!(!lightweight.annotated, "a plain `git tag` must be reported as lightweight");
        assert_eq!(lightweight.commit_id, head);
        assert_eq!(lightweight.message, None);

        let annotated = tag_details(path, "v1-annotated".into()).unwrap();
        assert!(annotated.annotated, "a `git tag -a` must be reported as annotated");
        assert_eq!(annotated.commit_id, head, "an annotated tag's commit_id must be the peeled commit, not the tag object's own oid");
        assert_eq!(annotated.message.as_deref(), Some("release notes here"));

        fs::remove_dir_all(base).unwrap();
    }

    // ---- Message C: submodule history vs. parent gitlink reference changes ----

    #[test]
    fn submodule_history_and_parent_reference_changes_never_share_a_commit() {
        // The exact report: "Submodule History" showing a commit that reads
        // like "Update submodule X to <sha>" — a parent-repository gitlink
        // bump, not anything that happened inside the submodule's own
        // repository. The two questions ("what did the submodule itself
        // commit" vs. "when did the parent last point at a new version of
        // it") are answered by two different backend calls on two different
        // repositories — submodule_repository_inner (the submodule's own
        // gitdir) and path_history (the parent, filtered to the submodule's
        // gitlink path) — and their results must never overlap.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-history-vs-refchanges-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&parent, &["branch", "-M", "main"]);
        run_git(&dependency, &["branch", "-M", "main"]);
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = parent.join(&added);

        // Advance the submodule and record the new version in the parent —
        // exactly the "Update submodule dep to <sha>" commit the report saw.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "Submodule's own second commit"]);
        commit_selected_internal(&parent_string, &[normalized(Path::new(&added))], &format!("Update submodule {added} to a new version")).unwrap();

        let submodule_history = submodule_repository_inner(parent_string.clone(), added.clone()).unwrap();
        let reference_changes = path_history(parent_string, added.clone()).unwrap();

        assert!(submodule_history.commits.iter().any(|c| c.subject == "Submodule's own second commit"), "the submodule's own commit must be in its own history");
        assert!(!submodule_history.commits.iter().any(|c| c.subject.starts_with("Update submodule")), "a parent gitlink-update commit must never appear in the submodule's own history");

        assert!(reference_changes.iter().any(|c| c.subject.starts_with("Update submodule")), "the parent's gitlink-update commit must appear in Submodule Reference Changes");
        assert!(reference_changes.iter().any(|c| c.subject == "Add dep submodule"), "the original gitlink-add commit is also a real parent reference change");
        assert!(!reference_changes.iter().any(|c| c.subject == "Submodule's own second commit"), "a commit that only happened inside the submodule's own repository must never appear in the parent's reference-change history");

        // The two views must be genuinely disjoint, not just individually correct.
        let submodule_subjects: HashSet<&str> = submodule_history.commits.iter().map(|c| c.subject.as_str()).collect();
        let reference_subjects: HashSet<&str> = reference_changes.iter().map(|c| c.subject.as_str()).collect();
        assert!(submodule_subjects.is_disjoint(&reference_subjects), "Submodule History and Submodule Reference Changes must never share a commit");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_repository_reports_a_distinct_gitdir_and_url_from_the_parent() {
        // Message C, point 2: the context this app resolves for a submodule
        // must include a gitdir and identity genuinely distinct from the
        // parent's own — the most direct possible check that "Submodule
        // History" is really talking to a different repository, not the
        // same one with a path filter.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-identity-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        let parent_data = load_repository_inner(parent_string.clone(), None).unwrap();
        let submodule_data = submodule_repository_inner(parent_string, added).unwrap();

        assert_ne!(submodule_data.repository.gitdir, parent_data.repository.gitdir, "the submodule's resolved gitdir must be genuinely distinct from the parent's own");
        assert_ne!(submodule_data.repository.path, parent_data.repository.path, "sanity check: the workdir paths must differ too");
        assert!(parent_data.repository.submodule_url.is_none(), "the parent itself is not a submodule — it must never report a submodule_url");
        assert!(submodule_data.repository.submodule_url.is_some(), "the submodule's own configured URL (from the parent's .gitmodules) must be reported");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn submodule_repository_reports_detached_head_correctly_not_as_a_fake_branch_name() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-detached-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&dependency, &["commit", "--allow-empty", "-m", "second"]);
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = parent.join(&added);
        let target = run_git_capture(&sub_path, &["rev-parse", "HEAD~1"]);
        run_git(&sub_path, &["checkout", "--detach", &target]);

        let data = submodule_repository_inner(parent_string, added).unwrap();
        assert!(data.repository.head_detached, "a detached submodule checkout must be reported as detached");
        assert_eq!(data.repository.current_branch, "", "current_branch must be empty, never a synthetic name, on a detached HEAD (see RepositoryInfo's own contract)");
        assert_eq!(data.repository.head_oid, target, "head_oid must be the exact commit actually checked out");

        fs::remove_dir_all(base).unwrap();
    }

    // ---- Message D: branch actions must operate on the repository they're actually given ----

    #[test]
    fn switch_rename_and_delete_branch_operate_on_a_submodules_own_path_never_the_parent() {
        // The frontend half of this report was the sidebar/branch actions
        // defaulting to state.repository.path (the parent) regardless of
        // which repository's Branch Map was actually on screen. The backend
        // side of the fix is this: switch_branch/rename_branch/delete_branch
        // must already be — and stay — fully generic on whatever
        // repository_path they're given, submodule or parent, with no
        // assumption baked in that it's always the parent. This is the
        // exact scenario the sidebar fix now relies on: call these three
        // directly against a submodule's own absolute path, and confirm the
        // parent's own branches are never touched or required to exist.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-submodule-branch-actions-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "README.md");
        create_libgit2_repository(&dependency, "module.txt");
        run_git(&parent, &["branch", "-M", "main"]);
        run_git(&dependency, &["branch", "-M", "main"]);
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let sub_path = parent.join(&added);
        let sub_path_string = sub_path.to_string_lossy().into_owned();

        // A branch that exists ONLY inside the submodule, never the parent —
        // proves switch/rename/delete below are genuinely operating on the
        // submodule's own refs, not coincidentally succeeding against the
        // parent's (which uses the conventional "main" only).
        run_git(&sub_path, &["branch", "feature-in-submodule"]);
        assert!(Repository::open(&parent).unwrap().find_branch("feature-in-submodule", BranchType::Local).is_err(), "sanity check: this branch must not exist in the parent");

        switch_branch(sub_path_string.clone(), "feature-in-submodule".into()).unwrap();
        assert_eq!(Repository::open(&sub_path).unwrap().head().unwrap().shorthand(), Some("feature-in-submodule"), "the submodule's own HEAD must have moved");
        assert_eq!(Repository::open(&parent).unwrap().head().unwrap().shorthand(), Some("main"), "the parent's own HEAD must be completely unaffected");

        rename_branch(sub_path_string.clone(), "feature-in-submodule".into(), "renamed-in-submodule".into()).unwrap();
        assert!(Repository::open(&sub_path).unwrap().find_branch("renamed-in-submodule", BranchType::Local).is_ok(), "the rename must have landed inside the submodule");

        // Switch off it first — delete_branch refuses to delete the current branch.
        switch_branch(sub_path_string.clone(), "main".into()).unwrap();
        delete_branch(sub_path_string, "renamed-in-submodule".into()).unwrap();
        assert!(Repository::open(&sub_path).unwrap().find_branch("renamed-in-submodule", BranchType::Local).is_err(), "the delete must have landed inside the submodule");
        assert!(Repository::open(&parent).unwrap().find_branch("main", BranchType::Local).is_ok(), "the parent's own main branch must still be completely untouched throughout");

        fs::remove_dir_all(base).unwrap();
    }
}
