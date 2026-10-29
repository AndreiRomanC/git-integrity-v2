//! Publish-only, allowlisted telemetry. Trace2 is consumed from a pipe: no raw
//! trace file, argv, URL, header, credential or message is written to our log.
//! Inclusive/nested timings are deliberately NOT added into a fake breakdown.
use super::*;
use std::io::Read;

const MAX_LINE: usize = 32 * 1024;
const MAX_CHILDREN: usize = 4096;
const MAX_EVENTS: usize = 50_000;
static NEXT_PUSH: AtomicU64 = AtomicU64::new(1);

pub(super) fn failure_category(error: &str) -> &'static str {
    let value = error.to_ascii_lowercase();
    if value.contains("authentication") { "authentication" }
    else if value.contains("non-fast-forward") || value.contains("server branch has newer") { "remote_ahead" }
    else if value.contains("timed out") { "timeout" }
    else if value.contains("index.lock") { "index_lock" }
    else { "other" }
}

pub(super) fn wait_with_output(mut child: std::process::Child, trace: PushTrace) -> std::io::Result<std::process::Output> {
    // Drain both streams concurrently using the same existing process wait /
    // timeout / process-group cleanup. Only the trace stream is filtered.
    let pipe = child.stderr.take().expect("publish stderr is piped");
    let reader = std::thread::spawn(move || trace.read_stderr(pipe));
    let output = child.wait_with_output();
    let stderr = reader.join().map_err(|_| std::io::Error::other("Publish diagnostic reader failed"))?;
    let mut output = output?;
    output.stderr = stderr?;
    Ok(output)
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

    pub(super) fn configure(&self, command: &mut Command) {
        command.env("GIT_TRACE2_EVENT", "2")
            .env("GIT_TRACE2_CONFIG_PARAMS", "push.recurseSubmodules,submodule.recurse,push.negotiate")
            .env("GIT_TRACE2_EVENT_NESTING", "3");
    }

    pub(super) fn read_stderr(self, mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
        let mut summary = Summary::default();
        let mut output = Vec::new();
        let mut line = Vec::new();
        let mut overflow = false;
        let mut bytes = [0u8; 8192];
        loop {
            let count = match pipe.read(&mut bytes) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => { self.log("event=trace_end coverage=incomplete reason=pipe_read_error"); return Err(error); }
            };
            if count == 0 { break; }
            for &byte in &bytes[..count] {
                if byte == b'\n' {
                    summary.line(&self, &line, overflow, &mut output);
                    line.clear(); overflow = false;
                } else if line.len() < MAX_LINE { line.push(byte); }
                else { overflow = true; }
            }
        }
        if !line.is_empty() || overflow { summary.line(&self, &line, overflow, &mut output); }
        if summary.output_truncated {
            output.extend_from_slice(b"\n[Git stderr capture truncated; process exit status is unchanged.]\n");
        }
        summary.finish(&self);
        Ok(output)
    }

    // libgit2 does not emit Git Trace2. Log callback milestones, not invented
    // network/auth phase durations; callback delivery excludes some waiting.
    pub(super) fn milestone(&self, phase: &'static str) { self.log(&format!("event=milestone phase={phase} timing=since_push_start")); }
    pub(super) fn libgit_end(&self, success: bool) {
        self.log(&format!("event=end success={success} coverage=libgit2_callback_milestones network_auth_server_wait=not_separated"));
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
    children: HashMap<(String, u64), &'static str>,
    totals: std::collections::BTreeMap<&'static str, Aggregate>,
    first: HashSet<&'static str>,
    configs: HashMap<&'static str, (&'static str, &'static str)>,
    version: bool,
    output_truncated: bool,
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
    match key {
        "push.recursesubmodules" => Some(("push.recurseSubmodules", match value.as_str() { "check" => "check", "on-demand" => "on-demand", "only" => "only", "no" | "false" => "no", _ => "other" })),
        "submodule.recurse" => Some(("submodule.recurse", match value.as_str() { "true" | "yes" | "1" => "true", "false" | "no" | "0" => "false", _ => "other" })),
        "push.negotiate" => Some(("push.negotiate", match value.as_str() { "true" | "yes" | "1" => "true", "false" | "no" | "0" => "false", _ => "other" })),
        _ => None,
    }
}

impl Summary {
    fn line(&mut self, trace: &PushTrace, line: &[u8], overflow: bool, output: &mut Vec<u8>) {
        let text = String::from_utf8_lossy(line);
        // Concurrent child stderr can interleave. Treat malformed/interleaved
        // Trace2 as incomplete, NEVER as ordinary console output with argv.
        let trace_line = text.contains("\"event\"");
        if !trace_line {
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
                if let Some((key, value)) = config_value(&event["param"].as_str().unwrap_or("").to_ascii_lowercase(), event["value"].as_str().unwrap_or("")) {
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
                    self.children.insert((sid.to_owned(), id), category);
                    if self.first.insert(category) { trace.log(&format!("event=first_child category={category}")); }
                }
            }
            "child_exit" => {
                if let Some(id) = event["child_id"].as_u64() {
                    if let Some(category) = self.children.remove(&(sid.to_owned(), id)) {
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
        for key in ["push.recurseSubmodules", "submodule.recurse", "push.negotiate"] {
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
