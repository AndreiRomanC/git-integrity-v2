//! Explicit recovery, never an automatic retry or part of reset/checkout.
//! A lock's age is not ownership evidence: require user confirmation as well
//! as local activity checks, a stable file identity and a fresh inspection.
use super::*;
use std::{fs::File, time::SystemTime};

#[cfg(windows)]
mod windows;

const MIN_AGE: Duration = Duration::from_secs(60);
const STABILITY_WAIT: Duration = Duration::from_millis(750);
const TICKET_TTL: Duration = Duration::from_secs(300);
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
static TICKETS: OnceLock<Mutex<HashMap<String, Ticket>>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    bytes: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    identity: (u64, u128),
}

struct Ticket {
    repository: String,
    lock_path: PathBuf,
    fingerprint: Fingerprint,
    inspected: Instant,
}

#[derive(Serialize)]
pub struct LockInspection {
    lock_path: String,
    bytes: u64,
    age_seconds: u64,
    ticket: String,
}

#[derive(Serialize)]
pub struct LockRecovery {
    backup_path: String,
}

fn tickets() -> &'static Mutex<HashMap<String, Ticket>> {
    TICKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_path(repository_path: &str) -> Result<PathBuf, String> {
    validate_path(repository_path)?;
    // Never discover an ancestor or accept a lock path supplied by JavaScript.
    // The existing exact-root helper handles submodule gitfiles/worktrees.
    let repo = internal_submodule_repository(Path::new(repository_path))?;
    Ok(fs::canonicalize(repo.path()).map_err(|e| format!("Cannot resolve the Git directory: {e}"))?.join("index.lock"))
}

fn fingerprint(file: &File) -> Result<Fingerprint, String> {
    let meta = file.metadata().map_err(|e| format!("Cannot inspect the lock: {e}"))?;
    if !meta.is_file() { return Err("The index lock is not a regular file. Nothing was changed.".into()); }
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 { return Err("The index lock has multiple hard links. Recovery was refused.".into()); }
        (meta.dev(), meta.ino() as u128)
    };
    #[cfg(windows)]
    let identity = windows::identity(file)?;
    Ok(Fingerprint { bytes: meta.len(), modified: meta.modified().map_err(|e| e.to_string())?, created: meta.created().ok(), identity })
}

fn open_lock(path: &Path, exclusive: bool) -> Result<File, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound { "The index lock is no longer present. Retry your original action manually.".into() }
        else { format!("Cannot inspect the index lock: {e}") }
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("The index lock is not a regular file (or is a link). Nothing was changed.".into());
    }
    #[cfg(windows)]
    { windows::open(path, exclusive) }
    #[cfg(unix)]
    {
        let _ = exclusive;
        let file = File::open(path).map_err(|e| format!("Cannot open the index lock: {e}"))?;
        use std::os::unix::fs::MetadataExt;
        let actual = file.metadata().map_err(|e| e.to_string())?;
        if meta.dev() != actual.dev() || meta.ino() != actual.ino() { return Err("The lock changed during inspection. Check again.".into()); }
        Ok(file)
    }
}

fn age(stamp: &Fingerprint, now: SystemTime) -> Result<Duration, String> {
    now.duration_since(stamp.modified).map_err(|_| "The lock has a future timestamp. Recovery was refused.".into())
}

fn require_old(stamp: &Fingerprint, now: SystemTime) -> Result<Duration, String> {
    let elapsed = age(stamp, now)?;
    if elapsed < MIN_AGE { return Err("The index lock was recently written. Wait at least a minute, then check again. It was not moved.".into()); }
    Ok(elapsed)
}

// Deliberately conservative: a Git process in another repository also blocks
// recovery. Names cannot identify every libgit2 client; the user must close
// other Git applications. No command lines, usernames or process lists logged.
fn is_git_process(name: &str) -> bool {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    name == "git" || name.starts_with("git-") || name.starts_with("git ")
        || ["gitextensions", "gitkraken", "githubdesktop", "github desktop", "sourcetree", "tortoisegitproc", "tortoisegitmerge", "smartgit", "gitdrilldown", "git drilldown"].iter().any(|candidate| name == *candidate)
}

fn parse_processes(text: &str, windows: bool, own_pid: u32) -> Result<Vec<String>, String> {
    let mut count = 0;
    let mut blockers = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (pid, name) = if windows {
            // tasklist /FO CSV /NH: quoted image name, PID, session, ...
            let fields: Vec<_> = line.trim().trim_matches('"').split("\",\"").collect();
            if fields.len() < 2 { return Err("Windows process listing was not understood. Nothing was changed.".into()); }
            (fields[1].parse::<u32>().map_err(|_| "Invalid process ID in Windows process listing")?, fields[0].to_string())
        } else {
            let line = line.trim();
            let split = line.find(char::is_whitespace).ok_or("Process listing was not understood")?;
            (line[..split].parse::<u32>().map_err(|_| "Invalid process ID in process listing")?, line[split..].trim().to_string())
        };
        count += 1;
        if pid != own_pid && is_git_process(&name) { blockers.push(format!("{name} (PID {pid})")); }
    }
    if count == 0 { return Err("The process check returned no data. Nothing was changed.".into()); }
    Ok(blockers)
}

fn require_idle(path: &Path) -> Result<(), String> {
    if !active_git_commands(None).is_empty() { return Err("A Git command is still running in Git DrillDown. Wait for it to finish; the lock was not moved.".into()); }
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("tasklist.exe");
        cmd.args(["/FO", "CSV", "/NH"]).creation_flags(0x08000000);
        cmd
    };
    #[cfg(unix)]
    let mut command = { let mut cmd = Command::new("/bin/ps"); cmd.args(["-axo", "pid=,comm="]); cmd };
    command.env("LC_ALL", "C");
    let output = run_with_timeout_labeled(command, Duration::from_secs(5), "Process inspection", "5 seconds")?;
    if !output.status.success() { return Err("Cannot verify running processes. Recovery was refused; no lock was moved.".into()); }
    let blockers = parse_processes(&String::from_utf8_lossy(&output.stdout), cfg!(windows), std::process::id())?;
    if !blockers.is_empty() {
        return Err(format!("Close other Git operations/applications before recovery, including those in other repositories:\n{}\nNo process was stopped and no lock was moved.", blockers.into_iter().take(8).collect::<Vec<_>>().join("\n")));
    }
    #[cfg(unix)]
    {
        // Unix rename does not refuse open files. Fail closed if this additional
        // open-handle check is unavailable or cannot be interpreted.
        let mut command = Command::new("/usr/sbin/lsof");
        command.args(["-nP", "-F", "p", "--"]).arg(path);
        let output = run_with_timeout_labeled(command, Duration::from_secs(5), "Lock handle inspection", "5 seconds")?;
        if !output.stdout.is_empty() { return Err("A process has the index lock open. Nothing was changed.".into()); }
        if output.status.code() != Some(1) || !output.stderr.is_empty() { return Err("Cannot verify open handles for the lock. Nothing was changed.".into()); }
    }
    #[cfg(windows)]
    { let _ = path; /* Exclusive handle check is performed by windows::open. */ }
    Ok(())
}

fn inspect_inner(repository_path: &str, idle: impl Fn(&Path) -> Result<(), String>, wait: Duration, min_age: bool) -> Result<LockInspection, String> {
    let path = lock_path(repository_path)?;
    let repo_lock = repo_write_lock(repository_path);
    let _guard = repo_lock.try_lock().map_err(|_| "A repository operation is running. Wait for it to finish.")?;
    idle(&path)?;
    let first = fingerprint(&open_lock(&path, true)?)?;
    if min_age { require_old(&first, SystemTime::now())?; }
    std::thread::sleep(wait);
    idle(&path)?;
    let second = fingerprint(&open_lock(&path, true)?)?;
    if first != second { return Err("The index lock is changing. Recovery was refused.".into()); }
    let ticket = SEQUENCE.fetch_add(1, Ordering::Relaxed).to_string();
    let mut store = tickets().lock().unwrap();
    store.retain(|_, value| value.inspected.elapsed() < TICKET_TTL && value.repository != repo_lock_key(repository_path));
    if store.len() >= 16 { return Err("Too many pending recovery checks. Close other checks and retry later.".into()); }
    store.insert(ticket.clone(), Ticket { repository: repo_lock_key(repository_path), lock_path: path.clone(), fingerprint: second.clone(), inspected: Instant::now() });
    Ok(LockInspection { lock_path: path.to_string_lossy().into_owned(), bytes: second.bytes, age_seconds: age(&second, SystemTime::now())?.as_secs(), ticket })
}

fn recover_inner(repository_path: &str, ticket_id: &str, confirmed: bool, idle: impl Fn(&Path) -> Result<(), String>) -> Result<LockRecovery, String> {
    if !confirmed { return Err("Confirm that other Git operations/applications are closed before recovery.".into()); }
    let ticket = tickets().lock().unwrap().remove(ticket_id).ok_or("Recovery check expired or already used. Check the lock again.")?;
    if ticket.repository != repo_lock_key(repository_path) || ticket.inspected.elapsed() >= TICKET_TTL { return Err("Repository changed or recovery check expired. Inspect the lock again.".into()); }
    let repo_lock = repo_write_lock(repository_path);
    let _guard = repo_lock.try_lock().map_err(|_| "A repository operation started. Nothing was changed; check again.")?;
    let path = lock_path(repository_path)?;
    if path != ticket.lock_path { return Err("The Git directory changed. Nothing was changed.".into()); }
    idle(&path)?;
    // On Windows this handle denies read/write/delete sharing and is retained
    // through the by-handle rename, not closed between checking and moving.
    let file = open_lock(&path, true)?;
    if fingerprint(&file)? != ticket.fingerprint { return Err("The lock was replaced or changed after inspection. Nothing was changed; check again.".into()); }
    #[cfg(windows)]
    idle(&path)?;
    #[cfg(unix)]
    {
        // Recheck the name as late as possible. Unix has no Windows-style
        // mandatory sharing denial: the explicit close-other-apps confirmation
        // is essential, not a promise that a process snapshot proves ownership.
        if fingerprint(&open_lock(&path, false)?)? != ticket.fingerprint { return Err("The lock path changed. Nothing was moved.".into()); }
    }
    let backup_dir = path.parent().ok_or("Invalid Git directory")?.join(format!("index-lock-recovery-{}-{}-{ticket_id}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()));
    // create_dir is exclusive. No pre-existing backup can be overwritten.
    fs::create_dir(&backup_dir).map_err(|e| format!("Cannot create lock backup directory: {e}. Nothing was changed."))?;
    let backup = backup_dir.join("index.lock.backup");
    #[cfg(windows)]
    let result = windows::move_to_backup(&file, &backup);
    #[cfg(unix)]
    let result = fs::rename(&path, &backup).map_err(|e| format!("Cannot move the lock to its backup: {e}"));
    if let Err(error) = result { let _ = fs::remove_dir(&backup_dir); return Err(error); }
    // Never touch index or promote the lock's incomplete contents into index.
    invalidate_git_metadata(repository_path);
    Ok(LockRecovery { backup_path: backup.to_string_lossy().into_owned() })
}

fn log_result<T>(repository_path: &str, phase: &str, result: &Result<T, String>, started: Instant) {
    // No raw paths, output or process names; the UI retains the full reason.
    let outcome = match result { Ok(_) => "ok", Err(_) => "refused" };
    let reason = match result.as_ref().err().map(|e| e.to_lowercase()) {
        None => "none",
        Some(e) if e.contains("recently") || e.contains("future timestamp") => "lock-timestamp",
        Some(e) if e.contains("operation") && e.contains("running") => "app-operation-active",
        Some(e) if e.contains("close other git") => "git-process-detected",
        Some(e) if e.contains("exclusive access") || e.contains("has the index lock open") => "file-busy-or-denied",
        Some(e) if e.contains("changed") || e.contains("replaced") => "changed-or-unverifiable",
        Some(e) if e.contains("expired") => "expired-ticket",
        Some(e) if e.contains("no longer present") => "lock-absent",
        _ => "inspection-or-recovery-refused",
    };
    perf_log(&format!("index_lock_recovery: repo={} phase={phase} result={outcome} reason={reason} backup_preserved={} index_changed=false server_changed=false git_retry=false", anonymized_repository_id(repository_path), phase == "recover" && result.is_ok()), started.elapsed());
}

#[tauri::command]
pub async fn inspect_index_lock(repository_path: String) -> Result<LockInspection, String> {
    off_main_thread(move || {
        let started = Instant::now();
        let result = inspect_inner(&repository_path, require_idle, STABILITY_WAIT, true);
        log_result(&repository_path, "inspect", &result, started);
        result
    }).await
}

#[tauri::command]
pub async fn recover_index_lock(repository_path: String, ticket: String, confirmed: bool) -> Result<LockRecovery, String> {
    off_main_thread(move || {
        let started = Instant::now();
        let result = recover_inner(&repository_path, &ticket, confirmed, require_idle);
        log_result(&repository_path, "recover", &result, started);
        result
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture { root: PathBuf, lock: PathBuf }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("gdd-index-recovery-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&root).unwrap();
            let repo = Repository::init(&root).unwrap();
            fs::write(root.join("file.txt"), "staged text").unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("file.txt")).unwrap(); index.write().unwrap();
            fs::write(root.join("file.txt"), "working text").unwrap();
            let lock = repo.path().join("index.lock");
            fs::write(&lock, b"unfinished index contents").unwrap();
            let old = SystemTime::now() - Duration::from_secs(120);
            File::options().write(true).open(&lock).unwrap().set_times(fs::FileTimes::new().set_modified(old)).unwrap();
            Self { root, lock }
        }
        fn path(&self) -> &str { self.root.to_str().unwrap() }
        fn inspect(&self) -> LockInspection { inspect_inner(self.path(), |_| Ok(()), Duration::ZERO, true).unwrap() }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }

    #[test]
    fn recovery_preserves_index_working_files_head_and_lock_backup_without_retry() {
        let f = Fixture::new();
        let index = fs::read(f.root.join(".git/index")).unwrap();
        let head = fs::read(f.root.join(".git/HEAD")).unwrap();
        let report = f.inspect();
        assert!(f.lock.exists(), "inspection must be read-only");
        let result = recover_inner(f.path(), &report.ticket, true, |_| Ok(())).unwrap();
        assert!(!f.lock.exists());
        assert_eq!(fs::read(result.backup_path).unwrap(), b"unfinished index contents");
        assert_eq!(fs::read(f.root.join(".git/index")).unwrap(), index);
        assert_eq!(fs::read(f.root.join(".git/HEAD")).unwrap(), head);
        assert_eq!(fs::read_to_string(f.root.join("file.txt")).unwrap(), "working text");
        assert!(recover_inner(f.path(), &report.ticket, true, |_| Ok(())).is_err());
        // Subsequent index writes can acquire Git's lock normally.
        Repository::open(&f.root).unwrap().index().unwrap().write().unwrap();
    }

    #[test]
    fn recent_future_directory_and_hardlinked_locks_are_refused() {
        let f = Fixture::new();
        for time in [SystemTime::now(), SystemTime::now() + Duration::from_secs(3600)] {
            File::options().write(true).open(&f.lock).unwrap().set_times(fs::FileTimes::new().set_modified(time)).unwrap();
            assert!(inspect_inner(f.path(), |_| Ok(()), Duration::ZERO, true).is_err());
            assert!(f.lock.exists());
        }
        fs::remove_file(&f.lock).unwrap();
        fs::create_dir(&f.lock).unwrap();
        assert!(inspect_inner(f.path(), |_| Ok(()), Duration::ZERO, true).is_err());
        fs::remove_dir(&f.lock).unwrap();
        fs::hard_link(f.root.join(".git/index"), &f.lock).unwrap();
        assert!(inspect_inner(f.path(), |_| Ok(()), Duration::ZERO, false).is_err());
    }

    #[test]
    fn changed_replaced_and_disappeared_locks_cannot_use_an_old_confirmation() {
        for mode in ["changed", "replaced", "absent"] {
            let f = Fixture::new();
            let report = f.inspect();
            if mode != "changed" { fs::remove_file(&f.lock).unwrap(); }
            if mode != "absent" { fs::write(&f.lock, b"new lock").unwrap(); }
            assert!(recover_inner(f.path(), &report.ticket, true, |_| Ok(())).is_err(), "{mode}");
            if mode != "absent" { assert_eq!(fs::read(&f.lock).unwrap(), b"new lock"); }
        }
    }

    #[test]
    fn activity_confirmation_expiry_and_wrong_repository_block_recovery() {
        let f = Fixture::new();
        assert!(inspect_inner(f.path(), |_| Err("active process".into()), Duration::ZERO, true).is_err());
        let report = f.inspect();
        assert!(recover_inner(f.path(), &report.ticket, false, |_| Ok(())).is_err());
        assert!(recover_inner(f.path(), &report.ticket, true, |_| Err("active process".into())).is_err());
        let report = f.inspect();
        tickets().lock().unwrap().get_mut(&report.ticket).unwrap().inspected = Instant::now() - TICKET_TTL;
        assert!(recover_inner(f.path(), &report.ticket, true, |_| Ok(())).is_err());
        let other = Fixture::new();
        let report = f.inspect();
        assert!(recover_inner(other.path(), &report.ticket, true, |_| Ok(())).is_err());
        assert!(f.lock.exists() && other.lock.exists());
    }

    #[test]
    fn in_app_write_lock_is_respected_and_git_root_is_never_discovered_upward() {
        let f = Fixture::new();
        let report = f.inspect();
        let lock = repo_write_lock(f.path()); let guard = lock.lock().unwrap();
        assert!(inspect_inner(f.path(), |_| Ok(()), Duration::ZERO, true).is_err());
        assert!(recover_inner(f.path(), &report.ticket, true, |_| Ok(())).is_err());
        drop(guard);
        let child = f.root.join("uninitialized-module"); fs::create_dir(&child).unwrap();
        assert!(lock_path(child.to_str().unwrap()).is_err());
        assert!(f.lock.exists());
    }

    #[test]
    fn actual_git_directory_is_used_for_gitfiles() {
        let f = Fixture::new();
        fs::rename(f.root.join(".git"), f.root.join("metadata")).unwrap();
        fs::write(f.root.join(".git"), "gitdir: metadata\n").unwrap();
        let report = f.inspect();
        assert!(report.lock_path.ends_with("metadata/index.lock") || report.lock_path.ends_with("metadata\\index.lock"));
        let result = recover_inner(f.path(), &report.ticket, true, |_| Ok(())).unwrap();
        assert_eq!(fs::read(result.backup_path).unwrap(), b"unfinished index contents");
        assert!(f.root.join("metadata/index").exists());
    }

    #[test]
    fn process_probe_rejects_unknown_output_and_ignores_only_this_process() {
        let windows = "\"git.exe\",\"42\",\"Console\",\"1\",\"2,000 K\"\n\"git-drilldown.exe\",\"44\",\"Console\",\"1\",\"3,000 K\"";
        assert_eq!(parse_processes(windows, true, 44).unwrap().len(), 1);
        assert_eq!(parse_processes(windows, true, 99).unwrap().len(), 2);
        assert_eq!(parse_processes("10 /usr/bin/git\n11 /Applications/Git Extensions/GitExtensions\n12 /bin/ps", false, 12).unwrap().len(), 2);
        for input in ["", "Access denied", "\"git.exe\",\"bad-pid\""] { assert!(parse_processes(input, true, 10).is_err()); }
    }

    #[test]
    fn changes_during_the_stability_window_are_not_offered_for_recovery() {
        let f = Fixture::new();
        let probes = std::cell::Cell::new(0);
        let result = inspect_inner(f.path(), |_| {
            probes.set(probes.get() + 1);
            if probes.get() == 2 { fs::write(&f.lock, "still writing").unwrap(); }
            Ok(())
        }, Duration::ZERO, true);
        assert!(result.err().unwrap().contains("changing"));
        assert_eq!(fs::read_to_string(&f.lock).unwrap(), "still writing");
    }

    #[test]
    fn backup_failure_keeps_original_lock_and_index() {
        let f = Fixture::new();
        let report = f.inspect();
        // Simulate an external target change between confirmation and recovery.
        fs::rename(f.root.join(".git"), f.root.join("moved-metadata")).unwrap();
        fs::write(f.root.join(".git"), "gitdir: moved-metadata\n").unwrap();
        assert!(recover_inner(f.path(), &report.ticket, true, |_| Ok(())).is_err());
        assert!(f.root.join("moved-metadata/index.lock").exists());
        assert!(f.root.join("moved-metadata/index").exists());
    }

    #[test]
    #[ignore = "explicit local smoke test; requires process visibility and no other Git applications running"]
    fn real_os_probe_and_recovery_on_disposable_repository() {
        let f = Fixture::new();
        let report = inspect_inner(f.path(), require_idle, STABILITY_WAIT, true).unwrap();
        let result = recover_inner(f.path(), &report.ticket, true, require_idle).unwrap();
        assert!(!f.lock.exists());
        assert_eq!(fs::read(result.backup_path).unwrap(), b"unfinished index contents");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_lock_is_never_followed_or_moved() {
        let f = Fixture::new();
        fs::remove_file(&f.lock).unwrap();
        std::os::unix::fs::symlink(f.root.join(".git/index"), &f.lock).unwrap();
        assert!(inspect_inner(f.path(), |_| Ok(()), Duration::ZERO, true).is_err());
        assert!(fs::symlink_metadata(&f.lock).unwrap().file_type().is_symlink());
    }
}
