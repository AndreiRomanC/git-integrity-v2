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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchItem {
    id: String, title: String,
    #[serde(default)] item_type: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    id: String, title: String, kind: String,
    #[serde(default)] item_type: String,
    #[serde(default)] status: String,
    #[serde(default)] assignees: Vec<String>,
    #[serde(default)] fields: std::collections::BTreeMap<String, String>,
    #[serde(default)] description: String,
    #[serde(default)] fetched_at: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Workspace {
    #[serde(default)] primary_task: Option<Snapshot>,
    #[serde(default)] document: Option<Snapshot>,
    #[serde(default)] elements: Vec<Snapshot>,
    #[serde(default)] elements_complete: bool,
    #[serde(default)] elements_saved_at: String,
    #[serde(default)] elements_revision: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct HttpDiagnostic {
    step: String, route: String, code: String,
    #[serde(default)] http: u16,
    #[serde(default)] duration_ms: u64,
    #[serde(default)] page: usize,
    #[serde(default)] query: bool,
    #[serde(default)] id_star: bool,
    #[serde(default)] leading_wildcard: bool,
    #[serde(default)] media: String,
    #[serde(default)] reason: String,
    #[serde(default)] parameter: String,
    #[serde(default)] resource_type: String,
    #[serde(default)] count: usize,
    #[serde(default)] more: bool,
    #[serde(default)] allow: Vec<String>,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ServerMetadata {
    #[serde(default)] version: String,
    #[serde(default)] build: String,
    #[serde(default)] max_page_size: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    code: String,
    #[serde(default)] http: u16,
    #[serde(default)] session_token: bool,
    #[serde(default)] more: bool,
    #[serde(default)] received: usize,
    #[serde(default)] prefix_fallback: bool,
    #[serde(default)] metadata_fallback: bool,
    #[serde(default)] catalog_cached: bool,
    #[serde(default)] catalog_pages: usize,
    #[serde(default)] matched: usize,
    #[serde(default)] snapshot: Option<Snapshot>,
    #[serde(default)] user_id: String,
    #[serde(default)] user_name: String,
    #[serde(default)] incomplete: usize,
    #[serde(default)] page_key: String,
    #[serde(default)] elements: Vec<Snapshot>,
    #[serde(default)] items: Vec<SearchItem>,
    #[serde(default)] diagnostics: Vec<HttpDiagnostic>,
    #[serde(default)] diagnostics_omitted: usize,
    #[serde(default)] http_requests: usize,
    #[serde(default)] server_metadata: Option<ServerMetadata>,
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
    // A fragment such as AQ4 must also match VWAQ4. Keep very short terms
    // prefix-only to avoid broad leading-wildcard queries on large servers.
    // User-provided query operators remain disallowed above.
    Ok(value.split_whitespace().map(|word| {
        let prefix = if word.chars().count() >= 3 { "*" } else { "" };
        format!("{prefix}{}*", word.replace('-', "\\-"))
    }).collect::<Vec<_>>().join(" AND "))
}
fn search_url(kind: &str, project: &str, query: &str, user: &str, page: usize) -> Result<String, String> {
    if page == 0 || page > 1000 { return Err("Invalid search page".into()); }
    let words = if kind == "documents" {
        if query.len() > 240 || query.chars().any(char::is_control) { return Err("Use a short document name or path.".into()); }
        String::new()
    } else { term(query)? };
    let mut url = reqwest::Url::parse("https://polarion.vitesco.io/polarion/rest/v1/projects").unwrap();
    let filter;
    if kind == "projects" {
        // Display pages are applied after case-insensitive substring matching.
        // The portal reads all accessible catalog pages on explicit search.
        url.query_pairs_mut().append_pair("fields[projects]", "id,name,trackerPrefix")
            .append_pair("page[size]", "100").append_pair("page[number]", "1").append_pair("sort", "id");
        return Ok(url.to_string());
    } else if (kind == "tasks" || kind == "containers") && identifier(project) {
        if kind == "containers" && query.trim().chars().count() < 3 { return Err("Type at least three characters to find a StructDoc work item.".into()); }
        url.set_path(&format!("/polarion/rest/v1/projects/{project}/workitems"));
        filter = format!("{}{}{}", if kind == "tasks" { "type:task" } else { "id:*" },
            if words.is_empty() { String::new() } else { format!(" AND (id:({words}) OR title:({words}))") },
            if user.is_empty() { String::new() } else {
                if user.len() > 120 || !user.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.@-".contains(&c)) { return Err("Enter your Polarion user ID to filter assigned tasks.".into()); }
                format!(" AND assignee.id:\"{user}\"")
            });
        url.query_pairs_mut().append_pair("fields[workitems]", "id,title,type");
    } else if kind == "documents" && identifier(project) {
        url.set_path(&format!("/polarion/rest/v1/projects/{project}/documents"));
        url.query_pairs_mut().append_pair("fields[documents]", "moduleName,moduleFolder,title,type")
            .append_pair("page[size]", "100").append_pair("page[number]", "1").append_pair("sort", "id");
        return Ok(url.to_string());
    } else { return Err("Select a valid Polarion project first.".into()); }
    url.query_pairs_mut().append_pair("query", &filter).append_pair("page[size]", "25")
        .append_pair("page[number]", &page.to_string()).append_pair("sort", "id");
    Ok(url.to_string())
}

fn resource_segments(id: &str, expected: usize) -> Result<Vec<&str>, String> {
    let parts: Vec<_> = id.split('/').collect();
    if parts.len() != expected || !identifier(parts[0]) || parts.iter().any(|part| part.is_empty() || part.len() > 240 || *part == "." || *part == ".." || part.chars().any(|c| c.is_control() || c == '\\')) {
        return Err("Invalid Polarion resource identifier".into());
    }
    Ok(parts)
}

fn read_url(kind: &str, id: &str, page: usize) -> Result<String, String> {
    if page == 0 || page > 200 { return Err("Invalid Polarion page".into()); }
    let mut url = reqwest::Url::parse("https://polarion.vitesco.io/polarion/rest/v1/").unwrap();
    if kind == "user" { url.set_path("/polarion/rest/v1/user"); url.query_pairs_mut().append_pair("fields[users]", "id,name"); return Ok(url.to_string()); }
    let is_doc = kind == "document" || kind == "elements";
    let parts = resource_segments(id, if is_doc { 3 } else { 2 })?;
    let mut segments = url.path_segments_mut().map_err(|_| "Invalid Polarion URL")?;
    segments.pop_if_empty().push("projects").push(parts[0]);
    if is_doc { segments.push("spaces").push(parts[1]).push("documents").push(parts[2]); }
    else { segments.push("workitems").push(parts[1]); }
    match kind {
        "elements" => { segments.push("parts"); },
        "links" => { segments.push("linkedworkitems"); },
        "workitem" | "document" => {},
        _ => return Err("Unsupported Polarion read operation".into()),
    }
    drop(segments);
    match kind {
        "document" => { url.query_pairs_mut().append_pair("fields[documents]", "title,type,status,moduleName,moduleFolder,created,updated"); },
        "workitem" => { url.query_pairs_mut().append_pair("fields[workitems]", "@all"); },
        "elements" | "links" => {
            url.query_pairs_mut().append_pair("page[size]", "25").append_pair("page[number]", &page.to_string())
                .append_pair("include", "workItem").append_pair("fields[workitems]", "id,title,type,status");
            if kind == "elements" { url.query_pairs_mut().append_pair("fields[document_parts]", "id,type,level,workItem"); }
            else { url.query_pairs_mut().append_pair("fields[linkedworkitems]", "role,workItem,sourceWorkItem"); }
        },
        _ => {},
    }
    Ok(url.to_string())
}

fn read_args(kind: &str, resource: &str, page: usize, revision: Option<&str>) -> Result<serde_json::Value, String> {
    let from_link = kind == "document_link";
    let resource = if from_link { diagnostic_resource(resource)?.ok_or("Paste a StructDoc link first")? } else { resource.to_string() };
    let kind = if from_link { "document" } else { kind };
    let mut url = reqwest::Url::parse(&read_url(kind, &resource, page)?).map_err(|_| "Invalid Polarion URL")?;
    if let Some(revision) = revision.filter(|r| !r.is_empty()) {
        if revision.len() > 120 || !revision.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)) { return Err("Invalid Polarion revision".into()); }
        url.query_pairs_mut().append_pair("revision", revision);
    }
    let fallback = if kind == "document" || kind == "elements" {
        let mut fallback = url.clone();
        let pairs: Vec<_> = fallback.query_pairs().map(|(key, value)| {
            let value = match key.as_ref() {
                "fields[documents]" => "title".to_string(),
                "fields[document_parts]" => "id,type,workItem".to_string(),
                _ => value.into_owned(),
            };
            (key.into_owned(), value)
        }).collect();
        fallback.query_pairs_mut().clear().extend_pairs(pairs);
        Some(fallback.to_string())
    } else { None };
    Ok(serde_json::json!({ "url":url.to_string(), "fallbackUrl":fallback, "readFallback":fallback.is_some(), "kind":kind,
        "resourceId":resource, "project":resource.split('/').next().unwrap_or(""), "page":page, "fromLink":from_link }))
}

fn valid_snapshot(snapshot: &Snapshot) -> bool {
    let count = match snapshot.kind.as_str() { "workitem" => 2, "document" => 3, _ => return false };
    resource_segments(&snapshot.id, count).is_ok() && snapshot.title.len() <= 2000 && snapshot.item_type.len() <= 240 && snapshot.status.len() <= 240
        && snapshot.description.len() <= 40_000 && snapshot.fetched_at.len() <= 80 && snapshot.assignees.len() <= 100
        && snapshot.assignees.iter().all(|s| s.len() <= 240) && snapshot.fields.len() <= 25
        && snapshot.fields.iter().all(|(k,v)| k.len() <= 80 && v.len() <= 1000)
}

fn workspace_file(path: &str) -> Result<PathBuf, String> { Ok(crate::notes::polarion_project_path(path)?.with_extension("workspace.json")) }
fn load_workspace(path: &Path) -> Result<Workspace, String> {
    if !path.exists() { return Ok(Workspace::default()); }
    if fs::metadata(path).map_err(|_| "Cannot read Polarion workspace")?.len() > 8_000_000 { return Err("Polarion workspace exceeds its safe size limit".into()); }
    let workspace: Workspace = serde_json::from_slice(&fs::read(path).map_err(|_| "Cannot read Polarion workspace")?).map_err(|_| "Invalid Polarion workspace; existing file was preserved")?;
    validate_workspace(&workspace)?;
    Ok(workspace)
}
fn validate_workspace(workspace: &Workspace) -> Result<(), String> {
    if workspace.primary_task.as_ref().is_some_and(|s| !valid_snapshot(s) || s.kind != "workitem" || s.item_type != "task")
        || workspace.document.as_ref().is_some_and(|s| !valid_snapshot(s)) || workspace.elements.len() > 5000
        || workspace.elements.iter().any(|s| !valid_snapshot(s) || s.kind != "workitem") || workspace.elements_saved_at.len() > 80 || workspace.elements_revision.len() > 120 {
        return Err("Invalid Polarion workspace snapshot".into());
    }
    Ok(())
}
#[tauri::command]
pub async fn polarion_workspace(repository_path: String, action: String, snapshot: Option<Snapshot>, elements: Option<Vec<Snapshot>>, document_id: Option<String>) -> Result<Workspace, String> {
    off_main_thread(move || {
        let root = context_repository(&repository_path, "")?;
        let file = workspace_file(&root)?;
        let _guard = METADATA.lock().map_err(|_| "Metadata lock unavailable")?;
        update_workspace(&file, &action, snapshot, elements, document_id.as_deref())
    }).await
}
fn update_workspace(file: &Path, action: &str, snapshot: Option<Snapshot>, elements: Option<Vec<Snapshot>>, document_id: Option<&str>) -> Result<Workspace, String> {
        let mut workspace = load_workspace(file)?;
        match action {
            "load" => return Ok(workspace),
            "primary" => { workspace.primary_task = snapshot; },
            "document" => {
                if workspace.document.as_ref().map(|s| (&s.id, &s.kind)) != snapshot.as_ref().map(|s| (&s.id, &s.kind)) {
                    workspace.elements.clear(); workspace.elements_complete = false; workspace.elements_saved_at.clear(); workspace.elements_revision.clear();
                }
                workspace.document = snapshot;
            },
            "elements" => {
                let selected = snapshot.as_ref().ok_or("Missing StructDoc identity")?;
                if workspace.document.as_ref().map(|s| (&s.id, &s.kind)) != Some((&selected.id, &selected.kind)) { return Err("StructDoc changed while its elements were loading; previous snapshot was preserved".into()); }
                if workspace.document.as_ref().and_then(|s| s.fields.get("revision")) != selected.fields.get("revision") { return Err("StructDoc revision changed while its elements were loading; previous snapshot was preserved".into()); }
                workspace.elements = elements.ok_or("Missing StructDoc elements")?;
                workspace.elements_complete = true;
                workspace.elements_saved_at = selected.fetched_at.clone();
                workspace.elements_revision = selected.fields.get("revision").cloned().unwrap_or_default();
            },
            "element" => {
                if workspace.document.as_ref().map(|s| s.id.as_str()) != document_id { return Err("StructDoc changed while item details were loading; previous snapshot was preserved".into()); }
                let snapshot = snapshot.ok_or("Missing element snapshot")?;
                let mut found = false;
                for item in &mut workspace.elements { if item.id == snapshot.id { *item = snapshot.clone(); found = true; } }
                if !found { return Err("This item is not in the saved StructDoc; no snapshot was changed".into()); }
            },
            _ => return Err("Unsupported Polarion workspace action".into()),
        }
        validate_workspace(&workspace)?;
        let bytes = serde_json::to_vec_pretty(&workspace).map_err(|_| "Cannot encode Polarion workspace")?;
        if bytes.len() > 8_000_000 { return Err("Polarion workspace exceeds its safe size limit; previous snapshot was preserved".into()); }
        fs::create_dir_all(file.parent().ok_or("Missing Polarion metadata directory")?).map_err(|_| "Cannot create Polarion metadata directory")?;
        let tmp = file.with_extension("json.tmp");
        fs::write(&tmp, bytes).map_err(|_| "Cannot save Polarion workspace")?;
        fs::rename(tmp, file).map_err(|_| "Cannot replace Polarion workspace; previous snapshot was preserved")?;
        perf_log(&format!("polarion: workspace_saved action={action} elements={} outside_git=true", workspace.elements.len()), Duration::ZERO);
        Ok(workspace)
}

fn prefix_fallback_url(url: &str) -> Result<String, String> {
    let mut fallback = reqwest::Url::parse(url).map_err(|_| "Invalid search URL")?;
    let pairs: Vec<(String, String)> = fallback.query_pairs().map(|(key, value)| {
        let value = if key == "query" { value.replace("(*", "(").replace("AND *", "AND ") } else { value.into_owned() };
        (key.into_owned(), value)
    }).collect();
    fallback.query_pairs_mut().clear().extend_pairs(pairs);
    Ok(fallback.to_string())
}

fn decode_portal_segment(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let digit = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
            let hi = bytes.get(i + 1).and_then(|b| digit(*b));
            let lo = bytes.get(i + 2).and_then(|b| digit(*b));
            decoded.push(hi.zip(lo).map(|(a,b)| a * 16 + b).ok_or("Invalid encoding in StructDoc link")?);
            i += 3;
        } else { decoded.push(bytes[i]); i += 1; }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| "Invalid encoding in StructDoc link")?;
    if decoded.contains('/') { return Err("Unexpected slash in StructDoc link".into()); }
    Ok(decoded)
}

// Resolve only the portal location supplied by the user. It is never fetched
// directly and never followed to another host or used as an arbitrary API URL.
fn diagnostic_resource(link: &str) -> Result<Option<String>, String> {
    if link.trim().is_empty() { return Ok(None); }
    if link.len() > 2000 { return Err("StructDoc link is too long".into()); }
    let url = reqwest::Url::parse(link.trim()).map_err(|_| "Paste a Polarion StructDoc link")?;
    if url.scheme() != "https" || url.host_str() != Some(HOST) || url.port().is_some()
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.path() != "/polarion/" {
        return Err("Use a document link from https://polarion.vitesco.io/polarion/ without credentials or query parameters".into());
    }
    let path = url.fragment().unwrap_or("");
    if path.contains('?') { return Err("Remove selection or other query parameters from the StructDoc link".into()); }
    let parts: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    if parts.len() != 5 || parts[0] != "project" || parts[2] != "wiki" { return Err("Expected a Polarion #/project/…/wiki/space/document link".into()); }
    let id = [parts[1], parts[3], parts[4]].into_iter().map(decode_portal_segment).collect::<Result<Vec<_>, _>>()?.join("/");
    resource_segments(&id, 3)?;
    Ok(Some(id))
}

fn diagnostic_args(project: &str, link: &str, query: &str) -> Result<serde_json::Value, String> {
    let resource = diagnostic_resource(link)?;
    let project = resource.as_deref().map(|id| id.split('/').next().unwrap()).unwrap_or(project);
    if !project.is_empty() && !identifier(project) { return Err("Select a valid Polarion project".into()); }
    let mut probes = Vec::new();
    let mut add = |step: &str, url: String| { probes.push(serde_json::json!({ "step":step, "url":url })); };
    let root = format!("https://{HOST}/polarion/rest/v1");
    add("metadata", format!("{root}/metadata"));
    add("current_user", read_url("user", "", 1)?);
    if !project.is_empty() {
        add("project_access", format!("{root}/projects/{project}?fields[projects]=id"));
        let limited = |url: String| -> Result<String, String> {
            let mut parsed = reqwest::Url::parse(&url).map_err(|_| "Invalid diagnostic URL")?;
            let pairs: Vec<_> = parsed.query_pairs().map(|(k,v)| {
                let value = if k == "page[size]" { "1".into() } else { v.into_owned() };
                (k.into_owned(), value)
            }).collect();
            parsed.query_pairs_mut().clear().extend_pairs(pairs);
            Ok(parsed.to_string())
        };
        add("tasks_baseline", limited(search_url("tasks", project, "", "", 1)?)?);
        add("documents_catalog", limited(search_url("documents", project, "", "", 1)?)?);
        add("spaces", format!("{root}/projects/{project}/spaces?page[size]=1&page[number]=1"));
        if let Some(id) = resource.as_deref() {
            // Minimal sparse fields isolate endpoint access from optional
            // metadata-field compatibility. Normal reads remain unchanged.
            let mut document = reqwest::Url::parse(&read_url("document", id, 1)?).unwrap();
            document.set_query(Some("fields[documents]=title"));
            let mut page = document.clone();
            page.set_path(&page.path().replace("/documents/", "/pages/"));
            page.set_query(Some("fields[pages]=title"));
            add("document_exact", document.to_string());
            add("page_exact", page.to_string());
            let parts = resource_segments(id, 3)?;
            for (step, collection) in [("space_documents", "documents"), ("space_pages", "pages")] {
                let mut url = reqwest::Url::parse(&root).unwrap();
                url.path_segments_mut().unwrap().push("projects").push(project).push("spaces").push(parts[1]).push(collection);
                url.query_pairs_mut().append_pair("page[size]", "1").append_pair("page[number]", "1");
                add(step, url.to_string());
            }
            // Probed only after the exact resource has been confirmed as a Document.
            let mut parts = reqwest::Url::parse(&read_url("elements", id, 1)?).unwrap();
            parts.set_query(Some("page[size]=1&page[number]=1&fields[document_parts]=id,type"));
            probes.push(serde_json::json!({ "step":"document_parts", "url":parts.to_string(), "requires":"document_exact" }));
        }
        if !query.trim().is_empty() {
            let current = limited(search_url("containers", project, query, "", 1)?)?;
            let prefix = prefix_fallback_url(&current)?;
            let mut plain = reqwest::Url::parse(&prefix).unwrap();
            let pairs: Vec<_> = plain.query_pairs().map(|(k,v)| {
                let value = if k == "query" { v.strip_prefix("id:* AND ").unwrap_or(&v).to_string() } else { v.into_owned() };
                (k.into_owned(), value)
            }).collect();
            plain.query_pairs_mut().clear().extend_pairs(pairs);
            probes.push(serde_json::json!({ "step":"query_current", "url":current }));
            probes.push(serde_json::json!({ "step":"query_prefix", "url":prefix }));
            probes.push(serde_json::json!({ "step":"query_without_id_star", "url":plain.to_string() }));
        }
    }
    Ok(serde_json::json!({ "kind":"diagnostics", "project":project, "resourceId":resource, "query":query, "probes":probes, "page":1 }))
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
    if !["ok", "auth_required", "forbidden", "api_unavailable", "query_rejected", "http_error", "not_json", "network_error", "timeout", "wrong_origin", "invalid_response", "catalog_limit", "response_limit"].contains(&result.code.as_str())
        || result.items.len() > 25 || result.items.iter().any(|item| (!identifier(&item.id) && resource_segments(&item.id, 3).is_err() && resource_segments(&item.id, 2).is_err()) || item.title.len() > 2000 || item.item_type.len() > 240)
        || result.elements.len() > 25 || result.elements.iter().any(|s| !valid_snapshot(s))
        || result.snapshot.as_ref().is_some_and(|s| !valid_snapshot(s)) || result.user_id.len() > 120 || result.user_name.len() > 500 || result.page_key.len() > 30_000
        || result.diagnostics.len() > 24 || result.http_requests > 210 || result.diagnostics_omitted > 210
        || result.diagnostics.iter().any(|d| !valid_diagnostic(d))
        || result.server_metadata.as_ref().is_some_and(|m| {
            (!m.version.is_empty() && (m.version.len() > 40 || !m.version.contains('.') || !m.version.bytes().all(|b| b.is_ascii_digit() || b == b'.')))
                || (!m.build.is_empty() && (m.build.len() > 90 || !m.build.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || m.build.get(..8).is_none_or(|v| !v.bytes().all(|b| b.is_ascii_digit())))) || m.max_page_size > 1_000_000
        }) {
        return Err("Invalid Polarion result; no entries were added".into());
    }
    Ok(result)
}

fn diagnostic_endpoint(route: &str) -> Option<&'static str> {
    Some(match route {
        "metadata" => "/metadata", "user" => "/user", "projects" => "/projects", "project" => "/projects/{project}",
        "workitems" => "/projects/{project}/workitems", "workitem" => "/projects/{project}/workitems/{item}",
        "documents" => "/projects/{project}/documents", "spaces" => "/projects/{project}/spaces",
        "space_documents" => "/projects/{project}/spaces/{space}/documents", "space_pages" => "/projects/{project}/spaces/{space}/pages",
        "document" => "/projects/{project}/spaces/{space}/documents/{document}", "page" => "/projects/{project}/spaces/{space}/pages/{page}",
        "parts" => "/projects/{project}/spaces/{space}/documents/{document}/parts", "links" => "/projects/{project}/workitems/{item}/linkedworkitems",
        "unknown" => "unknown", _ => return None,
    })
}

fn valid_diagnostic(d: &HttpDiagnostic) -> bool {
    ["primary", "prefix_fallback", "metadata_fallback", "catalog_page", "metadata", "current_user", "project_access", "tasks_baseline", "documents_catalog", "spaces", "document_exact", "page_exact", "space_documents", "space_pages", "document_parts", "query_current", "query_prefix", "query_without_id_star"].contains(&d.step.as_str())
        && diagnostic_endpoint(&d.route).is_some() && d.duration_ms <= 600_000 && d.http <= 599 && d.page <= 1000 && d.count <= 100_000
        && ["ok", "auth_required", "forbidden", "api_unavailable", "query_rejected", "http_error", "not_json", "network_error", "timeout", "invalid_response", "not_run"].contains(&d.code.as_str())
        && ["", "leading_wildcard", "unknown_field", "query_syntax", "unsupported_parameter", "permission_denied", "authentication_required", "not_found", "unclassified", "error_body_limit", "partial_errors", "unexpected_resource", "prerequisite_missing", "budget"].contains(&d.reason.as_str())
        && ["", "query", "fields", "page", "sort", "include", "revision", "other"].contains(&d.parameter.as_str())
        && ["", "json", "html", "text", "other"].contains(&d.media.as_str())
        && ["", "metadata", "users", "projects", "workitems", "documents", "pages", "spaces", "document_parts", "linkedworkitems"].contains(&d.resource_type.as_str())
        && d.allow.len() <= 7 && d.allow.iter().all(|v| ["GET", "HEAD", "OPTIONS", "POST", "PATCH", "PUT", "DELETE"].contains(&v.as_str()))
}

fn log_diagnostics(id: &str, result: &SearchResult) {
    for (i, d) in result.diagnostics.iter().enumerate() {
        perf_log(&format!("polarion: http request={id} event={} step={} method=GET endpoint={} code={} http={} media={} reason={} parameter={} page={} query_present={} id_star={} leading_wildcard={} resource_type={} count={} more={} allow={}", i + 1, d.step, diagnostic_endpoint(&d.route).unwrap_or("unknown"), d.code, d.http, d.media, d.reason, d.parameter, d.page, d.query, d.id_star, d.leading_wildcard, d.resource_type, d.count, d.more, d.allow.join(",")), Duration::from_millis(d.duration_ms));
    }
    if let Some(m) = &result.server_metadata {
        perf_log(&format!("polarion: server_metadata request={id} version={} build={} max_page_size={}", m.version, m.build, m.max_page_size), Duration::ZERO);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    fn pairs(url: &str) -> HashMap<String, String> { reqwest::Url::parse(url).unwrap().query_pairs().into_owned().collect() }
    #[test]
    fn project_search_uses_catalog_without_server_wildcard_filter() {
        let url = search_url("projects", "", "Drive Test", "", 2).unwrap();
        let params = pairs(&url);
        assert_eq!(params["page[size]"], "100");
        assert_eq!(params["page[number]"], "1");
        assert!(!params.contains_key("query"));
        assert_eq!(params["fields[projects]"], "id,name,trackerPrefix");
        assert!(search_url("projects", "", "", "", 1).is_ok(), "All projects is explicit and paginated");
    }
    #[test]
    fn fragment_search_matches_inside_identifiers_and_keeps_short_terms_bounded() {
        assert!(!pairs(&search_url("projects", "", "AQ4", "", 1).unwrap()).contains_key("query"));
        assert_eq!(term("  AQ4  Drive  ").unwrap(), "*AQ4* AND *Drive*");
        assert_eq!(term("AQ").unwrap(), "AQ*");
        assert_eq!(term("AQ4-123").unwrap(), "*AQ4\\-123*");
        let tasks = pairs(&search_url("tasks", "VWAQ4", "AQ4", "sg964219", 1).unwrap());
        assert!(tasks["query"].contains("id:(*AQ4*)"));
        assert!(tasks["query"].contains("assignee.id:\"sg964219\""));
        assert_eq!(tasks["page[size]"], "25");
        let fallback = prefix_fallback_url(&search_url("tasks", "VWAQ4", "AQ4 Drive", "sg964219", 2).unwrap()).unwrap();
        let legacy = pairs(&fallback);
        assert!(legacy["query"].contains("id:(AQ4* AND Drive*)"));
        assert!(legacy["query"].contains("assignee.id:\"sg964219\""));
        assert_eq!(legacy["page[number]"], "2");
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
    fn diagnostic_link_resolves_the_exact_space_without_credentials_or_arbitrary_hosts() {
        let link = "https://polarion.vitesco.io/polarion/#/project/VWAQ4/wiki/Structure%20Information/VWAQ4-SW_ERRM_StructDoc_2_0_0";
        assert_eq!(diagnostic_resource(link).unwrap().unwrap(), "VWAQ4/Structure Information/VWAQ4-SW_ERRM_StructDoc_2_0_0");
        assert_eq!(diagnostic_resource("").unwrap(), None);
        for bad in ["https://evil.test/polarion/#/project/P/wiki/S/D", "https://polarion.vitesco.io.evil.test/polarion/#/project/P/wiki/S/D", "https://user:password@polarion.vitesco.io/polarion/#/project/P/wiki/S/D", "https://polarion.vitesco.io/polarion/?token=secret#/project/P/wiki/S/D", "http://polarion.vitesco.io/polarion/#/project/P/wiki/S/D", "https://polarion.vitesco.io:444/polarion/#/project/P/wiki/S/D", "https://polarion.vitesco.io/polarion/#/project/P/wiki/%2E%2E/D", "https://polarion.vitesco.io/polarion/#/project/P/wiki/S/D%2Fextra", "https://polarion.vitesco.io/polarion/#/project/P/wiki/S/%zz", "https://polarion.vitesco.io/polarion/#/project/P/wiki/S/D?token=secret"] {
            assert!(diagnostic_resource(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn diagnostic_probes_are_bounded_get_locations_and_do_not_change_normal_query_behavior() {
        let link = "https://polarion.vitesco.io/polarion/#/project/VWAQ4/wiki/Structure%20Information/VWAQ4-SW_ERRM_StructDoc_2_0_0";
        let args = diagnostic_args("OtherProject", link, "errm").unwrap();
        assert_eq!(args["project"], "VWAQ4", "the link scopes checks, not saved project associations");
        assert_eq!(args["resourceId"], "VWAQ4/Structure Information/VWAQ4-SW_ERRM_StructDoc_2_0_0");
        let probes = args["probes"].as_array().unwrap();
        assert_eq!(probes.len(), 14);
        for probe in probes {
            let url = reqwest::Url::parse(probe["url"].as_str().unwrap()).unwrap();
            assert_eq!(url.host_str(), Some(HOST)); assert!(!url.path().contains("/actions/"));
            assert!(url.query_pairs().all(|(k,v)| k != "page[size]" || v == "1"));
        }
        let exact = probes.iter().find(|p| p["step"] == "page_exact").unwrap()["url"].as_str().unwrap();
        assert!(exact.contains("/spaces/Structure%20Information/pages/VWAQ4-SW_ERRM_StructDoc_2_0_0"), "{exact}");
        assert!(!exact.contains("%2520"));
        let parts = probes.iter().find(|p| p["step"] == "document_parts").unwrap();
        assert_eq!(parts["requires"], "document_exact");
        let without = probes.iter().find(|p| p["step"] == "query_without_id_star").unwrap()["url"].as_str().unwrap();
        assert_eq!(pairs(without)["query"], "(id:(errm*) OR title:(errm*))");
        assert!(pairs(&search_url("containers", "VWAQ4", "errm", "", 1).unwrap())["query"].starts_with("id:* AND "), "diagnostics must not silently modify normal searches");
        assert_eq!(diagnostic_args("", "", "").unwrap()["probes"].as_array().unwrap().len(), 2);
        assert!(diagnostic_args("../bad", "", "").is_err());
        assert!(diagnostic_args("P", "", "secret OR *").is_err());
    }
    #[test]
    fn diagnostic_bridge_rejects_raw_messages_headers_routes_and_unbounded_entries() {
        let good = serde_json::json!({"code":"ok", "http_requests":1, "diagnostics":[{"step":"primary", "route":"workitems", "code":"query_rejected", "http":400, "reason":"leading_wildcard", "parameter":"query", "media":"json", "allow":["GET"]}]});
        assert!(validate_result(&good.to_string()).is_ok());
        for (key, value) in [("route", "https://secret"), ("step", "raw query"), ("reason", "raw server secret"), ("parameter", "token"), ("media", "secret"), ("resource_type", "private title")] {
            let mut bad = good.clone(); bad["diagnostics"][0][key] = value.into();
            assert!(validate_result(&bad.to_string()).is_err(), "{key}");
        }
        let mut bad = good.clone(); bad["diagnostics"][0]["allow"] = serde_json::json!(["Bearer secret"]);
        assert!(validate_result(&bad.to_string()).is_err());
        bad = good.clone(); bad["diagnostics"][0]["duration_ms"] = 600001.into();
        assert!(validate_result(&bad.to_string()).is_err());
        bad = good.clone(); bad["diagnostics"] = serde_json::json!(vec![good["diagnostics"][0].clone(); 25]);
        assert!(validate_result(&bad.to_string()).is_err());
        bad = good.clone(); bad["server_metadata"] = serde_json::json!({"version":"SECRET"});
        assert!(validate_result(&bad.to_string()).is_err());
        assert_eq!(diagnostic_endpoint("page"), Some("/projects/{project}/spaces/{space}/pages/{page}"));
    }
    #[test]
    fn read_urls_are_read_only_precise_and_encode_document_names() {
        assert_eq!(reqwest::Url::parse(&read_url("user", "", 1).unwrap()).unwrap().path(), "/polarion/rest/v1/user");
        let url = read_url("document", "P/30 Development/Structure Information", 1).unwrap();
        assert!(url.contains("/spaces/30%20Development/documents/Structure%20Information"));
        let parts = pairs(&read_url("elements", "P/S/D", 2).unwrap());
        assert_eq!(parts["include"], "workItem"); assert_eq!(parts["page[number]"], "2");
        assert!(read_url("workitem", "P/P-1", 1).unwrap().contains("/projects/P/workitems/P-1?"));
        for id in ["P/../Doc", "P/S/D/extra", "P/S/..", "P/space\\bad/Doc"] { assert!(read_url("document", id, 1).is_err()); }
        assert!(read_url("delete", "P/P-1", 1).is_err());
        assert!(read_url("elements", "P/S/D", 201).is_err());
        assert!(search_url("containers", "P", "ab", "", 1).is_err());
        assert!(search_url("documents", "P", "30 Development/Structure", "", 1).is_ok());
    }
    #[test]
    fn opening_a_document_link_reuses_exact_reads_without_catalogs_or_probes() {
        let link = "https://polarion.vitesco.io/polarion/#/project/VWAQ4/wiki/Structure%20Information/VWAQ4-SW_ERRM_StructDoc_2_0_0";
        let args = read_args("document_link", link, 1, None).unwrap();
        assert_eq!(args["kind"], "document"); assert_eq!(args["project"], "VWAQ4"); assert_eq!(args["fromLink"], true);
        assert_eq!(args["resourceId"], "VWAQ4/Structure Information/VWAQ4-SW_ERRM_StructDoc_2_0_0");
        assert!(args["url"].as_str().unwrap().contains("/spaces/Structure%20Information/documents/VWAQ4-SW_ERRM_StructDoc_2_0_0?"));
        assert!(args.get("probes").is_none()); assert!(args.get("query").is_none());
        assert_eq!(pairs(args["fallbackUrl"].as_str().unwrap())["fields[documents]"], "title");
        assert!(read_args("document_link", "https://evil.test/polarion/#/project/P/wiki/S/D", 1, None).is_err());
        assert!(read_args("document_link", "", 1, None).is_err());
    }
    #[test]
    fn document_compatibility_fallback_preserves_scope_revision_and_relationships() {
        let args = read_args("elements", "P/S/D", 2, Some("12345")).unwrap();
        assert_eq!(args["resourceId"], "P/S/D"); assert_eq!(args["page"], 2);
        let original = pairs(args["url"].as_str().unwrap());
        let fallback = pairs(args["fallbackUrl"].as_str().unwrap());
        for key in ["revision", "include", "page[size]", "page[number]", "fields[workitems]"] { assert_eq!(original[key], fallback[key], "{key}"); }
        assert_eq!(fallback["fields[document_parts]"], "id,type,workItem");
        assert_eq!(fallback["revision"], "12345");
        assert_eq!(read_args("workitem", "P/P-1", 1, None).unwrap()["fallbackUrl"], serde_json::Value::Null);
        assert_eq!(read_args("user", "", 1, None).unwrap()["fallbackUrl"], serde_json::Value::Null);
        assert!(read_args("elements", "P/S/D", 1, Some("123&token=secret")).is_err());
    }
    fn sample_snapshot(id: &str, kind: &str, item_type: &str) -> Snapshot {
        Snapshot { id: id.into(), title: "Saved title".into(), kind: kind.into(), item_type: item_type.into(), fetched_at: "2026-10-16T12:00:00Z".into(), ..Snapshot::default() }
    }
    #[test]
    fn workspace_roundtrip_is_separate_from_project_and_rejects_stale_or_invalid_updates() {
        let root = std::env::temp_dir().join(format!("ddt-polarion-workspace-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("workspace.json");
        assert!(load_workspace(&file).unwrap().primary_task.is_none());
        let task = sample_snapshot("P/P-1", "workitem", "task");
        let doc = sample_snapshot("P/S/D", "document", "structureDocument");
        update_workspace(&file, "primary", Some(task.clone()), None, None).unwrap();
        update_workspace(&file, "document", Some(doc.clone()), None, None).unwrap();
        let element = sample_snapshot("P/P-2", "workitem", "architecturalElement");
        update_workspace(&file, "elements", Some(doc.clone()), Some(vec![element.clone()]), None).unwrap();
        let loaded = load_workspace(&file).unwrap();
        assert_eq!(loaded.primary_task.unwrap().id, task.id);
        assert_eq!(loaded.elements[0].item_type, "architecturalElement"); assert!(loaded.elements_complete);
        let previous = fs::read(&file).unwrap();
        assert!(update_workspace(&file, "elements", Some(sample_snapshot("P/S/Other", "document", "")), Some(Vec::new()), None).is_err());
        assert!(update_workspace(&file, "primary", Some(element.clone()), None, None).is_err());
        assert!(update_workspace(&file, "element", Some(element.clone()), None, Some("P/S/Other")).is_err());
        assert_eq!(fs::read(&file).unwrap(), previous);
        let mut refreshed = element; refreshed.status = "approved".into();
        let updated = update_workspace(&file, "element", Some(refreshed), None, Some("P/S/D")).unwrap();
        assert_eq!(updated.elements[0].status, "approved");
        update_workspace(&file, "document", Some(sample_snapshot("P/S/Other", "document", "")), None, None).unwrap();
        let final_state = load_workspace(&file).unwrap();
        assert!(final_state.elements.is_empty()); assert!(!final_state.elements_complete);
        assert!(final_state.primary_task.is_some(), "StructDoc association must not reset the primary task");
        fs::remove_dir_all(root).unwrap();
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
    let fallback = prefix_fallback_url(&url)?;
    portal_request(app, serde_json::json!({"url":url,"fallbackUrl":fallback,"kind":kind,"project":project,"query":query,"page":page,"assigned":!user.is_empty()})).await
}

#[tauri::command]
pub async fn polarion_diagnostics(app: tauri::AppHandle, project: String, resource_url: String, query: String) -> Result<SearchResult, String> {
    // Explicit only, bounded read-only GETs through the existing portal session.
    // No login prompt, saved association, token export or Git operation.
    let args = diagnostic_args(&project, &resource_url, &query).map_err(|error| {
        perf_log("polarion: diagnostics_rejected reason=invalid_input no_http_requests=true", Duration::ZERO);
        error
    })?;
    portal_request(app, args).await
}

#[tauri::command]
pub async fn polarion_read(app: tauri::AppHandle, kind: String, resource_id: String, page: usize, revision: Option<String>) -> Result<SearchResult, String> {
    portal_request(app, read_args(&kind, &resource_id, page, revision.as_deref())?).await
}

async fn portal_request(app: tauri::AppHandle, mut args: serde_json::Value) -> Result<SearchResult, String> {
    off_main_thread(move || {
        let started = Instant::now();
        let window = app.get_webview_window(WINDOW).ok_or_else(|| {
            perf_log("polarion: search code=browser_not_open", started.elapsed());
            "Open Polarion, sign in if needed, then retry Search.".to_string()
        })?;
        let id = NEXT.fetch_add(1, Ordering::Relaxed).to_string();
        let (send, receive) = mpsc::channel();
        {
            let mut slot = pending().lock().map_err(|_| "Search lock unavailable")?;
            if slot.is_some() { return Err("A Polarion search is already running.".into()); }
            *slot = Some((id.clone(), send));
        }
        // Project matching uses the paginated accessible catalog in the portal,
        // independent of server wildcard/case rules. Task search is unchanged.
        args["request"] = serde_json::Value::String(id.clone());
        let kind = args["kind"].as_str().unwrap_or("");
        let page = args["page"].as_u64().unwrap_or(1);
        let query = args["query"].as_str().unwrap_or("");
        let script = include_str!("polarion-search.js").replace("__DDT_ARGUMENTS__", &args.to_string());
        let contains_terms = query.split_whitespace().filter(|word| word.chars().count() >= 3).count();
        let matching = match kind { "projects" | "documents" => "catalog_case_insensitive_contains", "tasks" | "containers" => "contains_3plus", "diagnostics" => "explicit_read_checks", _ => "exact_resource" };
        perf_log(&format!("polarion: search_start request={id} kind={kind} page={page} project={} resource_present={} from_link={} probes={} query_len={} assigned_filter={} match={matching} contains_terms={contains_terms}", args["project"].as_str().filter(|p| identifier(p)).unwrap_or("none"), args["resourceId"].as_str().is_some(), args["fromLink"].as_bool().unwrap_or(false), args["probes"].as_array().map_or(0, |p| p.len()), query.len(), args["assigned"].as_bool().unwrap_or(false)), Duration::ZERO);
        let mut bridge_code = "eval_failed";
        let result = if window.eval(script).is_err() { Err("Polarion browser could not start the search".into()) }
            else {
                bridge_code = "bridge_timeout_or_closed";
                receive.recv_timeout(Duration::from_secs(if kind == "projects" || kind == "documents" { 95 } else { 35 })).map_err(|_| "Polarion did not reply before the time limit (or its window was closed). Check the log and portal window, then retry; this does not by itself prove authentication failed.".to_string()).and_then(|raw| {
                    bridge_code = "invalid_response";
                    let result = validate_result(&raw)?;
                    if result.snapshot.as_ref().is_some_and(|s| Some(s.id.as_str()) != args["resourceId"].as_str()) { return Err("Polarion returned a different resource; no snapshot was applied".into()); }
                    log_diagnostics(&id, &result);
                    Ok(result)
                })
            };
        if let Ok(mut slot) = pending().lock() { if slot.as_ref().is_some_and(|(key, _)| key == &id) { *slot = None; } }
        let detail = match &result {
            Ok(data) => format!("code={} http={} session_token={} received={} matched={} results={} more={} prefix_fallback={} metadata_fallback={} catalog_cached={} catalog_pages={} identity_detected={} snapshot={} elements={} incomplete={} http_requests={} diagnostics_omitted={}", data.code, data.http, data.session_token, data.received, data.matched, data.items.len(), data.more, data.prefix_fallback, data.metadata_fallback, data.catalog_cached, data.catalog_pages, !data.user_id.is_empty(), data.snapshot.is_some(), data.elements.len(), data.incomplete, data.http_requests, data.diagnostics_omitted),
            Err(_) => format!("code={bridge_code}"),
        };
        perf_log(&format!("polarion: search_end request={id} kind={kind} page={page} {detail}"), started.elapsed());
        result
    }).await
}
