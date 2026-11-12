use super::*;
use std::io::Cursor;

fn trace() -> PushTrace { PushTrace { id: 42, started: Instant::now(), ui: Default::default(), command_id: None } }

impl PushTrace {
    fn read_stderr(&self, mut input: impl std::io::Read) -> std::io::Result<Vec<u8>> {
        let mut capture = Capture::default();
        let mut block = [0u8; 37]; // Exercise real stream segmentation, including inside JSON strings.
        loop {
            let n = input.read(&mut block)?;
            if n == 0 { return Ok(capture.finish(self, true)); }
            capture.feed(self, &block[..n]);
        }
    }
}

#[test]
fn pending_helpers_report_only_action_count_age_and_recognized_error() {
    let mut c = Capture::default();
    let t = trace();
    c.feed(&t, b"{\"event\":\"child_start\",\"child_id\":1,\"sid\":\"PRIVATE\",\"argv\":[\"git credential-manager get\"]}\n");
    c.feed(&t, b"{\"event\":\"child_start\",\"child_id\":2,\"sid\":\"PRIVATE\",\"argv\":[\"git\",\"credential-manager\",\"store\"]}\n");
    c.feed(&t, b"fatal: Authentication failed for https://PRIVATE\n");
    let s = c.summary.snapshot();
    assert!(s.contains("credential_helper/get:1@") && s.contains("credential_helper/store:1@") && s.contains("stderr_reason=authentication"));
    assert!(!s.contains("PRIVATE"));
    c.feed(&t, b"{\"event\":\"child_exit\",\"child_id\":1,\"sid\":\"PRIVATE\",\"code\":0,\"t_rel\":1}\n");
    assert!(!c.summary.snapshot().contains("helper/get:"));
    assert_eq!(failure_category("Git push timed out. Last recognized Git error: authentication"), "timeout");
}

#[test]
fn prompt_diagnostics_are_observations_not_new_authentication_policy() {
    let t = trace();
    let mut cmd = Command::new("git");
    t.configure(&mut cmd);
    assert!(cmd.get_envs().all(|(k, _)| k.to_string_lossy().starts_with("GIT_TRACE2_")));
    assert_eq!(prompt_setting(Some("PRIVATE")), "other");
    assert_eq!(config_value("credential.guiPrompt", "false"), Some(("credential.guiPrompt", "disabled")));
    let (_, logs) = perf_logging::capture_for_test(|| {
        t.log_command(true, false);
        let mut c = Capture::default();
        for id in 0..15 {
            let e = serde_json::json!({"event":"child_start","child_id": id, "sid":"PRIVATE","argv":["git credential-manager get"]});
            c.feed(&t, format!("{e}\n").as_bytes());
        }
    });
    assert!(logs.iter().any(|s| s.contains("mode=fast source=selected_commit force=false")));
    assert_eq!(logs.iter().filter(|s| s.contains("first_child category=credential_helper action=get")).count(), 1);
    assert!(!logs.join("\n").contains("PRIVATE"));
}

const FIXTURE: &str = "repository::publish_diagnostics::tests::push_child_process_fixture";
fn fixture(role: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--ignored", "--exact", FIXTURE, "--nocapture"]).env("GDD_TEST_PUSH_CHILD_ROLE", role);
    cmd
}

#[test]
#[ignore = "only invoked as a disposable subprocess by the runner tests"]
fn push_child_process_fixture() {
    use std::io::Write;
    match std::env::var("GDD_TEST_PUSH_CHILD_ROLE").as_deref() {
        Ok("hold_pipes") => std::thread::sleep(Duration::from_secs(5)),
        Ok("exit_ok") | Ok("exit_failed") => {
            let _writer = fixture("hold_pipes").spawn().unwrap(); // deliberately inherits both pipes
            eprintln!("diagnostic before exit");
            std::process::exit(if std::env::var("GDD_TEST_PUSH_CHILD_ROLE").unwrap()=="exit_ok" { 0 } else { 7 });
        }
        Ok("await_helper") => {
            eprintln!(r#"{{"event":"child_start","sid":"PRIVATE","child_id":1,"argv":["git credential-manager get"]}}"#);
            eprintln!("fatal: Authentication failed for https://PRIVATE");
            std::thread::sleep(Duration::from_secs(20));
        }
        Ok("noisy") => {
            for _ in 0..60 { std::io::stdout().write_all(&[b'x'; 8192]).unwrap(); eprintln!("{}", "y".repeat(8192)); }
            eprintln!("fatal: Authentication failed for https://PRIVATE");
            std::process::exit(7);
        }
        _ => panic!("fixture requires an explicit role"),
    }
}

#[test]
fn inherited_pipes_never_turn_an_exited_push_into_a_timeout_or_successful_retry() {
    for (role, code) in [("exit_ok",0),("exit_failed",7)] {
        let started = Instant::now();
        let (output, logs) = perf_logging::capture_for_test(|| run_with_timeout_labeled_traced(fixture(role), Duration::from_secs(3), "Git", "3 seconds", Some(trace())).unwrap());
        assert_eq!(output.status.code(), Some(code));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(String::from_utf8_lossy(&output.stderr).contains("diagnostic before exit"));
        assert!(logs.iter().any(|s| s.contains("git_exit")));
        assert!(logs.iter().any(|s| s.contains("capture_finished") && s.contains("stderr=open") && s.contains("timeout_requested=false")));
        assert!(!logs.iter().any(|s| s.contains("event=timeout")));
    }
}

#[test]
fn actual_timeout_preserves_allowlisted_evidence_and_warns_about_unknown_remote_state() {
    let (err, logs) = perf_logging::capture_for_test(|| run_with_timeout_labeled_traced(fixture("await_helper"), Duration::from_secs(2), "Git", "test budget", Some(trace())).unwrap_err());
    assert!(err.contains("timed out") && err.contains("no exit was observed") && err.contains("authentication") && err.contains("remote result is unknown"));
    assert!(logs.iter().any(|s| s.contains("event=timeout git=running") && s.contains("credential_helper/get:1@")));
    assert!(!err.contains("PRIVATE") && !logs.join("\n").contains("PRIVATE"));
}

#[test]
fn credential_wait_can_stop_only_its_own_observed_push() {
    let id = begin_git_command("fixture", &["push", "origin", "HEAD:refs/heads/test"]);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut t = trace();
    t.command_id = Some(id);
    t.ui.credential_wait = Some(tauri::ipc::Channel::new(move |body| {
        tx.send(body.deserialize::<Option<u64>>().unwrap()).unwrap();
        Ok(())
    }));
    assert!(stop_git_command(id).is_err());
    let runner = std::thread::spawn(move || run_with_timeout_labeled_traced(fixture("await_helper"),
        Duration::from_secs(15), "Git", "fixture timeout", Some(t)));
    let reported = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(reported, Some(id));
    stop_git_command(id).unwrap();
    let error = runner.join().unwrap().unwrap_err();
    finish_git_command(id, false, None, "", "", &error);
    assert!(error.starts_with("Git push stopped"));
    assert!(stop_git_command(id).is_err());
}

#[test]
fn modal_parent_is_passed_only_to_the_git_child_without_changing_helper_policy() {
    let mut t = trace();
    t.ui.parent_hwnd = Some(9182);
    let mut cmd = Command::new("git");
    t.configure(&mut cmd);
    let vars: std::collections::HashMap<_, _> = cmd.get_envs().collect();
    assert_eq!(vars[std::ffi::OsStr::new("GCM_MODAL_PARENTHWND")], Some(std::ffi::OsStr::new("9182")));
    assert!(!vars.contains_key(std::ffi::OsStr::new("GCM_INTERACTIVE")));
}

#[test]
fn large_concurrent_output_is_drained_and_bounded_without_changing_exit_status() {
    let (output, logs) = perf_logging::capture_for_test(|| run_with_timeout_labeled_traced(fixture("noisy"), Duration::from_secs(15), "Git", "15 seconds", Some(trace())).unwrap());
    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.len() <= RECENT_GIT_COMMAND_OUTPUT_LIMIT + 128 && output.stderr.len() <= RECENT_GIT_COMMAND_OUTPUT_LIMIT + 128);
    assert!(logs.iter().any(|s| s.contains("stderr_reason=authentication")));
    assert!(!logs.join("\n").contains("PRIVATE"));
}

#[test]
fn trace_stream_is_allowlisted_and_never_reaches_console_output() {
    let input = concat!(
        "{\"event\":\"version\",\"sid\":\"root\",\"exe\":\"2.49.1.windows.1_PRIVATE\"}\n",
        "{\"event\":\"start\",\"sid\":\"root\",\"argv\":[\"push\",\"https://PRIVATE_TOKEN@PRIVATE_HOST/repo\"]}\n",
        "{\"event\":\"def_param\",\"sid\":\"root\",\"param\":\"push.recurseSubmodules\",\"value\":\"check\"}\n",
        "{\"event\":\"def_param\",\"sid\":\"root\",\"param\":\"http.extraHeader\",\"value\":\"Authorization: PRIVATE_TOKEN\"}\n",
        "{\"event\":\"child_start\",\"sid\":\"root\",\"child_id\":0,\"child_class\":\"hook\",\"hook_name\":\"pre-push\",\"argv\":[\"PRIVATE_HOOK\"]}\n",
        "{\"event\":\"child_exit\",\"sid\":\"root\",\"child_id\":0,\"t_rel\":0.15}\n",
        "{\"event\":\"child_start\",\"sid\":\"root/child\",\"child_id\":0,\"argv\":[\"git credential-manager get\",\"PRIVATE\"]}\n",
        "{\"event\":\"child_exit\",\"sid\":\"root/child\",\"child_id\":0,\"t_rel\":1.25}\n",
        "{\"event\":\"region_leave\",\"sid\":\"root\",\"category\":\"push\",\"label\":\"check_submodules\",\"t_rel\":2.0,\"msg\":\"PRIVATE\"}\n",
        "normal Git message\n"
    );
    let (output, logs) = perf_logging::capture_for_test(|| trace().read_stderr(Cursor::new(input)).unwrap());
    assert_eq!(output, b"normal Git message\n");
    let logs = logs.join("\n");
    assert!(!logs.contains("PRIVATE"));
    assert!(logs.contains("numeric=2.49.1"));
    assert!(logs.contains("key=push.recurseSubmodules value=check"));
    assert!(logs.contains("category=credential_helper count=1 inclusive_ms=1250.0"));
    assert!(logs.contains("category=pre_push_hook count=1 inclusive_ms=150.0"));
    assert!(logs.contains("category=region_check_submodules count=1 inclusive_ms=2000.0"));
    assert!(logs.contains("additive=false"));
    assert!(logs.contains("coverage=observed_events"));
}

#[test]
fn malformed_interleaved_oversized_and_future_events_do_not_leak() {
    let input = format!("noise {{\"event\":\"start\",\"argv\":[\"PRIVATE\"]}}\n{{\"event\":broken PRIVATE\n{{\"event\":\"future\",\"secret\":\"{}\"}}\nnormal\n", "PRIVATE".repeat(8000));
    let (output, logs) = perf_logging::capture_for_test(|| trace().read_stderr(Cursor::new(input)).unwrap());
    assert_eq!(output, b"normal\n");
    assert!(!logs.join("\n").contains("PRIVATE"));
    assert!(logs.last().unwrap().contains("dropped=3"));
}

#[test]
fn child_ids_are_scoped_by_process_and_nested_times_are_not_summed_as_wall_time() {
    let events = [
        serde_json::json!({"event":"child_start","sid":"parent","child_id":0,"argv":["git-remote-https","PRIVATE"]}),
        serde_json::json!({"event":"child_start","sid":"parent/child","child_id":0,"argv":["git","pack-objects"]}),
        serde_json::json!({"event":"child_exit","sid":"parent/child","child_id":0,"t_rel":1.0}),
        serde_json::json!({"event":"child_exit","sid":"parent","child_id":0,"t_rel":5.0}),
    ];
    let input = events.iter().map(|v| format!("{v}\n")).collect::<String>();
    let (_, logs) = perf_logging::capture_for_test(|| trace().read_stderr(Cursor::new(input)).unwrap());
    assert!(logs.iter().any(|s| s.contains("category=pack_objects count=1 inclusive_ms=1000.0")));
    assert!(logs.iter().any(|s| s.contains("category=transport count=1 inclusive_ms=5000.0")));
    assert!(logs.last().unwrap().contains("unmatched_children=0"));
}

#[test]
fn missing_trace_incomplete_children_and_limits_are_explicit() {
    let (_, logs) = perf_logging::capture_for_test(|| trace().read_stderr(Cursor::new("ordinary error\n")).unwrap());
    assert!(logs.last().unwrap().contains("coverage=unavailable"));
    let mut summary = Summary::default();
    let trace = trace();
    let (_, logs) = perf_logging::capture_for_test(|| {
        for id in 0..MAX_CHILDREN + 2 {
            summary.line(&trace, format!("{{\"event\":\"child_start\",\"sid\":\"p\",\"child_id\":{id},\"argv\":[\"PRIVATE\"]}}").as_bytes(), false, &mut Vec::new());
        }
        assert_eq!(summary.children.len(), MAX_CHILDREN);
        summary.finish(&trace);
    });
    assert!(logs.len() < 8, "thousands of events must not flood the log");
    assert!(logs.last().unwrap().contains("coverage=partial"));
    assert!(!logs.join("\n").contains("PRIVATE"));
}

#[test]
fn unknown_config_values_are_not_logged_verbatim() {
    assert_eq!(config_value("push.recursesubmodules", "PRIVATE_TOKEN"), Some(("push.recurseSubmodules", "other")));
    assert_eq!(config_value("http.extraheader", "PRIVATE_TOKEN"), None);
    assert_eq!(config_value("submodule.recurse", "True"), Some(("submodule.recurse", "true")));
    assert_eq!(failure_category("Push failed: https://PRIVATE_TOKEN@server"), "other");
    assert_eq!(failure_category("Push authentication failed PRIVATE_TOKEN"), "authentication");
}

#[test]
fn config_observations_keep_local_override_and_ignore_child_settings() {
    let input = concat!(
        "{\"event\":\"def_param\",\"sid\":\"p\",\"scope\":\"global\",\"param\":\"push.recurseSubmodules\",\"value\":\"check\"}\n",
        "{\"event\":\"def_param\",\"sid\":\"p\",\"scope\":\"local\",\"param\":\"push.recurseSubmodules\",\"value\":\"no\"}\n",
        "{\"event\":\"def_param\",\"sid\":\"p/c\",\"scope\":\"local\",\"param\":\"push.recurseSubmodules\",\"value\":\"on-demand\"}\n"
    );
    let (_, logs) = perf_logging::capture_for_test(|| trace().read_stderr(Cursor::new(input)).unwrap());
    assert!(logs.iter().any(|s| s.contains("key=push.recurseSubmodules value=no scope=local")));
    assert!(!logs.iter().any(|s| s.contains("value=check") || s.contains("value=on-demand")));
    assert!(logs.iter().any(|s| s.contains("key=submodule.recurse value=not_observed")));
}

#[cfg(unix)]
#[test]
fn observed_runner_preserves_success_failure_and_timeout_without_raw_trace() {
    for (script, success) in [
        ("printf '{\"event\":\"start\",\"argv\":[\"PRIVATE\"]}\\n' >&2; printf ok; printf normal >&2", true),
        ("printf '{\"event\":\"start\",\"argv\":[\"PRIVATE\"]}\\n' >&2; printf rejected >&2; exit 7", false),
    ] {
        let mut command = Command::new("/bin/sh"); command.args(["-c", script]);
        let output = run_with_timeout_labeled_traced(command, Duration::from_secs(5), "test", "5 seconds", Some(trace())).unwrap();
        assert_eq!(output.status.success(), success);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE"));
        if success { assert_eq!(output.stdout, b"ok"); assert_eq!(output.stderr, b"normal\n"); }
        else { assert_eq!(output.status.code(), Some(7)); assert_eq!(output.stderr, b"rejected\n"); }
    }
    let mut command = Command::new("/bin/sh"); command.args(["-c", "sleep 20"]);
    let started = Instant::now();
    assert!(run_with_timeout_labeled_traced(command, Duration::from_millis(100), "test", "test budget", Some(trace())).unwrap_err().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn real_local_git_push_keeps_hooks_refspec_index_and_rejection_behaviour() {
    let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("gdd-push-diag-test-{}-{stamp}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let source = root.join("source");
    let repo = Repository::init(&source).unwrap();
    let server = root.join("server.git");
    let remote = Repository::init_bare(&server).unwrap();
    repo.remote("origin", server.to_str().unwrap()).unwrap();
    repo.config().unwrap().set_str("push.recurseSubmodules", "check").unwrap();
    fs::write(source.join("file.txt"), "snapshot\n").unwrap();
    let mut index = repo.index().unwrap(); index.add_path(Path::new("file.txt")).unwrap(); index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Fixture", "fixture@example.test").unwrap();
    let oid = repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[]).unwrap();
    let index_before = fs::read(repo.path().join("index")).unwrap();
    let config_before = fs::read(repo.path().join("config")).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        let hook = repo.path().join("hooks/pre-push");
        fs::write(&hook, "#!/bin/sh\nprintf 'hook executed\\n' >&2\n").unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let refspec = format!("{oid}:refs/heads/topic");
    let result = git_with_timeout_control_traced(source.to_str().unwrap(), &["push", "origin", &refspec], Duration::from_secs(10), "test", false, Some(PushTrace::new(&repo, "origin", false)));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(remote.refname_to_id("refs/heads/topic").unwrap(), oid);
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index_before);
    assert_eq!(fs::read(repo.path().join("config")).unwrap(), config_before);
    let history = recent_git_commands_store().lock().unwrap();
    let record = history.iter().rev().find(|entry| entry.command.contains(&refspec)).unwrap();
    assert!(!record.stderr.contains("\"event\""));
    #[cfg(unix)] assert!(record.stderr.contains("hook executed"));
    drop(history);
    // An unrelated existing remote tip must still reject a non-force publish.
    let other = repo.commit(None, &sig, &sig, "unrelated", &tree, &[]).unwrap();
    let rejected = git_with_timeout_control_traced(source.to_str().unwrap(), &["push", "origin", &format!("{other}:refs/heads/topic")], Duration::from_secs(10), "test", false, Some(PushTrace::new(&repo, "origin", false)));
    assert!(rejected.is_err()); assert_eq!(remote.refname_to_id("refs/heads/topic").unwrap(), oid);
    drop(tree); drop(index); drop(repo); drop(remote);
    fs::remove_dir_all(root).unwrap();
}
