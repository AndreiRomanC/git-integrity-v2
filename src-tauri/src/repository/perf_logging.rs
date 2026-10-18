//! Bounded diagnostic output only. No repository locks, Git calls or background jobs.
use std::{fs, io::{self, Write}, path::{Path, PathBuf}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

pub(super) const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 64 * 1024;
const SIZE_RECHECK_INTERVAL: Duration = Duration::from_secs(5);

pub(super) struct RotatingLog {
    path: PathBuf,
    previous: PathBuf,
    max_bytes: u64,
    bytes: u64,
    size_checked: Option<Instant>,
    header_written: bool,
    rotated: bool,
    header: String,
}

impl RotatingLog {
    pub(super) fn new(path: PathBuf) -> Self { Self::with_limit(path, MAX_LOG_BYTES) }

    fn with_limit(path: PathBuf, max_bytes: u64) -> Self {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let previous = path.with_file_name(format!("{stem}.previous.log"));
        let unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        let header = format!("=== session start: build={} ===\n=== log settings: max_bytes={max_bytes} previous={} pid={} unix_ms={unix_ms} ===\n",
            env!("GIT_DRILLDOWN_BUILD_SHA"), previous.file_name().unwrap_or_default().to_string_lossy(), std::process::id());
        Self { path, previous, max_bytes, bytes: 0, size_checked: None, header_written: false, rotated: false, header }
    }

    pub(super) fn append(&mut self, label: &str, elapsed: Duration) -> io::Result<()> {
        let result = self.append_inner(label, elapsed);
        if result.is_err() { self.size_checked = None; }
        result
    }

    fn append_inner(&mut self, label: &str, elapsed: Duration) -> io::Result<()> {
        // Most appends use the byte count from the previous write, not another
        // stat. Periodically notice an externally removed/truncated log.
        if self.size_checked.is_none_or(|at| at.elapsed() >= SIZE_RECHECK_INTERVAL) {
            let actual = regular_file_size(&self.path)?.unwrap_or(0);
            if actual < self.bytes { self.header_written = false; }
            self.bytes = actual;
            self.size_checked = Some(Instant::now());
        }
        let entry_limit = MAX_ENTRY_BYTES.min((self.max_bytes / 4) as usize);
        let mut cut = label.len().min(entry_limit);
        while !label.is_char_boundary(cut) { cut -= 1; }
        let truncation = if cut < label.len() { format!(" [diagnostic entry truncated: original_bytes={} limit={entry_limit}]", label.len()) } else { String::new() };
        let line = format!("[{:>7.1}ms] {}{truncation}\n", elapsed.as_secs_f64() * 1000.0, &label[..cut]);
        let header_bytes = if self.header_written { 0 } else { self.header.len() as u64 };
        if self.bytes > 0 && self.bytes.saturating_add(header_bytes + line.len() as u64) > self.max_bytes {
            // Close-per-append makes this possible on Windows too. Refuse to
            // replace directories/symlinks. A failed rename leaves the current
            // file intact; no truncate/delete fallback can destroy evidence.
            regular_file_size(&self.path)?;
            regular_file_size(&self.previous)?;
            match fs::rename(&self.path, &self.previous) {
                Ok(()) => { self.rotated = true; }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
            self.bytes = 0; self.header_written = false;
        }
        let mut record = String::new();
        if !self.header_written {
            record.push_str(&self.header);
            if self.rotated { record.push_str("=== log rotated: previous file retained; older backup replaced ===\n"); }
        }
        record.push_str(&line);
        let mut file = fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        file.write_all(record.as_bytes())?;
        self.bytes += record.len() as u64;
        self.header_written = true; self.rotated = false;
        Ok(())
    }
}

fn regular_file_size(path: &Path) -> io::Result<Option<u64>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(Some(metadata.len())),
        Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, "Diagnostic log target is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn fixture() -> (PathBuf, RotatingLog) {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("git-drilldown-log-rotation-{}-{suffix}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let writer = RotatingLog::with_limit(dir.join("git-integrity-perf.log"), 1024);
        (dir, writer)
    }

    #[test]
    fn header_is_written_once_and_existing_log_is_preserved() {
        let (dir, mut log) = fixture();
        fs::write(&log.path, "old session\n").unwrap();
        log.append("first", Duration::from_millis(3)).unwrap();
        log.append("second", Duration::ZERO).unwrap();
        let text = fs::read_to_string(&log.path).unwrap();
        assert!(text.starts_with("old session\n"));
        assert_eq!(text.matches("=== session start:").count(), 1);
        assert!(text.contains("build=")); assert!(text.contains("unix_ms="));
        assert!(text.contains("[    3.0ms] first")); assert!(text.contains("second"));
        assert!(!log.previous.exists()); fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotation_retains_the_complete_previous_file_and_replaces_only_the_older_backup() {
        let (dir, mut log) = fixture();
        let original = "x".repeat(1000);
        fs::write(&log.path, &original).unwrap(); fs::write(&log.previous, "older backup").unwrap();
        log.append("after rotation", Duration::ZERO).unwrap();
        assert_eq!(fs::read_to_string(&log.previous).unwrap(), original);
        let current = fs::read_to_string(&log.path).unwrap();
        assert!(current.starts_with("=== session start:")); assert!(current.contains("log rotated:"));
        assert!(current.contains("after rotation")); assert!(current.len() <= 1024);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repeated_rotations_remain_bounded_to_current_and_previous_files() {
        let (dir, mut log) = fixture();
        for i in 0..200 { log.append(&format!("record={i} {}", "a".repeat(90)), Duration::ZERO).unwrap(); }
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        for path in [&log.path, &log.previous] {
            let text = fs::read_to_string(path).unwrap();
            assert!(text.len() <= 1024); assert!(text.starts_with("=== session start:"));
        }
        assert!(fs::read_to_string(&log.path).unwrap().contains("record=199"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_oversized_legacy_log_is_archived_without_truncating_its_evidence() {
        let (dir, mut log) = fixture(); let original = "legacy\n".repeat(1000);
        fs::write(&log.path, &original).unwrap(); log.append("new build", Duration::ZERO).unwrap();
        assert_eq!(fs::read_to_string(&log.previous).unwrap(), original);
        assert!(fs::metadata(&log.path).unwrap().len() <= 1024); fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn extremely_long_entries_are_explicitly_bounded_at_utf8_boundaries() {
        let (dir, mut log) = fixture(); log.append(&"ă".repeat(10000), Duration::ZERO).unwrap();
        let text = fs::read_to_string(&log.path).unwrap();
        assert!(text.contains("diagnostic entry truncated: original_bytes=20000"));
        assert!(text.len() <= 1024); fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_rotation_keeps_the_current_log_and_unrelated_target_intact() {
        let (dir, mut log) = fixture(); let original = "x".repeat(1000);
        fs::write(&log.path, &original).unwrap(); fs::create_dir(&log.previous).unwrap();
        fs::write(log.previous.join("keep.txt"), "untouched").unwrap();
        assert!(log.append("must not destroy logs", Duration::ZERO).is_err());
        assert_eq!(fs::read_to_string(&log.path).unwrap(), original);
        assert_eq!(fs::read_to_string(log.previous.join("keep.txt")).unwrap(), "untouched");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_writers_do_not_interleave_records_or_repeat_the_header() {
        let (dir, _) = fixture();
        let log = Arc::new(Mutex::new(RotatingLog::new(dir.join("git-integrity-perf.log"))));
        let threads: Vec<_> = (0..8).map(|worker| { let log = log.clone(); std::thread::spawn(move || {
            for i in 0..50 { log.lock().unwrap().append(&format!("worker={worker} entry={i}"), Duration::ZERO).unwrap(); }
        }) }).collect();
        for thread in threads { thread.join().unwrap(); }
        let text = fs::read_to_string(&log.lock().unwrap().path).unwrap();
        assert_eq!(text.matches("=== session start:").count(), 1);
        assert_eq!(text.lines().filter(|line| line.starts_with('[')).count(), 400);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn externally_truncated_log_gets_a_new_header_on_the_next_size_check() {
        let (dir, mut log) = fixture(); log.append("before", Duration::ZERO).unwrap();
        fs::write(&log.path, "").unwrap(); log.size_checked = None;
        log.append("after", Duration::ZERO).unwrap();
        let text = fs::read_to_string(&log.path).unwrap();
        assert!(text.starts_with("=== session start:")); assert!(text.contains("after"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_backup_is_never_followed_or_replaced() {
        let (dir, mut log) = fixture(); let target = dir.join("keep.txt");
        fs::write(&target, "untouched").unwrap(); std::os::unix::fs::symlink(&target, &log.previous).unwrap();
        fs::write(&log.path, "x".repeat(1000)).unwrap();
        assert!(log.append("rotate", Duration::ZERO).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
        assert!(fs::symlink_metadata(&log.previous).unwrap().file_type().is_symlink());
        fs::remove_dir_all(dir).unwrap();
    }
}
