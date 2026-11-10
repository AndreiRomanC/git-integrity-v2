//! Publish-only, allowlisted telemetry. Trace2 is consumed from a pipe: no raw
//! trace file, argv, URL, header, credential or message is written to our log.
//! Inclusive/nested timings are deliberately NOT added into a fake breakdown.
use super::*;
pub(super) mod runner;

const MAX_LINE: usize = 32 * 1024;
const MAX_CHILDREN: usize = 4096;
const MAX_EVENTS: usize = 50_000;
static NEXT_PUSH: AtomicU64 = AtomicU64::new(1);

pub(super) fn failure_category(error: &str) -> &'static str {
    let value = error.to_ascii_lowercase();
    if value.contains("timed out") { "timeout" }
    else if value.contains("authentication") { "authentication" }
    else if value.contains("non-fast-forward") || value.contains("server branch has newer") { "remote_ahead" }
    else if value.contains("index.lock") { "index_lock" }
    else { "other" }
}

pub(super) struct PushTrace {
    pub(super) id: u64,
    started: Instant,
}

impl PushTrace {
    pub(super) fn new(repo: &Repository, remote_name: &str, token: bool) -> Self {
        let trace = Self { id: NEXT_PUSH.fetch_add(1, Ordering::Relaxed), started: Instant::now() };
        let remote = repo.find_remote(remote_name).ok();
        let url = remote.as_ref().and_then(|r| r.pushurl().or_else(|| r.url())).unwrap_or("");
        let transport = if url.starts_with("https://") { "https" } else if url.starts_with("http://") { "http" }
            else if url.starts_with("ssh://") || (url.contains('@') && url.contains(':')) { "ssh" }
            else if url.starts_with("git://") { "git" } else { "local_or_other" };
        trace.log(&format!("event=start engine={} transport={transport} config_changed=false extra_network_requests=false", if token { "libgit2" } else { "git_cli" }));
        trace
    }

    fn log(&self, fields: &str) { perf_log(&format!("publish_diag: push={} {fields}", self.id), self.started.elapsed()); }

    pub(super) fn log_command(&self, fast: bool, is_tip: bool) {
        self.log(&format!("event=command shape=git_push_remote_oid_to_branch mode={} source={} force=false hooks=normal recursion=git_config terminal_prompt=false stdin=null helper_interactivity=inherited timeout_s={} automatic_retry=false",
            if fast { "fast" } else { "safe" }, if is_tip { "branch_tip" } else { "selected_commit" }, GIT_COMMAND_TIMEOUT.as_secs()));
        let presence = |key| if std::env::var_os(key).is_some() { "set" } else { "unset" };
        let interactive = std::env::var("GCM_INTERACTIVE").ok();
        self.log(&format!("event=prompt_environment git_askpass={} ssh_askpass={} gcm_interactive={} values_redacted=true",
            presence("GIT_ASKPASS"), presence("SSH_ASKPASS"), prompt_setting(interactive.as_deref())));
    }

    pub(super) fn configure(&self, command: &mut Command) {
        command.env("GIT_TRACE2_EVENT", "2")
            .env("GIT_TRACE2_CONFIG_PARAMS", "push.recurseSubmodules,submodule.recurse,push.negotiate,credential.interactive,credential.guiPrompt")
            .env("GIT_TRACE2_EVENT_NESTING", "3");
    }

    // libgit2 does not emit Git Trace2. Log callback milestones, not invented
    // network/auth phase durations; callback delivery excludes some waiting.
    pub(super) fn milestone(&self, phase: &'static str) { self.log(&format!("event=milestone phase={phase} timing=since_push_start")); }
    pub(super) fn libgit_end(&self, success: bool) {
        self.log(&format!("event=end success={success} coverage=libgit2_callback_milestones network_auth_server_wait=not_separated"));
    }
}

#[derive(Default)]
struct Capture {
    summary: Summary,
    output: Vec<u8>,
    line: Vec<u8>,
    overflow: bool,
}

impl Capture {
    fn feed(&mut self, trace: &PushTrace, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.flush_line(trace);
            } else if self.line.len() < MAX_LINE { self.line.push(byte); }
            else { self.overflow = true; }
        }
    }

    fn flush_line(&mut self, trace: &PushTrace) {
        self.summary.line(trace, &self.line, self.overflow, &mut self.output);
        self.line.clear();
        self.overflow = false;
    }

    fn finish(mut self, trace: &PushTrace, eof: bool) -> Vec<u8> {
        // A cut-off JSON/Trace2 prefix may contain raw argv later in the line.
        if eof && (!self.line.is_empty() || self.overflow) { self.flush_line(trace); }
        if !eof || self.summary.output_truncated {
            self.output.extend_from_slice(b"\n[Git stderr capture incomplete; process exit status is unchanged.]\n");
        }
        self.summary.finish(trace);
        self.output
    }
}

#[derive(Default)]
struct Aggregate { count: u64, seconds: f64, max: f64 }
impl Aggregate {
    fn add(&mut self, seconds: f64) { self.count += 1; self.seconds += seconds; self.max = self.max.max(seconds); }
}

#[derive(Default)]
struct Summary {
    events: usize,
    dropped: usize,
    children: HashMap<(String, u64), (&'static str, &'static str, Instant)>,
    totals: std::collections::BTreeMap<&'static str, Aggregate>,
    first: HashSet<&'static str>,
    helper_events: HashSet<(&'static str, bool)>,
    configs: HashMap<&'static str, (&'static str, &'static str)>,
    version: bool,
    output_truncated: bool,
    stderr_reason: Option<&'static str>,
}

fn helper_action(event: &serde_json::Value) -> &'static str {
    // Both ["git", "credential-manager", "get"] and ["git credential-manager get"] occur.
    match event["argv"].as_array().and_then(|a| a.last()).and_then(|s| s.as_str())
        .and_then(|s| s.split_ascii_whitespace().last()) {
        Some("get") => "get", Some("store") => "store", Some("erase") => "erase", _ => "unknown",
    }
}

fn prompt_setting(value: Option<&str>) -> &'static str {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("never" | "false" | "0" | "no") => "disabled",
        Some("true" | "1" | "yes") => "enabled", Some("auto") => "auto",
        None => "unset", Some(_) => "other",
    }
}

fn stderr_reason(text: &str) -> Option<&'static str> {
    let s = text.to_ascii_lowercase();
    if s.contains("authentication failed") || s.contains("terminal prompts disabled") || s.contains("could not read username") { Some("authentication") }
    else if (s.contains("403") && s.contains("http")) || s.contains("returned error: 403") { Some("http_forbidden") }
    else if s.contains("could not resolve host") { Some("dns") }
    else if s.contains("could not resolve proxy") { Some("proxy") }
    else if s.contains("ssl certificate") || s.contains("certificate verify failed") { Some("tls_certificate") }
    else if s.contains("failed to connect") || s.contains("connection reset") { Some("connection") }
    else if s.contains("non-fast-forward") || s.contains("[rejected]") { Some("remote_rejection") }
    else { None }
}

fn child_category(event: &serde_json::Value) -> &'static str {
    if event["child_class"] == "hook" { return if event["hook_name"] == "pre-push" { "pre_push_hook" } else { "other_hook" }; }
    let args = event["argv"].as_array();
    let word = |index: usize| args.and_then(|a| a.get(index)).and_then(|v| v.as_str()).unwrap_or("");
    let first = word(0).rsplit(['/', '\\']).next().unwrap_or("");
    // Some credential helpers are shell command strings; classify only known
    // prefixes. Unknown/custom scripts remain 'other_child', never log argv.
    if first.starts_with("git credential-") || first.starts_with("git-credential-") || first.starts_with("credential-") || word(1).starts_with("credential-") { return "credential_helper"; }
    let program = if first == "git" || first == "git.exe" { word(1) } else { first };
    match program.trim_end_matches(".exe") {
        "pack-objects" | "git-pack-objects" => "pack_objects",
        "rev-list" => "history_walk",
        "submodule" | "submodule--helper" | "git-submodule" => "submodule",
        "remote-https" | "remote-http" | "git-remote-https" | "git-remote-http" | "ssh" | "plink" | "git-receive-pack" => "transport",
        "send-pack" => "send_pack",
        _ => "other_child",
    }
}

fn config_value(key: &str, value: &str) -> Option<(&'static str, &'static str)> {
    let value = value.to_ascii_lowercase();
    match key.to_ascii_lowercase().as_str() {
        "push.recursesubmodules" => Some(("push.recurseSubmodules", match value.as_str() { "check" => "check", "on-demand" => "on-demand", "only" => "only", "no" | "false" => "no", _ => "other" })),
        "submodule.recurse" => Some(("submodule.recurse", match value.as_str() { "true" | "yes" | "1" => "true", "false" | "no" | "0" => "false", _ => "other" })),
        "push.negotiate" => Some(("push.negotiate", match value.as_str() { "true" | "yes" | "1" => "true", "false" | "no" | "0" => "false", _ => "other" })),
        "credential.interactive" => Some(("credential.interactive", prompt_setting(Some(&value)))),
        "credential.guiprompt" => Some(("credential.guiPrompt", prompt_setting(Some(&value)))),
        _ => None,
    }
}

impl Summary {
    fn helper_observed_open(&self) -> bool {
        self.children.values().any(|(category, _, _)| *category == "credential_helper")
    }
    fn snapshot(&self) -> String {
        let mut active = std::collections::BTreeMap::new();
        for &(category, action, since) in self.children.values() {
            let entry = active.entry((category, action)).or_insert((0usize, 0u128));
            entry.0 += 1;
            entry.1 = entry.1.max(since.elapsed().as_millis());
        }
        let groups = active.iter().map(|((c,a),(count,age))| format!("{c}/{a}:{count}@{age}ms")).collect::<Vec<_>>().join(",");
        format!("observed_open_children={} stderr_reason={} trace_events={} dropped={}",
            if groups.is_empty() { "none" } else { &groups }, self.stderr_reason.unwrap_or("not_observed"), self.events, self.dropped)
    }

    fn line(&mut self, trace: &PushTrace, line: &[u8], overflow: bool, output: &mut Vec<u8>) {
        let text = String::from_utf8_lossy(line);
        // Concurrent child stderr can interleave. Treat malformed/interleaved
        // Trace2 as incomplete, NEVER as ordinary console output with argv.
        let trace_line = text.contains("\"event\"");
        if !trace_line {
            if !overflow {
                if let Some(reason) = stderr_reason(&text) { self.stderr_reason = Some(reason); }
            }
            let available = RECENT_GIT_COMMAND_OUTPUT_LIMIT.saturating_sub(output.len());
            self.output_truncated |= overflow || line.len() > available;
            output.extend_from_slice(&line[..line.len().min(available)]);
            if output.len() < RECENT_GIT_COMMAND_OUTPUT_LIMIT { output.push(b'\n'); }
            return;
        }
        if overflow || self.events >= MAX_EVENTS { self.dropped += 1; return; }
        let Ok(event) = serde_json::from_slice::<serde_json::Value>(line) else { self.dropped += 1; return; };
        self.events += 1;
        let sid = event["sid"].as_str().filter(|s| s.len() <= 1024).unwrap_or("");
        let seconds = event["t_rel"].as_f64().filter(|s| s.is_finite() && *s >= 0.0 && *s <= 86400.0);
        match event["event"].as_str().unwrap_or("") {
            "version" if !self.version => {
                self.version = true;
                let version = event["exe"].as_str().unwrap_or("").split('.').take(3)
                    .map(|s| s.chars().take(4).take_while(char::is_ascii_digit).collect::<String>()).collect::<Vec<_>>().join(".");
                trace.log(&format!("event=git_version numeric={version}"));
            }
            "def_param" => {
                if let Some((key, value)) = config_value(event["param"].as_str().unwrap_or(""), event["value"].as_str().unwrap_or("")) {
                    // Keep the last ROOT observation (global/local/command
                    // values can repeat); never substitute a child's config.
                    // This is reported as an observation, not a Git override.
                    let scope = match event["scope"].as_str().unwrap_or("") {
                        "system" => "system", "global" => "global", "local" => "local", "worktree" => "worktree", "command" => "command", _ => "unknown",
                    };
                    if !sid.contains('/') { self.configs.insert(key, (value, scope)); }
                }
            }
            "child_start" => {
                if let Some(id) = event["child_id"].as_u64() {
                    if self.children.len() >= MAX_CHILDREN { self.dropped += 1; return; }
                    let category = child_category(&event);
                    let action = if category == "credential_helper" { helper_action(&event) } else { "-" };
                    self.children.insert((sid.to_owned(), id), (category, action, Instant::now()));
                    let first = if category == "credential_helper" { self.helper_events.insert((action, false)) } else { self.first.insert(category) };
                    if first {
                        trace.log(&format!("event=first_child category={category} action={action}"));
                    }
                }
            }
            "child_exit" => {
                if let Some(id) = event["child_id"].as_u64() {
                    if let Some((category, action, _)) = self.children.remove(&(sid.to_owned(), id)) {
                        if category == "credential_helper" && self.helper_events.insert((action, true)) {
                            trace.log(&format!("event=first_helper_exit action={action} exit_code={}", event["code"].as_i64().map(|v| v.to_string()).unwrap_or_else(|| "unknown".into())));
                        }
                        if let Some(seconds) = seconds { self.totals.entry(category).or_default().add(seconds); }
                        else { self.dropped += 1; }
                    }
                }
            }
            "region_leave" => {
                let category = match (event["category"].as_str(), event["label"].as_str()) {
                    (Some("push"), Some("check_submodules")) => Some("region_check_submodules"),
                    (Some("push"), Some("push_submodules")) => Some("region_push_submodules"),
                    (Some("push"), Some("transport_push")) => Some("region_transport_push"),
                    (Some("transport_push"), Some("push_refs")) => Some("region_push_refs"),
                    _ => None,
                };
                if let (Some(category), Some(seconds)) = (category, seconds) { self.totals.entry(category).or_default().add(seconds); }
            }
            _ => {},
        }
    }

    fn finish(self, trace: &PushTrace) {
        for key in ["push.recurseSubmodules", "submodule.recurse", "push.negotiate", "credential.interactive", "credential.guiPrompt"] {
            match self.configs.get(key) {
                Some((value, scope)) => trace.log(&format!("event=config key={key} value={value} scope={scope} interpretation=last_root_observation")),
                None => trace.log(&format!("event=config key={key} value=not_observed")),
            }
        }
        for (category, time) in &self.totals {
            trace.log(&format!("event=timing category={category} count={} inclusive_ms={:.1} max_ms={:.1} additive=false", time.count, time.seconds * 1000.0, time.max * 1000.0));
        }
        trace.log(&format!("event=trace_end events={} dropped={} unmatched_children={} coverage={} missing_categories=not_proof_of_absence network_and_server_wait=not_separated",
            self.events, self.dropped, self.children.len(), if self.events == 0 { "unavailable" } else if self.dropped > 0 || !self.children.is_empty() { "partial" } else { "observed_events" }));
    }
}

#[cfg(test)]
mod tests;
