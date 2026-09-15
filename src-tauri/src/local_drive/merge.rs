use super::{
    contains_git_metadata, copy_file_new, decode_local_text, existing_directory, fingerprint_file,
    replace_with_staged_file, temporary_sibling, LocalCopyResult, MAX_LOCAL_TEXT_BYTES,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::hash::Hasher;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};

const MAX_FOLDER_ITEMS: usize = 50_000;

#[derive(Clone)]
enum ItemKind {
    File { bytes: u64 },
    Symlink,
}

type ItemMap = BTreeMap<String, (PathBuf, ItemKind)>;

#[derive(Serialize)]
pub struct LocalDirectoryDifference {
    relative_path: String,
    left_path: Option<String>,
    right_path: Option<String>,
    status: String,
    left_bytes: u64,
    right_bytes: u64,
    reviewable: bool,
    right_fingerprint: Option<String>,
}

#[derive(Default, Serialize)]
pub struct LocalDirectoryCounts {
    same: u64,
    modified: u64,
    left_only: u64,
    right_only: u64,
    conflicts: u64,
}

#[derive(Serialize)]
pub struct LocalDirectoryComparison {
    left_root: String,
    right_root: String,
    entries: Vec<LocalDirectoryDifference>,
    counts: LocalDirectoryCounts,
}

fn safe_relative_path(relative_path: &str) -> Result<PathBuf, String> {
    let value = PathBuf::from(relative_path.trim());
    if value.as_os_str().is_empty() || value.is_absolute() {
        return Err("Choose one item inside the compared folder".into());
    }
    for component in value.components() {
        match component {
            Component::Normal(name) if !name.to_string_lossy().eq_ignore_ascii_case(".git") => {}
            _ => return Err("The merge item must stay inside the selected folders and cannot include .git metadata".into()),
        }
    }
    Ok(value)
}

fn comparison_key(relative: &Path) -> String {
    let value = relative.to_string_lossy().replace('\\', "/");
    if cfg!(windows) { value.to_lowercase() } else { value }
}

fn collect_items(root: &Path, current: &Path, output: &mut ItemMap) -> Result<(), String> {
    for item in fs::read_dir(current).map_err(|error| format!("Cannot read '{}': {error}", current.display()))? {
        let item = item.map_err(|error| error.to_string())?;
        if item.file_name().to_string_lossy().eq_ignore_ascii_case(".git") { continue; }
        let path = item.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| format!("Cannot inspect '{}': {error}", path.display()))?;
        let relative = path.strip_prefix(root).map_err(|_| "A folder item escaped its selected root")?.to_path_buf();
        if metadata.file_type().is_symlink() {
            output.insert(comparison_key(&relative), (relative, ItemKind::Symlink));
        } else if metadata.is_dir() {
            collect_items(root, &path, output)?;
        } else if metadata.is_file() {
            output.insert(comparison_key(&relative), (relative, ItemKind::File { bytes: metadata.len() }));
        }
        if output.len() > MAX_FOLDER_ITEMS {
            return Err(format!("Folder comparison stopped safely after {MAX_FOLDER_ITEMS} files. Narrow the two folders and compare again."));
        }
    }
    Ok(())
}

fn files_identical_and_right_fingerprint(left: &Path, right: &Path, bytes: u64) -> Result<(bool, String), String> {
    let right_bytes = fs::metadata(right).map_err(|error| format!("Cannot inspect '{}': {error}", right.display()))?.len();
    if right_bytes != bytes { return Ok((false, fingerprint_file(right)?)); }
    let mut left = BufReader::new(fs::File::open(left).map_err(|error| error.to_string())?);
    let mut right = BufReader::new(fs::File::open(right).map_err(|error| error.to_string())?);
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    let mut right_hasher = std::collections::hash_map::DefaultHasher::new();
    let mut identical = true;
    loop {
        let left_count = left.read(&mut left_buffer).map_err(|error| error.to_string())?;
        let right_count = right.read(&mut right_buffer).map_err(|error| error.to_string())?;
        right_hasher.write(&right_buffer[..right_count]);
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] { identical = false; }
        if left_count == 0 && right_count == 0 {
            return Ok((identical, format!("{:016x}:{}", right_hasher.finish(), right_bytes)));
        }
    }
}

fn probably_reviewable_text(path: &Path, bytes: u64) -> bool {
    if bytes > MAX_LOCAL_TEXT_BYTES { return false; }
    fs::read(path).ok().and_then(|content| decode_local_text(content).ok()).is_some()
}

fn path_string(root: &Path, relative: &Path) -> String {
    root.join(relative).to_string_lossy().into_owned()
}

fn compare_local_directories_inner(left_path: String, right_path: String) -> Result<LocalDirectoryComparison, String> {
    let left_root = existing_directory(&left_path)?;
    let right_root = existing_directory(&right_path)?;
    if left_root == right_root { return Err("Choose two different folders to compare".into()); }
    if contains_git_metadata(&left_root) || contains_git_metadata(&right_root) {
        return Err("Git metadata (.git) is protected in Local Drive".into());
    }
    let mut left = BTreeMap::new();
    let mut right = BTreeMap::new();
    collect_items(&left_root, &left_root, &mut left)?;
    collect_items(&right_root, &right_root, &mut right)?;
    let mut paths = left.keys().chain(right.keys()).cloned().collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let mut counts = LocalDirectoryCounts::default();
    let mut entries = Vec::with_capacity(paths.len());
    for key in paths {
        let left_record = left.get(&key);
        let right_record = right.get(&key);
        let relative = left_record.map(|record| &record.0).or_else(|| right_record.map(|record| &record.0)).ok_or("Folder comparison lost an item")?;
        let left_item = left_record.map(|record| &record.1);
        let right_item = right_record.map(|record| &record.1);
        let (status, left_bytes, right_bytes, reviewable, right_fingerprint) = match (left_item, right_item) {
            (Some(ItemKind::File { bytes: left_bytes }), Some(ItemKind::File { bytes: right_bytes })) => {
                let left_file = left_root.join(&left_record.unwrap().0);
                let right_file = right_root.join(&right_record.unwrap().0);
                let (identical, right_fingerprint) = files_identical_and_right_fingerprint(&left_file, &right_file, *left_bytes)?;
                if identical {
                    counts.same += 1;
                    ("same", *left_bytes, *right_bytes, false, None)
                } else {
                    counts.modified += 1;
                    let reviewable = probably_reviewable_text(&left_file, *left_bytes) && probably_reviewable_text(&right_file, *right_bytes);
                    ("modified", *left_bytes, *right_bytes, reviewable, Some(right_fingerprint))
                }
            }
            (Some(ItemKind::File { bytes }), None) => {
                counts.left_only += 1;
                ("left-only", *bytes, 0, probably_reviewable_text(&left_root.join(&left_record.unwrap().0), *bytes), None)
            }
            (None, Some(ItemKind::File { bytes })) => {
                counts.right_only += 1;
                ("right-only", 0, *bytes, false, None)
            }
            (Some(ItemKind::Symlink), Some(ItemKind::Symlink)) => {
                counts.conflicts += 1;
                ("unsupported", 0, 0, false, None)
            }
            _ => {
                counts.conflicts += 1;
                ("type-conflict", 0, 0, false, None)
            }
        };
        entries.push(LocalDirectoryDifference {
            relative_path: relative.to_string_lossy().into_owned(),
            left_path: left_record.map(|record| path_string(&left_root, &record.0)),
            right_path: right_record.map(|record| path_string(&right_root, &record.0)),
            status: status.into(), left_bytes, right_bytes, reviewable, right_fingerprint,
        });
    }
    Ok(LocalDirectoryComparison {
        left_root: left_root.to_string_lossy().into_owned(),
        right_root: right_root.to_string_lossy().into_owned(),
        entries, counts,
    })
}

#[tauri::command]
pub async fn compare_local_directories(left_path: String, right_path: String) -> Result<LocalDirectoryComparison, String> {
    super::off_main_thread(move || compare_local_directories_inner(left_path, right_path)).await
}

fn checked_merge_paths(left_root: String, right_root: String, relative_path: String) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let left_root = existing_directory(&left_root)?;
    let right_root = existing_directory(&right_root)?;
    if left_root == right_root { return Err("Source and destination folders must be different".into()); }
    let relative = safe_relative_path(&relative_path)?;
    let source = left_root.join(&relative);
    let source_metadata = fs::symlink_metadata(&source).map_err(|error| format!("Cannot open source '{}': {error}", source.display()))?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_file() { return Err("Guided merge accepts regular files only".into()); }
    let source = fs::canonicalize(&source).map_err(|error| error.to_string())?;
    if !source.starts_with(&left_root) { return Err("The source escaped the selected left folder".into()); }

    let mut destination_parent = right_root.clone();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            let Component::Normal(name) = component else { return Err("Invalid destination path".into()); };
            destination_parent.push(name);
            if destination_parent.exists() {
                let metadata = fs::symlink_metadata(&destination_parent).map_err(|error| error.to_string())?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() { return Err(format!("Destination parent '{}' is not a safe folder", destination_parent.display())); }
            } else {
                fs::create_dir(&destination_parent).map_err(|error| format!("Cannot create destination folder '{}': {error}", destination_parent.display()))?;
            }
        }
    }
    Ok((source, right_root.join(&relative), right_root))
}

fn copy_local_merge_file_inner(left_root: String, right_root: String, relative_path: String) -> Result<LocalCopyResult, String> {
    let (source, destination, _) = checked_merge_paths(left_root, right_root, relative_path)?;
    if destination.exists() { return Err(format!("'{}' now exists. Nothing was overwritten — compare the folders again.", destination.display())); }
    let bytes = copy_file_new(&source, &destination)?;
    Ok(LocalCopyResult { destination: destination.to_string_lossy().into_owned(), files: 1, directories: 0, bytes })
}

#[tauri::command]
pub async fn copy_local_merge_file(left_root: String, right_root: String, relative_path: String) -> Result<LocalCopyResult, String> {
    super::off_main_thread(move || copy_local_merge_file_inner(left_root, right_root, relative_path)).await
}

fn replace_local_merge_file_inner(left_root: String, right_root: String, relative_path: String, expected_right_fingerprint: String) -> Result<LocalCopyResult, String> {
    let (source, destination, _) = checked_merge_paths(left_root, right_root, relative_path)?;
    let metadata = fs::symlink_metadata(&destination).map_err(|error| format!("Cannot open destination '{}': {error}", destination.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() { return Err("The destination is no longer a regular file. Nothing was overwritten.".into()); }
    if fingerprint_file(&destination)? != expected_right_fingerprint {
        return Err(format!("'{}' changed after the folder scan. Nothing was overwritten — compare the folders again.", destination.display()));
    }
    let staged = temporary_sibling(&destination, "merge")?;
    let bytes = match copy_file_new(&source, &staged) {
        Ok(bytes) => bytes,
        Err(error) => { let _ = fs::remove_file(&staged); return Err(error); }
    };
    if fingerprint_file(&destination)? != expected_right_fingerprint {
        let _ = fs::remove_file(&staged);
        return Err(format!("'{}' changed while the replacement was prepared. Nothing was overwritten.", destination.display()));
    }
    if let Err(error) = replace_with_staged_file(&staged, &destination) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    Ok(LocalCopyResult { destination: destination.to_string_lossy().into_owned(), files: 1, directories: 0, bytes })
}

#[tauri::command]
pub async fn replace_local_merge_file(left_root: String, right_root: String, relative_path: String, expected_right_fingerprint: String) -> Result<LocalCopyResult, String> {
    super::off_main_thread(move || replace_local_merge_file_inner(left_root, right_root, relative_path, expected_right_fingerprint)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn roots(label: &str) -> (PathBuf, PathBuf, PathBuf) {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("git-drilldown-folder-merge-{label}-{suffix}"));
        let left = root.join("left");
        let right = root.join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        (root, left, right)
    }

    #[test]
    fn folder_comparison_classifies_same_modified_and_one_sided_files() {
        let (root, left, right) = roots("compare");
        fs::write(left.join("same.txt"), "same").unwrap();
        fs::write(right.join("same.txt"), "same").unwrap();
        fs::write(left.join("changed.txt"), "from left").unwrap();
        fs::write(right.join("changed.txt"), "on right").unwrap();
        fs::write(left.join("new.txt"), "new").unwrap();
        fs::write(right.join("keep.txt"), "keep").unwrap();
        let result = compare_local_directories_inner(left.to_string_lossy().into_owned(), right.to_string_lossy().into_owned()).unwrap();
        assert_eq!(result.counts.same, 1);
        assert_eq!(result.counts.modified, 1);
        assert_eq!(result.counts.left_only, 1);
        assert_eq!(result.counts.right_only, 1);
        assert!(result.entries.iter().find(|item| item.relative_path == "changed.txt").unwrap().reviewable);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn guided_copy_creates_a_new_nested_file_but_never_overwrites_it() {
        let (root, left, right) = roots("copy");
        fs::create_dir_all(left.join("nested")).unwrap();
        fs::write(left.join("nested/new.txt"), "source").unwrap();
        let result = copy_local_merge_file_inner(left.to_string_lossy().into_owned(), right.to_string_lossy().into_owned(), "nested/new.txt".into()).unwrap();
        assert_eq!(result.files, 1);
        assert_eq!(fs::read_to_string(right.join("nested/new.txt")).unwrap(), "source");
        fs::write(left.join("nested/new.txt"), "newer source").unwrap();
        assert!(copy_local_merge_file_inner(left.to_string_lossy().into_owned(), right.to_string_lossy().into_owned(), "nested/new.txt".into()).is_err());
        assert_eq!(fs::read_to_string(right.join("nested/new.txt")).unwrap(), "source");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn guided_replace_has_a_stale_destination_guard() {
        let (root, left, right) = roots("replace");
        fs::write(left.join("file.bin"), b"left").unwrap();
        fs::write(right.join("file.bin"), b"right").unwrap();
        let expected = fingerprint_file(&right.join("file.bin")).unwrap();
        fs::write(right.join("file.bin"), b"external").unwrap();
        let result = replace_local_merge_file_inner(
            left.to_string_lossy().into_owned(), right.to_string_lossy().into_owned(), "file.bin".into(), expected,
        );
        assert!(result.unwrap_err().contains("changed after the folder scan"));
        assert_eq!(fs::read(right.join("file.bin")).unwrap(), b"external");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn guided_replace_installs_the_exact_left_file_after_a_fresh_scan() {
        let (root, left, right) = roots("replace-success");
        fs::write(left.join("file.bin"), b"left replacement").unwrap();
        fs::write(right.join("file.bin"), b"old right").unwrap();
        let expected = fingerprint_file(&right.join("file.bin")).unwrap();
        let result = replace_local_merge_file_inner(
            left.to_string_lossy().into_owned(), right.to_string_lossy().into_owned(), "file.bin".into(), expected,
        ).unwrap();
        assert_eq!(result.files, 1);
        assert_eq!(fs::read(right.join("file.bin")).unwrap(), b"left replacement");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_comparison_never_enters_git_metadata() {
        let (root, left, right) = roots("git-metadata");
        fs::create_dir_all(left.join(".git/objects")).unwrap();
        fs::create_dir_all(right.join(".git/objects")).unwrap();
        fs::write(left.join(".git/objects/left-secret"), "left").unwrap();
        fs::write(right.join(".git/objects/right-secret"), "right").unwrap();
        fs::write(left.join("visible.txt"), "same").unwrap();
        fs::write(right.join("visible.txt"), "same").unwrap();
        let result = compare_local_directories_inner(left.to_string_lossy().into_owned(), right.to_string_lossy().into_owned()).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].relative_path, "visible.txt");
        fs::remove_dir_all(root).unwrap();
    }
}
