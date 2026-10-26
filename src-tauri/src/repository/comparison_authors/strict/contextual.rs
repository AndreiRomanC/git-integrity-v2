//! Bounded fallback for large/repetitive text. Context identifies an occurrence;
//! a real added/deleted line in the witness commit must also match. Merely
//! changing a neighbouring line must never establish ownership of the target.
use super::*;

const MAX_BLOB: usize = 48 * 1024 * 1024;
const MAX_WINDOW: usize = 64 * 1024;
const MAX_WINDOWS: usize = 512;
const CONTEXT: &str = "The changed text has no unique, surviving contextual witness.";

struct Window {
    after: String,
    target_line: usize,
    // Standalone deletion: identify the removed occurrence in the parent.
    before: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_context_is_not_unique() {
        assert!(context_line("x\nx\nx\n", "x\nx\n").is_err());
        assert_eq!(unique_line("<A>\nx\nx\n</A>\n", "<A>\nx\nx\n"), Some(1));
    }

    #[test]
    fn context_requires_whole_line_boundaries_including_eof() {
        assert_eq!(unique_line("prefix-value\n", "value\n"), None);
        assert_eq!(unique_line("value-extra", "value"), None);
        assert_eq!(unique_line("other\nvalue", "value"), Some(2));
        assert_eq!(unique_line("\u{00e9}\nvalue", "value"), Some(2));
    }
}

fn add_window(windows: &mut Vec<(Oid, String, Window)>, tip: Oid, path: &str, window: Window, truncated: &mut bool) {
    if windows.len() < MAX_WINDOWS && window.after.len() <= MAX_WINDOW && window.before.as_ref().is_none_or(|b| b.len() <= MAX_WINDOW) {
        windows.push((tip, path.to_string(), window));
    } else { *truncated = true; }
}

fn unique_line(text: &str, window: &str) -> Option<usize> {
    context_line(text, window).ok().flatten()
}

fn context_line(text: &str, window: &str) -> Result<Option<usize>, &'static str> {
    if window.is_empty() { return Ok(None); }
    let mut first = None; let mut cursor = 0;
    // Include overlapping windows (e.g. two equal blocks in three repeated
    // lines), otherwise an ambiguous historical occurrence could look unique.
    while let Some(found) = text[cursor..].find(window) {
        let offset = cursor + found;
        if (offset == 0 || text.as_bytes()[offset - 1] == b'\n') && (window.ends_with('\n') || offset + window.len() == text.len()) {
            if first.is_some() { return Err("The same context occurs more than once in historical content."); }
            first = Some(offset);
        }
        cursor = offset + window.chars().next().unwrap().len_utf8();
    }
    Ok(first.map(|offset| text.as_bytes()[..offset].iter().filter(|b| **b == b'\n').count() + 1))
}

impl Analysis<'_> {
    fn contextual_blob(&mut self, id: Oid, path: &str) -> Result<Result<Option<git2::Blob<'_>>, &'static str>, String> {
        let entry = self.entry(id, path)?;
        let Some(entry) = entry else { return Ok(Ok(None)); };
        if entry.mode == 0o160000 || entry.mode == 0o40000 { return Ok(Err("A folder/submodule conversion is not a text edit.")); }
        let blob = self.repo.find_blob(entry.id).map_err(|e| e.to_string())?;
        if blob.size() > MAX_BLOB { return Ok(Err("Text exceeds the 48 MiB per-version contextual analysis limit.")); }
        if blob.is_binary() || blob.content().contains(&0) { return Ok(Err("Binary or NUL-encoded content cannot be attributed as text.")); }
        if std::str::from_utf8(blob.content()).is_err() { return Ok(Err("Contextual analysis requires UTF-8 text; this encoding is unsupported.")); }
        Ok(Ok(Some(blob)))
    }

    fn contextual_match(&mut self, id: Oid, path: &str, window: &str) -> Result<Result<Option<usize>, &'static str>, String> {
        self.budget()?;
        Ok(match self.contextual_blob(id, path)? {
            Ok(blob) => {
                let text = std::str::from_utf8(blob.as_ref().map(|b| b.content()).unwrap_or(&[])).unwrap();
                context_line(text, window)
            },
            Err(reason) => Err(reason),
        })
    }

    fn trace_context(&mut self, id: Oid, path: &str, window: &Window, depth: usize,
        memo: &mut HashMap<(Oid, String), Proof>) -> Result<Proof, String> {
        self.budget()?;
        let id = self.unchanged_origin(id, path)?;
        if !self.exclusive.contains_key(&id) { return Ok(Proof::Other); }
        if depth > 128 { return Ok(Proof::Unknown("More than 128 contextual history steps are needed.")); }
        let key = (id, path.to_string());
        if let Some(proof) = memo.get(&key) { return Ok(proof.clone()); }
        let start = match self.contextual_match(id, path, &window.after)? {
            Ok(Some(start)) => start,
            Ok(None) => return Ok(Proof::Unknown(CONTEXT)),
            Err(reason) => return Ok(Proof::Unknown(reason)),
        };
        let info = self.commit(id)?;
        let mut inherited = Vec::new();
        for parent in &info.parents {
            let Some(parent_path) = self.parent_path(id, *parent, path, &Probe::Line(Vec::new(), true))? else {
                inherited.push(Proof::Unknown("An edited rename has no unambiguous historical identity.")); continue;
            };
            match self.contextual_match(*parent, &parent_path, &window.after)? {
                Ok(Some(_)) => inherited.push(self.trace_context(*parent, &parent_path, window, depth + 1, memo)?),
                Ok(None) => {},
                Err(reason) => inherited.push(Proof::Unknown(reason)),
            }
        }
        let proof = if !inherited.is_empty() {
            if inherited.iter().all(|p| matches!(p, Proof::Own(..))) || inherited.iter().all(|p| matches!(p, Proof::Structure(..))) { inherited.remove(0) }
            else if inherited.iter().all(|p| matches!(p, Proof::Other)) { Proof::Other }
            else { inherited.into_iter().find(|p| matches!(p, Proof::Unknown(_))).unwrap_or(Proof::Unknown("Matching merge parents have different possible authors.")) }
        } else if info.parents.len() > 1 {
            Proof::Unknown("The change first appears in a merge resolution; its author cannot be established.")
        } else if info.author != self.author {
            Proof::Other
        } else if let Some(root) = info.parents.first().map(|p| self.conversion_root(id, *p, path)).transpose()?.flatten() {
            Proof::Structure(id, info.parents.first().copied(), root)
        } else {
            let parent = info.parents.first().copied();
            let parent_path = match parent {
                Some(p) => self.parent_path(id, p, path, &Probe::Line(Vec::new(), true))?,
                None => Some(path.to_string()),
            };
            if let Some(parent_path) = parent_path {
                self.verify_context_change(id, parent, &parent_path, path, start, window)?
            } else { Proof::Unknown("An edited rename has no unambiguous historical identity.") }
        };
        memo.insert(key, proof.clone());
        Ok(proof)
    }

    fn verify_context_change(&mut self, id: Oid, parent: Option<Oid>, parent_path: &str, path: &str,
        start: usize, window: &Window) -> Result<Proof, String> {
        // Load each pair once, without retaining large historical blobs in caches.
        let old_entry = parent.map(|p| self.entry(p, parent_path)).transpose()?.flatten();
        let new_entry = self.entry(id, path)?;
        let old = old_entry.map(|e| self.repo.find_blob(e.id)).transpose().map_err(|e| e.to_string())?;
        let new = new_entry.map(|e| self.repo.find_blob(e.id)).transpose().map_err(|e| e.to_string())?;
        let old_text = old.as_ref().map(|b| b.content()).unwrap_or(&[]);
        let new_text = new.as_ref().map(|b| b.content()).unwrap_or(&[]);
        if old_text.len() > MAX_BLOB || new_text.len() > MAX_BLOB { return Ok(Proof::Unknown("Text exceeds the 48 MiB per-version contextual analysis limit.")); }
        let target = if let Some(before) = &window.before {
            let Some(line) = std::str::from_utf8(old_text).ok().and_then(|text| unique_line(text, before)) else { return Ok(Proof::Unknown(CONTEXT)); };
            line + window.target_line
        } else { start + window.target_line };
        let mut options = git2::DiffOptions::new(); options.context_lines(0).max_size(MAX_BLOB as i64);
        let patch = git2::Patch::from_buffers(old_text, None, new_text, None, Some(&mut options)).map_err(|e| e.to_string())?;
        for h in 0..patch.num_hunks() {
            let (_, count) = patch.hunk(h).map_err(|e| e.to_string())?;
            let mut found = false; let mut additions = false;
            for n in 0..count {
                let line = patch.line_in_hunk(h, n).map_err(|e| e.to_string())?;
                additions |= line.origin() == '+';
                found |= if window.before.is_some() { line.origin() == '-' && line.old_lineno() == Some(target as u32) }
                    else { line.origin() == '+' && line.new_lineno() == Some(target as u32) };
            }
            if found && (window.before.is_none() || !additions) { return Ok(Proof::Own(id, parent, path.into())); }
        }
        Ok(Proof::Unknown("Only neighbouring context could be traced, not the changed line itself."))
    }

    pub(super) fn contextual(&mut self, left: Oid, right: Oid, old_path: &str, new_path: &str) -> Result<Proof, String> {
        // Small-file maps are no longer needed for this fallback.
        self.texts.clear(); self.proofs.clear(); self.deletions.clear(); self.bytes = 0;
        let a = self.entry(left, old_path)?;
        let b = self.entry(right, new_path)?;
        if a.into_iter().chain(b).any(|e| e.mode == 0o40000 || e.mode == 0o160000) { return Ok(Proof::Unknown("A folder/submodule conversion is not a text edit.")); }
        let old = match self.contextual_blob(left, old_path)? { Ok(blob) => blob, Err(reason) => return Ok(Proof::Unknown(reason)) };
        // Owned endpoint text allows further mutable history reads; max 96 MiB
        // for both inputs, independent of history depth. No per-line allocations.
        let old_text = old.as_ref().map(|b| std::str::from_utf8(b.content()).unwrap()).unwrap_or("").to_string();
        drop(old);
        let new = match self.contextual_blob(right, new_path)? { Ok(blob) => blob, Err(reason) => return Ok(Proof::Unknown(reason)) };
        let new_text = new.as_ref().map(|b| std::str::from_utf8(b.content()).unwrap()).unwrap_or("").to_string();
        drop(new);
        perf_log(&format!("comparison_author_contributions: request={} contextual_fallback left_bytes={} right_bytes={} max_version_bytes={MAX_BLOB} max_windows={MAX_WINDOWS}", self.request_id, old_text.len(), new_text.len()), self.file_started.elapsed());
        let mut windows = Vec::new(); let mut truncated = false; let mut attempts = 0;
        for (from, to, tip, path) in [(&old_text, &new_text, right, new_path), (&new_text, &old_text, left, old_path)] {
            let mut options = git2::DiffOptions::new(); options.context_lines(3).interhunk_lines(0).max_size(MAX_BLOB as i64);
            let patch = git2::Patch::from_buffers(from.as_bytes(), None, to.as_bytes(), None, Some(&mut options)).map_err(|e| e.to_string())?;
            for h in 0..patch.num_hunks() {
                self.budget()?;
                let (_, count) = patch.hunk(h).map_err(|e| e.to_string())?;
                let mut after = Vec::new(); let mut before = Vec::new(); let mut deleted = None;
                for n in 0..count {
                    let line = patch.line_in_hunk(h, n).map_err(|e| e.to_string())?;
                    let content = std::str::from_utf8(line.content()).map_err(|_| "Invalid UTF-8 diff line")?;
                    if line.origin() == ' ' || line.origin() == '+' { after.push((content, line.origin() == '+')); }
                    if line.origin() == '-' { deleted.get_or_insert(before.len()); }
                    if line.origin() == ' ' || line.origin() == '-' { before.push(content); }
                }
                for i in 0..after.len() {
                    self.budget()?;
                    if windows.len() >= MAX_WINDOWS || attempts >= MAX_WINDOWS * 2 { truncated = true; break; }
                    if !after[i].1 { continue; }
                    attempts += 1;
                    let start = i.saturating_sub(3); let end = (i + 4).min(after.len());
                    if after[start..end].iter().map(|(s, _)| s.len()).sum::<usize>() > MAX_WINDOW { truncated = true; continue; }
                    let text = after[start..end].iter().map(|(s, _)| *s).collect::<String>();
                    // Context is only an identifier. It must differ at the two
                    // endpoints, and be unique on the side being traced.
                    if !text.is_empty() && !from.contains(&text) && unique_line(to, &text).is_some() {
                        add_window(&mut windows, tip, path, Window { after: text, target_line: i - start, before: None }, &mut truncated);
                    }
                }
                if !after.iter().any(|(_, added)| *added) {
                    if let Some(target_line) = deleted {
                        let text = after.iter().map(|(s, _)| *s).collect::<String>();
                        if !text.is_empty() && !from.contains(&text) && unique_line(to, &text).is_some() {
                            add_window(&mut windows, tip, path, Window { after: text, target_line, before: Some(before.concat()) }, &mut truncated);
                        }
                    }
                }
            }
        }
        drop(old_text); drop(new_text);
        let mut reason = if truncated { "Contextual analysis reached its limit: 512 witnesses, 1024 candidate windows or 64 KiB per window." } else { CONTEXT };
        let mut structure = None;
        for (tip, path, window) in windows {
            self.budget()?;
            match self.trace_context(tip, &path, &window, 0, &mut HashMap::new())? {
                own @ Proof::Own(..) => return Ok(own),
                value @ Proof::Structure(..) => structure = Some(value),
                Proof::Unknown(actual) if !truncated => reason = actual,
                Proof::Unknown(_) => {},
                Proof::Other => {},
            }
        }
        Ok(structure.unwrap_or(Proof::Unknown(reason)))
    }
}
