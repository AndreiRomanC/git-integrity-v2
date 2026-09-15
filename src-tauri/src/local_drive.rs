use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Serialize)]
pub struct LocalDriveEntry {
    name: String,
    path: String,
    kind: String,
    size: u64,
    modified: u64,
}

#[derive(Serialize)]
pub struct LocalDriveDirectory {
    path: String,
    parent: Option<String>,
    entries: Vec<LocalDriveEntry>,
}

#[derive(Debug, Serialize)]
pub struct LocalCopyResult {
    destination: String,
    files: u64,
    directories: u64,
    bytes: u64,
}

#[derive(Serialize)]
pub struct LocalTextFile {
    path: String,
    content: String,
    bytes: u64,
    encoding: String,
}

const MAX_LOCAL_TEXT_BYTES: u64 = 2 * 1024 * 1024;

async fn off_main_thread<T: Send + 'static>(body: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    match tauri::async_runtime::spawn_blocking(body).await {
        Ok(result) => result,
        Err(error) => Err(format!("Local Drive operation failed internally: {error}")),
    }
}

fn require_absolute(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() { return Err("Choose a local folder first".into()); }
    let value = PathBuf::from(trimmed);
    if !value.is_absolute() { return Err("Local Drive accepts only absolute paths".into()); }
    Ok(value)
}

fn existing_directory(path: &str) -> Result<PathBuf, String> {
    let value = require_absolute(path)?;
    let canonical = fs::canonicalize(&value).map_err(|error| format!("Cannot open '{}': {error}", value.display()))?;
    if !canonical.is_dir() { return Err(format!("'{}' is not a folder", value.display())); }
    Ok(canonical)
}

fn contains_git_metadata(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => name.to_string_lossy().eq_ignore_ascii_case(".git"),
        _ => false,
    })
}

fn safe_new_folder_name(name: &str) -> Result<&str, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() { return Err("Enter a folder name".into()); }
    if trimmed.contains('/') || trimmed.contains('\\') { return Err("Enter one folder name, without a path".into()); }
    let mut components = Path::new(trimmed).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err("Enter one folder name, without a path".into());
    }
    if trimmed == "." || trimmed == ".." || trimmed.eq_ignore_ascii_case(".git") {
        return Err("That folder name is protected".into());
    }
    Ok(trimmed)
}

fn list_local_directory_inner(path: String) -> Result<LocalDriveDirectory, String> {
    let requested = require_absolute(&path)?;
    existing_directory(&path)?;
    let mut entries = Vec::new();
    for item in fs::read_dir(&requested).map_err(|error| format!("Cannot read '{}': {error}", requested.display()))? {
        let item = item.map_err(|error| error.to_string())?;
        let item_path = item.path();
        let metadata = fs::symlink_metadata(&item_path).map_err(|error| error.to_string())?;
        let kind = if metadata.file_type().is_symlink() { "symlink" } else if metadata.is_dir() { "folder" } else { "file" };
        let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|value| value.as_secs()).unwrap_or(0);
        entries.push(LocalDriveEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            path: item_path.to_string_lossy().into_owned(),
            kind: kind.into(),
            size: if metadata.is_file() { metadata.len() } else { 0 },
            modified,
        });
    }
    entries.sort_by_cached_key(|entry| (entry.kind != "folder", entry.name.to_lowercase()));
    Ok(LocalDriveDirectory {
        path: requested.to_string_lossy().into_owned(),
        parent: requested.parent().filter(|parent| parent != &requested).map(|parent| parent.to_string_lossy().into_owned()),
        entries,
    })
}

#[tauri::command]
pub async fn list_local_directory(path: String) -> Result<LocalDriveDirectory, String> {
    off_main_thread(move || list_local_directory_inner(path)).await
}

fn create_local_directory_inner(parent_path: String, name: String) -> Result<String, String> {
    let parent = existing_directory(&parent_path)?;
    let name = safe_new_folder_name(&name)?;
    let destination = parent.join(name);
    fs::create_dir(&destination).map_err(|error| {
        if destination.exists() { format!("'{}' already exists", destination.display()) }
        else { format!("Cannot create '{}': {error}", destination.display()) }
    })?;
    Ok(destination.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn create_local_directory(parent_path: String, name: String) -> Result<String, String> {
    off_main_thread(move || create_local_directory_inner(parent_path, name)).await
}

fn copy_file_new(source: &Path, destination: &Path) -> Result<u64, String> {
    let input = fs::File::open(source).map_err(|error| format!("Cannot read '{}': {error}", source.display()))?;
    let output = OpenOptions::new().write(true).create_new(true).open(destination)
        .map_err(|error| if destination.exists() { format!("'{}' already exists — nothing was overwritten", destination.display()) } else { format!("Cannot create '{}': {error}", destination.display()) })?;
    let mut input = BufReader::new(input);
    let mut output = BufWriter::new(output);
    match std::io::copy(&mut input, &mut output) {
        Ok(bytes) => {
            if let Err(error) = output.flush() {
                drop(output);
                let _ = fs::remove_file(destination);
                return Err(format!("Copy failed while finishing '{}': {error}", source.display()));
            }
            if let Ok(metadata) = fs::metadata(source) { let _ = fs::set_permissions(destination, metadata.permissions()); }
            Ok(bytes)
        }
        Err(error) => {
            let _ = fs::remove_file(destination);
            Err(format!("Copy failed for '{}': {error}", source.display()))
        }
    }
}

fn copy_directory_new(source: &Path, destination: &Path, result: &mut LocalCopyResult) -> Result<(), String> {
    fs::create_dir(destination).map_err(|error| if destination.exists() { format!("'{}' already exists — nothing was overwritten", destination.display()) } else { format!("Cannot create '{}': {error}", destination.display()) })?;
    result.directories += 1;
    let operation = (|| {
        for item in fs::read_dir(source).map_err(|error| format!("Cannot read '{}': {error}", source.display()))? {
            let item = item.map_err(|error| error.to_string())?;
            let source_item = item.path();
            let destination_item = destination.join(item.file_name());
            let metadata = fs::symlink_metadata(&source_item).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err(format!("Copy stopped safely at symbolic link '{}'. Symbolic-link copying is not enabled.", source_item.display()));
            }
            if metadata.is_dir() {
                copy_directory_new(&source_item, &destination_item, result)?;
            } else if metadata.is_file() {
                result.bytes += copy_file_new(&source_item, &destination_item)?;
                result.files += 1;
            } else {
                return Err(format!("Copy stopped safely at unsupported item '{}'.", source_item.display()));
            }
        }
        if let Ok(metadata) = fs::metadata(source) { let _ = fs::set_permissions(destination, metadata.permissions()); }
        Ok(())
    })();
    if operation.is_err() { let _ = fs::remove_dir_all(destination); }
    operation
}

fn copy_local_item_inner(source_path: String, destination_directory: String) -> Result<LocalCopyResult, String> {
    let source_requested = require_absolute(&source_path)?;
    let source_metadata = fs::symlink_metadata(&source_requested).map_err(|error| format!("Cannot open '{}': {error}", source_requested.display()))?;
    if source_metadata.file_type().is_symlink() { return Err("Symbolic-link copying is not enabled because following it could copy an unexpected location".into()); }
    if contains_git_metadata(&source_requested) { return Err("Git metadata (.git) is protected in Local Drive".into()); }
    let source = fs::canonicalize(&source_requested).map_err(|error| error.to_string())?;
    let destination_directory = existing_directory(&destination_directory)?;
    if source_metadata.is_dir() && destination_directory.starts_with(&source) {
        return Err("A folder cannot be copied into itself or one of its descendants".into());
    }
    let name = source.file_name().ok_or("A filesystem root cannot be copied as one item")?;
    let destination = destination_directory.join(name);
    if destination.exists() { return Err(format!("'{}' already exists — nothing was overwritten", destination.display())); }
    let mut result = LocalCopyResult { destination: destination.to_string_lossy().into_owned(), files: 0, directories: 0, bytes: 0 };
    if source_metadata.is_dir() {
        copy_directory_new(&source, &destination, &mut result)?;
    } else if source_metadata.is_file() {
        result.bytes = copy_file_new(&source, &destination)?;
        result.files = 1;
    } else {
        return Err("This item type cannot be copied safely".into());
    }
    Ok(result)
}

#[tauri::command]
pub async fn copy_local_item(source_path: String, destination_directory: String) -> Result<LocalCopyResult, String> {
    off_main_thread(move || copy_local_item_inner(source_path, destination_directory)).await
}

fn move_local_item_with(source_path: String, destination_directory: String, deleter: impl FnOnce(&Path) -> Result<(), String>) -> Result<LocalCopyResult, String> {
    let source = require_absolute(&source_path)?;
    if source.file_name().is_none() { return Err("A filesystem root cannot be moved".into()); }
    if contains_git_metadata(&source) { return Err("Git metadata (.git) is protected in Local Drive".into()); }
    if fs::symlink_metadata(&source).map_err(|error| format!("Cannot open '{}': {error}", source.display()))?.file_type().is_symlink() {
        return Err("Symbolic-link moving is not enabled because its target may be unexpected".into());
    }
    // Copy uses exclusive creation for every destination and therefore cannot
    // overwrite anything. Only after that complete copy succeeds do we move
    // the original to Trash. If Trash fails, both safe copies remain and the
    // UI reports that explicitly instead of risking data loss.
    let result = copy_local_item_inner(source_path, destination_directory)?;
    deleter(&source).map_err(|error| format!("The copy completed, but the original could not be moved to Trash. Both copies were kept safely. {error}"))?;
    Ok(result)
}

fn move_local_item_inner(source_path: String, destination_directory: String) -> Result<LocalCopyResult, String> {
    move_local_item_with(source_path, destination_directory, |target| {
        trash::delete(target).map_err(|error| format!("Could not move '{}' to Trash/Recycle Bin: {error}", target.display()))
    })
}

#[tauri::command]
pub async fn move_local_item(source_path: String, destination_directory: String) -> Result<LocalCopyResult, String> {
    off_main_thread(move || move_local_item_inner(source_path, destination_directory)).await
}

fn validate_trash_target(path: &str) -> Result<PathBuf, String> {
    let value = require_absolute(path)?;
    if value.parent().is_none() || value.file_name().is_none() { return Err("A filesystem root cannot be deleted".into()); }
    if contains_git_metadata(&value) { return Err("Git metadata (.git) is protected in Local Drive".into()); }
    fs::symlink_metadata(&value).map_err(|error| format!("Cannot open '{}': {error}", value.display()))?;
    Ok(value)
}

fn trash_local_item_with(path: String, deleter: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    let target = validate_trash_target(&path)?;
    deleter(&target)
}

fn trash_local_item_inner(path: String) -> Result<(), String> {
    trash_local_item_with(path, |target| trash::delete(target).map_err(|error| format!("Could not move '{}' to Trash/Recycle Bin: {error}", target.display())))
}

#[tauri::command]
pub async fn trash_local_item(path: String) -> Result<(), String> {
    off_main_thread(move || trash_local_item_inner(path)).await
}

fn existing_editable_file(path: &str) -> Result<(PathBuf, fs::Metadata), String> {
    let value = require_absolute(path)?;
    if contains_git_metadata(&value) { return Err("Git metadata (.git) is protected in Local Drive".into()); }
    let metadata = fs::symlink_metadata(&value).map_err(|error| format!("Cannot open '{}': {error}", value.display()))?;
    if metadata.file_type().is_symlink() { return Err("Symbolic links cannot be viewed or edited here".into()); }
    if !metadata.is_file() { return Err("Select a regular file first".into()); }
    if metadata.len() > MAX_LOCAL_TEXT_BYTES { return Err("The built-in viewer/editor supports text files up to 2 MB".into()); }
    Ok((value, metadata))
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, String> {
    if bytes.len() % 2 != 0 { return Err("The selected UTF-16 file has an incomplete final character".into()); }
    let units = bytes.chunks_exact(2).map(|pair| if little_endian {
        u16::from_le_bytes([pair[0], pair[1]])
    } else {
        u16::from_be_bytes([pair[0], pair[1]])
    }).collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| "The selected file contains invalid UTF-16 text".into())
}

fn cp1252_special(byte: u8) -> Option<char> {
    Some(match byte {
        0x80 => '€', 0x82 => '‚', 0x83 => 'ƒ', 0x84 => '„', 0x85 => '…', 0x86 => '†', 0x87 => '‡',
        0x88 => 'ˆ', 0x89 => '‰', 0x8a => 'Š', 0x8b => '‹', 0x8c => 'Œ', 0x8e => 'Ž', 0x91 => '‘',
        0x92 => '’', 0x93 => '“', 0x94 => '”', 0x95 => '•', 0x96 => '–', 0x97 => '—', 0x98 => '˜',
        0x99 => '™', 0x9a => 'š', 0x9b => '›', 0x9c => 'œ', 0x9e => 'ž', 0x9f => 'Ÿ',
        _ => return None,
    })
}

fn decode_windows_1252(bytes: &[u8]) -> Result<String, String> {
    let suspicious = bytes.iter().copied().filter(|byte| *byte < 0x09 || (0x0e..=0x1f).contains(byte)).count();
    if bytes.contains(&0) || suspicious > 0 {
        return Err("The selected file appears to be binary and cannot be opened in the built-in text viewer".into());
    }
    bytes.iter().map(|byte| match *byte {
        0x00..=0x7f => Ok(char::from(*byte)),
        0x80..=0x9f => cp1252_special(*byte).ok_or_else(|| "The selected file appears to contain binary or unsupported control bytes".to_string()),
        _ => char::from_u32(*byte as u32).ok_or_else(|| "The selected file is not supported text".to_string()),
    }).collect()
}

fn decode_local_text(bytes: Vec<u8>) -> Result<(String, &'static str), String> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        if bytes[3..].contains(&0) || bytes[3..].iter().any(|byte| *byte < 0x09 || (0x0e..=0x1f).contains(byte)) {
            return Err("The selected file appears to be binary and cannot be opened in the built-in text viewer".into());
        }
        return String::from_utf8(bytes[3..].to_vec()).map(|content| (content, "utf-8-bom"))
            .map_err(|_| "The selected file has an invalid UTF-8 byte-order mark".into());
    }
    if bytes.starts_with(&[0xff, 0xfe]) { return decode_utf16(&bytes[2..], true).map(|content| (content, "utf-16le")); }
    if bytes.starts_with(&[0xfe, 0xff]) { return decode_utf16(&bytes[2..], false).map(|content| (content, "utf-16be")); }

    // Some generated/source files omit the UTF-16 BOM. Only accept that case
    // when zero bytes strongly and consistently identify the byte order.
    if bytes.len() >= 4 && bytes.len() % 2 == 0 {
        let even_zeroes = bytes.iter().step_by(2).filter(|byte| **byte == 0).count();
        let odd_zeroes = bytes.iter().skip(1).step_by(2).filter(|byte| **byte == 0).count();
        let threshold = (bytes.len() / 2).saturating_div(4).max(1);
        if odd_zeroes >= threshold && odd_zeroes > even_zeroes.saturating_mul(2) {
            return decode_utf16(&bytes, true).map(|content| (content, "utf-16le-no-bom"));
        }
        if even_zeroes >= threshold && even_zeroes > odd_zeroes.saturating_mul(2) {
            return decode_utf16(&bytes, false).map(|content| (content, "utf-16be-no-bom"));
        }
    }
    if bytes.contains(&0) || bytes.iter().any(|byte| *byte < 0x09 || (0x0e..=0x1f).contains(byte)) {
        return Err("The selected file appears to be binary and cannot be opened in the built-in text viewer".into());
    }
    if let Ok(content) = String::from_utf8(bytes.clone()) { return Ok((content, "utf-8")); }
    decode_windows_1252(&bytes).map(|content| (content, "windows-1252"))
}

fn cp1252_byte(character: char) -> Option<u8> {
    Some(match character {
        '\u{0000}'..='\u{007f}' => character as u8,
        '\u{00a0}'..='\u{00ff}' => character as u8,
        '€' => 0x80, '‚' => 0x82, 'ƒ' => 0x83, '„' => 0x84, '…' => 0x85, '†' => 0x86, '‡' => 0x87,
        'ˆ' => 0x88, '‰' => 0x89, 'Š' => 0x8a, '‹' => 0x8b, 'Œ' => 0x8c, 'Ž' => 0x8e, '‘' => 0x91,
        '’' => 0x92, '“' => 0x93, '”' => 0x94, '•' => 0x95, '–' => 0x96, '—' => 0x97, '˜' => 0x98,
        '™' => 0x99, 'š' => 0x9a, '›' => 0x9b, 'œ' => 0x9c, 'ž' => 0x9e, 'Ÿ' => 0x9f,
        _ => return None,
    })
}

fn encode_local_text(content: &str, encoding: &str) -> Result<Vec<u8>, String> {
    match encoding {
        "utf-8" => Ok(content.as_bytes().to_vec()),
        "utf-8-bom" => Ok([&[0xef, 0xbb, 0xbf][..], content.as_bytes()].concat()),
        "utf-16le" | "utf-16le-no-bom" => {
            let mut bytes = if encoding == "utf-16le" { vec![0xff, 0xfe] } else { Vec::new() };
            for unit in content.encode_utf16() { bytes.extend_from_slice(&unit.to_le_bytes()); }
            Ok(bytes)
        }
        "utf-16be" | "utf-16be-no-bom" => {
            let mut bytes = if encoding == "utf-16be" { vec![0xfe, 0xff] } else { Vec::new() };
            for unit in content.encode_utf16() { bytes.extend_from_slice(&unit.to_be_bytes()); }
            Ok(bytes)
        }
        "windows-1252" => content.chars().map(|character| cp1252_byte(character).ok_or_else(|| {
            format!("The character '{character}' cannot be represented in the file's original Windows-1252 encoding")
        })).collect(),
        _ => Err(format!("Unsupported text encoding '{encoding}'")),
    }
}

fn read_local_text_file_inner(path: String) -> Result<LocalTextFile, String> {
    let (value, metadata) = existing_editable_file(&path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(&value)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| format!("Cannot read '{}': {error}", value.display()))?;
    let (content, encoding) = decode_local_text(bytes)?;
    Ok(LocalTextFile { path: value.to_string_lossy().into_owned(), bytes: metadata.len(), content, encoding: encoding.into() })
}

#[tauri::command]
pub async fn read_local_text_file(path: String) -> Result<LocalTextFile, String> {
    off_main_thread(move || read_local_text_file_inner(path)).await
}

fn write_local_text_file_inner(path: String, content: String, encoding: Option<String>) -> Result<(), String> {
    let (value, _) = existing_editable_file(&path)?;
    let bytes = encode_local_text(&content, encoding.as_deref().unwrap_or("utf-8"))?;
    if bytes.len() as u64 > MAX_LOCAL_TEXT_BYTES { return Err("The built-in viewer/editor supports text files up to 2 MB".into()); }
    let mut file = OpenOptions::new().write(true).truncate(true).open(&value)
        .map_err(|error| format!("Cannot edit '{}': {error}", value.display()))?;
    file.write_all(&bytes).and_then(|_| file.flush())
        .map_err(|error| format!("Could not finish saving '{}': {error}", value.display()))
}

#[tauri::command]
pub async fn write_local_text_file(path: String, content: String, encoding: Option<String>) -> Result<(), String> {
    off_main_thread(move || write_local_text_file_inner(path, content, encoding)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    fn temp_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("git-drilldown-local-drive-{label}-{suffix}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn lists_folders_before_files_and_keeps_absolute_paths() {
        let root = temp_dir("list");
        fs::write(root.join("z.txt"), "z").unwrap();
        fs::create_dir(root.join("a-folder")).unwrap();
        let result = list_local_directory_inner(root.to_string_lossy().into_owned()).unwrap();
        assert_eq!(result.entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), vec!["a-folder", "z.txt"]);
        assert!(result.entries.iter().all(|entry| Path::new(&entry.path).is_absolute()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creates_only_one_new_folder_and_never_accepts_a_path_as_its_name() {
        let root = temp_dir("mkdir");
        let root_string = root.to_string_lossy().into_owned();
        let created = create_local_directory_inner(root_string.clone(), "new folder".into()).unwrap();
        assert!(Path::new(&created).is_dir());
        assert!(create_local_directory_inner(root_string.clone(), "nested/folder".into()).is_err());
        assert!(create_local_directory_inner(root_string, ".git".into()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copies_a_tree_without_overwriting_an_existing_destination() {
        let root = temp_dir("copy");
        let source_parent = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(source_parent.join("folder/nested")).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source_parent.join("folder/a.txt"), "alpha").unwrap();
        fs::write(source_parent.join("folder/nested/b.txt"), "beta").unwrap();
        let result = copy_local_item_inner(source_parent.join("folder").to_string_lossy().into_owned(), destination.to_string_lossy().into_owned()).unwrap();
        assert_eq!(result.files, 2);
        assert_eq!(fs::read_to_string(destination.join("folder/nested/b.txt")).unwrap(), "beta");
        fs::write(destination.join("folder/a.txt"), "keep me").unwrap();
        assert!(copy_local_item_inner(source_parent.join("folder").to_string_lossy().into_owned(), destination.to_string_lossy().into_owned()).is_err());
        assert_eq!(fs::read_to_string(destination.join("folder/a.txt")).unwrap(), "keep me");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_copying_a_folder_into_itself() {
        let root = temp_dir("self-copy");
        let child = root.join("child");
        fs::create_dir(&child).unwrap();
        let result = copy_local_item_inner(root.to_string_lossy().into_owned(), child.to_string_lossy().into_owned());
        assert!(result.unwrap_err().contains("cannot be copied into itself"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn trash_validation_protects_roots_and_git_metadata_and_passes_the_exact_item_to_the_deleter() {
        let root = temp_dir("trash");
        let file = root.join("ordinary.txt");
        fs::write(&file, "keep until mock accepts it").unwrap();
        let observed = Arc::new(Mutex::new(None));
        let observed_in = Arc::clone(&observed);
        trash_local_item_with(file.to_string_lossy().into_owned(), move |target| {
            *observed_in.lock().unwrap() = Some(target.to_path_buf());
            Ok(())
        }).unwrap();
        assert_eq!(observed.lock().unwrap().as_ref(), Some(&file));
        assert!(file.exists(), "the mock test must never put a real file in the user's Trash");
        let git_dir = root.join(".git");
        fs::create_dir(&git_dir).unwrap();
        assert!(validate_trash_target(&git_dir.to_string_lossy()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_text_view_and_edit_support_common_windows_encodings_without_converting_them() {
        let root = temp_dir("text-editor");
        let file = root.join("notes.txt");
        fs::write(&file, "before").unwrap();
        let path = file.to_string_lossy().into_owned();
        let opened = read_local_text_file_inner(path.clone()).unwrap();
        assert_eq!(opened.content, "before");
        assert_eq!(opened.encoding, "utf-8");
        write_local_text_file_inner(path.clone(), "after".into(), Some(opened.encoding)).unwrap();
        assert_eq!(read_local_text_file_inner(path).unwrap().content, "after");

        let utf16 = root.join("windows-utf16.txt");
        fs::write(&utf16, [0xff, 0xfe, b'A', 0, b'B', 0]).unwrap();
        let utf16_path = utf16.to_string_lossy().into_owned();
        let opened = read_local_text_file_inner(utf16_path.clone()).unwrap();
        assert_eq!(opened.content, "AB");
        assert_eq!(opened.encoding, "utf-16le");
        write_local_text_file_inner(utf16_path, "CD".into(), Some(opened.encoding)).unwrap();
        assert_eq!(fs::read(&utf16).unwrap(), [0xff, 0xfe, b'C', 0, b'D', 0]);

        let ansi = root.join("windows-ansi.txt");
        fs::write(&ansi, [b'c', b'a', b'f', 0xe9]).unwrap();
        let ansi_path = ansi.to_string_lossy().into_owned();
        let opened = read_local_text_file_inner(ansi_path.clone()).unwrap();
        assert_eq!(opened.content, "café");
        assert_eq!(opened.encoding, "windows-1252");
        write_local_text_file_inner(ansi_path, "déjà".into(), Some(opened.encoding)).unwrap();
        assert_eq!(fs::read(&ansi).unwrap(), [b'd', 0xe9, b'j', 0xe0]);

        let binary = root.join("binary.dat");
        fs::write(&binary, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).unwrap();
        assert!(read_local_text_file_inner(binary.to_string_lossy().into_owned()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn move_copies_without_overwrite_before_removing_the_original() {
        let root = temp_dir("move");
        let source_parent = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source_parent).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let source = source_parent.join("move-me.txt");
        fs::write(&source, "content").unwrap();
        let result = move_local_item_with(
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
            |target| fs::remove_file(target).map_err(|error| error.to_string()),
        ).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(result.destination).unwrap(), "content");
        fs::remove_dir_all(root).unwrap();
    }
}
