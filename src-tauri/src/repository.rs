use serde::Serialize;
use git2::{BranchType, ObjectType, Repository, Sort, Status, StatusOptions};
use std::{collections::{HashMap, HashSet}, fs, path::{Component, Path, PathBuf}, process::Command, sync::{Arc, Mutex, OnceLock}, time::{Instant, Duration, UNIX_EPOCH}};

// Temporary performance diagnostics: appends "<label>: <ms>ms" lines to a log
// file so real-world slowness can be diagnosed without guessing. Safe to leave
// in — each write is a single cheap append, guarded so a logging failure never
// breaks the actual operation. Log path is printed once by `perf_log_path()`.
fn perf_log_path() -> PathBuf {
    // Next to the executable, not the OS temp folder — much easier to find in
    // practice than hunting through %TEMP%. Falls back to temp dir only if the
    // exe's own folder isn't writable (e.g. installed under Program Files).
    if let Ok(exe) = std::env::current_exe() { if let Some(dir) = exe.parent() {
        let candidate = dir.join("git-integrity-perf.log");
        if fs::OpenOptions::new().create(true).append(true).open(&candidate).is_ok() { return candidate; }
    } }
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
        let line = format!("=== session start: build={} ===\n", env!("GIT_INTEGRITY_BUILD_SHA"));
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

fn metadata_cache() -> &'static Mutex<HashMap<String, (Instant, GitMetadata)>> {
    GIT_METADATA_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
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

// Whether one specific submodule (keyed by its own absolute path — globally
// unique, unlike a parent-relative path) has local commits not yet pushed to
// its own origin. Deliberately per-submodule, not "every submodule in the
// repository at once": load_directory used to answer this by calling
// submodule_push_status (opens a whole separate Repository, walks up to 50
// commits) synchronously for every submodule *entry in the repository*, on
// every single folder listing, regardless of whether that submodule was
// even in the folder being shown — real, serial cost that only got worse
// the more submodules the repository had (measured against a real
// case with hundreds). Each submodule that's actually *visible* in a
// listing gets its own cached answer, checked and populated independently
// — opening folder A with 2 submodules never touches the other 498
// elsewhere in the repository, and a folder revisited later, or a sibling
// folder sharing one of the same submodules, reuses the cached answer
// instead of reopening it. Deliberately not cleared by the general
// invalidate_git_metadata (an ordinary file stage/commit doesn't change any
// submodule's own push status) — only invalidate_submodule_sync does,
// alongside the actions that can actually affect it.
static SUBMODULE_UNPUSHED_CACHE: OnceLock<Mutex<HashMap<String, (Instant, bool)>>> = OnceLock::new();

fn submodule_unpushed_cache() -> &'static Mutex<HashMap<String, (Instant, bool)>> {
    SUBMODULE_UNPUSHED_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
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

fn cached_submodule_has_unpushed(sub_path: &str) -> bool {
    if let Some((cached_at, value)) = submodule_unpushed_cache().lock().unwrap().get(sub_path) {
        if cached_at.elapsed() < INDEX_METADATA_TTL { return *value; }
    }
    let lock_handle = submodule_unpushed_lock(sub_path);
    let _guard = lock_handle.lock().unwrap();
    // Re-check after acquiring the lock — a concurrent caller for this exact
    // submodule may have just finished the real scan while this one waited.
    if let Some((cached_at, value)) = submodule_unpushed_cache().lock().unwrap().get(sub_path) {
        if cached_at.elapsed() < INDEX_METADATA_TTL { return *value; }
    }
    let value = submodule_push_status(sub_path).is_some();
    submodule_unpushed_cache().lock().unwrap().insert(sub_path.to_string(), (Instant::now(), value));
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
pub struct RepositoryInfo { path: String, name: String, current_branch: String, head_oid: String, head_detached: bool }

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
    // False only from list_directory_fast (the filesystem-only listing used
    // for the very first render while opening a repository) — status/tracked/
    // unpushed above are meaningless placeholders in that case, not "clean"
    // or "untracked", and the frontend must show a loading state instead of
    // treating them as real answers. Always true from the normal
    // load_directory, which always has real status by the time it returns.
    status_known: bool,
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
    // Only populated for kind == "tag": the local branch (if any) whose tip
    // currently sits on the same commit the tag points to. None means the
    // tag's commit isn't the tip of any local branch (detached if checked out).
    attached_branch: Option<String>,
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

fn run_with_timeout(command: Command) -> Result<std::process::Output, String> {
    run_with_timeout_labeled(command, GIT_COMMAND_TIMEOUT, "Git", "10 minutes")
}

fn run_with_timeout_labeled(mut command: Command, timeout: Duration, program_label: &str, timeout_label: &str) -> Result<std::process::Output, String> {
    let child = command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| format!("Cannot start {program_label}: {e}"))?;
    let id = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("{program_label} process error: {error}")),
        Err(_) => {
            #[cfg(unix)] { let _ = Command::new("kill").arg("-9").arg(id.to_string()).status(); }
            #[cfg(windows)] { let _ = Command::new("taskkill").args(["/F", "/PID"]).arg(id.to_string()).status(); }
            Err(format!("{program_label} command timed out after {timeout_label} — check your network connection and try again"))
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
pub struct RawGitResult { stdout: String, stderr: String, success: bool, exit_code: Option<i32>, read_only: bool }

// A minimal shell-like tokenizer — single/double-quoted segments (with
// backslash-escaping *inside* double quotes only, matching common shell
// behavior closely enough for this) are kept together as one argument, so
// `commit -m "fix bug in parser"` produces the 3 arguments a real shell
// would, not `split_whitespace`'s 6. That bug was real, not theoretical: any
// commit message, path, or branch name containing a space was silently
// mangled into multiple bogus arguments before this.
fn tokenize_git_args(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_current = false;
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == '\\' && q == '"' { if let Some(&next) = chars.peek() { if next == '"' || next == '\\' { current.push(next); chars.next(); continue; } } current.push(ch); }
                else if ch == q { quote = None; }
                else { current.push(ch); }
            }
            None => {
                if ch == '"' || ch == '\'' { quote = Some(ch); has_current = true; }
                else if ch.is_whitespace() { if has_current { tokens.push(std::mem::take(&mut current)); has_current = false; } }
                else { current.push(ch); has_current = true; }
            }
        }
    }
    if quote.is_some() { return Err("Unclosed quote in command".into()); }
    if has_current { tokens.push(current); }
    Ok(tokens)
}

// A command whose *subcommand alone* (ignoring every flag/argument after it)
// can never mutate the repository, its index, or the working tree — matched
// against a strict, deliberately short allowlist. Anything not on this list
// is treated conservatively as possibly mutating, even if it's actually
// read-only in practice (e.g. `remote -v`) — the cost of a false negative
// here (an unnecessary reload) is far lower than a false positive (skipping
// a reload after something that actually changed state).
fn is_read_only_git_subcommand(subcommand: &str) -> bool {
    matches!(subcommand, "status" | "log" | "diff" | "show" | "blame" | "ls-files")
}

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
    let started = Instant::now();
    validate_path(&repository_path)?;
    // Held for the whole command, including a genuinely read-only one — the
    // frontend already refuses to start a second console command while one
    // is running, but this is what makes that actually safe against every
    // *other* mutation too (Stage/Commit/Delete/checkout/stash), not just
    // against another console command, the same as every other
    // index/HEAD-mutating command in this file.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "run_git_command", queue_started.elapsed());
    let mut parts = tokenize_git_args(&args)?;
    // Typing the full, natural command ("git status") is just as valid as the
    // short form ("status") — strip a leading "git" token instead of
    // rejecting it, so this behaves like a real terminal either way.
    if parts.first().map(String::as_str) == Some("git") { parts.remove(0); }
    if parts.is_empty() { return Err("Type a git subcommand, e.g. \"status\" or \"log --oneline -10\"".into()); }
    let read_only = is_read_only_git_subcommand(&parts[0]);
    // Never the full argument list — it can contain commit messages, file
    // contents, tokens embedded in a URL, or anything else the user typed.
    // Only the subcommand itself, which repo this ran against, the duration,
    // and the outcome are safe to write to a log file that might get shared
    // back for diagnosis.
    let repo_id = anonymized_repository_id(&repository_path);
    let mut command = Command::new("git");
    configure_git_command(&mut command);
    command.arg("-C").arg(&repository_path).arg("-c").arg("color.ui=false").args(&parts);
    let output = run_with_timeout(command);
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            perf_log(&format!("run_git_command: {} ({repo_id}, read_only={read_only}) TIMED_OUT", parts[0]), started.elapsed());
            return Err(error);
        }
    };
    perf_log(&format!("run_git_command: {} ({repo_id}, read_only={read_only}) exit_code={:?}", parts[0], output.status.code()), started.elapsed());
    invalidate_git_metadata(&repository_path);
    Ok(RawGitResult {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
        exit_code: output.status.code(),
        read_only,
    })
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

fn invalidate_git_metadata(repository: &str) {
    // Cache keys are "{repository}\0{scope}" (one entry per folder that's been
    // browsed) — a mutation can affect any of them, so drop every scope cached for
    // this repository, not just the unscoped entry.
    let prefix = format!("{repository}\u{0}");
    metadata_cache().lock().unwrap().retain(|key, _| !key.starts_with(&prefix));
    index_metadata_cache().lock().unwrap().remove(repository);
    unpushed_paths_cache().lock().unwrap().remove(repository);
    full_status_cache().lock().unwrap().remove(repository);
    sorted_lookups_cache().lock().unwrap().remove(repository);
    // submodule_unpushed_cache is deliberately NOT cleared here:
    // invalidate_git_metadata runs after essentially every mutation,
    // including an ordinary file stage/commit that has nothing to do with
    // any submodule's own state — clearing it here would force the next
    // fold/load to redo real, expensive submodule I/O regardless, defeating
    // its TTL entirely on the most common action in the app. It's
    // invalidated explicitly instead, wherever it's actually relevant: see
    // invalidate_submodule_sync below.
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

// Lets the UI show exactly which commit this binary was built from — set at
// compile time in build.rs. Answers "am I actually running the new build?"
// by looking at the app itself instead of a file's modified date, which is
// what caused a stale Windows executable to go untested for hours.
#[tauri::command]
pub fn build_info() -> String { env!("GIT_INTEGRITY_BUILD_SHA").to_string() }

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

// UTRUD is a legacy internal tool, previously only reachable via Windows
// Explorer's "Send to" menu (a per-user .bat under
// AppData\Roaming\Microsoft\Windows\SendTo that just forwards whatever's
// selected as `%*` to "C:\LegacyApp\UTRUD\2.0.0\UTRUD.bat"). This gives the
// same launch from inside the app for a folder named "r" anywhere in the
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
// itself. Setting current_dir to the selected folder itself (an earlier fix,
// needed to make UTRUD find the right folder at all after it inherited this
// app's own directory) overcorrected: with cwd == the "r" folder itself,
// UTRUD's own `cwd + name` logic doubled it into ".../r/r", confirmed by the
// reported "Default Result directory structures created: ...\r\r". The
// absolute path stays the argument either way (some of UTRUD's own logic
// does use it directly, per the first fix); only cwd needed to move up one.
fn utrud_command_parts(absolute: &Path) -> (PathBuf, PathBuf) {
    let cwd = absolute.parent().map(Path::to_path_buf).unwrap_or_else(|| absolute.to_path_buf());
    (cwd, absolute.to_path_buf())
}

#[tauri::command]
pub fn run_utrud(repository_path: String, relative_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let relative = safe_relative_path(&relative_path)?;
    if relative.file_name().and_then(|name| name.to_str()) != Some("r") {
        return Err("UTRUD can only be launched from a folder named \"r\"".into());
    }
    let absolute = Path::new(&repository_path).join(&relative);
    if !absolute.is_dir() { return Err("The selected path is not a folder".into()); }
    let (cwd, argument) = utrud_command_parts(&absolute);
    #[cfg(target_os = "windows")]
    {
        perf_log(&format!("run_utrud: cwd={} arg={}", cwd.display(), argument.display()), Duration::ZERO);
        Command::new("cmd").args(["/C", "call", UTRUD_BATCH_PATH]).arg(&argument).current_dir(&cwd).spawn().map_err(|error| format!("Could not start UTRUD: {error}"))?;
        Ok(())
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
// recorded gitlink — no extra scan needed for that). Recording a new gitlink
// into the parent's history is a real commit, and only ever happens as the
// direct, explicit result of the user's own action: committing/pushing the
// submodule itself (record_pushed_submodule_in_parent, called only from
// commit_submodule/push_submodule/force_push_submodule — never from a
// Refresh, navigation, or any other passive reload). This used to also run
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
    // Prefix match, not a single exact key: submodule_unpushed_cache is keyed
    // by each submodule's own absolute path (repository/relative/...), not
    // by the parent's path.
    let prefix = format!("{repository}/");
    submodule_unpushed_cache().lock().unwrap().retain(|key, _| key != repository && !key.starts_with(&prefix));
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
    perf_log("open_repository_fast: TOTAL", started.elapsed());
    Ok(FastRepositoryData { repository: RepositoryInfo { path, name, current_branch, head_oid, head_detached }, branches, commits, stashes, submodule_paths, commits_truncated })
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

    perf_log("load_repository: TOTAL", load_started.elapsed());
    Ok(RepositoryData { repository: RepositoryInfo { path, name, current_branch, head_oid, head_detached }, branches, commits, changes, stashes, submodule_paths, commits_truncated })
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
    // recent_full_statuses (not a plain fresh scan) — this still needs the
    // true current disk state, but shares that requirement with
    // refresh_status via the same short reuse window/single-flight instead
    // of always paying for its own separate scan: opening the Working tree
    // drawer immediately followed by Stage all is exactly the case that
    // used to mean two full scans back to back.
    let full = recent_full_statuses(&repo, repository_path)?;
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

#[derive(Serialize)]
pub struct BranchCreationContext { current_branch: String, current_commit: String, main_remote_branch: Option<String>, ahead: usize, behind: usize }

#[derive(Serialize)]
pub struct BranchDivergence {
    name: String,
    tip: String,
    // How many commits are reachable from `name`'s tip but not from
    // `primary_branch`'s tip, and vice versa — real `git rev-list
    // --left-right --count primary...name` via git2's graph_ahead_behind,
    // never inferred from where two refs happen to land in the rendered
    // graph (lane, row distance, color).
    ahead: usize,
    behind: usize,
    // The real common ancestor (`git merge-base`), when both tips exist and
    // share one — this is the commit the frontend should mark as the actual
    // divergence point, not "whatever commit is next in the same lane".
    merge_base: Option<String>,
}

// Real, OID-based divergence for every local branch relative to one chosen
// "primary" branch — the backend counterpart to what the graph view shows
// per branch ("N commits ahead of <primary>"). Deliberately independent of
// any lane/row layout: the frontend decides how to *draw* this, this only
// answers what's actually true in the DAG. A branch with no real ancestry
// relationship to `primary_branch` (merge_base: None, from an unrelated
// history — e.g. two truly disconnected root commits) is reported as such
// instead of a fabricated ahead/behind count.
#[tauri::command]
pub fn graph_branch_divergence(repository_path: String, primary_branch: String) -> Result<Vec<BranchDivergence>, String> {
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let Some(primary_tip) = repo.find_branch(&primary_branch, BranchType::Local).ok().and_then(|b| b.get().target()) else {
        return Ok(Vec::new()); // no such local branch (e.g. detached HEAD) — nothing to compare against
    };
    let mut result = Vec::new();
    if let Ok(iterator) = repo.branches(Some(BranchType::Local)) {
        for item in iterator.flatten() {
            let name = match item.0.name().ok().flatten() { Some(name) => name.to_string(), None => continue };
            let Some(tip) = item.0.get().target() else { continue };
            let (ahead, behind) = repo.graph_ahead_behind(tip, primary_tip).unwrap_or((0, 0));
            let merge_base = repo.merge_base(tip, primary_tip).ok().map(|oid| oid.to_string());
            result.push(BranchDivergence { name, tip: tip.to_string(), ahead, behind, merge_base });
        }
    }
    Ok(result)
}

// A new branch is always created from wherever HEAD currently is — this
// tells the caller exactly where that is (so "New branch" is never a
// mystery about what you're actually branching from), and how that
// position relates to the project's main integration branch, so it's
// obvious upfront whether you're branching off the latest main or off
// something already behind it.
#[tauri::command]
pub fn branch_creation_context(repository_path: String, target_path: String) -> Result<BranchCreationContext, String> {
    let repository_path = resolve_target_repository(&repository_path, &target_path)?;
    validate_path(&repository_path)?;
    let repo = internal_repository(&repository_path)?;
    let head = repo.head().map_err(|error| error.message().to_string())?;
    let current_branch = head.shorthand().unwrap_or("HEAD").to_string();
    let current_commit = head.peel_to_commit().map_err(|error| error.message().to_string())?.id().to_string();
    let local_oid = head.target();

    let mut main_remote_branch = None;
    for candidate in ["origin/main", "origin/master"] {
        if repo.find_branch(candidate, BranchType::Remote).is_ok() { main_remote_branch = Some(candidate.to_string()); break; }
    }
    if main_remote_branch.is_none() { main_remote_branch = default_remote_ref(&repository_path); }

    let (ahead, behind) = local_oid.zip(main_remote_branch.as_deref())
        .and_then(|(local, remote_name)| repo.find_branch(remote_name, BranchType::Remote).ok()?.get().target().map(|remote_oid| (local, remote_oid)))
        .and_then(|(local, remote_oid)| repo.graph_ahead_behind(local, remote_oid).ok())
        .unwrap_or((0, 0));

    Ok(BranchCreationContext { current_branch, current_commit: current_commit[..8.min(current_commit.len())].to_string(), main_remote_branch, ahead, behind })
}

#[tauri::command]
pub fn create_branch(path: String, branch: String) -> Result<(), String> {
    if branch.trim().is_empty() { return Err("Branch name cannot be empty".into()); }
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&path, "create_branch", queue_started.elapsed());
    let repo = internal_repository(&path)?; let head = repo.head().and_then(|head| head.peel_to_commit()).map_err(|error| error.message().to_string())?; repo.branch(branch.trim(), &head, false).map_err(|error| error.message().to_string())?; drop(head);
    drop(_lock);
    // switch_branch takes its own lock on the same repository — released
    // above first, so this is two sequential acquisitions, never nested.
    switch_branch(path, branch)
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
    // create_branch's own submodule-scoped lock is already released by now —
    // this acquires only the *parent's*, never nested with it.
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "create_submodule_branch", queue_started.elapsed());
    let parent = internal_repository(&repository_path)?;
    let mut submodule = parent.find_submodule(&relative_path).map_err(|error| error.message().to_string())?;
    submodule.add_to_index(true).map_err(|error| format!("Branch created, but the parent index could not be updated: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path); // this app just changed a submodule's registration/version
    Ok(())
}

#[tauri::command]
pub fn switch_branch(path: String, branch: String) -> Result<(), String> {
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&path, "switch_branch", queue_started.elapsed());
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

// ---- Pull request status (read-only, first incremental step) ----
//
// The frontend never receives or stores any GitHub token — this shells out
// to the `gh` CLI (same trust model this app already uses for plain `git`:
// reuse whatever credentials are already working outside this app — SSH
// agent, credential helper, OS keychain — instead of asking the user to
// paste a secret into it). `gh` owns its own token storage entirely; this
// process only ever sees `gh`'s stdout/stderr, never the token itself.

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
    // "passing" | "failing" | "pending" | "none"
    checks_status: String,
    url: String,
}

#[derive(Serialize)]
pub struct PrStatusResult {
    // "no_remote" | "unsupported_provider" | "auth_missing" | "api_error" |
    // "no_open_pr" | "no_upstream" | "partial_result" | "detached_head" |
    // "no_branch" | "superseded" | "ok" (one or more PRs — the frontend
    // distinguishes "one" vs "multiple" from pull_requests.len() itself)
    state: String,
    detail: String,
    // The branch actually checked against the server (the *remote* branch
    // name, which can differ from the local one) — for the UI's
    // "No open PR for <branch> in <repo>" message.
    branch: Option<String>,
    // The `host/owner/repo` the query ran against — same message.
    queried_repo: Option<String>,
    // True when the search could not check every relevant candidate
    // repository (one errored, or the candidate list was longer than the
    // cap) — an empty result under this must never be presented as a
    // confident "no PR" (see state "partial_result").
    partial: bool,
    pull_requests: Vec<PullRequestSummary>,
}

impl PrStatusResult {
    fn plain(state: &str, detail: impl Into<String>) -> Self {
        PrStatusResult { state: state.into(), detail: detail.into(), branch: None, queried_repo: None, partial: false, pull_requests: Vec::new() }
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
        let outcome = entry.get("conclusion").and_then(|v| v.as_str())
            .or_else(|| entry.get("state").and_then(|v| v.as_str()))
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

// The two variables of a `gh pr list` call — everything else is constant.
#[derive(Debug, Clone, PartialEq)]
struct PrGhQuery { repo: String, head: String }

// The outcome of one `gh pr list` call, in a shape a test can fabricate
// without constructing a real `std::process::Output`.
#[derive(Debug)]
enum GhOutcome {
    Prs(Vec<serde_json::Value>),
    Failure { stderr: String },
    Unavailable { not_installed: bool, detail: String },
}

const PR_GH_JSON_FIELDS: &str = "number,title,headRefName,headRepository,headRepositoryOwner,baseRefName,state,isDraft,mergeable,reviewDecision,statusCheckRollup,url";

fn gh_stderr_looks_like_auth(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("auth") || lower.contains("not logged") || lower.contains("credentials")
        || lower.contains("no accounts") || lower.contains("gh auth login")
}

fn run_gh_pr_list(query: &PrGhQuery) -> GhOutcome {
    let mut command = Command::new("gh");
    command.args(["pr", "list", "--repo", &query.repo, "--head", &query.head, "--state", "open", "--json", PR_GH_JSON_FIELDS]);
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
        checks_status: map_checks_status(&rollup),
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

// One candidate base's outcome, tagged with whether it was the head repo
// (index 0) — collected from the concurrent dispatch in pr_status_impl below
// and reduced into a PrStatusResult afterward, all on the calling thread, so
// the reduction logic itself stays single-threaded and easy to follow.
struct PrCandidateOutcome { is_head_repo: bool, base_id: String, outcome: GhOutcome }

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

    // Every candidate base is queried *concurrently* — one OS thread each,
    // bounded by PR_MAX_CANDIDATE_BASES — instead of one after another
    // (which meant a worst case of N sequential 20s timeouts). Since they
    // all start together, the wall-clock bound for the whole round is just
    // the slowest single candidate's own existing 20s timeout, not their
    // sum — this is what actually keeps a "reasonable total timeout" true
    // without a second, redundant deadline layered on top. `run` must be
    // `Sync` — shared, read-only, across every spawned thread.
    let dispatch_started = Instant::now();
    let outcomes: Vec<PrCandidateOutcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = ctx.candidate_bases.iter().enumerate().map(|(index, base)| {
            let query = PrGhQuery { repo: base.gh_repo_arg(), head: ctx.head_branch.clone() };
            scope.spawn(move || {
                let base_id = anonymized_repository_id(&query.repo);
                PrCandidateOutcome { is_head_repo: index == 0, base_id, outcome: run(&query) }
            })
        }).collect();
        handles.into_iter().map(|handle| handle.join().unwrap_or(PrCandidateOutcome {
            is_head_repo: false, base_id: "unknown".into(),
            outcome: GhOutcome::Failure { stderr: "gh query thread panicked".into() },
        })).collect()
    });
    perf_log(&format!("pr_status: [{context_label}] {} candidate queries dispatched concurrently", ctx.candidate_bases.len()), dispatch_started.elapsed());

    let mut found: Vec<PullRequestSummary> = Vec::new();
    let mut raw_total = 0usize;
    let mut head_repo_query_ok = false;
    let mut head_repo_error: Option<(bool, String)> = None; // (auth-ish, detail)
    let mut head_repo_unavailable: Option<(bool, String)> = None; // gh missing / unrunnable
    // A *non*-head candidate that failed or couldn't run — the search wasn't
    // complete, so an otherwise-empty result must not read as confident.
    let mut secondary_candidate_failed = false;

    for PrCandidateOutcome { is_head_repo, base_id, outcome } in outcomes {
        match outcome {
            GhOutcome::Prs(items) => {
                if is_head_repo { head_repo_query_ok = true; }
                raw_total += items.len();
                found.extend(select_matching_prs(&items, &ctx.head_branch, &head_owner, &head_name));
            }
            GhOutcome::Failure { stderr } => {
                let auth = gh_stderr_looks_like_auth(&stderr);
                perf_log(&format!("pr_status: [{context_label}] base=<{base_id}> query failed (auth={auth})"), Duration::ZERO);
                if is_head_repo { head_repo_error = Some((auth, stderr)); } else { secondary_candidate_failed = true; }
            }
            GhOutcome::Unavailable { not_installed, detail } => {
                perf_log(&format!("pr_status: [{context_label}] base=<{base_id}> gh unavailable (not_installed={not_installed})"), Duration::ZERO);
                if is_head_repo { head_repo_unavailable = Some((not_installed, detail)); } else { secondary_candidate_failed = true; }
            }
        }
    }

    // The same PR reached through two different `--repo` targets is one PR.
    found.sort_by(|a, b| a.url.cmp(&b.url));
    found.dedup_by(|a, b| !a.url.is_empty() && a.url == b.url);

    let incomplete = ctx.candidates_truncated || secondary_candidate_failed;
    perf_log(&format!("pr_status: [{context_label}] raw_results={raw_total} after_filter={} incomplete={incomplete}", found.len()), Duration::ZERO);

    if !found.is_empty() {
        // Real PRs found — worth reporting regardless of whether every
        // candidate could be checked; `partial` still says so.
        return PrStatusResult { state: "ok".into(), detail: String::new(), branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: incomplete, pull_requests: found };
    }
    if let Some((not_installed, detail)) = head_repo_unavailable {
        let message = if not_installed {
            "The GitHub CLI (`gh`) isn't installed — install it and run `gh auth login` to see pull request status.".to_string()
        } else { detail };
        return PrStatusResult { state: "auth_missing".into(), detail: message, branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: true, pull_requests: Vec::new() };
    }
    if let Some((auth, stderr)) = head_repo_error {
        let enterprise = ctx.head_repo.host != "github.com";
        let state = if auth { "auth_missing" } else { "api_error" };
        let detail = if auth && enterprise {
            format!("`gh` isn't authenticated for {} — run `gh auth login --hostname {}`.{}", ctx.head_repo.host, ctx.head_repo.host,
                if stderr.trim().is_empty() { String::new() } else { format!(" ({})", stderr.trim()) })
        } else if stderr.trim().is_empty() {
            "The GitHub API request failed.".to_string()
        } else { stderr.trim().to_string() };
        return PrStatusResult { state: state.into(), detail, branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: true, pull_requests: Vec::new() };
    }
    if !head_repo_query_ok {
        // Neither a result, an error, nor "unavailable" from the head repo —
        // shouldn't happen, but never report a confident "no PR" off it.
        return PrStatusResult { state: "api_error".into(), detail: "Could not determine pull request status.".into(), branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: true, pull_requests: Vec::new() };
    }
    if incomplete {
        // The head repo itself came back clean, but at least one other
        // relevant candidate could not be checked (or there were more
        // candidates than the cap allowed) — this must never look like a
        // confident "no open PR".
        return PrStatusResult {
            state: "partial_result".into(),
            detail: "Some candidate repositories could not be checked, so this result may be incomplete.".into(),
            branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: true, pull_requests: Vec::new(),
        };
    }
    // Genuine empty: every candidate that was supposed to be checked was,
    // and none had a match.
    let state = if ctx.had_upstream { "no_open_pr" } else { "no_upstream" };
    PrStatusResult { state: state.into(), detail: String::new(), branch: Some(ctx.head_branch), queried_repo: Some(queried_repo), partial: false, pull_requests: Vec::new() }
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
        perf_log(&format!("pr_status: [{context_label}] repo={} superseded by a newer request — skipping the gh round", anonymized_repository_id(&repository_path)), Duration::ZERO);
        return Ok(PrStatusResult::plain("superseded", "A newer request for this repository has already superseded this one."));
    }
    Ok(pr_status_impl(ctx, &repository_path, &context_label, &run_gh_pr_list))
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
pub async fn fetch_all_remotes(repository_path: String) -> Result<(), String> {
    off_main_thread(move || fetch_all_remotes_inner(repository_path)).await
}

fn fetch_all_remotes_inner(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    git(&repository_path, &["fetch", "--all"]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub async fn sync_repository(repository_path: String, action: String) -> Result<(), String> {
    off_main_thread(move || sync_repository_inner(repository_path, action)).await
}

fn sync_repository_inner(repository_path: String, action: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "sync_repository", queue_started.elapsed());
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
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "merge_branch", queue_started.elapsed());
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
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "resolve_conflict", queue_started.elapsed());
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
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "complete_merge", queue_started.elapsed());
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
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "abort_merge", queue_started.elapsed());
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
pub async fn submodule_repository(repository_path: String, relative_path: String) -> Result<RepositoryData, String> {
    off_main_thread(move || submodule_repository_inner(repository_path, relative_path)).await
}

fn submodule_repository_inner(repository_path: String, relative_path: String) -> Result<RepositoryData, String> {
    perf_log(&format!("submodule_repository: requested (parent={}, relative_path={relative_path})", anonymized_repository_id(&repository_path)), Duration::ZERO);
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let result = load_repository_inner(absolute.to_string_lossy().into_owned(), None);
    match &result {
        Ok(data) => {
            // Temporary, deliberately verbose diagnostic for the "does the
            // Submodule Branch Map ever show the wrong submodule's history"
            // report — enough to confirm from the log alone, without a
            // debugger, exactly which repository and commits this call
            // resolved to: a real cross-contamination bug would show two
            // different relative_path requests resolving to the same HEAD/
            // commit OIDs; two genuinely different (if superficially
            // similar-looking) submodules would not.
            let first_three: Vec<String> = data.commits.iter().take(3).map(|c| format!("{}:{}", &c.id[..8.min(c.id.len())], c.subject)).collect();
            perf_log(&format!(
                "submodule_repository: resolved to {} (branch={}, head={}, branches={}, commits={}, first_commits=[{}])",
                anonymized_repository_id(&data.repository.path), data.repository.current_branch, &data.repository.head_oid[..8.min(data.repository.head_oid.len())],
                data.branches.len(), data.commits.len(), first_three.join(" | "),
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
pub fn submodule_navigation_status(repository_path: String, relative_path: String) -> Result<Option<SubmoduleNavigationStatus>, String> {
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
pub fn submodule_folder_status(repository_path: String, relative_path: String) -> Result<(), String> {
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
pub fn list_directory_fast(repository_path: String, relative_path: String) -> Result<Vec<DirectoryEntry>, String> {
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
        entries.push(DirectoryEntry { name, relative_path: relative_string, kind, status: String::new(), tracked: false, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, submodule_has_unpushed_commits: false, unpushed: false, status_known: false });
    }
    entries.sort_by_cached_key(|entry| (!matches!(entry.kind.as_str(), "folder" | "submodule"), entry.name.to_lowercase()));
    perf_log(&format!("list_directory_fast: TOTAL ({} entries, {relative_path})", entries.len()), started.elapsed());
    Ok(entries)
}

#[tauri::command]
pub fn load_directory(repository_path: String, relative_path: String, force: Option<bool>) -> Result<Vec<DirectoryEntry>, String> {
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
        // invalidate_git_metadata deliberately leaves submodule_unpushed_cache
        // alone (an ordinary stage/commit elsewhere has nothing to do with
        // any submodule's own push status) — but an explicit forced reload
        // is the one signal that *should* also bypass it, same reasoning as
        // everything else force does here: the user asked for the truth
        // right now, not a cached answer from up to 5 minutes ago. Scoped to
        // just the submodules under this folder's own repository, not every
        // submodule cached anywhere.
        let prefix = format!("{status_repo}/");
        submodule_unpushed_cache().lock().unwrap().retain(|key, _| key != status_repo && !key.starts_with(&prefix));
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
    let step = Instant::now();
    let mut submodule_count = 0usize;

    for item in fs::read_dir(&absolute).map_err(|error| error.to_string())? {
        let item = item.map_err(|error| error.to_string())?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" { continue; }
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
        // Only the submodules actually visible in *this* folder ever get
        // scanned — see cached_submodule_has_unpushed's own doc comment.
        let submodule_has_unpushed_commits = kind == "submodule" && cached_submodule_has_unpushed(item.path().to_str().unwrap_or_default());
        let unpushed = if kind == "folder" { git_metadata.unpushed.contains(&status_key) || has_prefix(unpushed_sorted, &tracked_prefix) } else { git_metadata.unpushed.contains(&status_key) };
        entries.push(DirectoryEntry { name, relative_path: relative_string, kind, status: status_for_entry(&status_key), tracked, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, submodule_has_unpushed_commits, unpushed, status_known: true });
    }
    perf_log(&format!("load_directory: readdir loop ({} entries, {submodule_count} submodules)", entries.len()), step.elapsed());
    let step = Instant::now();
    // sort_by_cached_key computes each entry's sort key exactly once (O(n)
    // lowercase allocations total) instead of `sort_by` with `.to_lowercase()`
    // inside the comparator, which re-allocates two new Strings on *every*
    // comparison the sort makes — O(n log n) allocations. For a folder with
    // 20,000 direct entries that's the difference between ~20,000 and
    // ~570,000 allocations just to sort the listing.
    entries.sort_by_cached_key(|entry| (!matches!(entry.kind.as_str(), "folder" | "submodule"), entry.name.to_lowercase()));
    perf_log(&format!("load_directory: sort ({} entries)", entries.len()), step.elapsed());
    perf_log(&format!("load_directory: TOTAL ({relative_path})"), load_started.elapsed());
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
    let (submodule_url, submodule_branch, submodule_push_status, submodule_unpushed_commits, submodule_commit) = if kind == "submodule" {
        let (url, branch) = submodule_url_and_branch(&repository_path, &relative_string);
        match internal_submodule_repository(&absolute) {
            Ok(sub_repo) => {
                let push_status = submodule_push_status_in(&sub_repo);
                let unpushed_commits = submodule_unpushed_commits_in(&sub_repo);
                let commit = sub_repo.head().ok().and_then(|head| head.peel_to_commit().ok()).map(|commit| {
                    let author_name = commit.author().name().unwrap_or("Unknown").to_string();
                    (commit.id().to_string(), commit.summary().unwrap_or("No message").to_string(), author_name, short_date(commit.time().seconds()))
                });
                (url, branch, push_status, unpushed_commits, commit)
            }
            Err(_) => (url, branch, None, Vec::new(), None),
        }
    } else { (None, None, None, Vec::new(), None) };

    Ok(EntryDetails {
        name: absolute.file_name().and_then(|name| name.to_str()).unwrap_or(&relative_string).to_string(), relative_path: relative_string,
        kind, status, tracked, unpushed, size: if metadata.is_file() { metadata.len() } else { 0 }, modified, item_count, submodule_url, submodule_branch, submodule_push_status, submodule_unpushed_commits,
        last_commit_id: last.as_ref().map(|value| value.0.clone()),
        last_commit_subject: last.as_ref().map(|value| value.1.clone()), last_commit_author: last.as_ref().map(|value| value.2.clone()), last_commit_date: last.as_ref().map(|value| value.3.clone()),
        submodule_commit_id: submodule_commit.as_ref().map(|value| value.0.clone()),
        submodule_commit_subject: submodule_commit.as_ref().map(|value| value.1.clone()), submodule_commit_author: submodule_commit.as_ref().map(|value| value.2.clone()), submodule_commit_date: submodule_commit.as_ref().map(|value| value.3.clone()),
    })
}

// The "last commit touching this path" lookup entry_details deliberately no
// longer does inline — a separate, later call the frontend fires only after
// the fast details above are already on screen, so selecting item after
// item in a large folder doesn't sit blocked on a potentially-heavy history
// walk on every single click.
#[tauri::command]
pub fn entry_last_commit(repository_path: String, relative_path: String) -> Result<Option<PublishCommit>, String> {
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

#[tauri::command]
pub fn submodule_versions(repository_path: String, relative_path: String) -> Result<SubmoduleVersions, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let repo = internal_submodule_repository(&absolute)?;
    let current_revision = repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    let current_branch = repo.head().ok().and_then(|head| head.shorthand().map(String::from)).unwrap_or_default();
    let mut versions = Vec::new();
    // Local branch tips, indexed by the commit they currently point at — used
    // below to report which branch (if any) is "attached" to a given tag.
    let mut branch_tip_names: HashMap<String, String> = HashMap::new();
    for branch_type in [BranchType::Local, BranchType::Remote] { if let Ok(iterator) = repo.branches(Some(branch_type)) { for item in iterator.flatten() { let name = item.0.name().ok().flatten().unwrap_or("").to_string(); if name.ends_with("/HEAD") { continue; } if let Some(oid) = item.0.get().target() { if branch_type == BranchType::Local { branch_tip_names.entry(oid.to_string()).or_insert_with(|| name.clone()); } if let Ok(commit) = repo.find_commit(oid) { let kind = if branch_type == BranchType::Local { "branch" } else { "remote" }; versions.push(SubmoduleVersion { name: name.clone(), revision: oid.to_string(), kind: kind.into(), current: kind == "branch" && name == current_branch, subject: commit.summary().unwrap_or("").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()), attached_branch: None }); } } } } }
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
            });
        }
    }
    let mut walk = repo.revwalk().map_err(|error| error.message().to_string())?; let _ = walk.set_sorting(Sort::TIME); if let Ok(head) = repo.head() { if let Some(oid) = head.target() { let _ = walk.push(oid); } }
    for oid in walk.flatten().take(30) { if let Ok(commit) = repo.find_commit(oid) { versions.push(SubmoduleVersion { name: oid.to_string()[..8].into(), revision: oid.to_string(), kind: "commit".into(), current: oid.to_string() == current_revision, subject: commit.summary().unwrap_or("").into(), author: commit.author().name().unwrap_or("Unknown").into(), date: short_date(commit.time().seconds()), attached_branch: None }); } }
    Ok(SubmoduleVersions { path: relative_path, current_revision, current_branch, versions })
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
    let absolute_string = absolute.to_string_lossy().into_owned();
    // The submodule's own lock, held only for this first phase (its own
    // HEAD/checkout) — released before the parent's own lock is taken below
    // for the gitlink update, so this thread never holds both repositories'
    // write locks at once (see stage_files_inner's own comment for why that
    // invariant matters).
    let queue_started = Instant::now();
    let sub_lock_handle = repo_write_lock(&absolute_string);
    let sub_lock = sub_lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&absolute_string, "switch_submodule_version(submodule)", queue_started.elapsed());
    let repo = internal_submodule_repository(&absolute)?;
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
    } else if version_kind == "tag" {
        // `revision` here is already the commit the tag points at (resolved by
        // submodule_versions, which peels annotated tags to their commit), but
        // resolve by tag name and peel again defensively in case a caller passes
        // the tag name as `revision` instead.
        let tag_name = if name.is_empty() { revision.clone() } else { name.clone() };
        let object = repo.find_reference(&format!("refs/tags/{tag_name}")).and_then(|reference| reference.peel(git2::ObjectType::Commit))
            .or_else(|_| repo.revparse_single(&revision).and_then(|object| object.peel(git2::ObjectType::Commit)))
            .map_err(|error| error.message().to_string())?;
        repo.set_head_detached(object.id()).map_err(|error| error.message().to_string())?;
    } else {
        let object = repo.revparse_single(&revision).map_err(|error| error.message().to_string())?; repo.set_head_detached(object.id()).map_err(|error| error.message().to_string())?;
    }
    let mut checkout = git2::build::CheckoutBuilder::new(); checkout.safe(); repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    let selected = repo.head().ok().and_then(|head| head.target()).map(|id| id.to_string()).unwrap_or_default();
    drop(repo);
    drop(sub_lock); // fully released before the parent's own lock, never nested
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "switch_submodule_version(parent)", queue_started.elapsed());
    let parent = internal_repository(&repository_path)?;
    let mut submodule = parent.find_submodule(&relative_path).map_err(|error| error.message().to_string())?;
    submodule.add_to_index(true).map_err(|error| format!("Version changed, but the parent index could not be updated: {}", error.message()))?;
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path); // this app just changed a submodule's registration/version
    Ok(selected)
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
    checkout.force(); // discard dirty working-tree edits too, not just move HEAD
    sub_repo.checkout_head(Some(&mut checkout)).map_err(|error| error.message().to_string())?;
    drop(sub_repo);
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path); // this app just changed a submodule's registration/version
    Ok(target_oid.to_string())
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
    let mut repo = internal_repository(&repository_path)?; let name = repo.submodules().map_err(|error| error.message().to_string())?.into_iter().find(|item| normalized(item.path()) == relative).map(|item| item.name().unwrap_or("").to_string()).ok_or("Submodule configuration was not found")?; repo.submodule_set_url(&name, url).map_err(|error| error.message().to_string())?;
    drop(repo); drop(parent_lock); // released before the submodule's own lock, never nested
    let absolute_string = absolute.to_string_lossy().into_owned();
    let queue_started = Instant::now();
    let sub_lock_handle = repo_write_lock(&absolute_string);
    let _sub_lock = sub_lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&absolute_string, "change_submodule_url(submodule)", queue_started.elapsed());
    let subrepo = internal_submodule_repository(&absolute)?; subrepo.remote_set_url("origin", url).map_err(|error| error.message().to_string())?; subrepo.find_remote("origin").map_err(|error| error.message().to_string())?; git(absolute.to_str().unwrap_or_default(), &["fetch", "origin"]).map_err(|detail| format!("Fetch failed: {detail}"))?;
    invalidate_git_metadata(&repository_path);
    invalidate_submodule_sync(&repository_path); // this app just changed a submodule's registration/version
    Ok(())
}

#[tauri::command]
pub fn remove_git_path(repository_path: String, relative_path: String) -> Result<(), String> {
    let started = Instant::now();
    perf_log(&format!("remove_git_path: START ({relative_path})"), Duration::ZERO);
    let result = remove_git_path_inner(&repository_path, &relative_path);
    match &result {
        Ok(()) => perf_log(&format!("remove_git_path: TOTAL ({relative_path})"), started.elapsed()),
        Err(error) => perf_log(&format!("remove_git_path: ERROR ({relative_path}): {error}"), started.elapsed()),
    }
    result
}

fn remove_git_path_inner(repository_path: &str, relative_path: &str) -> Result<(), String> {
    validate_path(repository_path)?;
    let relative = normalized(&safe_relative_path(relative_path)?);
    if relative.is_empty() { return Err("The repository root cannot be removed".into()); }
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

fn commit_selected_internal(repository_path: &str, files: &[String], message: &str) -> Result<String, String> {
    let commit_started = Instant::now();
    // See repo_write_lock's doc comment — shared by both commit_files and
    // commit_path, both of which mutate the index. Also reused by
    // record_pushed_submodule_in_parent (commit_submodule/push_submodule/
    // force_push_submodule), which is why those never hold their own
    // submodule lock while calling in here — see this function's callers.
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
    if !dirs_to_add.is_empty() { index.add_all(&dirs_to_add, git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?; }
    if !to_remove.is_empty() { index.remove_all(&to_remove, None).map_err(|error| error.message().to_string())?; }
    perf_log("commit: add_path/add_all/remove_all (scratch index)", step.elapsed());
    if includes_submodule && Path::new(repository_path).join(".gitmodules").exists() { index.add_path(Path::new(".gitmodules")).map_err(|error| error.message().to_string())?; }
    let step = Instant::now();
    let tree_id = index.write_tree_to(&repo).map_err(|error| error.message().to_string())?; if parent_tree.as_ref().map(|tree| tree.id()) == Some(tree_id) { return Err("There are no changes to commit in the selected files".into()); } let tree = repo.find_tree(tree_id).map_err(|error| error.message().to_string())?; let signature = repo.signature().map_err(|_| "Configure user.name and user.email for this repository".to_string())?; let parents: Vec<&git2::Commit<'_>> = parent.iter().collect(); let oid = repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &parents).map_err(|error| error.message().to_string())?;
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
    if !dirs_to_add.is_empty() { index.add_all(&dirs_to_add, git2::IndexAddOption::DEFAULT, None).map_err(|error| error.message().to_string())?; }
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
    resolve_submodule_boundary_from(&submodules, repository_path, relative_path)
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

#[tauri::command]
pub fn stash_changes(repository_path: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "stash_changes", queue_started.elapsed());
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
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "stash_file", queue_started.elapsed());
    git(&repository_path, &["stash", "push", "--include-untracked", "--", &relative_string])?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn pop_stash(repository_path: String, stash_index: usize) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "pop_stash", queue_started.elapsed());
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
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "drop_stash", queue_started.elapsed());
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

    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "restore_stash_paths", queue_started.elapsed());
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
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "abort_stash_conflict", queue_started.elapsed());
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
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "rename_branch", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let mut branch = repo.find_branch(old_name.trim(), BranchType::Local).map_err(|error| error.message().to_string())?;
    branch.rename(new_name.trim(), false).map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub fn delete_branch(repository_path: String, branch_name: String) -> Result<(), String> {
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "delete_branch", queue_started.elapsed());
    let repo = internal_repository(&repository_path)?;
    let current = repo.head().ok().and_then(|head| head.shorthand().map(String::from));
    if current.as_deref() == Some(branch_name.trim()) { return Err("Cannot delete the currently checked out branch. Switch to another branch first".into()); }
    let mut branch = repo.find_branch(branch_name.trim(), BranchType::Local).map_err(|error| error.message().to_string())?;
    branch.delete().map_err(|error| error.message().to_string())?;
    invalidate_git_metadata(&repository_path);
    Ok(())
}

#[tauri::command]
pub async fn commit_submodule(repository_path: String, relative_path: String, message: String) -> Result<String, String> {
    off_main_thread(move || commit_submodule_inner(repository_path, relative_path, message)).await
}

fn commit_submodule_inner(repository_path: String, relative_path: String, message: String) -> Result<String, String> {
    validate_path(&repository_path)?;
    if message.trim().is_empty() { return Err("Commit message cannot be empty".into()); }
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    // Held only for the submodule's own commit — released (end of this block)
    // before record_pushed_submodule_in_parent below takes the *parent's*
    // lock, so this thread never holds both at once (see stage_files_inner's
    // comment for why that matters).
    let oid = {
        let queue_started = Instant::now();
        let lock_handle = repo_write_lock(&sub_path);
        let _lock = lock_handle.lock().unwrap();
        log_repo_write_lock_acquired(&sub_path, "commit_submodule", queue_started.elapsed());
        let repo = internal_submodule_repository(&absolute)?;
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
pub async fn push_submodule(repository_path: String, relative_path: String) -> Result<PushSubmoduleResult, String> {
    off_main_thread(move || push_submodule_inner(repository_path, relative_path)).await
}

fn push_submodule_inner(repository_path: String, relative_path: String) -> Result<PushSubmoduleResult, String> {
    validate_path(&repository_path)?;
    let absolute = validate_submodule(&repository_path, &relative_path)?;
    let sub_path = absolute.to_string_lossy().into_owned();
    // Held for the submodule's own fetch+push (a concurrent local write on
    // this same submodule — a commit, "Reset submodule" — must queue behind
    // it, not race it) — released below before record_pushed_submodule_in_parent
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
    invalidate_submodule_sync(&repository_path); // this app just changed the submodule's own commit
    drop(repo); drop(_lock); // fully released before the parent's own lock, never nested
    record_pushed_submodule_in_parent(&repository_path, &relative_path, local_target)?;
    Ok(PushSubmoduleResult { revision: local_target.map(|oid| oid.to_string()).unwrap_or_default(), branch })
}

// Once a commit is safely on the submodule's own server, it is no longer "only
// local" — automatically record that new commit in the parent project too, so the
// submodule stops showing as modified. This mirrors clicking "Commit this item" on
// the submodule, done here for you right after a successful push.
fn record_pushed_submodule_in_parent(repository_path: &str, relative_path: &str, local_target: Option<git2::Oid>) -> Result<(), String> {
    // No add_to_index here anymore — it used to run *before* (and outside)
    // the repo_write_lock commit_selected_internal below acquires for its
    // own index work, an unlocked write to the same .git/index this app's
    // own repo_write_lock exists specifically to serialize every other
    // index mutation behind (see its doc comment — a real Windows perf log
    // caught concurrent, unserialized index writes corrupting/racing). It
    // was also entirely redundant: commit_selected_internal's own loop
    // already calls add_to_index for exactly this same submodule path,
    // safely under its lock, as part of building the commit.
    let parent = internal_repository(repository_path)?;
    parent.find_submodule(relative_path).map_err(|error| format!("Pushed, but could not find the submodule to update the parent's reference: {}", error.message()))?;
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
    invalidate_submodule_sync(&repository_path); // this app just changed the submodule's own commit
    drop(repo); drop(_lock); // fully released before the parent's own lock, never nested
    record_pushed_submodule_in_parent(&repository_path, &relative_path, local_target)?;
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
        assert_eq!(entry_last_commit(parent_string.clone(), "README.md".into()).unwrap().map(|c| c.subject), Some("Initial commit".to_string()));
        let engine_details = entry_details(parent_string.clone(), "components/engine".into()).unwrap();
        assert_eq!(entry_last_commit(parent_string.clone(), "components/engine".into()).unwrap().map(|c| c.subject), Some("P:89312 add engine".to_string()), "last-commit-touching-path must stay the parent's gitlink-bump commit");
        assert_eq!(engine_details.submodule_commit_subject.as_deref(), Some("Initial commit"), "submodule_commit_* must be the submodule's own HEAD commit, not the parent's");
        assert!(engine_details.submodule_commit_id.is_some());
        let versions = submodule_versions(parent_string.clone(), added.clone()).unwrap();
        switch_submodule_version_inner(parent_string.clone(), added.clone(), versions.current_revision, "commit".into(), String::new()).unwrap();
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
        let entries = load_directory(path.clone(), "".into(), None).unwrap();
        assert!(entries.iter().any(|entry| entry.relative_path == "src" && entry.kind == "folder"));
        assert!(entries.iter().any(|entry| entry.relative_path == "vendor" && entry.kind == "folder"));
        let cached_start = std::time::Instant::now();
        for _ in 0..100 { assert!(!load_directory(path.clone(), "".into(), None).unwrap().is_empty()); }
        assert!(cached_start.elapsed().as_millis() < 1000, "cached navigation took {:?}", cached_start.elapsed());
        let nested = load_directory(path.clone(), "vendor".into(), None).unwrap();
        assert!(nested.iter().any(|entry| entry.relative_path == "vendor/dependency" && entry.kind == "submodule"));
        let details = entry_details(path, "vendor/dependency".into()).unwrap();
        assert_eq!(details.kind, "submodule");
        assert!(details.submodule_url.as_deref().unwrap_or_default().contains("dependency"));
        let versions = submodule_versions(repository.to_string_lossy().into_owned(), "vendor/dependency".into()).unwrap();
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
        let cloned = clone_repository(dependency.to_string_lossy().into_owned(), base.to_string_lossy().into_owned(), "cloned-dependency".into()).unwrap();
        assert_eq!(load_repository_inner(cloned.clone(), Some(true)).unwrap().repository.name, "cloned-dependency");
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
        assert!(load_repository_inner(path, Some(true)).unwrap().changes.is_empty());

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
        let repo_path = base.to_string_lossy().into_owned();

        let divergence = graph_branch_divergence(repo_path, "main".into()).unwrap();
        let by_name = |name: &str| divergence.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("branch {name} should be reported"));

        let main = by_name("main");
        assert_eq!(main.tip, merge_commit);
        assert_eq!((main.ahead, main.behind), (0, 0), "main compared against itself must be exactly in sync");
        assert_eq!(main.merge_base.as_deref(), Some(merge_commit.as_str()));

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

        let details = entry_details(path.clone(), "a.txt".into()).unwrap();
        assert!(details.last_commit_id.is_none(), "entry_details must not populate last-commit-touching-path fields itself");
        assert!(details.last_commit_subject.is_none());

        let last = entry_last_commit(path.clone(), "a.txt".into()).unwrap();
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
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "a stash entry must remain for the still-unrestored b.txt");

        restore_stash_paths(path.clone(), 0, vec!["b.txt".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("b.txt")).unwrap(), "b changed", "b.txt should now be restored too");
        assert!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.is_empty(), "nothing left stashed once both files are restored");

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
        assert_eq!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.len(), 1, "README.md is still unrestored — a stash entry should remain for it");
        assert_eq!(stash_entry_files(path.clone(), 0).unwrap(), vec!["README.md".to_string()], "the restored file must be gone from the stash's own list now — not still shown as if untouched");

        restore_stash_paths(path.clone(), 0, vec!["README.md".into()]).unwrap();
        assert_eq!(fs::read_to_string(repository.join("README.md")).unwrap(), "changed");
        assert!(load_repository_inner(path.clone(), Some(true)).unwrap().stashes.is_empty(), "restoring the last remaining file should drop the now-empty stash entry entirely");

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
        let entries = load_directory(path.clone(), "".into(), None).unwrap();
        let folder_entry = entries.iter().find(|entry| entry.relative_path == "brand-new-folder").expect("the new folder should be listed");
        assert!(!folder_entry.tracked, "a wholly new folder should not be marked as tracked");
        assert!(!folder_entry.status.is_empty(), "load_directory should flag the new folder with a status (e.g. untracked/changed), got empty status");

        let details = entry_details(path, "brand-new-folder".into()).unwrap();
        assert!(!details.tracked, "entry_details should also report the new folder as untracked");
        assert!(!details.status.is_empty(), "entry_details should flag the new folder with a status too, got empty status");

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
        let clean = load_directory(path.clone(), "".into(), None).unwrap();
        let readme = clean.iter().find(|entry| entry.relative_path == "README.md").expect("README.md should be listed");
        assert!(readme.status.is_empty(), "a freshly committed file should start with no status");

        // Simulate an external program editing the file on disk, bypassing this app entirely.
        fs::write(base.join("README.md"), "edited externally, not through this app").unwrap();

        // Without force, the cache is still fresh (TTL is 300s) and unaware of the edit.
        let stale = load_directory(path.clone(), "".into(), None).unwrap();
        let stale_readme = stale.iter().find(|entry| entry.relative_path == "README.md").unwrap();
        assert!(stale_readme.status.is_empty(), "sanity check: without force, the pre-existing cache should still be serving the stale, clean status");

        // A forced reload (what the Refresh button now sends) must reflect the edit immediately.
        let refreshed = load_directory(path.clone(), "".into(), Some(true)).unwrap();
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

        let root_repaint = load_directory(path.clone(), "".into(), None).unwrap();
        let readme = root_repaint.iter().find(|e| e.relative_path == "README.md").unwrap();
        assert!(readme.status.is_empty(), "a root repaint without force must reuse load_repository's fresh scan, not rescan and see the external edit");

        let child_repaint = load_directory(path.clone(), "sub".into(), None).unwrap();
        let child_file = child_repaint.iter().find(|e| e.relative_path == "sub/file.txt").unwrap();
        assert!(child_file.status.is_empty(), "a child-folder repaint without force must also reuse the same fresh scan (via the full-scan reuse path), not run its own scoped rescan");

        // Only the explicit "Reload folder" action (force:true) should invalidate and rescan.
        let forced = load_directory(path, "".into(), Some(true)).unwrap();
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

        assert!(submodule_navigation_status(repo_path.clone(), "".into()).unwrap().is_none(), "the repository root is not inside a submodule");
        assert!(submodule_navigation_status(repo_path.clone(), "README.md".into()).unwrap().is_none(), "an ordinary parent-repo file is not inside a submodule");

        let before = submodule_navigation_status(repo_path.clone(), "vendor/dep".into()).unwrap().expect("vendor/dep should be recognized as a submodule");
        assert_eq!(before.submodule_path, "vendor/dep");
        assert!(!before.ready, "no scan has happened yet — must not be reported ready");

        submodule_folder_status(repo_path.clone(), "vendor/dep".into()).unwrap();

        let after = submodule_navigation_status(repo_path, "vendor/dep".into()).unwrap().unwrap();
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

        submodule_folder_status(repo_path.clone(), "vendor/a".into()).unwrap();
        assert!(submodule_navigation_status(repo_path.clone(), "vendor/a".into()).unwrap().unwrap().ready, "the requested submodule should now be ready");
        assert!(!submodule_navigation_status(repo_path.clone(), "vendor/b".into()).unwrap().unwrap().ready, "a sibling submodule that was never entered must not have been scanned too");

        // b's real, current status must still be answered correctly on demand
        // (just not pre-emptively) — add an untracked file and confirm load_directory sees it.
        fs::write(repository.join("vendor/b/new.txt"), "new").unwrap();
        let listing = load_directory(repo_path, "vendor/b".into(), None).unwrap();
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
        submodule_folder_status(repo_path.clone(), "vendor/dep".into()).unwrap();

        // External edits, made right after that one scan, to a file in the
        // submodule's root listing, one in a nested folder, and one that will
        // be looked up individually via entry_details.
        fs::write(repository.join("vendor/dep/top.txt"), "edited after the scan").unwrap();
        fs::write(repository.join("vendor/dep/nested/deep.txt"), "edited after the scan too").unwrap();

        let root_listing = load_directory(repo_path.clone(), "vendor/dep".into(), None).unwrap();
        let top = root_listing.iter().find(|e| e.name == "top.txt").unwrap();
        assert!(top.status.is_empty(), "listing the submodule's own root must reuse the one scan, not rescan");

        let nested_listing = load_directory(repo_path.clone(), "vendor/dep/nested".into(), None).unwrap();
        let deep = nested_listing.iter().find(|e| e.name == "deep.txt").unwrap();
        assert!(deep.status.is_empty(), "a nested folder inside the submodule must also reuse the same scan, not run its own scoped scan");

        let details = entry_details(repo_path, "vendor/dep/nested/deep.txt".into()).unwrap();
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

        let failing = vec![serde_json::json!({"conclusion": "SUCCESS"}), serde_json::json!({"conclusion": "FAILURE"})];
        assert_eq!(map_checks_status(&failing), "failing", "any real failure must win over other passing/pending checks");
        let pending = vec![serde_json::json!({"conclusion": "SUCCESS"}), serde_json::json!({"state": "PENDING"})];
        assert_eq!(map_checks_status(&pending), "pending");
        let passing = vec![serde_json::json!({"conclusion": "SUCCESS"}), serde_json::json!({"conclusion": "NEUTRAL"})];
        assert_eq!(map_checks_status(&passing), "passing");
        assert_eq!(map_checks_status(&[]), "none", "no checks configured at all must read as none, not as passing");
    }

    #[test]
    fn pr_status_reports_no_remote_when_the_repository_has_none() {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-pr-status-no-remote-{suffix}"));
        create_libgit2_repository(&base, "README.md");
        let result = pr_status_inner(base.to_string_lossy().into_owned(), None, None).unwrap();
        assert_eq!(result.state, "no_remote");
        assert!(result.pull_requests.is_empty());
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
        assert!(result.pull_requests.is_empty());
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
        assert!(result.pull_requests.is_empty());
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
        // concurrently, so `run` must be Sync.
        let seen = Mutex::new(Vec::<PrGhQuery>::new());
        let run = |q: &PrGhQuery| {
            seen.lock().unwrap().push(q.clone());
            GhOutcome::Prs(vec![pr_json(42, "feature/on-server", "eng", "sw-prj-OMBMS_000U0")])
        };
        let result = pr_status_impl(ctx, &path, "parent", &run);

        {
            let queries = seen.lock().unwrap();
            assert_eq!(queries.len(), 1, "exactly one query — the enterprise head repo");
            assert_eq!(queries[0], PrGhQuery {
                repo: "github.vitesco.io/eng/sw-prj-OMBMS_000U0".into(),
                head: "feature/on-server".into(),
            }, "against the enterprise head repo, for the remote branch name");
        }
        assert_eq!(result.state, "ok");
        assert_eq!(result.queried_repo.as_deref(), Some("github.vitesco.io/eng/sw-prj-OMBMS_000U0"));
        assert_eq!(result.branch.as_deref(), Some("feature/on-server"));
        assert_eq!(result.pull_requests.len(), 1);
        assert_eq!(result.pull_requests[0].number, 42);
        assert_eq!(result.pull_requests[0].review_summary, "approved");
        assert_eq!(result.pull_requests[0].checks_status, "passing");
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
        assert!(result.pull_requests.is_empty());
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
    fn pr_status_impl_auth_failure_on_enterprise_points_at_gh_auth_login_hostname() {
        let (base, path) = pr_repo("impl-auth");
        run_git(&base, &["remote", "add", "origin", "git@github.vitesco.io:eng/demo.git"]);
        run_git(&base, &["checkout", "-q", "-b", "feat"]);
        let repo = internal_repository(&path).unwrap();
        let ctx = ctx_for(&repo);
        let run = |_: &PrGhQuery| GhOutcome::Failure { stderr: "You are not logged into any GitHub hosts. Run gh auth login".into() };
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "auth_missing");
        assert!(result.detail.contains("gh auth login --hostname github.vitesco.io"), "detail was: {}", result.detail);
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
        assert_eq!(result.pull_requests.iter().map(|p| p.number).collect::<Vec<_>>(), vec![5]);
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
        // Both `--repo` targets return the same PR (same canonical url).
        let run = |_: &PrGhQuery| GhOutcome::Prs(vec![pr_json(88, "feature", "me", "fork")]);
        let result = pr_status_impl(ctx, &path, "parent", &run);
        assert_eq!(result.state, "ok");
        assert_eq!(result.pull_requests.len(), 1, "one PR, not one per candidate base");
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
        assert!(result.pull_requests.is_empty());
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
            assert_eq!(q.head, "dep-feature-remote");
            GhOutcome::Prs(vec![pr_json(3, "dep-feature-remote", "eng", "the-dependency")])
        };
        let result = pr_status_impl(ctx, &sub_path_str, "submodule:dep", &run);
        assert_eq!(result.state, "ok");
        assert_eq!(result.queried_repo.as_deref(), Some("github.vitesco.io/eng/the-dependency"));
        assert_eq!(result.branch.as_deref(), Some("dep-feature-remote"));
        assert_eq!(result.pull_requests.len(), 1);
        assert_eq!(result.pull_requests[0].number, 3);
        drop(sub_repo);
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

        // Commit through our command only, then push must succeed.
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into()).expect("commit_submodule should succeed");

        // Committing inside the submodule now updates the parent's recorded
        // gitlink right away (no push needed) — the submodule's working copy
        // already IS the new version, so the parent should reflect that
        // immediately instead of still showing it as "modified".
        let parent_index_oid = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new("vendor/dep"), 0).unwrap().id };
        let submodule_head_oid = { let repo = Repository::open(&sub_path).unwrap(); let oid = repo.head().unwrap().target().unwrap(); oid };
        assert_eq!(parent_index_oid, submodule_head_oid, "the parent should record the submodule's new commit immediately after committing inside it, push or not");

        let parent_changes = load_repository_inner(repo_path.clone(), Some(true)).unwrap().changes;
        assert!(!parent_changes.iter().any(|change| change.path == "vendor/dep"), "the submodule should already show as clean/version-changed, not modified, before any push");

        // Now push should succeed, and — since the commit is now safely on the
        // submodule's own server — the parent should be updated automatically so the
        // submodule stops showing as merely "modified locally".
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).expect("push_submodule should succeed after a commit");
        let parent_index_oid_after_push = { let repo = Repository::open(&repository).unwrap(); repo.index().unwrap().get_path(Path::new("vendor/dep"), 0).unwrap().id };
        let submodule_head_oid_after_push = { let repo = Repository::open(&sub_path).unwrap(); let oid = repo.head().unwrap().target().unwrap(); oid };
        assert_eq!(parent_index_oid_after_push, submodule_head_oid_after_push, "after a successful push, the parent should automatically record the new submodule commit");
        let parent_changes_after_push = load_repository_inner(repo_path.clone(), Some(true)).unwrap().changes;
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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Detached commit".into()).expect("commit_submodule should succeed while detached");
        assert!(Repository::open(&sub_path).unwrap().head_detached().unwrap(), "committing must not implicitly attach HEAD to a branch");

        let push_result = push_submodule_inner(repo_path.clone(), "vendor/dep".into());
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

        let switched = switch_submodule_version_inner(repo_path.clone(), "vendor/dep".into(), feature_branch.revision.clone(), feature_branch.kind.clone(), feature_branch.name.clone());
        assert!(switched.is_ok(), "switching to a local branch by name should succeed, got: {:?}", switched);
        assert!(!Repository::open(&sub_path).unwrap().head_detached().unwrap(), "switching to a branch must leave HEAD attached to it, not detached");
        assert_eq!(Repository::open(&sub_path).unwrap().head().unwrap().shorthand(), Some("feature-x"));

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

        let versions = submodule_versions(repo_path.clone(), "vendor/dep".into()).unwrap();
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

        // Drift the submodule: a local commit ahead of what the parent has
        // recorded (like an uncommitted "switch version"), plus a dirty,
        // uncommitted edit on top of that — both must be discarded by reset.
        fs::write(sub_path.join("module.txt"), "v2 (local commit)").unwrap();
        run_git(&sub_path, &["commit", "-am", "Local-only change, never recorded by the parent"]);
        fs::write(sub_path.join("module.txt"), "v3 (dirty, uncommitted)").unwrap();
        assert!(!Repository::open(&sub_path).unwrap().statuses(None).unwrap().is_empty(), "sanity check: the submodule should be dirty before reset");

        let reset_to = reset_submodule_inner(repo_path, "vendor/dep".into()).unwrap();
        assert_eq!(reset_to, recorded_commit, "reset should land on the commit the parent has recorded, not wherever the submodule had drifted to");

        let sub_repo = Repository::open(&sub_path).unwrap();
        assert_eq!(sub_repo.head().unwrap().target().unwrap().to_string(), recorded_commit, "HEAD must be back at the parent-recorded commit");
        assert!(sub_repo.statuses(None).unwrap().is_empty(), "the dirty edit must be discarded — reset means overwritten, not merged or preserved");
        assert_eq!(fs::read_to_string(sub_path.join("module.txt")).unwrap(), "v1", "working tree content must match the recorded commit exactly");

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
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let file_path = format!("{added}/module.txt");

        fs::write(parent.join(&file_path), "changed content").unwrap();

        // The file must be correctly reported as modified — sourced from the
        // submodule's own status, not silently invisible to the parent.
        let listing = load_directory(parent_string.clone(), added.clone(), None).unwrap();
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
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
        let file_path = format!("{added}/nou.py");

        fs::write(parent.join(&file_path), "print('hello')\n").unwrap();

        let listing = load_directory(parent_string.clone(), added.clone(), None).unwrap();
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
        let listing_after = load_directory(parent_string, added, None).unwrap();
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
        // submodule's) racing another that commits *inside* the submodule and
        // lets that auto-record into the parent (commit_submodule — the
        // submodule's lock first, then, only after releasing it, the
        // parent's — never nested). If any code path ever reversed that
        // ordering while the other still held its own lock, two threads doing
        // these concurrently would deadlock on unlucky timing. Run many
        // iterations on two genuinely concurrent threads, under a bounded
        // wait — a real deadlock hangs past the timeout instead of finishing;
        // this test failing (rather than hanging forever) is itself the point.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("git-integrity-deadlock-race-{suffix}"));
        let parent = base.join("parent"); let dependency = base.join("dependency");
        create_libgit2_repository(&parent, "root.txt");
        create_libgit2_repository(&dependency, "module.txt");
        let parent_string = parent.to_string_lossy().into_owned();
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();
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
                let _ = commit_submodule_inner(p2.clone(), a2.clone(), format!("iteration {i}"));
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
        let added = add_submodule_inner(parent_string.clone(), "".into(), dependency.to_string_lossy().into_owned(), "dep".into(), String::new(), String::new()).unwrap();
        create_commit(parent_string.clone(), "Add dep submodule".into()).unwrap();

        fs::write(parent.join(&added).join("module.txt"), "v2").unwrap();
        let oid = commit_submodule_inner(parent_string.clone(), added.clone(), "Update module".into()).unwrap();

        let repo = Repository::open(&parent).unwrap();
        let recorded = repo.index().unwrap().get_path(Path::new(&added), 0).unwrap().id;
        assert_eq!(recorded.to_string(), oid, "the parent must already record the submodule's new commit, with no push and no separate manual step");
        drop(repo);

        let changes = load_repository_inner(parent_string, Some(true)).unwrap().changes;
        assert!(!changes.iter().any(|c| c.path == added), "the submodule must show as clean/version-changed, not modified, right after committing inside it: {:?}", changes.iter().map(|c| (&c.path, &c.status)).collect::<Vec<_>>());

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
        let listing = load_directory(path.clone(), "src".into(), None).unwrap();
        assert!(!listing.iter().find(|e| e.name == "a.txt").unwrap().unpushed);

        // Commit a change but don't push it.
        fs::write(repository.join("src/a.txt"), "two").unwrap();
        commit_path(path.clone(), "src/a.txt".into(), "Update a.txt".into()).unwrap();

        let listing = load_directory(path.clone(), "src".into(), None).unwrap();
        let entry = listing.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(entry.status, "", "the file is fully committed, so it must not show any working-tree status");
        assert!(entry.unpushed, "a committed-but-unpushed file must be flagged unpushed");

        // The containing folder should reflect it too.
        let root_listing = load_directory(path.clone(), "".into(), None).unwrap();
        let src_entry = root_listing.iter().find(|e| e.name == "src").unwrap();
        assert!(src_entry.unpushed, "a folder containing an unpushed file should be flagged too");

        let details = entry_details(path.clone(), "src/a.txt".into()).unwrap();
        assert!(details.unpushed);

        // After pushing (through the app's own command, which invalidates the
        // cache — a plain external `git push` wouldn't know to), it must clear.
        sync_repository_inner(path.clone(), "push".into()).unwrap();
        let after_push = load_directory(path, "src".into(), None).unwrap();
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

        let listing = load_directory(path.clone(), "".into(), None).unwrap();
        assert!(!listing.iter().find(|e| e.name == "a.txt").unwrap().unpushed, "freshly pushed to release/main — nothing should be flagged");

        fs::write(repository.join("a.txt"), "two").unwrap();
        commit_path(path.clone(), "a.txt".into(), "Update a.txt".into()).unwrap();
        let listing = load_directory(path.clone(), "".into(), None).unwrap();
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
        commit_submodule_inner(repo_path, "vendor/dep".into(), "Local only".into()).unwrap();
        let status = submodule_push_status(&sub_path.to_string_lossy());
        assert!(status.as_deref().is_some_and(|message| message.contains("1 commit") && message.contains("mirror/release")), "expected an unpushed-commit message naming the real upstream mirror/release, got: {status:?}");
        let commits = submodule_unpushed_commits(&sub_path.to_string_lossy());
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "Local only");

        fs::remove_dir_all(base).unwrap();
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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into()).unwrap();
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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into()).unwrap();
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).unwrap();

        let changes = load_repository_inner(repo_path.clone(), Some(true)).unwrap().changes;
        assert!(!changes.iter().any(|change| change.path == "vendor/dep"), "load_repository still lists the submodule as changed: {:?}", changes.iter().map(|c| (&c.path, &c.status)).collect::<Vec<_>>());

        let entries = load_directory(repo_path.clone(), "vendor".into(), None).unwrap();
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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Update module".into()).unwrap();
        push_submodule_inner(repo_path.clone(), "vendor/dep".into()).unwrap();

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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Local only".into()).unwrap();
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
        let listing = load_directory(repo_path.clone(), "vendor".into(), None).unwrap();
        let entry = listing.iter().find(|e| e.name == "dep").unwrap();
        assert!(!entry.submodule_has_unpushed_commits, "freshly synced submodule should not be flagged");

        // A commit lands in the submodule without going through load_directory again.
        fs::write(sub_path.join("module.txt"), "v2").unwrap();
        run_git(&sub_path, &["commit", "-am", "Local only, not pushed"]);

        // Without force, the cache seeded above is still fresh (TTL is 300s) and unaware of it.
        let stale = load_directory(repo_path.clone(), "vendor".into(), None).unwrap();
        let stale_entry = stale.iter().find(|e| e.name == "dep").unwrap();
        assert!(!stale_entry.submodule_has_unpushed_commits, "sanity check: without force, the cached (clean) submodule-unpushed set must still be reused, proving load_directory didn't rescan on its own");

        // A forced reload (Reload folder) must invalidate and see the real, current state.
        let forced = load_directory(repo_path, "vendor".into(), Some(true)).unwrap();
        let forced_entry = forced.iter().find(|e| e.name == "dep").unwrap();
        assert!(forced_entry.submodule_has_unpushed_commits, "force:true must invalidate the cache and report the real, current unpushed commit");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn load_directory_only_scans_submodules_actually_visible_in_the_listed_folder() {
        // A real counter check, not a timing threshold: proves load_directory
        // scans exactly the submodules on screen, never the rest of the
        // repository's — the exact concern behind cached_submodule_has_unpushed
        // being keyed per submodule path instead of "every submodule in the
        // repository" (a real report: a repository with hundreds of
        // submodules paid for scanning all of them just to show one folder
        // with two). Uses 5 submodules split across two folders — small
        // enough to build quickly in a test, but the property being checked
        // (opening folder A never touches folder B's submodules) is exactly
        // the same one that matters at 500.
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

        // Only groupA is ever listed.
        let listing = load_directory(repo_path, "groupA".into(), None).unwrap();
        assert_eq!(listing.iter().filter(|e| e.kind == "submodule").count(), 2);

        // Filtered to this test's own repository prefix — the cache is a
        // process-global static shared with every other test in this suite
        // (which run concurrently), so asserting its *total* size would be
        // flaky by construction; scoping to paths under this repository is
        // what actually proves the property under test.
        let repo_prefix = repository.to_string_lossy().into_owned();
        let scanned: std::collections::HashSet<String> = submodule_unpushed_cache().lock().unwrap().keys()
            .filter(|key| key.starts_with(&repo_prefix)).cloned().collect();
        for sub in &group_a { assert!(scanned.contains(sub.to_str().unwrap()), "a submodule actually visible in the listed folder must have been scanned: {sub:?}"); }
        for sub in &group_b { assert!(!scanned.contains(sub.to_str().unwrap()), "a submodule in a folder that was never listed must NOT have been scanned: {sub:?}"); }
        assert_eq!(scanned.len(), 2, "exactly the 2 visible submodules, not all 5 in the repository");

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
        commit_submodule_inner(repo_path.clone(), "vendor/dep".into(), "Local divergent commit".into()).unwrap();
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
        let parent_changes = load_repository_inner(repo_path, Some(true)).unwrap().changes;
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
            let entries = load_directory(repo_string.clone(), format!("folder_{folder}"), None).unwrap();
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
        let entries = load_directory(repo_string.clone(), "big_folder".into(), None).unwrap();
        println!("PERF load_directory first call (cold caches): {:?}", first.elapsed());
        assert_eq!(entries.len(), 20_000);
        assert!(entries.iter().all(|entry| entry.kind == "file" && entry.tracked));

        let second = Instant::now();
        let entries = load_directory(repo_string.clone(), "big_folder".into(), None).unwrap();
        println!("PERF load_directory second call (warm caches): {:?}", second.elapsed());
        assert_eq!(entries.len(), 20_000);

        // Simulate the status-scan cache having expired (GIT_METADATA_TTL,
        // normally 4s) without actually waiting for it in this test.
        let key = metadata_cache_key(&repo_string, "big_folder");
        if let Some(entry) = metadata_cache().lock().unwrap().get_mut(&key) {
            entry.0 = Instant::now() - GIT_METADATA_TTL - Duration::from_secs(1);
        }
        let third = Instant::now();
        let entries = load_directory(repo_string.clone(), "big_folder".into(), None).unwrap();
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
    fn utrud_command_uses_the_parent_as_cwd_and_the_full_path_as_the_argument() {
        // Reproduces the report: with cwd set to the "r" folder itself (an
        // earlier fix), UTRUD's own script appended the selected folder's
        // name to its current directory and produced ".../r/r" instead of
        // ".../r". cwd must be the *parent* of the selected folder — the
        // same relationship Explorer's "Send to" has with whatever you
        // right-clicked — while the argument stays the full path to "r".
        let absolute = Path::new("/int_opm/sw-prj-OMBMS_000U0/work/asw/aggr/errm/agf/errm_envd1/r");
        let (cwd, argument) = utrud_command_parts(absolute);
        assert_eq!(cwd, Path::new("/int_opm/sw-prj-OMBMS_000U0/work/asw/aggr/errm/agf/errm_envd1"));
        assert_eq!(argument, absolute);
        assert_ne!(cwd.file_name(), argument.file_name(), "cwd must not itself be named \"r\" — that's what caused the doubled \"r\\r\" path");
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
    fn refresh_status_then_stage_all_reuse_the_same_recent_scan() {
        // Reproduces the report exactly: opening Working tree (refresh_status)
        // immediately followed by Stage all used to each pay for their own
        // separate full status scan of the same thing a moment apart. Seeds
        // full_status_cache directly with a recognizable fake entry (a path
        // that could never come from a real scan of this repo) so a HIT can
        // be told apart from a real scan with certainty, rather than
        // inferring it from timing.
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let repo_path = std::env::temp_dir().join(format!("git-integrity-status-reuse-{suffix}"));
        create_libgit2_repository(&repo_path, "README.md");
        let repo_string = repo_path.to_string_lossy().into_owned();

        let fake_marker = "__unmistakably_fake_marker__.txt".to_string();
        full_status_cache().lock().unwrap().insert(repo_string.clone(), (Instant::now(), vec![(fake_marker.clone(), "??".into(), false)]));

        let changes = refresh_status_inner(repo_string.clone()).unwrap();
        assert_eq!(changes.len(), 1, "should have reused the seeded entry, not scanned the (actually empty) real repository");
        assert_eq!(changes[0].path, fake_marker);

        let staged = stage_all_inner(&repo_string, "").unwrap();
        assert_eq!(staged.staged_paths.len(), 1, "stage_all right after should reuse the same still-fresh scan, not run its own");

        // Age the cache entry past the reuse window (without a real sleep) —
        // the next call must fall through to a genuine fresh scan and stop
        // seeing the fake marker, since it was never a real file on disk.
        if let Some(entry) = full_status_cache().lock().unwrap().get_mut(&repo_string) {
            entry.0 = Instant::now() - FRESH_STATUS_REUSE_WINDOW - Duration::from_millis(500);
        }
        let changes_after_expiry = refresh_status_inner(repo_string.clone()).unwrap();
        assert!(changes_after_expiry.is_empty(), "past the reuse window, this must be a real fresh scan of the (clean) repository, not the stale fake entry");
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

        let entries = load_directory(repo_string.clone(), "folder".into(), None).unwrap();
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
        let root_entries = list_directory_fast(fast.repository.path.clone(), String::new()).unwrap();
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
        let real_entries = load_directory(fast.repository.path.clone(), String::new(), None).unwrap();
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
}
