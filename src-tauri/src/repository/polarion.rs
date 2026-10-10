//! Read-only Polarion portal search. Credentials stay inside the portal webview.
use super::*;
use serde::Deserialize;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use std::sync::mpsc;

const HOST: &str = "polarion.vitesco.io";
const WINDOW: &str = "polarion-portal";
const RELAY: &str = "/polarion/__ddt_search_result";
static NEXT: AtomicU64 = AtomicU64::new(1);
type Pending = Option<(String, mpsc::Sender<String>)>;
static PENDING: OnceLock<Mutex<Pending>> = OnceLock::new();
static METADATA: Mutex<()> = Mutex::new(());
fn pending() -> &'static Mutex<Pending> { PENDING.get_or_init(|| Mutex::new(None)) }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project { id: String, name: String }
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchItem { id: String, title: String }
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    code: String,
    #[serde(default)] http: u16,
    #[serde(default)] session_token: bool,
    #[serde(default)] more: bool,
    #[serde(default)] received: usize,
    #[serde(default)] items: Vec<SearchItem>,
}
#[derive(Serialize)]
pub struct Context { repository_path: String, project: Option<Project> }

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 120 && value.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
fn term(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.len() > 100 || !value.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_') {
        return Err("Use letters, numbers, spaces, hyphens or underscores in the search.".into());
    }
    Ok(value.split_whitespace().map(|word| format!("{}*", word.replace('-', "\\-"))).collect::<Vec<_>>().join(" AND "))
}
fn search_url(kind: &str, project: &str, query: &str, user: &str, page: usize) -> Result<String, String> {
    if page == 0 || page > 1000 { return Err("Invalid search page".into()); }
    let words = term(query)?;
    let mut url = reqwest::Url::parse("https://polarion.vitesco.io/polarion/rest/v1/projects").unwrap();
    let filter;
    if kind == "projects" {
        if words.is_empty() { return Err("Type part of a project name or ID first.".into()); }
        filter = format!("(id:({words}) OR name:({words}) OR trackerPrefix:({words}))");
        url.query_pairs_mut().append_pair("fields[projects]", "id,name");
    } else if kind == "tasks" && identifier(project) {
        url.set_path(&format!("/polarion/rest/v1/projects/{project}/workitems"));
        filter = format!("type:task{}{}",
            if words.is_empty() { String::new() } else { format!(" AND (id:({words}) OR title:({words}))") },
            if user.is_empty() { String::new() } else {
                if user.len() > 120 || !user.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.@-".contains(&c)) { return Err("Enter your Polarion user ID to filter assigned tasks.".into()); }
                format!(" AND assignee.id:\"{user}\"")
            });
        url.query_pairs_mut().append_pair("fields[workitems]", "id,title,type");
    } else { return Err("Select a valid Polarion project first.".into()); }
    url.query_pairs_mut().append_pair("query", &filter).append_pair("page[size]", "25")
        .append_pair("page[number]", &page.to_string()).append_pair("sort", "id");
    Ok(url.to_string())
}

fn context_repository(repository_path: &str, relative_path: &str) -> Result<String, String> {
    validate_path(repository_path)?;
    let relative = safe_relative_path(relative_path)?;
    let requested = fs::canonicalize(repository_path).map_err(|_| "Cannot resolve repository directory")?.join(relative);
    let mut target = requested.clone();
    while !target.is_dir() { target = target.parent().ok_or("Invalid repository context")?.to_path_buf(); }
    let repo = Repository::discover(target).map_err(|_| "Cannot resolve the selected Git repository")?;
    let workdir = repo.workdir().ok_or("Polarion project associations require a local working tree")?;
    // An absent/uninitialized gitlink must not silently inherit the parent's
    // association. Only inspect selected ancestors in the index, never status.
    if let Some(inside) = requested.strip_prefix(workdir).ok().filter(|path| !path.as_os_str().is_empty()) {
        let index = repo.index().map_err(|_| "Cannot read selected repository metadata")?;
        if inside.ancestors().take_while(|path| !path.as_os_str().is_empty()).any(|path| index.get_path(path, 0).is_some_and(|entry| entry.mode == 0o160000)) {
            return Err("Initialize or open this submodule before choosing its Polarion project.".into());
        }
    }
    Ok(workdir.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn polarion_project_context(repository_path: String, relative_path: String, project: Option<Project>) -> Result<Context, String> {
    off_main_thread(move || {
        let path = context_repository(&repository_path, &relative_path)?;
        let file = crate::notes::polarion_project_path(&path)?;
        let _guard = METADATA.lock().map_err(|_| "Metadata lock unavailable")?;
        let saved = if let Some(project) = project {
            if !identifier(&project.id) || project.name.len() > 500 { return Err("Invalid Polarion project".into()); }
            fs::create_dir_all(file.parent().unwrap()).map_err(|_| "Cannot create external metadata directory")?;
            let tmp = file.with_extension("json.tmp");
            fs::write(&tmp, serde_json::to_vec_pretty(&project).map_err(|_| "Cannot encode project")?).map_err(|_| "Cannot save project association")?;
            fs::rename(tmp, &file).map_err(|_| "Cannot replace project association; previous file was preserved")?;
            Some(project)
        } else if file.exists() {
            Some(serde_json::from_slice::<Project>(&fs::read(file).map_err(|_| "Cannot read project association")?).map_err(|_| "Project association is invalid; existing file was not modified")?)
        } else { None };
        Ok(Context { repository_path: path, project: saved })
    }).await
}

#[tauri::command]
pub async fn polarion_connect(app: tauri::AppHandle) -> Result<(), String> {
    perf_log("polarion: connect_requested mode=browser", Duration::ZERO);
    if let Some(window) = app.get_webview_window(WINDOW) {
        window.show().map_err(|_| "Cannot show Polarion browser")?;
        perf_log("polarion: browser_reused", Duration::ZERO);
        return window.set_focus().map_err(|_| "Cannot focus Polarion browser".into());
    }
    WebviewWindowBuilder::new(&app, WINDOW, WebviewUrl::External("https://polarion.vitesco.io/polarion/".parse().unwrap()))
        .title("Polarion — sign in, then return to Git DrillDown and Search")
        .inner_size(1050.0, 760.0)
        .on_navigation(|url| {
            if url.scheme() == "https" && url.host_str() == Some(HOST) && url.path() == RELAY {
                let pairs: HashMap<_, _> = url.query_pairs().into_owned().collect();
                if let Ok(mut slot) = pending().lock() {
                    if slot.as_ref().is_some_and(|(id, _)| Some(id) == pairs.get("request")) {
                        if let Some(data) = pairs.get("data").filter(|data| data.len() < 64_000) {
                            if let Some((_, sender)) = slot.take() { let _ = sender.send(data.clone()); }
                        }
                    }
                }
                return false; // Data returns locally; never send the relay URL to the server.
            }
            let allowed = url.scheme() == "https" || url.as_str() == "about:blank";
            perf_log(&format!("polarion: navigation destination={} allowed={allowed}", if url.host_str() == Some(HOST) { "portal" } else { "external_or_sign_in" }), Duration::ZERO);
            allowed
        })
        .build().map_err(|_| {
            perf_log("polarion: browser_open_failed", Duration::ZERO);
            "Cannot open the Polarion sign-in browser"
        })?;
    perf_log("polarion: browser_opened; session=dedicated_webview; no_system_browser_cookie_import", Duration::ZERO);
    Ok(())
}

fn validate_result(raw: &str) -> Result<SearchResult, String> {
    let result: SearchResult = serde_json::from_str(raw).map_err(|_| "Invalid Polarion search response")?;
    if !["ok", "auth_required", "forbidden", "api_unavailable", "query_rejected", "http_error", "not_json", "network_error", "timeout", "wrong_origin", "invalid_response"].contains(&result.code.as_str())
        || result.items.len() > 25 || result.items.iter().any(|item| !identifier(&item.id) || item.title.len() > 2000) {
        return Err("Invalid Polarion result; no entries were added".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pairs(url: &str) -> HashMap<String, String> { reqwest::Url::parse(url).unwrap().query_pairs().into_owned().collect() }
    #[test]
    fn project_search_is_server_filtered_and_paged() {
        let url = search_url("projects", "", "Drive Test", "", 2).unwrap();
        let params = pairs(&url);
        assert_eq!(params["page[size]"], "25");
        assert_eq!(params["page[number]"], "2");
        assert!(params["query"].contains("name:(Drive* AND Test*)"));
        assert!(search_url("projects", "", "", "", 1).is_err());
    }
    #[test]
    fn task_search_scopes_project_type_and_optional_assignee() {
        let url = search_url("tasks", "VWAQ4", "VWAQ4-123", "sg964219", 1).unwrap();
        assert_eq!(reqwest::Url::parse(&url).unwrap().path(), "/polarion/rest/v1/projects/VWAQ4/workitems");
        let params = pairs(&url);
        assert!(params["query"].starts_with("type:task AND"));
        assert!(params["query"].contains("VWAQ4\\-123*"));
        assert!(params["query"].contains("assignee.id:\"sg964219\""));
        assert_eq!(pairs(&search_url("tasks", "VWAQ4", "", "", 1).unwrap())["query"], "type:task");
    }
    #[test]
    fn invalid_queries_do_not_reach_browser() {
        for project in ["../secret", "p/a", "p?x", ""] { assert!(search_url("tasks", project, "", "", 1).is_err()); }
        for query in ["foo OR type:*", "\"", "x&token=secret"] { assert!(search_url("projects", "", query, "", 1).is_err()); }
        assert!(search_url("tasks", "P", "", "user\" OR *", 1).is_err());
        assert!(search_url("tasks", "P", "", "", 0).is_err());
    }
    #[test]
    fn response_diagnostics_are_allowlisted_not_raw_server_errors() {
        assert!(validate_result(r#"{"code":"forbidden","http":403,"session_token":true}"#).is_ok());
        assert!(validate_result(r#"{"code":"secret from server"}"#).is_err());
        assert!(validate_result(r#"{"code":"ok","items":[{"id":"../bad","title":"x"}]}"#).is_err());
        assert!(validate_result("<html>login</html>").is_err());
    }
    #[test]
    fn context_resolves_folder_file_and_nested_repository_without_status() {
        let root = std::env::temp_dir().join(format!("ddt-polarion-context-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let repo = Repository::init(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        fs::create_dir_all(root.join("folder")).unwrap();
        fs::write(root.join("folder/file.txt"), "untouched").unwrap();
        let nested = Repository::init(root.join("module")).unwrap();
        let actual = |relative| fs::canonicalize(context_repository(&root.to_string_lossy(), relative).unwrap()).unwrap();
        assert_eq!(actual("folder"), root);
        assert_eq!(actual("folder/file.txt"), root);
        assert_eq!(actual("module"), fs::canonicalize(nested.workdir().unwrap()).unwrap());
        assert!(context_repository(&root.to_string_lossy(), "../other").is_err());

        let tree = repo.find_tree(repo.treebuilder(None).unwrap().write().unwrap()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[]).unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("uninitialized", oid, 0o160000).unwrap();
        let mut index = repo.index().unwrap();
        index.read_tree(&repo.find_tree(builder.write().unwrap()).unwrap()).unwrap();
        index.write().unwrap();
        assert!(context_repository(&root.to_string_lossy(), "uninitialized").unwrap_err().contains("Initialize"));
        assert_eq!(repo.head().unwrap().target(), Some(oid));
        assert_eq!(fs::read_to_string(root.join("folder/file.txt")).unwrap(), "untouched");
    }
}

#[tauri::command]
pub async fn polarion_search(app: tauri::AppHandle, kind: String, project: String, query: String, user: String, page: usize) -> Result<SearchResult, String> {
    let url = search_url(&kind, &project, &query, &user, page)?;
    off_main_thread(move || {
        let started = Instant::now();
        let window = app.get_webview_window(WINDOW).ok_or_else(|| {
            perf_log("polarion: search code=browser_not_open", started.elapsed());
            "Open Connect / Polarion, sign in, then retry Search.".to_string()
        })?;
        let id = NEXT.fetch_add(1, Ordering::Relaxed).to_string();
        let (send, receive) = mpsc::channel();
        {
            let mut slot = pending().lock().map_err(|_| "Search lock unavailable")?;
            if slot.is_some() { return Err("A Polarion search is already running.".into()); }
            *slot = Some((id.clone(), send));
        }
        let script = include_str!("polarion-search.js")
            .replace("__DDT_ARGUMENTS__", &serde_json::json!({"url":url,"kind":kind,"project":project,"request":id}).to_string());
        perf_log(&format!("polarion: search_start request={id} kind={kind} page={page} query_len={} assigned_filter={}", query.len(), !user.is_empty()), Duration::ZERO);
        let mut bridge_code = "eval_failed";
        let result = if window.eval(script).is_err() { Err("Polarion browser could not start the search".into()) }
            else {
                bridge_code = "bridge_timeout_or_closed";
                receive.recv_timeout(Duration::from_secs(35)).map_err(|_| "Polarion search timed out. Sign in in its window, return here and retry. Check the log for diagnostics.".to_string()).and_then(|raw| {
                    bridge_code = "invalid_response";
                    validate_result(&raw)
                })
            };
        if let Ok(mut slot) = pending().lock() { if slot.as_ref().is_some_and(|(key, _)| key == &id) { *slot = None; } }
        let detail = match &result {
            Ok(data) => format!("code={} http={} session_token={} received={} results={} more={}", data.code, data.http, data.session_token, data.received, data.items.len(), data.more),
            Err(_) => format!("code={bridge_code}"),
        };
        perf_log(&format!("polarion: search_end request={id} kind={kind} page={page} {detail}"), started.elapsed());
        result
    }).await
}
