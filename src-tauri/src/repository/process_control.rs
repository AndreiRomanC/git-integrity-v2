//! Explicit interruption for standalone Terminal Git commands and one
//! init/update call only. Do not opt multi-step workflows in here: an inner
//! failure may otherwise trigger their retry/fallback or next mutation.
use super::*;
use std::io::Read;
use std::sync::atomic::AtomicBool;

struct StopHandle(u64);
impl Drop for StopHandle {
    fn drop(&mut self) {
        if let Some(entry) = recent_git_commands_store().lock().unwrap().iter_mut().find(|entry| entry.id == self.0) {
            entry.stop_request = None;
        }
    }
}

fn kill_tree(child: &mut std::process::Child) -> Result<(), String> {
    // We still own the unreaped Child. The UI sends a session-unique command
    // ID, never a PID. A stale confirmation cannot target a replacement job.
    if child.try_wait().map_err(|error| error.to_string())?.is_some() { return Ok(()); }
    #[cfg(unix)]
    let result = Command::new("kill").arg("-9").arg(format!("-{}", child.id())).output();
    #[cfg(windows)]
    let result = {
        use std::os::windows::process::CommandExt;
        Command::new("taskkill").creation_flags(0x08000000)
            .args(["/F", "/T", "/PID"]).arg(child.id().to_string()).output()
    };
    let output = result.map_err(|error| error.to_string())?;
    if output.status.success() || child.try_wait().map_err(|error| error.to_string())?.is_some() { Ok(()) }
    else { Err(String::from_utf8_lossy(&output.stderr).trim().to_string()) }
}

pub(super) fn run(mut command: Command, timeout: Duration, label: &str, timeout_label: &str, history_id: u64) -> Result<std::process::Output, String> {
    #[cfg(unix)] { use std::os::unix::process::CommandExt; command.process_group(0); }
    let mut child = command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().map_err(|error| format!("Cannot start {label}: {error}"))?;
    let read = |mut pipe: Box<dyn Read + Send>| std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stdout = read(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = read(Box::new(child.stderr.take().expect("piped stderr")));
    let requested = Arc::new(AtomicBool::new(false));
    {
        let mut store = recent_git_commands_store().lock().unwrap();
        if let Some(entry) = store.iter_mut().find(|entry| entry.id == history_id) {
            entry.stop_request = Some(Arc::clone(&requested));
        }
    }
    let handle = StopHandle(history_id);
    let started = Instant::now();
    perf_log(&format!("git_process: id={history_id} started stoppable=true"), Duration::ZERO);
    let mut interruption = None;
    let mut next_timeout_attempt = timeout;
    let status = loop {
        // Completion wins over a late Stop request.
        if let Some(status) = child.try_wait().map_err(|error| format!("{label} process error: {error}"))? { break status; }
        let user_stop = requested.load(Ordering::Relaxed);
        if interruption.is_none() && (user_stop || started.elapsed() >= next_timeout_attempt) {
            match kill_tree(&mut child) {
                Ok(()) => {
                    interruption = Some(if user_stop {
                        format!("{label} command stopped by user. Completed local or server changes were not rolled back. Inspect repository status before continuing; an interrupted command may leave lock files or an unfinished operation. No automatic reset, clean, commit or push was run.")
                    } else {
                        format!("{label} command timed out after {timeout_label}. Completed changes were not rolled back.")
                    });
                    perf_log(&format!("git_process: id={history_id} termination_requested reason={}", if user_stop { "user" } else { "timeout" }), started.elapsed());
                }
                Err(error) => {
                    // Do not release the caller's write lock or claim it
                    // stopped while the original process may still write.
                    requested.store(false, Ordering::Relaxed);
                    next_timeout_attempt = started.elapsed() + Duration::from_secs(30);
                    if let Some(entry) = recent_git_commands_store().lock().unwrap().iter_mut().find(|entry| entry.id == history_id) {
                        entry.error = format!("Could not stop this process: {error}. It is still running. You can retry Stop.");
                    }
                    perf_log(&format!("git_process: id={history_id} termination_failed still_running=true"), started.elapsed());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    drop(handle);
    let stdout = stdout.join().map_err(|_| format!("{label} output reader failed"))?.map_err(|error| error.to_string())?;
    let stderr = stderr.join().map_err(|_| format!("{label} error reader failed"))?.map_err(|error| error.to_string())?;
    perf_log(&format!("git_process: id={history_id} exited interrupted={}", interruption.is_some()), started.elapsed());
    if let Some(message) = interruption {
        // Preserve output produced before interruption in the existing error
        // transcript. The caller performs its usual failed-operation refresh.
        return Err(format!("{message}\n{}{}",
            bounded_command_output(&String::from_utf8_lossy(&stdout)),
            bounded_command_output(&String::from_utf8_lossy(&stderr))));
    }
    Ok(std::process::Output { status, stdout, stderr })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_rejects_unknown_finished_and_unsupported_commands() {
        assert!(stop_git_command(u64::MAX).is_err());
        let id = begin_git_command("/tmp/stop-test", &["merge"]);
        assert!(stop_git_command(id).unwrap_err().contains("not available"));
        finish_git_command(id, true, Some(0), "", "", "");
        assert!(stop_git_command(id).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stop_interrupts_the_process_group_and_keeps_partial_output() {
        let id = begin_git_command("/tmp/stop-test", &["test-only"]);
        let worker = std::thread::spawn(move || {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "printf 'started\\n'; sleep 30; printf 'should-not-complete\\n'"]);
            let result = run(command, Duration::from_secs(40), "Test", "40 seconds", id);
            finish_git_command(id, false, None, "", "", result.as_ref().err().map(String::as_str).unwrap_or(""));
            result
        });
        let started = Instant::now();
        while !active_git_commands().iter().any(|entry| entry.id == id && entry.stoppable) {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        stop_git_command(id).unwrap();
        let error = worker.join().unwrap().unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5), "child sleep must not keep stdout alive for 30 seconds");
        assert!(error.contains("stopped by user") && error.contains("started\n"));
        assert!(!error.contains("should-not-complete"));
        assert!(stop_git_command(id).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn normal_completion_and_timeout_unregister_stop_handles() {
        for (script, timeout, success) in [("printf ok", Duration::from_secs(5), true), ("sleep 30", Duration::from_millis(100), false)] {
            let id = begin_git_command("/tmp/stop-test", &["test-only"]);
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            let result = run(command, timeout, "Test", "test budget", id);
            assert_eq!(result.is_ok(), success);
            if success { assert_eq!(result.unwrap().stdout, b"ok"); }
            else { assert!(result.unwrap_err().contains("timed out")); }
            assert!(stop_git_command(id).is_err(), "handle must be gone even before history completion");
            finish_git_command(id, success, None, "", "", "");
        }
    }
}
