//! Git CLI transport and snapshots shared by preview, merge, review and commit.
//! No alternate merge engine or automatic retry after a mutation.
use super::super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub path: String,
    pub id: Oid,
    pub mode: u32,
    pub stage: u8,
}
#[derive(Clone, Default)]
pub(super) struct Index {
    pub entries: BTreeMap<(String, u8), Entry>,
}
impl Index {
    pub fn get(&self, path: &str, stage: u8) -> Option<&Entry> {
        self.entries.get(&(path.into(), stage))
    }
    pub fn conflicted(&self) -> bool {
        self.entries.values().any(|e| e.stage != 0)
    }
    pub fn links(&self) -> HashMap<String, Oid> {
        self.entries
            .values()
            .filter(|e| e.stage == 0 && e.mode == 0o160000)
            .map(|e| (e.path.clone(), e.id))
            .collect()
    }
    pub fn conflict_paths(&self) -> std::collections::BTreeSet<String> {
        self.entries
            .values()
            .filter(|e| e.stage != 0)
            .map(|e| e.path.clone())
            .collect()
    }
}
pub(super) fn output(root: &str, args: &[&str]) -> Result<std::process::Output, String> {
    // Match preview and execution. No saved conflict resolutions or automatic
    // stash are applied behind the review UI; no configuration is persisted.
    let mut full = vec![
        "--no-optional-locks",
        "--literal-pathspecs",
        "-c",
        "rerere.enabled=false",
        "-c",
        "merge.autoStash=false",
        "-c",
        "submodule.recurse=false",
    ];
    full.extend_from_slice(args);
    // Negative ancestry/missing optional config and conflict-bearing previews
    // are valid query results, not subprocess failures in command history.
    let accepted: &[i32] = if matches!(
        args.first(),
        Some(&"merge-base" | &"merge-tree" | &"symbolic-ref" | &"config")
    ) {
        &[0, 1]
    } else {
        &[0]
    };
    git_output_with_timeout_control(
        root,
        &full,
        GIT_COMMAND_TIMEOUT,
        "10 minutes",
        false,
        None,
        accepted,
    )
}
pub(super) fn failure(out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    let text = String::from_utf8_lossy(&out.stdout);
    format!(
        "Git exited with {:?}: {}{}{}",
        out.status.code(),
        err.trim(),
        if !err.is_empty() && !text.is_empty() {
            "\n"
        } else {
            ""
        },
        text.trim()
    )
}
pub(super) fn bytes(root: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let o = output(root, args)?;
    if o.status.success() {
        Ok(o.stdout)
    } else {
        Err(failure(&o))
    }
}
pub(super) fn text(root: &str, args: &[&str]) -> Result<String, String> {
    String::from_utf8(bytes(root, args)?)
        .map(|s| s.trim_end_matches(['\r', '\n']).into())
        .map_err(|_| "Git returned non-UTF-8 metadata; operation stopped.".into())
}
pub(super) fn oid(root: &str, revision: &str) -> Result<Oid, String> {
    let spec = format!("{revision}^{{commit}}");
    let raw = text(root, &["rev-parse", "--verify", "--end-of-options", &spec])?;
    Oid::from_str(&raw).map_err(|e| e.to_string())
}
pub(super) fn branch(root: &str) -> Result<Option<String>, String> {
    let o = output(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    match o.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8(o.stdout)
                .map_err(|_| "Invalid branch name")?
                .trim()
                .into(),
        )),
        Some(1) => Ok(None),
        _ => Err(failure(&o)),
    }
}
pub(super) fn git_path(root: &str, name: &str) -> Result<PathBuf, String> {
    let s = text(root, &["rev-parse", "--git-path", name])?;
    let p = PathBuf::from(s);
    Ok(if p.is_absolute() {
        p
    } else {
        Path::new(root).join(p)
    })
}
pub(in crate::repository) fn in_progress(root: &str) -> Result<bool, String> {
    Ok(git_path(root, "MERGE_HEAD")?.is_file())
}
pub(super) fn heads(root: &str) -> Result<Vec<Oid>, String> {
    let p = git_path(root, "MERGE_HEAD")?;
    let raw = fs::read_to_string(p).map_err(|e| format!("Cannot read MERGE_HEAD: {e}"))?;
    raw.lines()
        .map(|s| Oid::from_str(s.trim()).map_err(|e| e.to_string()))
        .collect()
}
pub(super) fn bases(root: &str, a: Oid, b: Oid) -> Result<Vec<Oid>, String> {
    let o = output(
        root,
        &["merge-base", "--all", &a.to_string(), &b.to_string()],
    )?;
    if o.status.code() == Some(1) {
        return Ok(vec![]);
    }
    if !o.status.success() {
        return Err(failure(&o));
    }
    String::from_utf8(o.stdout)
        .map_err(|_| "Invalid merge bases")?
        .lines()
        .map(|s| Oid::from_str(s).map_err(|e| e.to_string()))
        .collect()
}
pub(super) fn ancestor(root: &str, a: Oid, b: Oid) -> Result<bool, String> {
    let o = output(
        root,
        &[
            "merge-base",
            "--is-ancestor",
            &a.to_string(),
            &b.to_string(),
        ],
    )?;
    match o.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(failure(&o)),
    }
}
fn parse_record(raw: &[u8], tree: bool) -> Result<Entry, String> {
    let (header, path) = raw.split_at(
        raw.iter()
            .position(|b| *b == b'\t')
            .ok_or("Malformed Git entry")?,
    );
    let h = std::str::from_utf8(header)
        .map_err(|_| "Invalid entry header")?
        .split(' ')
        .collect::<Vec<_>>();
    if h.len() != 3 {
        return Err("Malformed Git entry fields".into());
    }
    let mode = u32::from_str_radix(h[0], 8).map_err(|_| "Invalid entry mode")?;
    let id = Oid::from_str(h[if tree { 2 } else { 1 }]).map_err(|e| e.to_string())?;
    let stage = if tree {
        0
    } else {
        h[2].parse::<u8>().map_err(|_| "Invalid index stage")?
    };
    if stage > 3 {
        return Err("Invalid index stage".into());
    }
    let path = String::from_utf8(path[1..].to_vec())
        .map_err(|_| "Non-UTF-8 paths are not supported in merge review")?;
    safe_relative_path(&path)?;
    Ok(Entry {
        path,
        id,
        mode,
        stage,
    })
}
fn parse_entries(raw: &[u8], tree: bool) -> Result<Index, String> {
    let mut index = Index::default();
    for record in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let e = parse_record(record, tree)?;
        if index.entries.insert((e.path.clone(), e.stage), e).is_some() {
            return Err("Duplicate Git entry".into());
        }
        if index.entries.len() > 250_000 {
            return Err("Merge exceeds its entry limit".into());
        }
    }
    Ok(index)
}
pub(super) fn index(root: &str) -> Result<Index, String> {
    parse_entries(&bytes(root, &["ls-files", "--stage", "-z"])?, false)
}
pub(super) fn tree(root: &str, revision: &str) -> Result<Index, String> {
    parse_entries(&bytes(root, &["ls-tree", "-r", "-z", revision])?, true)
}
pub(super) fn blob(root: &str, id: Oid) -> Result<Vec<u8>, String> {
    bytes(root, &["cat-file", "blob", &id.to_string()])
}

pub(super) struct Plan {
    pub current: Oid,
    pub incoming: Oid,
    pub branch: Option<String>,
    pub bases: Vec<Oid>,
    pub status: &'static str,
    pub result: Index,
    pub messages: Vec<(Vec<String>, String, String)>,
}
pub(super) fn plan(root: &str, source: &str, expected: Option<&str>) -> Result<Plan, String> {
    if in_progress(root)? {
        return Err("An operation is already in progress. Finish or abort it before previewing another merge.".into());
    }
    for name in [
        "rebase-merge",
        "rebase-apply",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        if git_path(root, name)?.exists() {
            return Err("Another Git operation is already in progress.".into());
        }
    }
    if text(root, &["rev-parse", "--is-shallow-repository"])? == "true" {
        return Err("Merge preview requires complete local ancestry. Fetch the missing history explicitly first.".into());
    }
    let current = oid(root, "HEAD")?;
    let incoming = oid(root, source)?;
    let branch = branch(root)?;
    if expected.is_some_and(|e| e != incoming.to_string()) {
        return Err("The selected branch moved since the graph was loaded. Refresh Branch Map and preview again.".into());
    }
    let bases = bases(root, current, incoming)?;
    if bases.is_empty() {
        return Err(
            "No usable common ancestry is available locally; no preview was produced.".into(),
        );
    }
    let (status, result, tree, messages) = if ancestor(root, incoming, current)? {
        let tid = Oid::from_str(&text(root, &["rev-parse", "HEAD^{tree}"])?)
            .map_err(|e| e.to_string())?;
        ("up_to_date", tree(root, &tid.to_string())?, tid, vec![])
    } else if ancestor(root, current, incoming)? {
        let tid = Oid::from_str(&text(
            root,
            &["rev-parse", &format!("{incoming}^{{tree}}")],
        )?)
        .map_err(|e| e.to_string())?;
        ("fast_forward", tree(root, &tid.to_string())?, tid, vec![])
    } else {
        let o = output(
            root,
            &[
                "merge-tree",
                "--write-tree",
                "-z",
                "--messages",
                &current.to_string(),
                &incoming.to_string(),
            ],
        )?;
        if !matches!(o.status.code(), Some(0 | 1)) {
            return Err(format!("Git merge preview is unavailable. A Git version supporting merge-tree --write-tree is required. No merge was started. {}",failure(&o)));
        }
        let fields = o.stdout.split(|b| *b == 0).collect::<Vec<_>>();
        let tid = Oid::from_str(
            std::str::from_utf8(fields.first().ok_or("Missing preview tree")?)
                .map_err(|_| "Invalid tree id")?,
        )
        .map_err(|e| e.to_string())?;
        let mut result = tree(root, &tid.to_string())?;
        let mut at = 1;
        while at < fields.len() && !fields[at].is_empty() {
            let e = parse_record(fields[at], false)?;
            result.entries.remove(&(e.path.clone(), 0));
            result.entries.insert((e.path.clone(), e.stage), e);
            at += 1;
        }
        at += 1;
        let mut messages = Vec::new();
        while at < fields.len() && !fields[at].is_empty() {
            let count = std::str::from_utf8(fields[at])
                .map_err(|_| "Invalid message count")?
                .parse::<usize>()
                .map_err(|_| "Invalid message count")?;
            at += 1;
            if count > fields.len().saturating_sub(at + 2) {
                return Err("Incomplete merge message".into());
            }
            let paths = fields[at..at + count]
                .iter()
                .map(|p| {
                    String::from_utf8(p.to_vec()).map_err(|_| "Invalid conflict path".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            at += count;
            messages.push((
                paths,
                String::from_utf8_lossy(fields[at]).into(),
                String::from_utf8_lossy(fields[at + 1]).into(),
            ));
            at += 2;
        }
        (
            if o.status.success() {
                "clean"
            } else {
                "conflicts"
            },
            result,
            tid,
            messages,
        )
    };
    if oid(root, "HEAD")? != current
        || self::branch(root)? != branch
        || oid(root, source)? != incoming
    {
        return Err("A branch changed during merge preview. Refresh and preview again.".into());
    }
    perf_log(&format!("merge_native: plan engine=git_cli current={current} incoming={incoming} tree={tree} status={status}"),Duration::ZERO);
    Ok(Plan {
        current,
        incoming,
        branch,
        bases,
        status,
        result,
        messages,
    })
}
pub(super) fn unregistered(root: &str, index: &Index) -> Result<Vec<String>, String> {
    if (1..=3).any(|s| index.get(".gitmodules", s).is_some()) {
        return Err(
            "Resolve the .gitmodules conflict before reviewing or updating submodules.".into(),
        );
    }
    let mut registered = HashMap::new();
    if let Some(e) = index.get(".gitmodules", 0) {
        if !matches!(e.mode, 0o100644 | 0o100755) {
            return Err("The staged .gitmodules must be a regular configuration file.".into());
        }
        let o = output(
            root,
            &[
                "config",
                "--no-includes",
                "--null",
                "--blob",
                &e.id.to_string(),
                "--get-regexp",
                "^submodule\\..*\\.path$",
            ],
        )?;
        if !(o.status.success() || (o.status.code() == Some(1) && o.stderr.is_empty())) {
            return Err(format!("Cannot validate .gitmodules: {}", failure(&o)));
        }
        let raw = String::from_utf8(o.stdout).map_err(|_| "Invalid .gitmodules paths")?;
        for record in raw.split('\0').filter(|r| !r.is_empty()) {
            let (key, path) = record
                .split_once('\n')
                .ok_or("Malformed .gitmodules paths")?;
            if registered
                .insert(key.to_owned(), path.to_owned())
                .is_some_and(|p| p != path)
            {
                return Err(
                    "The staged .gitmodules has conflicting duplicate submodule paths.".into(),
                );
            }
        }
    }
    let paths: HashSet<_> = registered.into_values().collect();
    let mut missing: Vec<_> = index
        .links()
        .into_keys()
        .filter(|p| !paths.contains(p))
        .collect();
    missing.sort();
    Ok(missing)
}
pub(super) fn set_link(root: &str, path: &str, id: Option<Oid>) -> Result<(), String> {
    safe_relative_path(path)?;
    if let Some(id) = id {
        text(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{id},{path}"),
            ],
        )?;
    } else {
        text(root, &["update-index", "--force-remove", "--", path])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_merge_index_parser_preserves_literal_paths_modes_and_conflict_stages() {
        let oid = "1111111111111111111111111111111111111111";
        let data = format!("100755 {oid} 0\tspace and ü/$(literal),name\0").into_bytes();
        let index = parse_entries(&data, false).unwrap();
        let entry = index.get("space and ü/$(literal),name", 0).unwrap();
        assert_eq!(entry.mode, 0o100755);
        let data = format!("160000 {oid} 1\tdep\0160000 {oid} 2\tdep\0");
        let index = parse_entries(data.as_bytes(), false).unwrap();
        assert!(index.conflicted());
        assert_eq!(index.conflict_paths().len(), 1);
        assert!(index.get("dep", 0).is_none());
    }
    #[test]
    fn native_merge_index_parser_rejects_traversal_duplicates_and_invalid_stages() {
        let oid = "1111111111111111111111111111111111111111";
        for data in [
            format!("100644 {oid} 0\t../outside\0"),
            format!("100644 {oid} 4\tfile\0"),
            format!("100644 {oid} 0\tfile\0100644 {oid} 0\tfile\0"),
        ] {
            assert!(parse_entries(data.as_bytes(), false).is_err());
        }
    }
}
