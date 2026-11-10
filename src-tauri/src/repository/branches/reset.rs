//! Explicit branch reset, deliberately separate from recursive Clean checkout.
//! Reads occur only when opening the dialog; execution never fetches or pushes.
use super::*;

#[derive(Serialize)]
pub struct ResetBranchPreview {
    branch: String,
    head: String,
    target: String,
    subject: String,
    relation: String,
    removed_commits: Option<usize>,
    upstream: Option<String>,
    push_status: String,
}

fn reset_repository(path: &str) -> Result<Repository, String> {
    // Never discover a parent if a submodule's metadata disappeared.
    let repo = Repository::open(path).map_err(|e| format!("Cannot open the selected repository: {}", e.message()))?;
    let workdir = repo.workdir().ok_or("Reset requires a working repository, not a bare repository.")?;
    if workdir.canonicalize().map_err(|e| e.to_string())? != Path::new(path).canonicalize().map_err(|e| e.to_string())? {
        return Err("The selected path is not this repository's root. Nothing was reset.".into());
    }
    Ok(repo)
}

fn checked_head(repo: &Repository, branch: &str, expected_head: &str) -> Result<Oid, String> {
    let head = repo.head().map_err(|e| e.message().to_string())?;
    if repo.head_detached().unwrap_or(true) || head.name() != Some(format!("refs/heads/{branch}").as_str())
        || head.target().map(|oid| oid.to_string()).as_deref() != Some(expected_head) {
        return Err("The active local branch or HEAD changed. Nothing was reset; reopen Reset on the current branch.".into());
    }
    if repo.state() != git2::RepositoryState::Clean {
        return Err("Finish or abort the active merge/rebase/cherry-pick before resetting. Nothing was reset.".into());
    }
    head.peel_to_commit().map(|commit| commit.id()).map_err(|e| e.message().to_string())
}

fn preview(repo: &Repository, branch: &str, expected_head: &str, target_ref: &str) -> Result<ResetBranchPreview, String> {
    let head = checked_head(repo, branch, expected_head)?;
    let commit = repo.revparse_single(target_ref).and_then(|obj| obj.peel_to_commit())
        .map_err(|_| "The target commit is unavailable locally. Fetch explicitly if needed, then reopen Reset.")?;
    let target = commit.id();
    // Missing/shallow ancestry must not be presented as a definitive verdict.
    let counts = if repo.is_shallow() { None } else { repo.graph_ahead_behind(head, target).ok() };
    let relation = if head == target { "same" } else { match counts {
        Some((_, 0)) => "backward", Some((0, _)) => "forward", Some(_) => "divergent", None => "unknown",
    }};
    let local = repo.find_branch(branch, BranchType::Local).map_err(|e| e.message().to_string())?;
    let upstream = local.upstream().ok();
    let upstream_name = upstream.as_ref().and_then(|b| b.get().shorthand()).map(str::to_string);
    let configured = repo.config().ok().and_then(|c| c.get_string(&format!("branch.{branch}.remote")).ok()).is_some();
    let push_status = match upstream.as_ref() {
        None if configured => "unknown",
        None => "not_configured",
        Some(b) if !b.get().name().unwrap_or("").starts_with("refs/remotes/") => "local_upstream",
        Some(b) => match b.get().peel_to_commit().ok().map(|c| c.id()) {
            Some(oid) if oid == target => "fast_forward",
            Some(oid) if !repo.is_shallow() => match repo.graph_descendant_of(target, oid) {
                Ok(true) => "fast_forward", Ok(false) => "non_fast_forward", Err(_) => "unknown",
            },
            _ => "unknown",
        },
    };
    Ok(ResetBranchPreview { branch: branch.into(), head: head.to_string(), target: target.to_string(),
        subject: commit.summary().unwrap_or("").to_string(), relation: relation.into(),
        removed_commits: counts.map(|(removed, _)| removed), upstream: upstream_name, push_status: push_status.into() })
}

#[tauri::command]
pub async fn preview_branch_reset(repository_path: String, branch: String, expected_head: String, target_ref: String) -> Result<ResetBranchPreview, String> {
    off_main_thread(move || {
        validate_path(&repository_path)?;
        preview(&reset_repository(&repository_path)?, &branch, &expected_head, &target_ref)
    }).await
}

#[tauri::command]
pub async fn reset_current_branch(repository_path: String, branch: String, expected_head: String, target_commit: String, mode: String) -> Result<(), String> {
    off_main_thread(move || reset_inner(&repository_path, &branch, &expected_head, &target_commit, &mode)).await
}

fn reset_inner(path: &str, branch: &str, expected_head: &str, target: &str, mode: &str) -> Result<(), String> {
    let flag = match mode { "soft" => "--soft", "mixed" => "--mixed", "hard" => "--hard", _ => return Err("Choose Soft, Mixed or Hard reset.".into()) };
    // Execution is bound to the immutable commit inspected in the dialog, never
    // a branch name which could resolve to a different commit on confirmation.
    let oid = Oid::from_str(target).map_err(|_| "Reset requires the exact previewed commit ID.")?;
    validate_path(path)?;
    let queued = Instant::now();
    let lock_handle = repo_write_lock(path);
    let _lock = lock_handle.lock().unwrap();
    log_repo_write_lock_acquired(path, "reset_current_branch", queued.elapsed());
    let repo = reset_repository(path)?;
    checked_head(&repo, branch, expected_head)?;
    repo.find_commit(oid).map_err(|_| "Target commit is unavailable; nothing was reset.")?;
    let own = repo.workdir().ok_or("The repository has no worktree.")?.canonicalize().map_err(|e| e.to_string())?;
    // Git can report the main worktree of an absorbed submodule as its actual
    // gitdir (.git/modules/...), not core.worktree. Recognize ONLY our own
    // metadata in that case. A linked worktree must never exempt the main one.
    let own_metadata = if repo.is_worktree() { None } else { Some(repo.path().canonicalize().map_err(|e| e.to_string())?) };
    let occupied = git(path, &["worktree", "list", "--porcelain", "-z"])?
        .split("\0\0").any(|record| {
            let belongs_here = record.split('\0').find_map(|line| line.strip_prefix("worktree "))
                .and_then(|p| fs::canonicalize(p).ok())
                .is_some_and(|p| p == own || own_metadata.as_ref() == Some(&p));
            record.split('\0').any(|line| line == format!("branch refs/heads/{branch}")) && !belongs_here
        });
    if occupied { return Err("This branch is also checked out in another worktree. Nothing was reset.".into()); }
    restore_checkpoint_wait_for_index(&repo.path().join("index.lock"), "before-branch-reset", RESTORE_INDEX_LOCK_WAIT)
        .map_err(|e| format!("Reset was not started. {e}"))?;
    checked_head(&reset_repository(path)?, branch, expected_head)?;
    drop(repo);
    let started = Instant::now();
    // Explicitly override submodule.recurse: a parent reset changes gitlinks,
    // not nested worktrees. Hard is standard Git reset, NOT git clean -ffd.
    let result = git(path, &["reset", flag, "--no-recurse-submodules", &oid.to_string(), "--"]);
    invalidate_git_metadata(path);
    invalidate_submodule_sync(path);
    perf_log(&format!("reset_current_branch: mode={mode} success={} recursive=false fetch=false push=false source_backup=false", result.is_ok()), started.elapsed());
    result.map_err(|e| format!("Reset did not complete. Git may have changed files, staging or HEAD; inspect the workspace before retrying. No source backup was created and nothing was pushed. {e}"))?;
    checked_head(&reset_repository(path)?, branch, &oid.to_string())
        .map_err(|e| format!("Git reset finished, but its resulting HEAD could not be verified. Refresh and inspect before continuing. {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture { root: PathBuf, a: String, b: String }
    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("gdd-reset-test-{}-{nonce}", std::process::id()));
            fs::create_dir(&root).unwrap();
            let mut f = Self { root, a: String::new(), b: String::new() };
            f.git(&["init", "-b", "topic"]);
            f.git(&["config", "user.name", "Test"]); f.git(&["config", "user.email", "test@example.invalid"]);
            f.write("tracked", "A\n"); f.git(&["add", "."]); f.git(&["commit", "-m", "A"]); f.a = f.git(&["rev-parse", "HEAD"]);
            f.write("tracked", "B\n"); f.git(&["commit", "-am", "B"]); f.b = f.git(&["rev-parse", "HEAD"]);
            f.git(&["branch", "other"]);
            f
        }
        fn path(&self) -> &str { self.root.to_str().unwrap() }
        fn git(&self, args: &[&str]) -> String {
            let output = std::process::Command::new("git").arg("-C").arg(&self.root).args(args).output().unwrap();
            assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout).trim().into()
        }
        fn write(&self, path: &str, text: &str) { fs::write(self.root.join(path), text).unwrap(); }
        fn preview(&self, target: &str) -> ResetBranchPreview {
            preview(&internal_repository(self.path()).unwrap(), "topic", &self.b, target).unwrap()
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }

    #[test]
    fn reset_modes_keep_their_exact_git_semantics_and_other_branches() {
        for mode in ["soft", "mixed", "hard"] {
            let f = Fixture::new();
            f.git(&["config", "branch.topic.remote", "company"]);
            f.git(&["config", "branch.topic.merge", "refs/heads/server-name"]);
            f.write("tracked", "staged\n"); f.git(&["add", "tracked"]);
            f.write("tracked", "unstaged\n"); f.write("untracked.tdcl", "not a backup\n");
            f.write(".gitignore", "*.build\n"); f.write("ignored.build", "build\n");
            reset_inner(f.path(), "topic", &f.b, &f.a, mode).unwrap();
            assert_eq!(f.git(&["branch", "--show-current"]), "topic");
            assert_eq!(f.git(&["rev-parse", "HEAD"]), f.a);
            assert_eq!(f.git(&["rev-parse", "other"]), f.b);
            assert_eq!(f.git(&["rev-parse", "ORIG_HEAD"]), f.b);
            assert_eq!(f.git(&["show", ":tracked"]), if mode == "soft" { "staged" } else { "A" });
            assert_eq!(fs::read_to_string(f.root.join("tracked")).unwrap(), if mode == "hard" { "A\n" } else { "unstaged\n" });
            assert!(f.root.join("untracked.tdcl").exists()); assert!(f.root.join("ignored.build").exists());
            assert_eq!(f.git(&["config", "branch.topic.remote"]), "company");
            assert_eq!(f.git(&["config", "branch.topic.merge"]), "refs/heads/server-name");
            assert!(!f.root.join(".git/gdd-restore").exists());
        }
    }

    #[test]
    fn preview_uses_interval_ancestry_and_real_upstream_without_mutation() {
        let f = Fixture::new();
        f.git(&["config", "remote.company.url", "https://example.invalid/repo"]);
        f.git(&["config", "remote.company.fetch", "+refs/heads/*:refs/remotes/company/*"]);
        f.git(&["config", "branch.topic.remote", "company"]); f.git(&["config", "branch.topic.merge", "refs/heads/server-name"]);
        f.git(&["update-ref", "refs/remotes/company/server-name", &f.b]);
        f.write("tracked", "staged\n"); f.git(&["add", "tracked"]); f.write("tracked", "unstaged\n");
        let index = fs::read(f.root.join(".git/index")).unwrap();
        let p = f.preview(&f.a);
        assert_eq!(p.relation, "backward"); assert_eq!(p.removed_commits, Some(1));
        assert_eq!(p.push_status, "non_fast_forward"); assert_eq!(p.upstream.as_deref(), Some("company/server-name"));
        assert_eq!(f.preview(&f.b).relation, "same"); assert_eq!(f.preview(&f.b).push_status, "fast_forward");
        f.git(&["update-ref", "refs/remotes/company/server-name", &f.a]);
        assert_eq!(f.preview(&f.a).push_status, "fast_forward");
        f.git(&["update-ref", "-d", "refs/remotes/company/server-name"]);
        assert_eq!(f.preview(&f.a).push_status, "unknown");
        assert_eq!(fs::read(f.root.join(".git/index")).unwrap(), index);
        assert_eq!(fs::read_to_string(f.root.join("tracked")).unwrap(), "unstaged\n");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), f.b);
    }

    #[test]
    fn reset_accepts_forward_or_divergent_commit_without_upstream_and_preserves_target_branch() {
        for divergent in [false, true] {
            let f = Fixture::new();
            f.git(&["checkout", "-b", "target", if divergent { &f.a } else { &f.b }]);
            f.write("tracked", "C\n"); f.git(&["commit", "-am", "C"]);
            let c = f.git(&["rev-parse", "HEAD"]); f.git(&["checkout", "topic"]);
            let p = f.preview("refs/heads/target");
            assert_eq!(p.relation, if divergent { "divergent" } else { "forward" });
            assert_eq!(p.push_status, "not_configured");
            reset_inner(f.path(), "topic", &f.b, &c, "mixed").unwrap();
            assert_eq!(f.git(&["rev-parse", "topic"]), c);
            assert_eq!(f.git(&["rev-parse", "target"]), c);
            assert_eq!(fs::read_to_string(f.root.join("tracked")).unwrap(), "B\n");
        }
    }

    #[test]
    fn reset_refuses_stale_selection_detached_operation_and_index_lock_without_deleting_work() {
        for reason in ["head", "branch", "detached", "operation", "lock", "mode", "target", "blob", "worktree"] {
            let f = Fixture::new();
            f.write("tracked", "keep edits\n");
            match reason {
                "detached" => { f.git(&["checkout", "--detach"]); }
                "operation" => { f.write(".git/MERGE_HEAD", &f.a); }
                "lock" => { f.write(".git/index.lock", "keep lock"); }
                "worktree" => { f.git(&["worktree", "add", "--force", f.root.join("other-worktree").to_str().unwrap(), "topic"]); }
                _ => {}
            }
            let blob = f.git(&["rev-parse", "HEAD:tracked"]);
            let error = reset_inner(f.path(), if reason == "branch" { "other" } else { "topic" },
                if reason == "head" { &f.a } else { &f.b },
                match reason { "target" => "HEAD~1", "blob" => &blob, _ => &f.a },
                if reason == "mode" { "--hard" } else { "hard" });
            assert!(error.is_err(), "{reason}");
            assert_eq!(f.git(&["rev-parse", "HEAD"]), f.b, "{reason}");
            assert_eq!(fs::read_to_string(f.root.join("tracked")).unwrap(), "keep edits\n", "{reason}");
            if reason == "lock" { assert_eq!(fs::read_to_string(f.root.join(".git/index.lock")).unwrap(), "keep lock"); }
        }
    }

    #[test]
    fn hard_reset_does_not_recurse_even_with_submodule_recurse_configured() {
        let f = Fixture::new(); let seed = Fixture::new();
        f.git(&["-c", "protocol.file.allow=always", "submodule", "add", seed.path(), "module"]);
        f.git(&["commit", "-am", "module at B"]);
        let with_module = f.git(&["rev-parse", "HEAD"]);
        f.git(&["-C", "module", "checkout", "--detach", &seed.a]);
        f.git(&["commit", "-am", "module at A"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        f.git(&["config", "submodule.recurse", "true"]);
        f.write("module/tracked", "module edits\n"); f.write("module/input.tdcl", "module input\n");
        reset_inner(f.path(), "topic", &head, &with_module, "hard").unwrap();
        assert!(f.git(&["ls-files", "--stage", "module"]).contains(&seed.b));
        assert_eq!(f.git(&["-C", "module", "rev-parse", "HEAD"]), seed.a);
        assert_eq!(fs::read_to_string(f.root.join("module/tracked")).unwrap(), "module edits\n");
        assert!(f.root.join("module/input.tdcl").exists());
        assert!(!f.root.join(".git/modules/module/gdd-restore").exists());
    }

    #[test]
    fn missing_submodule_metadata_never_resets_parent() {
        let f = Fixture::new();
        let missing = f.root.join("missing-module"); fs::create_dir(&missing).unwrap();
        assert!(reset_inner(missing.to_str().unwrap(), "topic", &f.b, &f.a, "hard").is_err());
        assert_eq!(f.git(&["rev-parse", "HEAD"]), f.b);
    }

    #[test]
    fn resetting_submodule_own_branch_does_not_stage_or_move_parent() {
        let f = Fixture::new(); let seed = Fixture::new();
        f.git(&["-c", "protocol.file.allow=always", "submodule", "add", seed.path(), "module"]);
        f.git(&["commit", "-am", "add module"]);
        let parent_head = f.git(&["rev-parse", "HEAD"]);
        let parent_index = fs::read(f.root.join(".git/index")).unwrap();
        f.git(&["-C", "module", "checkout", "-b", "module-work"]);
        reset_inner(f.root.join("module").to_str().unwrap(), "module-work", &seed.b, &seed.a, "hard").unwrap();
        assert_eq!(f.git(&["-C", "module", "rev-parse", "HEAD"]), seed.a);
        assert_eq!(f.git(&["-C", "module", "branch", "--show-current"]), "module-work");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), parent_head);
        assert_eq!(fs::read(f.root.join(".git/index")).unwrap(), parent_index);
    }

    #[test]
    fn linked_worktree_uses_its_own_head_index_and_lock() {
        let f = Fixture::new(); let linked = f.root.join("linked with spaces-é");
        f.git(&["worktree", "add", "-b", "linked", linked.to_str().unwrap(), "HEAD"]);
        let gitdir = reset_repository(linked.to_str().unwrap()).unwrap().path().to_path_buf();
        fs::write(gitdir.join("index.lock"), "lock").unwrap();
        assert!(reset_inner(linked.to_str().unwrap(), "linked", &f.b, &f.a, "hard").is_err());
        fs::remove_file(gitdir.join("index.lock")).unwrap(); // test-owned lock only
        reset_inner(linked.to_str().unwrap(), "linked", &f.b, &f.a, "hard").unwrap();
        assert_eq!(f.git(&["rev-parse", "linked"]), f.a);
        assert_eq!(f.git(&["rev-parse", "topic"]), f.b);
        assert_eq!(f.git(&["show", ":tracked"]), "B");
        assert_eq!(fs::read_to_string(linked.join("tracked")).unwrap(), "A\n");
    }

    #[test]
    fn shallow_ancestry_is_explicitly_unknown_not_a_false_push_verdict() {
        let f = Fixture::new();
        f.git(&["config", "remote.company.url", "https://example.invalid/repo"]);
        f.git(&["config", "remote.company.fetch", "+refs/heads/*:refs/remotes/company/*"]);
        f.git(&["config", "branch.topic.remote", "company"]); f.git(&["config", "branch.topic.merge", "refs/heads/main"]);
        f.git(&["update-ref", "refs/remotes/company/main", &f.a]);
        f.write(".git/shallow", &format!("{}\n", f.b));
        let p = f.preview(&f.b);
        assert_eq!(p.push_status, "unknown"); assert_eq!(p.removed_commits, None);
        assert_eq!(f.preview(&f.a).relation, "unknown");
    }
}
