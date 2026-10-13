//! Conservative, demand-driven provenance of endpoint differences. No checkout,
//! index, worktree, blame subprocess, fetch, or submodule traversal is involved.
//! Unlike a path-touch filter, a witness must explain a surviving endpoint fact.
use super::*;
use std::sync::Arc;

#[derive(Debug, Serialize, Default)]
pub struct Contributions {
    paths: Vec<String>,
    ambiguous_paths: BTreeMap<String, String>,
    evidence: BTreeMap<String, Vec<ComparisonTouch>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Entry { id: Oid, mode: i32 }

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Probe {
    // Presence AND absence are traced. Blaming the old line would incorrectly
    // attribute deletions to its original writer rather than its remover.
    Line(Vec<u8>, bool),
    Atomic(Option<Entry>),
    Exists(bool),
    Mode(Option<i32>),
}

#[derive(Clone, Debug)]
enum Proof { Other, Own(Oid, Option<Oid>, String), Unknown }

#[derive(Clone)]
struct CommitInfo { tree: Oid, parents: Vec<Oid>, author: String, time: i64 }
struct Text { counts: HashMap<Vec<u8>, usize>, supported: bool }

struct Analysis<'a> {
    repo: &'a Repository,
    author: &'a str,
    exclusive: HashSet<Oid>,
    commits: HashMap<Oid, CommitInfo>,
    entries: HashMap<(Oid, String), Option<Entry>>,
    texts: HashMap<Oid, Arc<Text>>,
    renames: HashMap<(Oid, Oid), HashMap<String, (String, bool)>>,
    proofs: HashMap<(Oid, String, Probe), Proof>,
    deletions: HashMap<(Oid, String), HashSet<Vec<u8>>>,
    unchanged_origins: HashMap<(Oid, String), Oid>,
    started: Instant,
    visits: usize,
    bytes: usize,
    peak_bytes: usize,
    current_path: String,
    request_id: u64,
    file_total: usize,
    outcomes: [usize; 3], // proven, inconclusive, no surviving witness
    file_started: Instant,
    file_visits: usize,
    probes: usize,
    probes_done: usize,
    detail_logs: usize,
}

fn strict_budget_reason(bytes: usize, elapsed: Duration) -> Option<&'static str> {
    if bytes > 64 * 1024 * 1024 { Some("per-file memory") }
    else if elapsed > Duration::from_secs(30) { Some("elapsed time") } else { None }
}

fn identity(commit: &git2::Commit<'_>) -> String {
    let signature = commit.author();
    let email = signature.email().unwrap_or("").trim();
    if email.is_empty() { format!("name:{}", signature.name().unwrap_or("Unknown author")) }
    else { format!("email:{email}") }
}

fn rename_counts(diff: &git2::Diff<'_>) -> (HashMap<Oid, usize>, HashMap<Oid, usize>) {
    let mut removed = HashMap::new(); let mut added = HashMap::new();
    for delta in diff.deltas() {
        if delta.status() == git2::Delta::Deleted { *removed.entry(delta.old_file().id()).or_insert(0) += 1; }
        if delta.status() == git2::Delta::Added { *added.entry(delta.new_file().id()).or_insert(0) += 1; }
    }
    (removed, added)
}

impl<'a> Analysis<'a> {
    fn budget(&mut self) -> Result<(), String> {
        self.visits += 1;
        self.peak_bytes = self.peak_bytes.max(self.bytes);
        // Cache hits and inexpensive line probes are not expensive Git walks.
        // A global count prematurely aborted large comparisons within seconds.
        // Retain real elapsed-time/memory limits and count visits diagnostically.
        let reason = strict_budget_reason(self.bytes, self.started.elapsed());
        if let Some(reason) = reason {
            perf_log(&format!("comparison_author_contributions: request={} budget_exceeded reason={reason} path={:?} visits={} file_visits={} bytes={} commits={} files_done={}/{} proven={} inconclusive={} no_witness={} probes_done={}/{} partial_results_applied=false", self.request_id, self.current_path, self.visits, self.visits - self.file_visits, self.bytes, self.exclusive.len(), self.outcomes.iter().sum::<usize>(), self.file_total, self.outcomes[0], self.outcomes[1], self.outcomes[2], self.probes_done, self.probes), self.started.elapsed());
            return Err(format!("Strict attribution exceeded its analysis budget ({reason}). No partial results were applied; choose closer revisions or disable the strict filter."));
        }
        Ok(())
    }

    fn begin_file(&mut self, path: &str) {
        // Evidence already stored in the result owns its data. Provenance caches
        // are only needed for the current file, not every previous delta.
        // Keep commit-pair rename maps shared: recomputing a whole-tree rename
        // diff separately for every added path would undo the performance gain.
        self.entries.clear(); self.texts.clear();
        self.proofs.clear(); self.deletions.clear(); self.unchanged_origins.clear();
        self.bytes = 0;
        self.current_path = path.into();
        self.file_started = Instant::now();
        self.file_visits = self.visits;
        self.probes = 0; self.probes_done = 0;
    }

    fn finish_file(&mut self, outcome: usize, witness: Option<Oid>) {
        self.outcomes[outcome] += 1;
        // Bound diagnostics: never log file contents, line text or credentials.
        if self.detail_logs < 20 || (self.file_started.elapsed() > Duration::from_millis(250) && self.detail_logs < 30) {
            self.detail_logs += 1;
            perf_log(&format!("comparison_author_contributions: request={} file={:?} outcome={} probes_done={}/{} visits={} bytes={} witness={}", self.request_id, self.current_path, ["proven", "inconclusive", "no_surviving_witness"][outcome], self.probes_done, self.probes, self.visits - self.file_visits, self.bytes, witness.map(|id| id.to_string()).unwrap_or_else(|| "none".into())), self.file_started.elapsed());
        }
    }

    fn unchanged_origin(&mut self, mut id: Oid, path: &str) -> Result<Oid, String> {
        let mut skipped = Vec::new();
        while self.exclusive.contains(&id) {
            let key = (id, path.to_string());
            if let Some(origin) = self.unchanged_origins.get(&key) { id = *origin; break; }
            self.budget()?;
            let info = self.commit(id)?;
            // Never jump across a merge, rename, mode change or content change.
            // Every supported probe has identical truth on identical entries.
            if info.parents.len() != 1 || self.entry(id, path)? != self.entry(info.parents[0], path)? { break; }
            skipped.push(key);
            id = info.parents[0];
        }
        // Cache the stopping point as well: every line in this file otherwise
        // repeats the same parent/entry equality check at the changed commit.
        if self.exclusive.contains(&id) { self.unchanged_origins.entry((id, path.to_string())).or_insert(id); }
        for key in skipped { self.unchanged_origins.insert(key, id); }
        Ok(id)
    }

    fn commit(&mut self, id: Oid) -> Result<CommitInfo, String> {
        if let Some(info) = self.commits.get(&id) { return Ok(info.clone()); }
        let commit = self.repo.find_commit(id).map_err(|e| e.to_string())?;
        let info = CommitInfo { tree: commit.tree_id(), parents: commit.parent_ids().collect(), author: identity(&commit), time: commit.time().seconds() };
        self.commits.insert(id, info.clone());
        Ok(info)
    }

    fn entry(&mut self, id: Oid, path: &str) -> Result<Option<Entry>, String> {
        let key = (id, path.to_string());
        if let Some(entry) = self.entries.get(&key) { return Ok(*entry); }
        self.budget()?;
        let info = self.commit(id)?;
        let tree = self.repo.find_tree(info.tree).map_err(|e| e.to_string())?;
        let entry = match tree.get_path(Path::new(path)) {
            Ok(entry) => Some(Entry { id: entry.id(), mode: entry.filemode() }),
            Err(e) if e.code() == git2::ErrorCode::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        self.entries.insert(key, entry);
        Ok(entry)
    }

    fn text(&mut self, entry: Option<Entry>) -> Result<Arc<Text>, String> {
        let Some(entry) = entry else { return Ok(Arc::new(Text { counts: HashMap::new(), supported: true })); };
        if entry.mode == 0o160000 || entry.mode == 0o40000 {
            return Ok(Arc::new(Text { counts: HashMap::new(), supported: false }));
        }
        if let Some(text) = self.texts.get(&entry.id) { return Ok(text.clone()); }
        let blob = self.repo.find_blob(entry.id).map_err(|e| e.to_string())?;
        let supported = blob.size() <= 2 * 1024 * 1024 && !blob.is_binary();
        let mut counts = HashMap::new();
        if supported {
            self.bytes += blob.size();
            self.budget()?;
            for line in blob.content().split_inclusive(|byte| *byte == b'\n') {
                *counts.entry(line.to_vec()).or_insert(0) += 1;
            }
        }
        let text = Arc::new(Text { counts, supported });
        self.texts.insert(entry.id, text.clone());
        Ok(text)
    }

    fn matches(&mut self, id: Oid, path: &str, probe: &Probe) -> Result<Option<bool>, String> {
        let entry = self.entry(id, path)?;
        Ok(match probe {
            Probe::Atomic(value) => Some(entry == *value),
            Probe::Exists(value) => Some(entry.is_some() == *value),
            Probe::Mode(value) => Some(entry.map(|e| e.mode) == *value),
            Probe::Line(line, present) => {
                let text = self.text(entry)?;
                let count = text.counts.get(line).copied().unwrap_or(0);
                if !text.supported || count > 1 { None } else { Some((count == 1) == *present) }
            }
        })
    }

    // Rename discovery is lazy (only when a present path is absent in a parent).
    // Exact blob renames can preserve provenance. Edited/similarity-only rename
    // associations are not sufficiently certain for strict content attribution.
    fn parent_path(&mut self, child: Oid, parent: Oid, path: &str, probe: &Probe) -> Result<Option<String>, String> {
        if !matches!(probe, Probe::Line(_, _)) || self.entry(parent, path)?.is_some() || self.entry(child, path)?.is_none() {
            return Ok(Some(path.into()));
        }
        let key = (child, parent);
        if !self.renames.contains_key(&key) {
            self.budget()?;
            let child_tree = self.commit(child)?.tree;
            let parent_tree = self.commit(parent)?.tree;
            let child_tree = self.repo.find_tree(child_tree).map_err(|e| e.to_string())?;
            let parent_tree = self.repo.find_tree(parent_tree).map_err(|e| e.to_string())?;
            let mut diff = self.repo.diff_tree_to_tree(Some(&parent_tree), Some(&child_tree), None).map_err(|e| e.to_string())?;
            let (removed, added) = rename_counts(&diff);
            let mut options = git2::DiffFindOptions::new();
            options.renames(true).rename_limit(200);
            diff.find_similar(Some(&mut options)).map_err(|e| e.to_string())?;
            let mut paths = HashMap::new();
            for delta in diff.deltas().filter(|d| d.status() == git2::Delta::Renamed) {
                if let (Some(old), Some(new)) = (delta.old_file().path(), delta.new_file().path()) {
                    let exact_unique = delta.old_file().id() == delta.new_file().id()
                        && removed.get(&delta.old_file().id()) == Some(&1) && added.get(&delta.new_file().id()) == Some(&1);
                    paths.insert(normalized(new), (normalized(old), exact_unique));
                }
            }
            self.renames.insert(key, paths);
        }
        Ok(match self.renames[&key].get(path) {
            Some((old, true)) => Some(old.clone()),
            Some((_, false)) => None,
            None => Some(path.into()),
        })
    }

    fn trace(&mut self, id: Oid, path: &str, probe: &Probe, depth: usize) -> Result<Proof, String> {
        let id = self.unchanged_origin(id, path)?;
        if !self.exclusive.contains(&id) { return Ok(Proof::Other); }
        if depth > 256 { return Ok(Proof::Unknown); }
        let key = (id, path.to_string(), probe.clone());
        if let Some(proof) = self.proofs.get(&key) { return Ok(proof.clone()); }
        self.bytes += path.len() + if let Probe::Line(line, _) = probe { line.len() } else { 64 };
        self.budget()?;
        if self.matches(id, path, probe)? != Some(true) { return Ok(Proof::Unknown); }
        let info = self.commit(id)?;
        let mut inherited = Vec::new();
        for parent in &info.parents {
            let Some(parent_path) = self.parent_path(id, *parent, path, probe)? else { inherited.push(Proof::Unknown); continue; };
            match self.matches(*parent, &parent_path, probe)? {
                Some(true) => inherited.push(self.trace(*parent, &parent_path, probe, depth + 1)?),
                None => inherited.push(Proof::Unknown),
                Some(false) => {},
            }
        }
        let proof = if !inherited.is_empty() {
            // Equal content in multiple parents has multiple possible origins.
            // Do not pick the author's parent simply because it is convenient.
            if inherited.iter().all(|p| matches!(p, Proof::Own(..))) { inherited.remove(0) }
            else if inherited.iter().all(|p| matches!(p, Proof::Other)) { Proof::Other }
            else { Proof::Unknown }
        } else if info.parents.len() > 1 {
            // Novel merge content/deletion may be a resolution or a different
            // historical merge driver. Never attribute it from authorship alone.
            Proof::Unknown
        } else if info.parents.is_empty() && matches!(probe, Probe::Line(_, false) | Probe::Atomic(None) | Probe::Exists(false) | Probe::Mode(None)) {
            Proof::Other
        } else if info.author == self.author {
            if let (Probe::Line(line, false), Some(parent)) = (probe, info.parents.first()) {
                // A->B->C must not credit B's author merely because A is still
                // absent. Replacement provenance is proved by surviving added
                // lines; absence alone proves only a standalone deletion hunk.
                if self.standalone_deletion(id, *parent, path, line)? {
                    Proof::Own(id, Some(*parent), path.into())
                } else { Proof::Unknown }
            } else { Proof::Own(id, info.parents.first().copied(), path.into()) }
        } else { Proof::Other };
        self.proofs.insert(key, proof.clone());
        Ok(proof)
    }

    fn standalone_deletion(&mut self, id: Oid, parent: Oid, path: &str, line: &[u8]) -> Result<bool, String> {
        let key = (id, path.to_string());
        if !self.deletions.contains_key(&key) {
            let old = self.entry(parent, path)?.map(|e| self.repo.find_blob(e.id)).transpose().map_err(|e| e.to_string())?;
            let new = self.entry(id, path)?.map(|e| self.repo.find_blob(e.id)).transpose().map_err(|e| e.to_string())?;
            let mut options = git2::DiffOptions::new(); options.context_lines(0).interhunk_lines(0);
            let patch = git2::Patch::from_buffers(old.as_ref().map(|b| b.content()).unwrap_or(&[]), None,
                new.as_ref().map(|b| b.content()).unwrap_or(&[]), None, Some(&mut options)).map_err(|e| e.to_string())?;
            let mut deleted = HashSet::new();
            for h in 0..patch.num_hunks() {
                let (_, count) = patch.hunk(h).map_err(|e| e.to_string())?;
                let mut removed = Vec::new(); let mut replacement = false;
                for i in 0..count {
                    let line = patch.line_in_hunk(h, i).map_err(|e| e.to_string())?;
                    if line.origin() == '+' { replacement = true; }
                    if line.origin() == '-' { removed.push(line.content().to_vec()); }
                }
                if !replacement { deleted.extend(removed); }
            }
            self.deletions.insert(key.clone(), deleted);
        }
        Ok(self.deletions[&key].contains(line))
    }
}

fn collect_strict(repo: &Repository, left: Oid, right: Oid, author: &str) -> Result<Contributions, String> {
    if repo.is_shallow() { return Err("Strict attribution requires complete local history. No results were applied.".into()); }
    let started = Instant::now();
    static NEXT_REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let request_id = NEXT_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    perf_log(&format!("comparison_author_contributions: request={request_id} start repo={} left={left} right={right} time_limit_ms=30000 per_file_bytes_limit=67108864 visit_limit=none", anonymized_repository_id(&repo.path().to_string_lossy())), Duration::ZERO);
    let mut exclusive = HashSet::new();
    for (tip, hide) in [(left, right), (right, left)] {
        let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
        walk.push(tip).map_err(|e| e.to_string())?;
        walk.hide(hide).map_err(|e| e.to_string())?;
        for id in walk {
            exclusive.insert(id.map_err(|e| e.to_string())?);
            if exclusive.len() > 100_000 || started.elapsed() > Duration::from_secs(30) {
                perf_log(&format!("comparison_author_contributions: request={request_id} history_budget_exceeded commits={}", exclusive.len()), started.elapsed());
                return Err("History exceeds the strict attribution budget. Choose closer revisions.".into());
            }
        }
    }
    let mut analysis = Analysis { repo, author, exclusive, commits: HashMap::new(), entries: HashMap::new(), texts: HashMap::new(), renames: HashMap::new(), proofs: HashMap::new(), deletions: HashMap::new(), unchanged_origins: HashMap::new(), started, visits: 0, bytes: 0, peak_bytes: 0, current_path: String::new(), request_id, file_total: 0, outcomes: [0; 3], file_started: started, file_visits: 0, probes: 0, probes_done: 0, detail_logs: 0 };
    let left_tree = repo.find_commit(left).and_then(|c| c.tree()).map_err(|e| e.to_string())?;
    let right_tree = repo.find_commit(right).and_then(|c| c.tree()).map_err(|e| e.to_string())?;
    let mut diff = repo.diff_tree_to_tree(Some(&left_tree), Some(&right_tree), None).map_err(|e| e.to_string())?;
    let (removed, added) = rename_counts(&diff);
    let mut options = git2::DiffFindOptions::new();
    options.renames(true).exact_match_only(true).rename_threshold(100).rename_limit(200);
    diff.find_similar(Some(&mut options)).map_err(|e| e.to_string())?;
    analysis.file_total = diff.deltas().len();
    perf_log(&format!("comparison_author_contributions: request={request_id} history_commits={} comparison_deltas={}", analysis.exclusive.len(), analysis.file_total), started.elapsed());
    let mut result = Contributions::default();
    for delta in diff.deltas() {
        let old = delta.old_file().path().map(normalized);
        let new = delta.new_file().path().map(normalized);
        let old_path = old.as_deref().or(new.as_deref()).unwrap_or("");
        let new_path = new.as_deref().or(old.as_deref()).unwrap_or("");
        analysis.begin_file(new_path);
        if delta.status() == git2::Delta::Renamed && (removed.get(&delta.old_file().id()) != Some(&1) || added.get(&delta.new_file().id()) != Some(&1)) {
            for path in [old, new].into_iter().flatten() {
                result.ambiguous_paths.insert(path, "Multiple identical files make rename pairing ambiguous. No historical file identity was guessed.".into());
            }
            analysis.finish_file(1, None);
            continue;
        }
        let a = analysis.entry(left, old_path)?;
        let b = analysis.entry(right, new_path)?;
        let mut probes = Vec::new();
        let mut ambiguous = false;
        if a.into_iter().chain(b).any(|e| e.mode == 0o160000) {
            probes.push((left, old_path.to_string(), Probe::Atomic(a)));
            probes.push((right, new_path.to_string(), Probe::Atomic(b)));
        } else {
            let at = analysis.text(a)?;
            let bt = analysis.text(b)?;
            if at.supported && bt.supported {
                let lines: std::collections::BTreeSet<_> = at.counts.keys().chain(bt.counts.keys()).cloned().collect();
                for line in lines {
                    let ac = at.counts.get(&line).copied().unwrap_or(0);
                    let bc = bt.counts.get(&line).copied().unwrap_or(0);
                    if ac == bc { continue; }
                    if ac > 1 || bc > 1 || line.len() > 8192 { ambiguous = true; continue; }
                    probes.push((left, old_path.to_string(), Probe::Line(line.clone(), ac == 1)));
                    probes.push((right, new_path.to_string(), Probe::Line(line, bc == 1)));
                }
                if at.counts.is_empty() && bt.counts.is_empty() && a.is_some() != b.is_some() {
                    probes.push((left, old_path.to_string(), Probe::Exists(a.is_some())));
                    probes.push((right, new_path.to_string(), Probe::Exists(b.is_some())));
                }
            } else {
                // Binary/oversized blobs have no safe partial-edit attribution.
                ambiguous = true;
            }
            if a.is_some() && b.is_some() && a.map(|e| e.mode) != b.map(|e| e.mode) {
                probes.push((left, old_path.to_string(), Probe::Mode(a.map(|e| e.mode))));
                probes.push((right, new_path.to_string(), Probe::Mode(b.map(|e| e.mode))));
            }
            if old_path != new_path {
                for path in [old_path, new_path] {
                    probes.push((left, path.to_string(), Probe::Exists(analysis.entry(left, path)?.is_some())));
                    probes.push((right, path.to_string(), Probe::Exists(analysis.entry(right, path)?.is_some())));
                }
            }
        }
        // Pure reordering/duplicate-line edits cannot be established by unique
        // line provenance. Report uncertainty instead of guessing line identity.
        if probes.is_empty() { ambiguous = true; }
        // A large endpoint diff can still contain one small, provable personal
        // contribution. Never discard every candidate because the file has
        // many imported lines. Trace until a witness is found; the existing
        // request time/memory budgets still fail closed if exhausted.
        let mut witness = None;
        analysis.probes = probes.len();
        for (tip, path, probe) in probes {
            analysis.probes_done += 1;
            match analysis.trace(tip, &path, &probe, 0)? {
                Proof::Own(commit, parent, path) => { witness = Some((commit, parent, path)); break; },
                Proof::Unknown => ambiguous = true,
                Proof::Other => {},
            }
        }
        analysis.finish_file(if witness.is_some() { 0 } else if ambiguous { 1 } else { 2 }, witness.as_ref().map(|(commit, _, _)| *commit));
        let paths: HashSet<_> = [old, new].into_iter().flatten().collect();
        for path in paths {
            if let Some((commit, parent, witness_path)) = &witness {
                result.paths.push(path.clone());
                // The witness's historic path may differ after an exact rename.
                result.evidence.insert(path, vec![ComparisonTouch { commit: commit.to_string(), date: short_date(analysis.commit(*commit)?.time), parent: parent.map(|p| p.to_string()), change: "Surviving contribution".into(), direct: true, witness_path: Some(witness_path.clone()) }]);
            } else if ambiguous {
                result.ambiguous_paths.insert(path, "Attribution is inconclusive (merge resolution, duplicate/moved lines, rename ambiguity, binary/large file, or history depth). Not included as a proven contribution.".into());
            }
        }
    }
    result.paths.sort(); result.paths.dedup();
    perf_log(&format!("comparison_author_contributions: request={request_id} complete left={left} right={right} commits={} paths={} ambiguous={} files_done={}/{} proven={} inconclusive={} no_witness={} visits={} peak_file_bytes={} detail_logs={}", analysis.exclusive.len(), result.paths.len(), result.ambiguous_paths.len(), analysis.outcomes.iter().sum::<usize>(), analysis.file_total, analysis.outcomes[0], analysis.outcomes[1], analysis.outcomes[2], analysis.visits, analysis.peak_bytes, analysis.detail_logs), started.elapsed());
    Ok(result)
}

#[tauri::command]
pub async fn comparison_author_contributions(repository_path: String, submodule_path: Option<String>, owner: Option<String>, repository_name: Option<String>, left_revision: String, right_revision: String, author_key: String, comparison_paths: Option<Vec<String>>) -> Result<Contributions, String> {
    off_main_thread(move || {
        if comparison_paths.is_some() {
            return Err("Strict attribution is available for two committed revisions. Local uncommitted differences have no Git author; compare HEAD with the other revision or disable the strict filter.".into());
        }
        validate_path(&repository_path)?;
        let path = match (owner, repository_name, submodule_path) {
            (Some(owner), Some(name), None) => { let (owner, name) = validate_github_module_identity(&owner, &name)?; github_module_cache_path(&owner, &name) },
            (None, None, Some(path)) => validate_submodule(&repository_path, &path)?,
            (None, None, None) => PathBuf::from(repository_path),
            _ => return Err("Invalid comparison repository context".into()),
        };
        let repo = internal_repository(&path.to_string_lossy())?;
        collect_strict(&repo, Oid::from_str(&left_revision).map_err(|e| e.to_string())?, Oid::from_str(&right_revision).map_err(|e| e.to_string())?, &author_key)
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    struct Fixture { repo: Repository, path: PathBuf }
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("ddt-strict-{}-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            Self { repo: Repository::init(&path).unwrap(), path }
        }
        fn commit(&self, author: &str, files: &[(&str, &str)], parents: &[Oid]) -> Oid {
            self.entries(author, &files.iter().map(|(p,c)| (*p,*c,0o100644)).collect::<Vec<_>>(), parents)
        }
        fn entries(&self, author: &str, files: &[(&str, &str, i32)], parents: &[Oid]) -> Oid {
            let mut builder = self.repo.treebuilder(None).unwrap();
            for (name, content, mode) in files {
                let id = if *mode == 0o160000 { Oid::from_str(content).unwrap() } else { self.repo.blob(content.as_bytes()).unwrap() };
                builder.insert(*name, id, *mode).unwrap();
            }
            let tree = self.repo.find_tree(builder.write().unwrap()).unwrap();
            let sig = git2::Signature::now(author, &format!("{author}@test")).unwrap();
            let parents: Vec<_> = parents.iter().map(|id| self.repo.find_commit(*id).unwrap()).collect();
            self.repo.commit(None, &sig, &sig, "fixture", &tree, &parents.iter().collect::<Vec<_>>()).unwrap()
        }
        fn check(&self, left: Oid, right: Oid, author: &str, paths: &[&str]) -> Contributions {
            let key = format!("email:{author}@test");
            let result = collect_strict(&self.repo,left,right,&key).unwrap();
            assert_eq!(result.paths,paths, "author={author}");
            for touch in result.evidence.values().flatten() {
                let witness = self.repo.find_commit(Oid::from_str(&touch.commit).unwrap()).unwrap();
                assert_eq!(touch.date, short_date(witness.time().seconds()));
            }
            let reverse = collect_strict(&self.repo,right,left,&key).unwrap();
            assert_eq!(reverse.paths,result.paths,"direction must not change inclusion");
            assert_eq!(reverse.ambiguous_paths.keys().collect::<Vec<_>>(),result.ambiguous_paths.keys().collect::<Vec<_>>());
            result
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.path); } }

    #[test]
    fn distant_commit_snapshots_skip_unchanged_file_history() {
        let f = Fixture::new();
        let base = f.commit("base", &[("large", "base\n")], &[]);
        let imported = (0..1500).map(|i| format!("imported unique line {i}\n")).collect::<String>();
        let bob = f.commit("bob", &[("large", &imported)], &[base]);
        let mut tip = f.commit("alice", &[("large", &imported), ("own", "mine\n")], &[bob]);
        for _ in 0..280 {
            tip = f.commit("bob", &[("large", &imported), ("own", "mine\n")], &[tip]);
        }
        f.check(base, tip, "alice", &["own"]);
        f.check(base, tip, "bob", &["large"]);
        // The UI resolves both branch names and selected commits to these same SHAs.
        f.repo.reference("refs/heads/start", base, true, "test").unwrap();
        f.repo.reference("refs/heads/end", tip, true, "test").unwrap();
        let named_left = f.repo.revparse_single("start").unwrap().peel_to_commit().unwrap().id();
        let named_right = f.repo.revparse_single("end").unwrap().peel_to_commit().unwrap().id();
        f.check(named_left, named_right, "alice", &["own"]);
    }

    #[test]
    fn many_files_do_not_accumulate_previous_file_provenance_memory() {
        let f = Fixture::new();
        let base = f.commit("base", &[], &[]);
        let padding = "padding\n".repeat(120_000);
        let files = (0..72).map(|i| (format!("file-{i:03}"), format!("own-{i}\n{padding}"))).collect::<Vec<_>>();
        assert!(files.iter().map(|(_, text)| text.len()).sum::<usize>() > 64 * 1024 * 1024);
        let refs = files.iter().map(|(name, text)| (name.as_str(), text.as_str())).collect::<Vec<_>>();
        let tip = f.commit("alice", &refs, &[base]);
        let paths = files.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>();
        f.check(base, tip, "alice", &paths);
    }

    #[test]
    fn older_commit_comparison_keeps_an_author_witness_after_many_imported_lines() {
        let f = Fixture::new();
        let base = f.commit("base", &[("f", "base\n")], &[]);
        let imported = (0..2100).map(|i| format!("imported-{i:04}\n")).collect::<String>();
        let bob = f.commit("bob", &[("f", &imported)], &[base]);
        let personal = format!("{imported}zz-personal\n");
        let tip = f.commit("alice", &[("f", &personal)], &[bob]);
        f.check(bob, tip, "alice", &["f"]);
        // Same surviving contribution, but a larger diff from an older commit.
        f.check(base, tip, "alice", &["f"]);
        f.repo.reference("refs/heads/start", base, true, "fixture").unwrap();
        f.repo.reference("refs/heads/end", tip, true, "fixture").unwrap();
        let path = f.path.to_string_lossy();
        let named = compare_git_tree_file_list(&path, "", "start", "end").unwrap();
        let commits = compare_git_tree_file_list(&path, "", &base.to_string(), &tip.to_string()).unwrap();
        assert_eq!(named.left_revision, commits.left_revision);
        assert_eq!(named.right_revision, commits.right_revision);
        assert_eq!(serde_json::to_value(&named.rows).unwrap(), serde_json::to_value(&commits.rows).unwrap());
        let filter = |result: &RevisionCompareDirectory| collect_strict(&f.repo,
            Oid::from_str(&result.left_revision).unwrap(), Oid::from_str(&result.right_revision).unwrap(), "email:alice@test").unwrap();
        assert_eq!(serde_json::to_value(filter(&named)).unwrap(), serde_json::to_value(filter(&commits)).unwrap());
        let imported_only = f.commit("alice", &[("f", &imported)], &[base, bob]);
        f.check(base, imported_only, "alice", &[]);
        let own_branch = f.commit("alice", &[("f", "base\nzz-personal\n")], &[base]);
        let merged = f.commit("integrator", &[("f", &personal)], &[own_branch, bob]);
        f.check(base, merged, "alice", &["f"]);
        f.check(base, merged, "integrator", &[]);
    }

    #[test]
    fn elapsed_time_and_memory_safety_limits_are_preserved() {
        assert_eq!(strict_budget_reason(795519, Duration::from_millis(2202)), None);
        assert_eq!(strict_budget_reason(64 * 1024 * 1024 + 1, Duration::ZERO), Some("per-file memory"));
        assert_eq!(strict_budget_reason(0, Duration::from_millis(30001)), Some("elapsed time"));
    }

    #[test]
    fn inexpensive_cached_probes_do_not_exhaust_a_global_visit_quota() {
        let f = Fixture::new();
        let names = (0..60).map(|i| format!("import-{i:03}")).collect::<Vec<_>>();
        let initial = names.iter().map(|name| (name.as_str(), "base\n")).collect::<Vec<_>>();
        let base = f.commit("base", &initial, &[]);
        let imported = (0..5000).map(|i| format!("imported-{i:05}\n")).collect::<String>();
        let mut files = names.iter().map(|name| (name.as_str(), imported.as_str())).collect::<Vec<_>>();
        let bob = f.commit("bob", &files, &[base]);
        files.push(("zz-own", "personal\n"));
        let tip = f.commit("alice", &files, &[bob]);
        // More than 250k cheap, cached line probes; no large blob or deep history.
        f.check(base, tip, "alice", &["zz-own"]);
    }

    #[test]
    fn direct_shared_file_keeps_full_diff_and_is_read_only() {
        let f = Fixture::new();
        let base = f.commit("base", &[("f","base\n")], &[]);
        let a = f.commit("alice", &[("f","base\nalice\n")], &[base]);
        let b = f.commit("bob", &[("f","base\nalice\nbob\n")], &[a]);
        f.repo.reference("refs/heads/main", b, true, "fixture").unwrap(); f.repo.set_head("refs/heads/main").unwrap();
        fs::write(f.path.join("f"), "staged").unwrap();
        let mut index = f.repo.index().unwrap(); index.add_path(Path::new("f")).unwrap(); index.write().unwrap();
        fs::write(f.path.join("f"), "unstaged").unwrap(); fs::write(f.path.join("untracked"),"untouched").unwrap();
        let index_before = fs::read(f.repo.path().join("index")).unwrap();
        f.check(base,b,"alice",&["f"]); f.check(base,b,"bob",&["f"]);
        assert_eq!(fs::read(f.repo.path().join("index")).unwrap(),index_before);
        assert_eq!(fs::read_to_string(f.path.join("f")).unwrap(),"unstaged");
        assert_eq!(fs::read_to_string(f.path.join("untracked")).unwrap(),"untouched");
        assert_eq!(f.repo.head().unwrap().target(),Some(b));
        let diff = compare_git_tree_file(&f.path.to_string_lossy(),"f",&base.to_string(),&b.to_string()).unwrap();
        assert_eq!(diff.remote_content,"base\nalice\nbob\n");
    }

    #[test]
    fn overwritten_and_reverted_contributions_do_not_qualify() {
        let f = Fixture::new();
        let base = f.commit("base", &[("f","base\n")], &[]);
        let a = f.commit("alice", &[("f","alice replacement\n")], &[base]);
        let b = f.commit("bob", &[("f","bob replacement\n")], &[a]);
        f.check(base,b,"alice",&[]); f.check(base,b,"bob",&["f"]);
        let reverted = f.commit("bob", &[("f","base\nbob extra\n")], &[b]);
        f.check(base,reverted,"alice",&[]);
    }

    #[test]
    fn standalone_deletions_and_empty_files_are_attributed() {
        let f = Fixture::new();
        let base = f.commit("base", &[("f","keep\ndelete\n"),("gone","old\n")], &[]);
        let a = f.commit("alice", &[("f","keep\n"),("empty","")], &[base]);
        f.check(base,a,"alice",&["empty","f","gone"]);
        let b = f.commit("bob", &[("f","keep\ndelete\n"),("empty","")], &[a]);
        f.check(base,b,"alice",&["empty","gone"]);
    }

    #[test]
    fn repeated_merges_preserve_original_authors_not_integrator() {
        let f = Fixture::new();
        let base = f.commit("base", &[("f","base\n")], &[]);
        let a = f.commit("alice", &[("f","base\nalice\n")], &[base]);
        let b = f.commit("bob", &[("f","base\nbob\n"),("import","bob\n")], &[base]);
        let m = f.commit("integrator", &[("f","base\nalice\nbob\n"),("import","bob\n")], &[a,b]);
        let c = f.commit("carol", &[("f","base\nbob\ncarol\n"),("import","bob\n")], &[b]);
        let tip = f.commit("integrator", &[("f","base\nalice\nbob\ncarol\n"),("import","bob\n")], &[m,c]);
        f.check(base,tip,"alice",&["f"]); f.check(base,tip,"bob",&["f","import"]);
        f.check(base,tip,"integrator",&[]); f.check(c,tip,"alice",&["f"]);
        f.check(c,tip,"bob",&[]);
    }

    #[test]
    fn feature_to_feature_and_same_author_side_branch() {
        let f = Fixture::new();
        let base = f.commit("base", &[("f","base\n")], &[]);
        let left = f.commit("alice", &[("f","base\nleft\n")], &[base]);
        let side = f.commit("alice", &[("f","base\nside\n"),("side","side\n")], &[base]);
        let tip = f.commit("bob", &[("f","base\nside\n"),("side","side\n")], &[base,side]);
        f.check(left,tip,"alice",&["f","side"]); f.check(base,tip,"bob",&[]);
    }

    #[test]
    fn novel_conflict_resolution_is_explicitly_inconclusive() {
        let f = Fixture::new();
        let base = f.commit("base", &[("f","base\n")], &[]);
        let a = f.commit("alice", &[("f","ours\n")], &[base]);
        let b = f.commit("bob", &[("f","theirs\n")], &[base]);
        let m = f.commit("resolver", &[("f","novel resolution\n")], &[a,b]);
        assert!(f.check(base,m,"resolver",&[]).ambiguous_paths.contains_key("f"));
        f.check(base,m,"alice",&[]); f.check(base,m,"bob",&[]);
    }

    #[test]
    fn same_content_multiple_parent_origins_are_not_guessed() {
        let f = Fixture::new();
        let base = f.commit("base", &[], &[]);
        let a = f.commit("alice", &[("f","same\n")], &[base]);
        let b = f.commit("bob", &[("f","same\n")], &[base]);
        let m = f.commit("resolver", &[("f","same\n")], &[a,b]);
        assert!(f.check(base,m,"alice",&[]).ambiguous_paths.contains_key("f"));
    }

    #[test]
    fn exact_rename_preserves_content_provenance_and_path_change() {
        let f = Fixture::new();
        let base = f.commit("base", &[("old","base\n")], &[]);
        let a = f.commit("alice", &[("old","base\nalice\n")], &[base]);
        let renamed = f.commit("bob", &[("new","base\nalice\n")], &[a]);
        // Endpoint edited renames are still represented as delete/add rows.
        let result = collect_strict(&f.repo,base,renamed,"email:alice@test").unwrap();
        assert!(result.paths.contains(&"new".to_string()));
        f.check(a,renamed,"bob",&["new","old"]);
    }

    #[test]
    fn gitlinks_are_atomic_and_do_not_require_submodule_objects() {
        let f = Fixture::new();
        let one = "1111111111111111111111111111111111111111";
        let two = "2222222222222222222222222222222222222222";
        let three = "3333333333333333333333333333333333333333";
        let base = f.entries("base", &[("module",one,0o160000)], &[]);
        let a = f.entries("alice", &[("module",two,0o160000)], &[base]);
        let imported = f.entries("integrator", &[("module",two,0o160000)], &[base,a]);
        f.check(base,imported,"alice",&["module"]); f.check(base,imported,"integrator",&[]);
        let overwritten = f.entries("bob", &[("module",three,0o160000)], &[imported]);
        f.check(base,overwritten,"alice",&[]); f.check(base,overwritten,"bob",&["module"]);
        let reverted = f.entries("bob", &[("module",one,0o160000)], &[overwritten]);
        f.check(base,reverted,"alice",&[]);
        let deleted = f.entries("alice", &[], &[base]);
        f.check(base,deleted,"alice",&["module"]);
    }

    #[test]
    fn duplicate_lines_binary_files_and_reordering_are_explicit() {
        let f = Fixture::new();
        let base = f.commit("base", &[("duplicate","x\nx\n"),("order","a\nb\n"),("binary","a\0b")], &[]);
        let a = f.commit("alice", &[("duplicate","x\n"),("order","b\na\n"),("binary","a\0c")], &[base]);
        let result = f.check(base,a,"alice",&[]);
        assert_eq!(result.ambiguous_paths.len(),3);
    }

    #[test]
    fn mode_only_changes_and_added_gitlinks_have_atomic_provenance() {
        let f = Fixture::new();
        let base = f.entries("base", &[("script","unchanged\n",0o100644)], &[]);
        let tip = f.entries("alice", &[("script","unchanged\n",0o100755),("module","1111111111111111111111111111111111111111",0o160000)], &[base]);
        f.check(base,tip,"alice",&["module","script"]);
        f.check(tip,tip,"alice",&[]);
    }

    #[test]
    fn incomplete_history_is_an_error_not_an_unfiltered_result() {
        let f = Fixture::new();
        let base = f.commit("base", &[], &[]);
        let tip = f.commit("alice", &[("f","own\n")], &[base]);
        assert!(collect_strict(&f.repo,base,Oid::zero(),"email:alice@test").is_err());
        fs::write(f.repo.path().join("shallow"),format!("{tip}\n")).unwrap();
        let reopened = Repository::open(&f.path).unwrap();
        assert!(collect_strict(&reopened,base,tip,"email:alice@test").unwrap_err().contains("complete local history"));
    }

    #[test]
    fn duplicate_blob_rename_pairing_is_never_used_as_certain_provenance() {
        let f = Fixture::new();
        let base = f.commit("base", &[("a","same\n"),("b","same\n")], &[]);
        let tip = f.commit("alice", &[("c","same\n"),("d","same\n")], &[base]);
        let result = f.check(base,tip,"alice",&[]);
        assert_eq!(result.ambiguous_paths.len(),4);
    }
}
