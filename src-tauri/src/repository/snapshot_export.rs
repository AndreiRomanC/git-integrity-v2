//! Read-only sources; all writes are confined to a newly-created export folder.
use super::*;
use serde::Deserialize;
use std::io::Write;

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SnapshotSource {
    Revision { repository_path: String, submodule_path: Option<String>, revision: String, relative_path: String },
    Blob { repository_path: String, oid: String, name: String },
    Local { path: String },
    Text { name: String, content: String, encoding: Option<String> },
    Missing,
}

fn destination_directory(path: &str) -> Result<PathBuf, String> {
    let destination = fs::canonicalize(path).map_err(|e| format!("Cannot open export destination: {e}"))?;
    if !destination.is_dir() || destination.components().any(|p| p.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git")) {
        return Err("Choose an existing export folder outside Git metadata".into());
    }
    Ok(destination)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path)
        .map_err(|e| format!("Cannot create export file {}: {e}", path.display()))?;
    file.write_all(bytes).map_err(|e| format!("Cannot finish export file {}: {e}", path.display()))
}

fn absent(destination: &Path) -> Result<(), String> {
    write_new(&destination.join(".gitdrilldown-absent"), b"This path does not exist on this side. No empty source file was fabricated.\n")
}

// Do not follow links/junctions into other locations. Git metadata is never copied.
// Local folders are copied as they exist at export time (not a filesystem snapshot).
fn copy_local(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|e| format!("Cannot read export source {}: {e}", source.display()))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 { return Err("Export stopped at a Windows reparse point; no linked location was copied".into()); }
    }
    if metadata.file_type().is_symlink() { return Err("Export stopped at a symbolic link; no linked location was copied".into()); }
    if metadata.is_dir() {
        fs::create_dir(destination).map_err(|e| e.to_string())?;
        for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            if name.to_string_lossy().eq_ignore_ascii_case(".git") { continue; }
            copy_local(&entry.path(), &destination.join(name))?;
        }
        Ok(())
    } else if metadata.is_file() {
        crate::local_drive::copy_file_new(source, destination).map(|_| ())
    } else { Err("Cannot export this special filesystem item".into()) }
}

fn revision_source(repository_path: &str, submodule_path: &Option<String>, relative_path: &str) -> Result<(String, PathBuf), String> {
    let relative = safe_relative_path(relative_path)?;
    let repository = if let Some(path) = submodule_path.as_ref().filter(|p| !p.is_empty()) {
        let absolute = validate_submodule(repository_path, path)?;
        internal_submodule_repository(&absolute)?;
        absolute.to_string_lossy().into_owned()
    } else { repository_path.to_string() };
    validate_path(&repository)?;
    Ok((repository, relative))
}

fn export_one(source: &SnapshotSource, destination: &Path) -> Result<(), String> {
    fs::create_dir(destination).map_err(|e| e.to_string())?;
    match source {
        SnapshotSource::Missing => absent(destination),
        SnapshotSource::Blob { repository_path, oid, name } => {
            if !is_safe_export_entry_name(name) { return Err("Unsafe export filename".into()); }
            validate_path(repository_path)?;
            let repo = Repository::open(repository_path).map_err(|e| e.message().to_string())?;
            let oid = Oid::from_str(oid).map_err(|e| e.message().to_string())?;
            let blob = repo.find_blob(oid).map_err(|e| e.message().to_string())?;
            write_new(&destination.join(name), blob.content())
        }
        SnapshotSource::Text { name, content, encoding } => {
            if !is_safe_export_entry_name(name) { return Err("Unsafe export filename".into()); }
            let bytes = crate::local_drive::encode_local_text(content, encoding.as_deref().unwrap_or("utf-8"))?;
            write_new(&destination.join(name), &bytes)
        }
        SnapshotSource::Local { path } => {
            let source = Path::new(path);
            match fs::symlink_metadata(source) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return absent(destination),
                Err(e) => return Err(e.to_string()),
                Ok(_) => {}
            }
            // Preserve the selected item's name inside the left/right directory.
            let name = source.file_name().ok_or("Select a folder, not a filesystem root")?;
            copy_local(source, &destination.join(name))
        }
        SnapshotSource::Revision { repository_path, submodule_path, revision, relative_path } => {
            let (repository, relative) = revision_source(repository_path, submodule_path, relative_path)?;
            let repo = internal_repository(&repository)?;
            let commit = repo.revparse_single(revision).and_then(|o| o.peel_to_commit()).map_err(|e| e.message().to_string())?;
            let tree = commit.tree().map_err(|e| e.message().to_string())?;
            if relative.as_os_str().is_empty() { return export_tree_to_directory(&repo, &tree, destination); }
            let entry = match tree.get_path(&relative) {
                Ok(entry) => entry,
                Err(e) if e.code() == git2::ErrorCode::NotFound => return absent(destination),
                Err(e) => return Err(e.message().to_string()),
            };
            let name = relative.file_name().and_then(|n| n.to_str()).ok_or("Invalid snapshot path")?;
            if !is_safe_export_entry_name(name) { return Err("Unsafe snapshot name".into()); }
            match entry.kind() {
                Some(ObjectType::Tree) => export_tree_to_directory(&repo, &repo.find_tree(entry.id()).map_err(|e| e.message().to_string())?, &destination.join(name)),
                Some(ObjectType::Blob) => write_new(&destination.join(name), repo.find_blob(entry.id()).map_err(|e| e.message().to_string())?.content()),
                Some(ObjectType::Commit) => write_new(&destination.join(".gitdrilldown-gitlink"), format!("{}\n", entry.id()).as_bytes()),
                _ => Err("Unsupported Git snapshot entry".into()),
            }
        }
    }
}

pub(super) fn export_snapshots_inner(name: String, sources: Vec<SnapshotSource>, destination_path: String) -> Result<String, String> {
    if sources.is_empty() || sources.len() > 2 { return Err("Select one snapshot or a comparison pair".into()); }
    let destination = destination_directory(&destination_path)?;
    for source in &sources {
        let protected = match source {
            SnapshotSource::Local { path } => {
                let path = Path::new(path);
                if !path.is_absolute() || path.components().any(|p| p.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git")) { return Err("Select an absolute source path outside Git metadata".into()); }
                // A missing side is valid. Resolve its nearest existing ancestor
                // to catch exports into the source through symlinked parents too.
                let ancestor = path.ancestors().find(|p| p.exists()).ok_or("Cannot resolve source path")?;
                Some(fs::canonicalize(ancestor).map_err(|e| e.to_string())?)
            }
            SnapshotSource::Revision { repository_path, submodule_path, relative_path, .. } => {
                let (path, _) = revision_source(repository_path, submodule_path, relative_path)?;
                Some(fs::canonicalize(path).map_err(|e| e.to_string())?)
            }
            SnapshotSource::Blob { repository_path, .. } => {
                validate_path(repository_path)?;
                Some(fs::canonicalize(repository_path).map_err(|e| e.to_string())?)
            }
            _ => None,
        };
        if protected.as_ref().is_some_and(|path| destination.starts_with(path)) {
            return Err("Choose a destination outside the source folder/repository so export cannot modify or copy itself".into());
        }
    }
    let root = unique_submodule_snapshot_root(&destination, &name, "snapshot", if sources.len() == 2 { "pair" } else { "single" })?;
    let result = (|| {
        for (index, source) in sources.iter().enumerate() {
            let label = if sources.len() == 1 { "snapshot" } else if index == 0 { "left" } else { "right" };
            export_one(source, &root.join(label))?;
        }
        write_new(&root.join("README.txt"), b"Git DrillDown snapshot export\nNo checkout, staging, restore, commit or push was performed.\nText comparisons contain the loaded text, not HTML or diff markers.\nLocal folders are copied at export time; avoid editing them during export. Git metadata is excluded.\nGit snapshots contain exact tracked blobs; nested submodules are represented by .gitdrilldown-gitlink SHA markers, not cloned. Git symlink blobs are exported as plain target-text files, not followed.\nA .gitdrilldown-absent marker means the path did not exist on that side.\n")
    })();
    if let Err(error) = result {
        // Only our freshly-created directory, never an existing user target.
        let cleanup = fs::remove_dir_all(&root);
        return Err(if cleanup.is_ok() { format!("Snapshot export failed; partial export removed: {error}") }
            else { format!("Snapshot export failed; incomplete files remain at {}: {error}", root.display()) });
    }
    Ok(root.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn export_snapshots(name: String, sources: Vec<SnapshotSource>, destination_path: String) -> Result<String, String> {
    off_main_thread(move || export_snapshots_inner(name, sources, destination_path)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("ddt-export-{}-{}-{}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&path).unwrap();
            fs::create_dir(path.join("out")).unwrap();
            Self(path)
        }
        fn export(&self, sources: Vec<SnapshotSource>) -> Result<PathBuf, String> {
            export_snapshots_inner("sample".into(), sources, self.0.join("out").to_string_lossy().into()).map(PathBuf::from)
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    fn text(content: &str) -> SnapshotSource { SnapshotSource::Text { name: "file.txt".into(), content: content.into(), encoding: None } }
    fn revision(repo: &Path, sha: git2::Oid, path: &str) -> SnapshotSource {
        SnapshotSource::Revision { repository_path: repo.to_string_lossy().into(), submodule_path: None, revision: sha.to_string(), relative_path: path.into() }
    }
    fn commit(repo: &Repository, content: &str, parent: Option<git2::Oid>) -> git2::Oid {
        let blob = repo.blob(content.as_bytes()).unwrap();
        let empty = repo.blob(b"").unwrap();
        let mut folder = repo.treebuilder(None).unwrap();
        folder.insert("file.txt", blob, 0o100644).unwrap();
        folder.insert("empty.txt", empty, 0o100644).unwrap();
        let folder_oid = folder.write().unwrap();
        let mut root = repo.treebuilder(None).unwrap();
        root.insert("folder", folder_oid, 0o040000).unwrap();
        root.insert("outside.txt", blob, 0o100644).unwrap();
        if let Some(id) = parent { root.insert("module", id, 0o160000).unwrap(); }
        let tree = repo.find_tree(root.write().unwrap()).unwrap();
        let signature = git2::Signature::now("Export test", "export@example.invalid").unwrap();
        let parents = parent.map(|id| repo.find_commit(id).unwrap());
        repo.commit(Some("HEAD"), &signature, &signature, "snapshot", &tree, &parents.iter().collect::<Vec<_>>()).unwrap()
    }
    #[test]
    fn snapshot_export_preserves_scoped_blobs_head_index_and_local_changes() {
        let fixture = Fixture::new();
        let path = fixture.0.join("repo");
        let repo = Repository::init(&path).unwrap();
        let first = commit(&repo, "old\r\n", None);
        let second = commit(&repo, "new\r\n", Some(first));
        fs::write(path.join("outside.txt"), "uncommitted work").unwrap();
        let mut index = repo.index().unwrap(); index.add_path(Path::new("outside.txt")).unwrap(); index.write().unwrap();
        fs::write(path.join("outside.txt"), "unstaged work").unwrap();
        let index_before = fs::read(path.join(".git/index")).unwrap();
        let out = fixture.export(vec![revision(&path, first, "folder"), revision(&path, second, "folder")]).unwrap();
        assert_eq!(fs::read(out.join("left/folder/file.txt")).unwrap(), b"old\r\n");
        assert_eq!(fs::read(out.join("right/folder/file.txt")).unwrap(), b"new\r\n");
        assert!(!out.join("left/outside.txt").exists());
        assert_eq!(repo.head().unwrap().target(), Some(second));
        assert_eq!(fs::read(path.join(".git/index")).unwrap(), index_before);
        assert_eq!(fs::read(path.join("outside.txt")).unwrap(), b"unstaged work");
    }
    #[test]
    fn snapshot_export_single_file_absence_empty_and_gitlink_are_distinct() {
        let fixture = Fixture::new(); let path = fixture.0.join("repo");
        let repo = Repository::init(&path).unwrap();
        let first = commit(&repo, "content", None); let second = commit(&repo, "content", Some(first));
        let out = fixture.export(vec![revision(&path, first, "folder/empty.txt"), revision(&path, first, "gone.txt")]).unwrap();
        assert_eq!(fs::read(out.join("left/empty.txt")).unwrap(), b"");
        assert!(out.join("right/.gitdrilldown-absent").exists());
        assert!(!out.join("right/gone.txt").exists());
        let out = fixture.export(vec![revision(&path, second, "module")]).unwrap();
        assert_eq!(fs::read_to_string(out.join("snapshot/.gitdrilldown-gitlink")).unwrap(), format!("{first}\n"));
    }
    #[test]
    fn snapshot_export_presence_in_working_comparison_does_not_confuse_empty_with_deleted() {
        let fixture = Fixture::new(); let path = fixture.0.join("repo");
        let repo = Repository::init(&path).unwrap(); let head = commit(&repo, "content", None);
        let mut index = repo.index().unwrap();
        index.read_tree(&repo.find_commit(head).unwrap().tree().unwrap()).unwrap(); index.write().unwrap();
        fs::create_dir(path.join("folder")).unwrap(); fs::write(path.join("folder/empty.txt"), "").unwrap();
        let compare = |staged| compare_working_area_file_inner(path.to_string_lossy().into(), "folder/empty.txt".into(), staged).unwrap();
        assert!(compare(false).local_exists && compare(false).remote_exists);
        fs::remove_file(path.join("folder/empty.txt")).unwrap();
        assert!(compare(false).local_exists && !compare(false).remote_exists);
        index.remove_path(Path::new("folder/empty.txt")).unwrap(); index.write().unwrap();
        assert!(compare(true).local_exists && !compare(true).remote_exists);
        let raw = compare_git_tree_file(&path.to_string_lossy(), "gone.txt", &head.to_string(), &head.to_string()).unwrap();
        assert!(!raw.local_exists && !raw.remote_exists);
    }
    #[test]
    fn snapshot_export_submodule_uses_its_own_objects_without_switching_either_head() {
        let fixture = Fixture::new(); let path = fixture.0.join("repo");
        let repo = Repository::init(&path).unwrap(); let initial = commit(&repo, "parent", None);
        let sub_path = path.join("module"); let sub = Repository::init(&sub_path).unwrap();
        let old = commit(&sub, "sub old", None); let current = commit(&sub, "sub current", Some(old));
        // The parent snapshot's gitlink registers module; no checkout needed.
        let parent_head = commit(&repo, "parent", Some(initial));
        let mut index = repo.index().unwrap();
        index.read_tree(&repo.find_commit(parent_head).unwrap().tree().unwrap()).unwrap(); index.write().unwrap();
        let out = fixture.export(vec![SnapshotSource::Revision { repository_path: path.to_string_lossy().into(),
            submodule_path: Some("module".into()), revision: old.to_string(), relative_path: "".into() }]).unwrap();
        assert_eq!(fs::read(out.join("snapshot/folder/file.txt")).unwrap(), b"sub old");
        assert_eq!(sub.head().unwrap().target(), Some(current));
        assert_eq!(repo.head().unwrap().target(), Some(parent_head));
    }
    #[test]
    fn snapshot_export_text_keeps_crlf_and_encoding_and_never_overwrites() {
        let fixture = Fixture::new();
        let first = fixture.export(vec![text("a\r\n"), SnapshotSource::Missing]).unwrap();
        let second = fixture.export(vec![text("new"), text("")]).unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(first.join("left/file.txt")).unwrap(), b"a\r\n");
        assert!(first.join("right/.gitdrilldown-absent").exists());
        let encoded = fixture.export(vec![SnapshotSource::Text { name: "unicode.txt".into(), content: "a".into(), encoding: Some("utf-16le".into()) }]).unwrap();
        assert_eq!(fs::read(encoded.join("snapshot/unicode.txt")).unwrap(), [0xff, 0xfe, 97, 0]);
    }
    #[test]
    fn snapshot_export_local_folder_excludes_git_metadata_and_rejects_self_copy() {
        let fixture = Fixture::new(); let source = fixture.0.join("source");
        fs::create_dir(&source).unwrap(); fs::create_dir(source.join(".git")).unwrap();
        fs::write(source.join(".git/config"), "private metadata").unwrap();
        fs::write(source.join("file.txt"), "local").unwrap();
        let out = fixture.export(vec![SnapshotSource::Local { path: source.to_string_lossy().into() }]).unwrap();
        assert_eq!(fs::read(out.join("snapshot/source/file.txt")).unwrap(), b"local");
        assert!(!out.join("snapshot/source/.git").exists());
        assert!(export_snapshots_inner("x".into(), vec![SnapshotSource::Local { path: source.to_string_lossy().into() }], source.to_string_lossy().into()).is_err());
    }
    #[test]
    fn snapshot_export_failure_cleans_only_its_new_output() {
        let fixture = Fixture::new(); let first = fixture.export(vec![text("keep")]).unwrap();
        assert!(fixture.export(vec![text("partial"), SnapshotSource::Text { name: "../escape".into(), content: "bad".into(), encoding: None }]).is_err());
        assert_eq!(fs::read_dir(fixture.0.join("out")).unwrap().count(), 1);
        assert_eq!(fs::read(first.join("snapshot/file.txt")).unwrap(), b"keep");
    }
    #[cfg(unix)]
    #[test]
    fn snapshot_export_does_not_follow_local_symlinks() {
        let fixture = Fixture::new(); let source = fixture.0.join("source"); fs::create_dir(&source).unwrap();
        std::os::unix::fs::symlink(&fixture.0, source.join("escape")).unwrap();
        assert!(fixture.export(vec![SnapshotSource::Local { path: source.to_string_lossy().into() }]).is_err());
        assert_eq!(fs::read_dir(fixture.0.join("out")).unwrap().count(), 0);
    }
}
