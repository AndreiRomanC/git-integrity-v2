//! Bounded, read-only text previews. Download sources survive preview limits and
//! refer to immutable Git blobs, not mutable branch names or rendered diff text.
use super::*;
use snapshot_export::SnapshotSource;
use std::io::Read;

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const MAX_PAIR_LINES: usize = 20_000;

pub(super) struct Version {
    content: String,
    exists: bool,
    size: u64,
    note: Option<String>,
    source: SnapshotSource,
}

pub(super) fn missing() -> Version {
    Version { content: String::new(), exists: false, size: 0, note: None, source: SnapshotSource::Missing }
}

fn name(path: &str) -> String {
    Path::new(path).file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string()
}

fn decode(bytes: Vec<u8>, source: SnapshotSource) -> Version {
    let size = bytes.len() as u64;
    let (content, note) = if size > MAX_BYTES { (String::new(), Some("File exceeds the 10 MiB text-preview limit.".into())) }
        else if bytes.contains(&0) { (String::new(), Some("Binary or non-UTF-8 text; use download to inspect the original file.".into())) }
        else { match String::from_utf8(bytes) {
            Ok(text) => (text, None),
            Err(_) => (String::new(), Some("Non-UTF-8 text; download preserves its original encoding.".into())),
        } };
    Version { content, exists: true, size, note, source }
}

fn oversized(size: u64, source: SnapshotSource) -> Version {
    Version { content: String::new(), exists: true, size,
        note: Some("File exceeds the 10 MiB text-preview limit.".into()), source }
}

pub(super) fn gitlink(oid: Oid, path: &str) -> Version {
    let content = format!("Subproject commit {oid}\n");
    decode(content.as_bytes().to_vec(), SnapshotSource::Text { name: name(path), content, encoding: None })
}

fn blob(repo: &Repository, repository: &str, oid: Oid, path: &str) -> Result<Version, String> {
    let source = SnapshotSource::Blob { repository_path: repository.into(), oid: oid.to_string(), name: name(path) };
    // Inspect the header before materializing a potentially very large blob.
    let (size, kind) = repo.odb().and_then(|odb| odb.read_header(oid))
        .map_err(|e| comparison::snapshot_error(repository, path, "selected", &oid.to_string(), "read-file-header", e))?;
    if kind != ObjectType::Blob { return Err("The selected object is not a file blob".into()); }
    if size as u64 > MAX_BYTES { return Ok(oversized(size as u64, source)); }
    let blob = repo.find_blob(oid)
        .map_err(|e| comparison::snapshot_error(repository, path, "selected", &oid.to_string(), "read-file", e))?;
    Ok(decode(blob.content().to_vec(), source))
}

pub(super) fn tree(repository: &str, revision: &str, path: &str) -> Result<Version, String> {
    let repo = Repository::open(repository).map_err(|e| e.message().to_string())?;
    let commit = comparison::snapshot_commit(&repo, repository, path, revision, "selected")?;
    let root = commit.tree().map_err(|e| comparison::snapshot_error(repository, path, "selected", revision, "read-root-tree", e))?;
    let Some(entry) = comparison::snapshot_path_entry(&repo, &root, Path::new(path))
        .map_err(|e| comparison::snapshot_error(repository, path, "selected", revision, "lookup-file", e))? else { return Ok(missing()); };
    if entry.filemode() == 0o160000 { return Ok(gitlink(entry.id(), path)); }
    blob(&repo, repository, entry.id(), path)
}

pub(super) fn index(repo: &Repository, repository: &str, path: &str) -> Result<Version, String> {
    let index = repo.index().map_err(|e| e.message().to_string())?;
    match index.get_path(Path::new(path), 0) {
        Some(entry) if entry.mode == 0o160000 => Ok(gitlink(entry.id, path)),
        Some(entry) => blob(repo, repository, entry.id, path),
        None => Ok(missing()),
    }
}

pub(super) fn worktree(repository: &str, path: &str) -> Result<Version, String> {
    let absolute = Path::new(repository).join(safe_relative_path(path)?);
    let file = match fs::File::open(&absolute) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(missing()),
        Err(e) => return Err(format!("Cannot read working-tree file: {e}")),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() { return Err("Choose a regular file for text comparison".into()); }
    let source = SnapshotSource::Local { path: absolute.to_string_lossy().into_owned() };
    let mut version = if metadata.len() > MAX_BYTES { oversized(metadata.len(), source) } else {
        let mut bytes = Vec::new();
        // Bound the read even if an external process grows the file meanwhile.
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        decode(bytes, source)
    };
    if version.note.is_none() {
        version.source = SnapshotSource::Text { name: name(path), content: version.content.clone(), encoding: None };
    } else {
        version.note.as_mut().unwrap().push_str(" Working-tree downloads copy the file at download time.");
    }
    Ok(version)
}

pub(super) fn pair(path: String, right_label: String, left: Version, right: Version) -> FileComparison {
    let mut notes = Vec::new();
    if let Some(note) = &left.note { notes.push(format!("Left: {note}")); }
    if let Some(note) = &right.note { notes.push(format!("Right: {note}")); }
    if notes.is_empty() && left.content.lines().count() + right.content.lines().count() > MAX_PAIR_LINES {
        notes.push("More than 20,000 source lines across both versions. Download both complete versions to compare externally.".into());
    }
    let preview_note = (!notes.is_empty()).then(|| notes.join("\n"));
    let show_text = preview_note.is_none();
    FileComparison {
        relative_path: path, remote_ref: right_label,
        local_content: if show_text { left.content } else { String::new() },
        remote_content: if show_text { right.content } else { String::new() },
        local_exists: left.exists, remote_exists: right.exists,
        local_size: left.size, remote_size: right.size, preview_note,
        download_sources: vec![left.source, right.source],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture { root: PathBuf, repo: Repository }
    impl Fixture {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("gdd-file-preview-{}-{stamp}", std::process::id()));
            fs::create_dir_all(root.join("out")).unwrap();
            let repo = Repository::init(root.join("repo")).unwrap();
            Self { root, repo }
        }
        fn path(&self) -> String { self.root.join("repo").to_string_lossy().into() }
        fn commit(&self, bytes: &[u8]) -> String {
            let blob = self.repo.blob(bytes).unwrap();
            let mut builder = self.repo.treebuilder(None).unwrap();
            builder.insert("Dcm.xdm", blob, 0o100644).unwrap();
            self.commit_tree(builder.write().unwrap())
        }
        fn commit_tree(&self, oid: Oid) -> String {
            let tree = self.repo.find_tree(oid).unwrap();
            let signature = git2::Signature::now("Preview", "preview@example.invalid").unwrap();
            let parent = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
            self.repo.commit(Some("HEAD"), &signature, &signature, "snapshot", &tree, &parent.iter().collect::<Vec<_>>()).unwrap().to_string()
        }
        fn export(&self, sources: Vec<SnapshotSource>) -> PathBuf {
            snapshot_export::export_snapshots_inner("Dcm.xdm".into(), sources, self.root.join("out").to_string_lossy().into()).unwrap().into()
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }

    #[test]
    fn preview_large_xml_and_exports_keep_exact_git_bytes_and_selected_revisions() {
        let f = Fixture::new();
        let before = format!("<root>\r\n{}\r\n</root>", "x".repeat(2 * 1024 * 1024));
        let after = before.replacen("xxxx", "yyyy", 1);
        let left = f.commit(before.as_bytes()); let right = f.commit(after.as_bytes());
        let comparison = compare_git_tree_file(&f.path(), "Dcm.xdm", &left, &right).unwrap();
        assert!(comparison.preview_note.is_none());
        assert_eq!(comparison.local_content, before); assert_eq!(comparison.remote_content, after);
        f.commit(b"another commit after the comparison was opened");
        let head = resolve_commit(&f.path(), "HEAD").unwrap();
        let export = f.export(comparison.download_sources);
        assert_eq!(fs::read(export.join("left/Dcm.xdm")).unwrap(), before.as_bytes());
        assert_eq!(fs::read(export.join("right/Dcm.xdm")).unwrap(), after.as_bytes());
        assert_eq!(resolve_commit(&f.path(), "HEAD").unwrap(), head);
    }

    #[test]
    fn preview_binary_non_utf8_and_oversized_blobs_remain_downloadable_in_full() {
        let f = Fixture::new();
        for bytes in [b"binary\0original".to_vec(), vec![0xff, 0xfe, 0x61], vec![b'x'; MAX_BYTES as usize + 111]] {
            let revision = f.commit(&bytes);
            let result = compare_git_tree_file(&f.path(), "Dcm.xdm", &revision, &revision).unwrap();
            assert!(result.preview_note.is_some()); assert!(result.local_content.is_empty());
            assert!(result.local_exists); assert_eq!(result.local_size, bytes.len() as u64);
            let export = f.export(result.download_sources);
            assert_eq!(fs::read(export.join("left/Dcm.xdm")).unwrap(), bytes);
            assert_eq!(fs::read(export.join("right/Dcm.xdm")).unwrap(), bytes);
        }
    }

    #[test]
    fn preview_working_area_and_workspace_share_limits_without_modifying_head_or_index() {
        let f = Fixture::new();
        let bytes = vec![b'a'; 2 * 1024 * 1024];
        let head = f.commit(&bytes);
        let mut staged = bytes.clone(); staged[0] = b'b';
        fs::write(f.root.join("repo/Dcm.xdm"), &staged).unwrap();
        let mut index = f.repo.index().unwrap(); index.add_path(Path::new("Dcm.xdm")).unwrap(); index.write().unwrap();
        let index_before = fs::read(f.root.join("repo/.git/index")).unwrap();
        let mut working = staged.clone(); working[1] = b'c';
        fs::write(f.root.join("repo/Dcm.xdm"), &working).unwrap();
        let staged_result = compare_working_area_file_inner(f.path(), "Dcm.xdm".into(), true).unwrap();
        assert_eq!(staged_result.local_content.as_bytes(), bytes); assert_eq!(staged_result.remote_content.as_bytes(), staged);
        let work_result = compare_working_area_file_inner(f.path(), "Dcm.xdm".into(), false).unwrap();
        assert_eq!(work_result.local_content.as_bytes(), staged); assert_eq!(work_result.remote_content.as_bytes(), working);
        let index_result = compare_index_worktree_file_inner(f.path(), "Dcm.xdm".into()).unwrap();
        assert_eq!(index_result.local_content.as_bytes(), working); assert_eq!(index_result.remote_content.as_bytes(), staged);
        let workspace = comparison::workspace_file(&f.path(), "Dcm.xdm", &head).unwrap();
        assert_eq!(workspace.local_content.as_bytes(), working); assert_eq!(workspace.remote_content.as_bytes(), bytes);
        fs::write(f.root.join("repo/Dcm.xdm"), "changed after preview").unwrap();
        let export = f.export(workspace.download_sources);
        assert_eq!(fs::read(export.join("left/Dcm.xdm")).unwrap(), working, "loaded text export is pinned, not reread");
        assert_eq!(fs::read(f.root.join("repo/.git/index")).unwrap(), index_before);
        assert_eq!(resolve_commit(&f.path(), "HEAD").unwrap(), head);
    }

    #[test]
    fn preview_line_budget_missing_empty_and_read_errors_are_distinct() {
        let f = Fixture::new();
        let lines = "line\n".repeat(10_001);
        let revision = f.commit(lines.as_bytes());
        let result = compare_git_tree_file(&f.path(), "Dcm.xdm", &revision, &revision).unwrap();
        assert!(result.preview_note.unwrap().contains("20,000"));
        let export = f.export(result.download_sources);
        assert_eq!(fs::read_to_string(export.join("left/Dcm.xdm")).unwrap(), lines);
        let empty = f.commit(b"");
        let result = compare_git_tree_file(&f.path(), "Dcm.xdm", &empty, &empty).unwrap();
        assert!(result.local_exists); assert!(result.preview_note.is_none());
        let result = compare_git_tree_file(&f.path(), "absent", &empty, &empty).unwrap();
        assert!(!result.local_exists); assert!(!result.remote_exists);
        let mut raw = b"100644 Dcm.xdm\0".to_vec(); raw.extend([0x22; 20]);
        let broken_tree = f.repo.odb().unwrap().write(ObjectType::Tree, &raw).unwrap();
        let broken = f.commit_tree(broken_tree);
        assert!(compare_git_tree_file(&f.path(), "Dcm.xdm", &empty, &broken).err().unwrap().contains("Comparison is incomplete"));
    }

    #[test]
    fn preview_large_worktree_download_is_explicitly_live_and_missing_side_exports_marker() {
        let f = Fixture::new();
        let bytes = vec![b'x'; MAX_BYTES as usize + 1];
        fs::write(f.root.join("repo/Dcm.xdm"), &bytes).unwrap();
        let result = pair("Dcm.xdm".into(), "missing".into(), worktree(&f.path(), "Dcm.xdm").unwrap(), missing());
        assert!(result.preview_note.unwrap().contains("at download time"));
        let export = f.export(result.download_sources);
        assert_eq!(fs::read(export.join("left/Dcm.xdm")).unwrap(), bytes);
        assert!(export.join("right/.gitdrilldown-absent").exists());
    }
}
