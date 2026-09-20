use git2::Repository;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
struct NotesFile {
    version: u32,
    notes: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct DrillDownNotes {
    notes: HashMap<String, String>,
    error: Option<String>,
}

fn normalized_folder_path(path: &str) -> Result<String, String> {
    let cleaned = path.trim().replace('\\', "/").trim_matches('/').to_string();
    if cleaned.is_empty() {
        return Err("Select a folder before adding a personal note".into());
    }
    if cleaned.contains(':') {
        return Err("Personal note paths must be repository-relative".into());
    }
    let mut parts = Vec::new();
    for part in cleaned.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("Personal note paths cannot leave the repository".into()),
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        return Err("Select a folder before adding a personal note".into());
    }
    Ok(parts.join("/"))
}

fn first_remote_url(repo: &Repository) -> Option<String> {
    repo.find_remote("origin")
        .ok()
        .or_else(|| {
            repo.remotes().ok().and_then(|names| {
                names
                    .iter()
                    .flatten()
                    .next()
                    .and_then(|name| repo.find_remote(name).ok())
            })
        })
        .and_then(|remote| remote.url().map(str::to_string))
}

fn repository_identity(repository_path: &str) -> Result<String, String> {
    let repo = Repository::open(repository_path).map_err(|error| error.message().to_string())?;
    if let Some(remote) = first_remote_url(&repo).filter(|url| !url.trim().is_empty()) {
        return Ok(format!("remote:{}", remote.trim()));
    }
    let canonical = fs::canonicalize(repository_path).map_err(|error| error.to_string())?;
    Ok(format!("path:{}", canonical.to_string_lossy()))
}

// FNV-1a 64-bit. The notes file name is derived from this, so it must never
// change between builds or platforms. std's DefaultHasher is explicitly not
// stable across Rust releases and would silently orphan every saved note.
fn stable_hash(value: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn app_data_root() -> PathBuf {
    if let Some(override_path) = std::env::var_os("GIT_DRILLDOWN_NOTES_DIR") {
        return PathBuf::from(override_path);
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("Git DrillDown").join("notes");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Git DrillDown")
                .join("notes");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(data_home).join("Git DrillDown").join("notes");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("Git DrillDown")
                .join("notes");
        }
    }
    std::env::temp_dir().join("Git DrillDown").join("notes")
}

fn notes_path_for_repository(repository_path: &str) -> Result<PathBuf, String> {
    let identity = repository_identity(repository_path)?;
    Ok(app_data_root().join(format!(
        "{}.drill-down-notes.json",
        stable_hash(&identity)
    )))
}

fn read_notes_file(path: &Path) -> Result<HashMap<String, String>, String> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let parsed: NotesFile = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    Ok(parsed
        .notes
        .into_iter()
        .filter_map(|(path, note)| {
            let normalized = normalized_folder_path(&path).ok()?;
            let trimmed = note.trim();
            (!trimmed.is_empty()).then(|| (normalized, trimmed.to_string()))
        })
        .collect())
}

fn write_notes_file(path: &Path, notes: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if notes.is_empty() {
        if path.exists() {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
        return Ok(());
    }
    let payload = NotesFile {
        version: 1,
        notes: notes.clone(),
    };
    let text = serde_json::to_string_pretty(&payload).map_err(|error| error.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|error| error.to_string())?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(first_error) if path.exists() => {
            fs::remove_file(path).map_err(|error| format!("{first_error}; cleanup failed: {error}"))?;
            fs::rename(&tmp, path).map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn ensure_existing_folder(repository_path: &str, relative_path: &str) -> Result<String, String> {
    let normalized = normalized_folder_path(relative_path)?;
    let absolute = Path::new(repository_path).join(&normalized);
    let metadata = fs::symlink_metadata(&absolute).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err("Personal notes are available for folders only".into());
    }
    for component in Path::new(&normalized).components() {
        if matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_)) {
            return Err("Personal note paths must be repository-relative".into());
        }
    }
    Ok(normalized)
}

#[tauri::command]
pub fn load_drill_down_notes(repository_path: String) -> Result<DrillDownNotes, String> {
    let path = notes_path_for_repository(&repository_path)?;
    match read_notes_file(&path) {
        Ok(notes) => Ok(DrillDownNotes { notes, error: None }),
        Err(error) => Ok(DrillDownNotes {
            notes: HashMap::new(),
            error: Some(format!("Personal notes could not be loaded: {error}")),
        }),
    }
}

#[tauri::command]
pub fn set_drill_down_note(
    repository_path: String,
    relative_path: String,
    note: String,
) -> Result<DrillDownNotes, String> {
    let path_key = ensure_existing_folder(&repository_path, &relative_path)?;
    let file_path = notes_path_for_repository(&repository_path)?;
    let mut notes = read_notes_file(&file_path).unwrap_or_default();
    let trimmed = note.trim();
    if trimmed.is_empty() {
        notes.remove(&path_key);
    } else {
        notes.insert(path_key, trimmed.to_string());
    }
    write_notes_file(&file_path, &notes)?;
    Ok(DrillDownNotes { notes, error: None })
}

#[tauri::command]
pub fn delete_drill_down_note(
    repository_path: String,
    relative_path: String,
) -> Result<DrillDownNotes, String> {
    let path_key = normalized_folder_path(&relative_path)?;
    let file_path = notes_path_for_repository(&repository_path)?;
    let mut notes = read_notes_file(&file_path).unwrap_or_default();
    notes.remove(&path_key);
    write_notes_file(&file_path, &notes)?;
    Ok(DrillDownNotes { notes, error: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NOTES_TEST_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn temp_root(label: &str) -> PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("git-drilldown-notes-{label}-{suffix}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn repo_at(root: &Path, name: &str, remote: Option<&str>) -> PathBuf {
        let repo = root.join(name);
        fs::create_dir_all(repo.join("work/a")).unwrap();
        fs::create_dir_all(repo.join("work/b")).unwrap();
        Repository::init(&repo).unwrap();
        if let Some(url) = remote {
            let repository = Repository::open(&repo).unwrap();
            repository.remote("origin", url).unwrap();
        }
        repo
    }

    fn with_notes_dir<T>(label: &str, run: impl FnOnce(PathBuf) -> T) -> T {
        let _guard = NOTES_TEST_ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let root = temp_root(label);
        std::env::set_var("GIT_DRILLDOWN_NOTES_DIR", &root);
        let value = run(root.clone());
        std::env::remove_var("GIT_DRILLDOWN_NOTES_DIR");
        value
    }

    #[test]
    fn repository_without_notes_loads_empty_without_creating_a_file() {
        with_notes_dir("empty", |notes_root| {
            let root = temp_root("empty-repo");
            let repo = repo_at(&root, "repo", Some("https://example.test/org/repo.git"));
            let loaded = load_drill_down_notes(repo.to_string_lossy().into_owned()).unwrap();
            assert!(loaded.notes.is_empty());
            assert!(loaded.error.is_none());
            assert!(fs::read_dir(notes_root).unwrap().next().is_none());
        });
    }

    #[test]
    fn add_edit_delete_and_multiple_folder_notes_round_trip() {
        with_notes_dir("roundtrip", |_| {
            let root = temp_root("roundtrip-repo");
            let repo = repo_at(&root, "repo", Some("https://example.test/org/repo.git"));
            let repo_string = repo.to_string_lossy().into_owned();
            let added = set_drill_down_note(repo_string.clone(), "work/a".into(), "First".into()).unwrap();
            assert_eq!(added.notes.get("work/a").map(String::as_str), Some("First"));
            let edited = set_drill_down_note(repo_string.clone(), "work\\a\\".into(), "Second".into()).unwrap();
            assert_eq!(edited.notes.get("work/a").map(String::as_str), Some("Second"));
            let multiple = set_drill_down_note(repo_string.clone(), "work/b".into(), "Other".into()).unwrap();
            assert_eq!(multiple.notes.len(), 2);
            assert!(!multiple.notes.contains_key("work/missing"));
            let deleted = delete_drill_down_note(repo_string.clone(), "work/a".into()).unwrap();
            assert!(!deleted.notes.contains_key("work/a"));
            assert_eq!(deleted.notes.get("work/b").map(String::as_str), Some("Other"));
        });
    }

    #[test]
    fn malformed_json_is_reported_without_breaking_load() {
        with_notes_dir("malformed", |_| {
            let root = temp_root("malformed-repo");
            let repo = repo_at(&root, "repo", Some("https://example.test/org/repo.git"));
            let path = notes_path_for_repository(&repo.to_string_lossy()).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "{ not json").unwrap();
            let loaded = load_drill_down_notes(repo.to_string_lossy().into_owned()).unwrap();
            assert!(loaded.notes.is_empty());
            assert!(loaded.error.unwrap().contains("could not be loaded"));
        });
    }

    #[test]
    fn notes_file_name_hash_is_pinned_so_saved_notes_survive_toolchain_updates() {
        // Published FNV-1a 64 test vectors. If these ever change, every user's
        // existing notes file would be orphaned.
        assert_eq!(stable_hash(""), "cbf29ce484222325");
        assert_eq!(stable_hash("a"), "af63dc4c8601ec8c");
        assert_eq!(stable_hash("foobar"), "85944171f73967e8");
    }

    #[test]
    fn paths_are_normalized_across_platform_separators() {
        assert_eq!(normalized_folder_path("work\\a\\").unwrap(), "work/a");
        assert_eq!(normalized_folder_path("./work//a").unwrap(), "work/a");
        assert!(normalized_folder_path("../outside").is_err());
        assert!(normalized_folder_path("C:/outside").is_err());
    }

    #[test]
    fn repository_identity_can_follow_a_moved_clone_when_remote_matches() {
        with_notes_dir("moved", |_| {
            let root = temp_root("moved-repo");
            let first = repo_at(&root, "first", Some("https://example.test/org/repo.git"));
            let second = repo_at(&root, "second", Some("https://example.test/org/repo.git"));
            set_drill_down_note(first.to_string_lossy().into_owned(), "work/a".into(), "Shared by remote".into()).unwrap();
            let loaded = load_drill_down_notes(second.to_string_lossy().into_owned()).unwrap();
            assert_eq!(loaded.notes.get("work/a").map(String::as_str), Some("Shared by remote"));
        });
    }

    #[test]
    fn large_tree_lookup_is_plain_hashmap_work_after_load() {
        with_notes_dir("large", |_| {
            let root = temp_root("large-repo");
            let repo = repo_at(&root, "repo", Some("https://example.test/org/repo.git"));
            let repo_string = repo.to_string_lossy().into_owned();
            for index in 0..100 {
                let folder = repo.join(format!("work/a/f{index:03}"));
                fs::create_dir_all(&folder).unwrap();
                set_drill_down_note(repo_string.clone(), format!("work/a/f{index:03}"), format!("Note {index}")).unwrap();
            }
            let loaded = load_drill_down_notes(repo_string).unwrap();
            assert_eq!(loaded.notes.len(), 100);
            assert_eq!(loaded.notes.get("work/a/f042").map(String::as_str), Some("Note 42"));
        });
    }
}
