use super::*;

#[derive(Serialize)]
pub struct RawGitResult {
    pub(in crate::repository) stdout: String,
    pub(in crate::repository) stderr: String,
    pub(in crate::repository) success: bool,
    pub(in crate::repository) exit_code: Option<i32>,
    pub(in crate::repository) read_only: bool,
}

pub type TerminalCommandResult = RawGitResult;

// Minimal shell-like parsing for the Git-only command box. Arguments are
// passed directly to Git, never to a shell.
pub(in crate::repository) fn tokenize_git_args(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_current = false;
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == '\\' && q == '"' {
                    if let Some(&next) = chars.peek() {
                        if next == '"' || next == '\\' {
                            current.push(next);
                            chars.next();
                            continue;
                        }
                    }
                    current.push(ch);
                } else if ch == q {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    has_current = true;
                } else if ch.is_whitespace() {
                    if has_current {
                        tokens.push(std::mem::take(&mut current));
                        has_current = false;
                    }
                } else {
                    current.push(ch);
                    has_current = true;
                }
            }
        }
    }
    if quote.is_some() { return Err("Unclosed quote in command".into()); }
    if has_current { tokens.push(current); }
    Ok(tokens)
}

// Deliberately conservative: an unnecessary refresh is safer than stale UI
// after a command that was incorrectly classified as read-only.
pub(in crate::repository) fn is_read_only_git_subcommand(subcommand: &str) -> bool {
    matches!(subcommand, "status" | "log" | "diff" | "show" | "blame" | "ls-files" | "merge-tree")
}

#[tauri::command]
pub fn run_git_command(repository_path: String, args: String) -> Result<RawGitResult, String> {
    let started = Instant::now();
    validate_path(&repository_path)?;
    let queue_started = Instant::now();
    let lock_handle = repo_write_lock(&repository_path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(&repository_path, "run_git_command", queue_started.elapsed());
    let mut parts = tokenize_git_args(&args)?;
    if parts.first().map(String::as_str) == Some("git") { parts.remove(0); }
    if parts.is_empty() { return Err("Type a git subcommand, e.g. \"status\" or \"log --oneline -10\"".into()); }
    let read_only = is_read_only_git_subcommand(&parts[0]);
    let repo_id = anonymized_repository_id(&repository_path);
    let arg_refs = parts.iter().map(String::as_str).collect::<Vec<_>>();
    let submodule_update = is_submodule_update(&arg_refs);
    let (timeout, timeout_label) = git_command_timeout(&arg_refs);
    let history_id = begin_git_command(&repository_path, &arg_refs);
    let mut command = Command::new("git");
    configure_git_command(&mut command);
    command.arg("-C").arg(&repository_path).arg("-c").arg("color.ui=false").args(&parts);
    if submodule_update {
        perf_log(&format!("run_git_command: submodule_update start ({repo_id}, timeout_seconds={}, automatic_cleanup=false)", timeout.as_secs()), Duration::ZERO);
    }
    let output = match run_with_timeout_labeled(command, timeout, "Git", timeout_label) {
        Ok(output) => output,
        Err(error) => {
            if !read_only { invalidate_git_metadata(&repository_path); }
            if submodule_update { invalidate_submodule_sync(&repository_path); }
            let error = submodule_update_error(error, submodule_update);
            finish_git_command(history_id, false, None, "", "", &error);
            perf_log(&format!("run_git_command: {} ({repo_id}, read_only={read_only}) PROCESS_ERROR cache_invalidated={}", parts[0], !read_only), started.elapsed());
            return Err(error);
        }
    };
    perf_log(&format!("run_git_command: {} ({repo_id}, read_only={read_only}) exit_code={:?}", parts[0], output.status.code()), started.elapsed());
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = submodule_update_error(String::from_utf8_lossy(&output.stderr).into_owned(), submodule_update && !output.status.success());
    finish_git_command(history_id, output.status.success(), output.status.code(), &stdout, &stderr, "");
    invalidate_git_metadata(&repository_path);
    if submodule_update { invalidate_submodule_sync(&repository_path); }
    Ok(RawGitResult {
        stdout,
        stderr,
        success: output.status.success(),
        exit_code: output.status.code(),
        read_only,
    })
}

pub(in crate::repository) fn is_definitely_read_only_terminal_command(input: &str) -> bool {
    if input.chars().any(|ch| matches!(ch, '\n' | '\r' | ';' | '|' | '&' | '>' | '<' | '`')) || input.contains("$(") { return false; }
    let Ok(parts) = tokenize_git_args(input) else { return false };
    let Some(program) = parts.first().map(|part| part.to_ascii_lowercase()) else { return false };
    if program == "git" {
        return parts.get(1).map(|part| is_read_only_git_subcommand(&part.to_ascii_lowercase())).unwrap_or(false);
    }
    matches!(program.as_str(), "pwd" | "ls" | "dir" | "whoami" | "hostname" | "which" | "where")
}

fn terminal_command_timeout(input: &str) -> (Duration, &'static str) {
    let Ok(parts) = tokenize_git_args(input) else { return (GIT_COMMAND_TIMEOUT, "10 minutes") };
    let Some(program) = parts.first() else { return (GIT_COMMAND_TIMEOUT, "10 minutes") };
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    if !name.eq_ignore_ascii_case("git") && !name.eq_ignore_ascii_case("git.exe") {
        return (GIT_COMMAND_TIMEOUT, "10 minutes");
    }
    git_command_timeout(&parts.iter().skip(1).map(String::as_str).collect::<Vec<_>>())
}

fn submodule_update_error(error: String, update_incomplete: bool) -> String {
    if !update_incomplete { return error; }
    format!("{error}\nSubmodule update is incomplete. Some modules may already have been updated; completed changes were not rolled back automatically. Inspect the refreshed working tree before committing. No automatic reset, clean or forced checkout was run.")
}

pub(in crate::repository) fn run_terminal_command_inner(repository_path: String, command_text: String) -> Result<TerminalCommandResult, String> {
    let (timeout, timeout_label) = terminal_command_timeout(&command_text);
    run_terminal_command_with_timeout(repository_path, command_text, timeout, timeout_label)
}

pub(in crate::repository) fn run_terminal_command_with_timeout(repository_path: String, command_text: String, timeout: Duration, timeout_label: &str) -> Result<TerminalCommandResult, String> {
    let started = Instant::now();
    validate_path(&repository_path)?;
    let command_text = command_text.trim();
    if command_text.is_empty() { return Err("Type a command, e.g. \"git status\", \"pwd\", or \"gh pr status\"".into()); }
    let read_only = is_definitely_read_only_terminal_command(command_text);

    // Mutating commands still lock the actual repository root while retaining
    // the selected directory as the process cwd. Clearly read-only terminal
    // commands (status/log/diff/show/ls/dir/...) deliberately skip the write
    // lock so a slow inspection command cannot make commit/publish/status feel
    // blocked for no reason.
    let repository_root = if read_only {
        repository_path.clone()
    } else {
        let repository = internal_repository(&repository_path)?;
        repository.workdir()
            .ok_or("Bare repositories are not supported by the embedded Terminal")?
            .to_string_lossy().into_owned()
    };
    let lock_handle = if read_only { None } else { Some(repo_write_lock(&repository_root)) };
    let queue_started = Instant::now();
    let _lock = match &lock_handle {
        Some(lock_handle) => {
            let guard = lock_handle.lock().unwrap();
            log_repo_write_lock_acquired(&repository_root, "run_terminal_command", queue_started.elapsed());
            Some(guard)
        }
        None => {
            perf_log(&format!("repo_write_lock: [run_terminal_command] skipped read_only (repo={})", anonymized_repository_id(&repository_root)), Duration::ZERO);
            None
        }
    };
    let repo_id = anonymized_repository_id(&repository_root);
    let submodule_update = terminal_command_timeout(command_text).0 == SUBMODULE_UPDATE_TIMEOUT;
    let terminal_git_args = tokenize_git_args(command_text)
        .ok()
        .and_then(|parts| {
            parts.first()
                .is_some_and(|program| program.eq_ignore_ascii_case("git"))
                .then(|| parts.into_iter().skip(1).collect::<Vec<_>>())
        })
        .filter(|parts| !parts.is_empty());
    let history_id = terminal_git_args.as_ref().map(|parts| {
        let arg_refs = parts.iter().map(String::as_str).collect::<Vec<_>>();
        begin_git_command(&repository_path, &arg_refs)
    });

    #[cfg(windows)]
    let mut command = {
        let shell = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
        let mut command = Command::new(shell);
        command.args(["/D", "/S", "/C"]).arg(command_text);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let shell = std::env::var_os("SHELL").filter(|value| Path::new(value).is_file()).unwrap_or_else(|| "/bin/sh".into());
        let mut command = Command::new(shell);
        command.args(["-l", "-c"]).arg(command_text);
        command
    };

    command.current_dir(&repository_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(std::process::Stdio::null());

    perf_log(&format!("run_terminal_command: start ({repo_id}, read_only={read_only}, submodule_update={submodule_update}, timeout_seconds={})", timeout.as_secs()), Duration::ZERO);
    let output = match run_with_timeout_labeled(command, timeout, "Terminal", timeout_label) {
        Ok(output) => output,
        Err(error) => {
            // Process failure is not a transaction rollback: cloned modules,
            // indexes and files may already have changed before the timeout.
            if !read_only { invalidate_git_metadata(&repository_root); }
            if submodule_update { invalidate_submodule_sync(&repository_root); }
            let error = submodule_update_error(error, submodule_update);
            if let Some(history_id) = history_id { finish_git_command(history_id, false, None, "", "", &error); }
            perf_log(&format!("run_terminal_command: ({repo_id}, read_only={read_only}) PROCESS_ERROR cache_invalidated={} submodule_update_incomplete={submodule_update}", !read_only), started.elapsed());
            return Err(error);
        }
    };
    perf_log(&format!("run_terminal_command: ({repo_id}, read_only={read_only}) exit_code={:?}", output.status.code()), started.elapsed());
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = submodule_update_error(String::from_utf8_lossy(&output.stderr).into_owned(), submodule_update && !output.status.success());
    if let Some(history_id) = history_id {
        finish_git_command(history_id, output.status.success(), output.status.code(), &stdout, &stderr, "");
    }
    if !read_only { invalidate_git_metadata(&repository_root); }
    if submodule_update { invalidate_submodule_sync(&repository_root); }
    Ok(RawGitResult {
        stdout,
        stderr,
        success: output.status.success(),
        exit_code: output.status.code(),
        read_only,
    })
}

#[tauri::command]
pub async fn run_terminal_command(repository_path: String, command_text: String) -> Result<TerminalCommandResult, String> {
    off_main_thread(move || run_terminal_command_inner(repository_path, command_text)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submodule_update_gets_the_long_budget_only_for_real_update_commands() {
        for args in [
            vec!["submodule", "update", "--init", "--recursive"],
            vec!["-c", "protocol.file.allow=always", "submodule", "update", "--", "vendor/dep"],
            vec!["-C", "folder with spaces", "--no-pager", "submodule", "--quiet", "update"],
            vec!["--git-dir=repo.git", "--work-tree=repo", "submodule", "update", "--force"],
        ] {
            assert_eq!(git_command_timeout(&args), (Duration::from_secs(3600), "60 minutes"), "{args:?}");
        }
        for args in [
            vec!["status"], vec!["fetch"], vec!["submodule", "status", "--recursive"],
            vec!["submodule", "sync", "--recursive"], vec!["submodule", "deinit", "--all"],
            vec!["show", "submodule", "update"], vec!["commit", "-m", "submodule", "update"],
            vec!["-c", "alias.update=submodule update", "status"], vec!["-C"],
        ] {
            assert_eq!(git_command_timeout(&args), (Duration::from_secs(600), "10 minutes"), "{args:?}");
        }
        for command in [
            "git submodule update --init --recursive",
            "git -C \"folder with spaces\" submodule update --init --recursive",
            "git -c protocol.file.allow=always submodule update --init --recursive -- vendor/dep",
            r#""C:\Program Files\Git\cmd\git.exe" submodule update --init --recursive"#,
        ] {
            assert_eq!(terminal_command_timeout(command).0, Duration::from_secs(3600), "{command}");
        }
        for command in ["git status", "git submodule status", "echo git submodule update", "git log -- submodule update"] {
            assert_eq!(terminal_command_timeout(command).0, Duration::from_secs(600), "{command}");
        }
    }

    #[test]
    fn incomplete_update_warning_preserves_the_original_error_and_does_not_promise_rollback() {
        let error = "Terminal command timed out after 60 minutes".to_string();
        let result = submodule_update_error(error.clone(), true);
        assert!(result.starts_with(&error));
        assert!(result.contains("not rolled back automatically"));
        assert!(result.contains("No automatic reset, clean or forced checkout"));
        assert_eq!(submodule_update_error(error.clone(), false), error);
    }
}
