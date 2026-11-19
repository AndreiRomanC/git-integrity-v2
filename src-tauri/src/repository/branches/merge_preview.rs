//! Committed-snapshot preview using the same installed Git/ort as the actual
//! merge. No checkout, real-index write, ref update, fetch or submodule init.
//! Git may store generated blob/tree objects in its object database.
use super::super::*;
use super::native_merge as native;
use serde::Deserialize;
use std::collections::BTreeMap;

const GITLINK: u32 = 0o160000;
const MAX_ENTRIES: usize = 250_000;
const MAX_ITEMS: usize = 25_000;
const MAX_TEXT: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    path: String,
    oid: String,
    mode: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    path: String,
    kind: String,
    outcome: String,
    change: String,
    conflict: Option<String>,
    base: Option<Version>,
    current: Option<Version>,
    incoming: Option<Version>,
    result: Option<Version>,
    // Directories have no index entry of their own. Do not invent a blob or
    // mistake a folder/gitlink conversion for an absent path in the report.
    directory_sides: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Preview {
    repository_path: String,
    current_branch: String,
    current_revision: String,
    source_ref: String,
    incoming_revision: String,
    merge_bases: Vec<String>,
    status: String,
    items: Vec<Item>,
    warnings: Vec<String>,
    elapsed_ms: u128,
}

fn message(e: git2::Error) -> String {
    e.message().to_owned()
}
fn version(path: &str, id: Oid, mode: u32) -> Version {
    Version {
        path: path.into(),
        oid: id.to_string(),
        mode,
    }
}
fn entries(index: &native::Index) -> BTreeMap<String, Version> {
    index
        .entries
        .values()
        .filter(|e| e.stage == 0)
        .map(|e| (e.path.clone(), version(&e.path, e.id, e.mode)))
        .collect()
}
fn kind(sides: &[Option<&Version>]) -> String {
    if sides.iter().flatten().any(|s| s.mode == GITLINK) {
        "submodule"
    } else {
        "file"
    }
    .into()
}
fn change(current: Option<&Version>, result: Option<&Version>) -> String {
    match (current, result) {
        (None, Some(_)) => "added",
        (Some(_), None) => "removed",
        (a, b) if a == b => "unchanged",
        _ => "modified",
    }
    .into()
}
fn outcome(
    current: Option<&Version>,
    incoming: Option<&Version>,
    result: Option<&Version>,
) -> &'static str {
    if result == current && result == incoming {
        "both"
    } else if result == incoming {
        "incoming"
    } else if result == current {
        "current"
    } else {
        "combined"
    }
}
fn contains_directory(entries: &BTreeMap<String, Version>, path: &str) -> bool {
    let prefix = format!("{path}/");
    entries
        .range(prefix.clone()..)
        .next()
        .is_some_and(|(key, _)| key.starts_with(&prefix))
}

#[tauri::command]
pub async fn preview_merge(
    repository_path: String,
    target_path: String,
    source_ref: String,
    expected_incoming: Option<String>,
) -> Result<Preview, String> {
    let queued = Instant::now();
    off_main_thread(move || {
        perf_log("merge_preview: worker_queue", queued.elapsed());
        let started = Instant::now();
        let result = preview(repository_path, target_path, source_ref, expected_incoming);
        if result.is_err() {
            perf_log(
                "merge_preview: failed; no result applied",
                started.elapsed(),
            );
        }
        result
    })
    .await
}

fn preview(
    root: String,
    target: String,
    source: String,
    expected_incoming: Option<String>,
) -> Result<Preview, String> {
    let started = Instant::now();
    let root = resolve_target_repository(&root, &target)?;
    validate_path(&root)?;
    // Serialize with application mutations, but do not invalidate status or
    // refresh the UI: this operation changes no checked-out repository state.
    let lock = repo_write_lock(&root);
    let _guard = lock
        .lock()
        .map_err(|_| "Repository operation lock is unavailable.")?;
    let plan = native::plan(&root, &source, expected_incoming.as_deref())?;
    let current = plan.current;
    let incoming = plan.incoming;
    let bases = &plan.bases;
    let status = plan.status;
    let branch = plan
        .branch
        .clone()
        .unwrap_or_else(|| "Detached HEAD".into());
    let mut warnings = Vec::new();
    if plan.branch.is_none() {
        warnings.push("Detached HEAD: this preview is available, but an actual merge requires a local branch.".into());
    }
    if bases.len() > 1 {
        warnings.push("Multiple merge bases: Git combines their ancestry. No single base version is claimed in the report.".into());
    }
    let ours = entries(&native::tree(&root, &current.to_string())?);
    let theirs = entries(&native::tree(&root, &incoming.to_string())?);
    let base_entries = if bases.len() == 1 {
        entries(&native::tree(&root, &bases[0].to_string())?)
    } else {
        BTreeMap::new()
    };
    let index = &plan.result;
    if index.entries.len() > MAX_ENTRIES {
        return Err("Merge preview exceeds its entry limit.".into());
    }
    let result = entries(index);
    let mut consumed = HashSet::new();
    let mut items = Vec::new();
    for path in index.conflict_paths() {
        let side = |stage| {
            index
                .get(&path, stage)
                .map(|e| version(&e.path, e.id, e.mode))
        };
        let base = side(1);
        let current = side(2);
        let incoming = side(3);
        consumed.insert(path.clone());
        let kind = kind(&[base.as_ref(), current.as_ref(), incoming.as_ref()]);
        let rename = plan
            .messages
            .iter()
            .any(|(paths, kind, _)| kind.contains("rename") && paths.contains(&path));
        let conflict_kind = if rename {
            "rename"
        } else if kind == "submodule" {
            "submodule pointer"
        } else if current.is_none() || incoming.is_none() {
            "modify / delete"
        } else if base.is_none() {
            "add / add"
        } else {
            "content or file type"
        };
        items.push(Item {
            path,
            kind,
            outcome: "conflict".into(),
            change: "unresolved".into(),
            conflict: Some(conflict_kind.into()),
            base,
            current,
            incoming,
            result: None,
            directory_sides: vec![],
            warnings: vec![],
        });
    }
    if status != "up_to_date" {
        let paths: std::collections::BTreeSet<_> = ours
            .keys()
            .chain(theirs.keys())
            .chain(result.keys())
            .collect();
        for path in paths {
            if consumed.contains(path) {
                continue;
            }
            let current = ours.get(path).cloned();
            let incoming = theirs.get(path).cloned();
            let final_version = result.get(path).cloned();
            if current == incoming && current == final_version {
                continue;
            }
            let base = base_entries.get(path).cloned();
            let outcome = outcome(current.as_ref(), incoming.as_ref(), final_version.as_ref());
            items.push(Item {
                path: path.clone(),
                kind: kind(&[
                    base.as_ref(),
                    current.as_ref(),
                    incoming.as_ref(),
                    final_version.as_ref(),
                ]),
                outcome: outcome.into(),
                change: change(current.as_ref(), final_version.as_ref()),
                conflict: None,
                base,
                current,
                incoming,
                result: final_version,
                directory_sides: vec![],
                warnings: vec![],
            });
        }
    }
    // The same registration rule checks the simulated Git result and the
    // real staged index before commit. Never consult the working .gitmodules.
    match native::unregistered(&root, index) {
        Ok(missing) => {
            for path in missing {
                if !items.iter().any(|item| item.path == path) {
                    let current = ours.get(&path).cloned();
                    let incoming = theirs.get(&path).cloned();
                    let final_version = result.get(&path).cloned();
                    items.push(Item {
                        path: path.clone(),
                        kind: "submodule".into(),
                        outcome: outcome(
                            current.as_ref(),
                            incoming.as_ref(),
                            final_version.as_ref(),
                        )
                        .into(),
                        change: change(current.as_ref(), final_version.as_ref()),
                        conflict: None,
                        base: base_entries.get(&path).cloned(),
                        current,
                        incoming,
                        result: final_version,
                        directory_sides: vec![],
                        warnings: vec![],
                    });
                }
                if let Some(item) = items.iter_mut().find(|item| item.path == path) {
                    item.warnings.push("Result contains a submodule pointer without a matching .gitmodules registration. Update/init cannot repair this inconsistency.".into());
                }
            }
        }
        Err(error) => warnings.push(format!(
            "Submodule registration could not be verified for this result: {error}"
        )),
    }
    if items.len() > MAX_ITEMS {
        return Err(
            "Merge preview exceeds 25,000 report items. No partial report was applied.".into(),
        );
    }
    for item in &mut items {
        for (side, entries, missing) in [
            ("current", &ours, item.current.is_none()),
            ("incoming", &theirs, item.incoming.is_none()),
            (
                "result",
                &result,
                item.result.is_none() && item.outcome != "conflict",
            ),
        ] {
            if missing && contains_directory(entries, &item.path) {
                item.directory_sides.push(side.into());
            }
        }
        if item.base.is_none() && contains_directory(&base_entries, &item.path) {
            item.directory_sides.push("base".into());
        }
    }
    items.sort_by(|a, b| {
        (a.outcome != "conflict", a.warnings.is_empty(), &a.path).cmp(&(
            b.outcome != "conflict",
            b.warnings.is_empty(),
            &b.path,
        ))
    });
    if native::oid(&root, "HEAD")? != current
        || native::branch(&root)? != plan.branch
        || native::oid(&root, &source)? != incoming
    {
        return Err(
            "A branch changed during merge preview. Refresh and run the preview again.".into(),
        );
    }
    perf_log(&format!("merge_preview: status={status} current={} incoming={} items={} conflicts={} submodules={} warnings={} checkout=false fetch=false",
        current, incoming, items.len(), items.iter().filter(|i|i.outcome=="conflict").count(), items.iter().filter(|i|i.kind=="submodule").count(), warnings.len()+items.iter().map(|i|i.warnings.len()).sum::<usize>()), started.elapsed());
    Ok(Preview {
        repository_path: root,
        current_branch: branch,
        current_revision: current.to_string(),
        source_ref: source,
        incoming_revision: incoming.to_string(),
        merge_bases: bases.iter().map(ToString::to_string).collect(),
        status: status.into(),
        items,
        warnings,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[derive(Serialize)]
pub struct Content {
    content: Option<String>,
    note: String,
}

// On-demand object reads only. Generated blobs are immutable, so a later branch
// change cannot substitute the content under an already displayed snapshot.
#[tauri::command]
pub async fn merge_preview_contents(
    repository_path: String,
    versions: Vec<Option<Version>>,
) -> Result<Vec<Content>, String> {
    off_main_thread(move || {
        if versions.len() > 4 {
            return Err("Select at most four preview versions.".into());
        }
        versions
            .into_iter()
            .map(|v| {
                let Some(v) = v else {
                    return Ok(Content {
                        content: Some(String::new()),
                        note: "Absent in this version".into(),
                    });
                };
                safe_relative_path(&v.path)?;
                if !matches!(v.mode, 0o100644 | 0o100755 | 0o120000) {
                    return Ok(Content {
                        content: None,
                        note: "Submodule pointer; not file content".into(),
                    });
                }
                let oid = Oid::from_str(&v.oid).map_err(message)?;
                let size = native::text(&repository_path, &["cat-file", "-s", &oid.to_string()])?
                    .parse::<usize>()
                    .map_err(|_| "Invalid blob size")?;
                if size > MAX_TEXT {
                    return Ok(Content {
                        content: None,
                        note: "File exceeds the 2 MiB preview limit; no content was truncated."
                            .into(),
                    });
                }
                let blob = native::blob(&repository_path, oid)?;
                if blob.iter().take(8000).any(|b| *b == 0) {
                    return Ok(Content {
                        content: None,
                        note: "Binary file; text preview unavailable.".into(),
                    });
                }
                match std::str::from_utf8(&blob) {
                    Ok(text) => Ok(Content {
                        content: Some(text.into()),
                        note: String::new(),
                    }),
                    Err(_) => Ok(Content {
                        content: None,
                        note: "Non-UTF-8 file; text preview unavailable.".into(),
                    }),
                }
            })
            .collect()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    struct Fixture { root: PathBuf, repo: Repository }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!("ddt-merge-preview-{}-{}-{}",std::process::id(),SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),NEXT.fetch_add(1,Ordering::Relaxed)));
            fs::create_dir_all(&root).unwrap();
            let repo=Repository::init(&root).unwrap(); repo.config().unwrap().set_str("user.name","Test").unwrap();repo.config().unwrap().set_str("user.email","test@example.invalid").unwrap();
            Self{root,repo}
        }
        fn root(&self)->String { self.root.to_string_lossy().into_owned() }
        fn blob(&self,text:&str)->Oid { self.repo.blob(text.as_bytes()).unwrap() }
        fn commit(&self,entries:&[(&str,Oid,u32)],parents:&[Oid])->Oid {
            let mut index=git2::Index::new().unwrap();
            for (path,id,mode) in entries {
                index.add(&git2::IndexEntry {ctime:git2::IndexTime::new(0,0),mtime:git2::IndexTime::new(0,0),dev:0,ino:0,mode:*mode,uid:0,gid:0,file_size:0,id:*id,flags:0,flags_extended:0,path:path.as_bytes().to_vec()}).unwrap();
            }
            let tree=self.repo.find_tree(index.write_tree_to(&self.repo).unwrap()).unwrap();
            let parents:Vec<_>=parents.iter().map(|id|self.repo.find_commit(*id).unwrap()).collect();
            let sig=git2::Signature::new("Preview test","preview@example.test",&git2::Time::new(1760000000,0)).unwrap();
            self.repo.commit(None,&sig,&sig,"Snapshot",&tree,&parents.iter().collect::<Vec<_>>()).unwrap()
        }
        fn branches(&self,current:Oid,incoming:Oid) {
            self.repo.reference("refs/heads/current",current,true,"fixture").unwrap();
            self.repo.reference("refs/heads/incoming",incoming,true,"fixture").unwrap();
            self.repo.set_head("refs/heads/current").unwrap();
            self.repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();
        }
        fn preview(&self)->Preview { preview(self.root(),String::new(),"incoming".into(),None).unwrap() }
        fn state(&self)->(Oid,Vec<u8>,Vec<u8>,git2::RepositoryState,Vec<u8>) {
            (self.repo.head().unwrap_or_else(|e|panic!("state of {} at {}: {e}",self.root.display(),self.repo.path().display())).target().unwrap(),fs::read(self.repo.path().join("index")).unwrap(),
                fs::read(self.repo.path().join("config")).unwrap(),self.repo.state(),fs::read(self.repo.path().join("logs/HEAD")).unwrap_or_default())
        }
    }
    impl Drop for Fixture {fn drop(&mut self){let _=fs::remove_dir_all(&self.root);}}
    const FILE:u32=0o100644;

    #[test]
    fn preview_combines_disjoint_edits_keeps_each_side_and_matches_actual_engine() {
        let f=Fixture::new(); let plain=f.blob("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
        let ours=f.blob("OUR\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
        let theirs=f.blob("a\nb\nc\nd\ne\nf\ng\nh\ni\nTHEIR\n");
        let base=f.commit(&[("shared",plain,FILE),("mine",plain,FILE),("theirs",plain,FILE),("gone",plain,FILE)],&[]);
        let current=f.commit(&[("shared",ours,FILE),("mine",ours,FILE),("theirs",plain,FILE),("gone",plain,FILE)],&[base]);
        let incoming=f.commit(&[("shared",theirs,FILE),("mine",plain,FILE),("theirs",theirs,FILE),("new",theirs,FILE)],&[base]);
        f.branches(current,incoming);
        let before=f.state(); let report=f.preview(); assert_eq!(before,f.state());
        assert_eq!(report.status,"clean");
        let find=|p|report.items.iter().find(|i|i.path==p).unwrap();
        assert_eq!(find("shared").outcome,"combined"); assert_eq!(find("mine").outcome,"current");
        assert_eq!(find("theirs").outcome,"incoming");assert_eq!(find("gone").change,"removed");assert_eq!(find("new").change,"added");
        native::text(&f.root(),&["merge","--no-ff","--no-commit","-s","ort","incoming"]).unwrap();
        let actual=native::index(&f.root()).unwrap();
        for item in &report.items {
            assert_eq!(actual.get(&item.path,0).map(|e| version(&e.path,e.id,e.mode)),item.result,"{}",item.path);
        }
        assert_eq!(f.repo.find_blob(Oid::from_str(&find("shared").result.as_ref().unwrap().oid).unwrap()).unwrap().content(),b"OUR\nb\nc\nd\ne\nf\ng\nh\ni\nTHEIR\n");
    }

    #[test]
    fn incoming_newer_version_is_taken_when_current_still_has_common_base() {
        let f=Fixture::new();let old=f.blob("old version\n");let new=f.blob("new version\n");
        let base=f.commit(&[("shared",old,FILE)],&[]);
        let ours=f.commit(&[("shared",old,FILE),("own",old,FILE)],&[base]);
        let theirs=f.commit(&[("shared",new,FILE)],&[base]);f.branches(ours,theirs);
        let report=f.preview();let item=report.items.iter().find(|i|i.path=="shared").unwrap();
        assert_eq!(item.current,item.base);assert_eq!(item.result,item.incoming);assert_eq!(item.outcome,"incoming");
    }

    #[test]
    fn conflicts_have_no_fabricated_result_and_preserve_worktree_index_and_refs() {
        let f=Fixture::new();let a=f.blob("base\n");let b=f.blob("ours\n");let c=f.blob("theirs\n");
        let base=f.commit(&[("shared",a,FILE),("removed",a,FILE)],&[]);
        let ours=f.commit(&[("shared",b,FILE)],&[base]);let theirs=f.commit(&[("shared",c,FILE),("removed",c,FILE)],&[base]);
        f.branches(ours,theirs);
        fs::write(f.root.join("untracked"),b"keep").unwrap();fs::write(f.root.join("shared"),b"local work").unwrap();
        let mut index=f.repo.index().unwrap();index.add_path(Path::new("shared")).unwrap();index.write().unwrap();
        let before=f.state();let report=f.preview();assert_eq!(before,f.state());
        assert_eq!(fs::read(f.root.join("shared")).unwrap(),b"local work");assert_eq!(fs::read(f.root.join("untracked")).unwrap(),b"keep");
        assert_eq!(report.status,"conflicts");assert_eq!(report.items.len(),2);
        for item in &report.items {assert_eq!(item.outcome,"conflict");assert!(item.result.is_none());}
        assert!(report.items.iter().any(|i|i.conflict.as_deref()==Some("modify / delete")));
    }

    #[test]
    fn fast_forward_and_up_to_date_never_move_head() {
        let f=Fixture::new();let a=f.blob("old");let b=f.blob("new");
        let base=f.commit(&[("f",a,FILE)],&[]);let tip=f.commit(&[("f",b,FILE)],&[base]);
        f.branches(base,tip);let before=f.state();assert_eq!(f.preview().status,"fast_forward");assert_eq!(before,f.state());
        f.branches(tip,base);let report=f.preview();assert_eq!(report.status,"up_to_date");assert!(report.items.is_empty());assert_eq!(f.repo.head().unwrap().target(),Some(tip));
    }

    #[test]
    fn submodule_report_uses_actual_pointer_result_and_never_initializes_it() {
        let f=Fixture::new();let a=Oid::from_str(&"1".repeat(40)).unwrap();let b=Oid::from_str(&"2".repeat(40)).unwrap();let text=f.blob("a");
        let config=f.blob("[submodule \"dep\"]\npath = dep\nurl = ../dep.git\n");
        let base=f.commit(&[("dep",a,GITLINK),(".gitmodules",config,FILE)],&[]);
        let ours=f.commit(&[("dep",a,GITLINK),(".gitmodules",config,FILE),("mine",text,FILE)],&[base]);
        let theirs=f.commit(&[("dep",b,GITLINK),(".gitmodules",config,FILE)],&[base]);f.branches(ours,theirs);
        let before=f.state();let report=f.preview();assert_eq!(before,f.state());
        let item=report.items.iter().find(|i|i.path=="dep").unwrap();assert_eq!(item.kind,"submodule");assert_eq!(item.result.as_ref().unwrap().oid,b.to_string());
        assert_eq!(item.outcome,"incoming");assert!(item.warnings.is_empty());assert!(!f.root.join("dep/.git").exists());assert!(!f.repo.path().join("modules").exists());
    }

    #[test]
    fn orphan_pointer_is_reported_from_simulated_gitmodules_not_staged_copy() {
        let f=Fixture::new();let oid=Oid::from_str(&"1".repeat(40)).unwrap();let config=f.blob("[submodule \"dep\"]\npath = dep\nurl = ../dep.git\n");
        let base=f.commit(&[(".gitmodules",config,FILE)],&[]);
        let ours=f.commit(&[(".gitmodules",config,FILE),("dep",oid,GITLINK)],&[base]);
        let incoming=f.commit(&[],&[base]);f.branches(ours,incoming);
        let before=f.state();let report=f.preview();assert_eq!(before,f.state());
        let item=report.items.iter().find(|i|i.path=="dep").unwrap();assert_eq!(item.outcome,"current");assert_eq!(item.change,"unchanged");assert_eq!(item.warnings.len(),1);
        assert!(fs::read_to_string(f.root.join(".gitmodules")).unwrap().contains("path = dep"));
    }

    #[test]
    fn current_only_addition_is_not_an_incoming_deletion_even_for_submodules() {
        let f=Fixture::new();let link=Oid::from_str(&"c".repeat(40)).unwrap();let text=f.blob("own content\n");
        let config=f.blob("[submodule \"dep\"]\npath=dep\nurl=../dep.git\n");
        let base=f.commit(&[],&[]);
        let ours=f.commit(&[("dep",link,GITLINK),(".gitmodules",config,FILE),("own-file",text,FILE)],&[base]);
        let theirs=f.commit(&[("incoming-file",text,FILE)],&[base]);f.branches(ours,theirs);
        let before=f.state();let report=f.preview();assert_eq!(before,f.state());
        for path in ["dep","own-file"] {
            let row=report.items.iter().find(|i|i.path==path).unwrap();
            assert!(row.base.is_none());assert!(row.incoming.is_none());assert!(row.current.is_some());
            assert_eq!(row.result,row.current);assert_eq!(row.outcome,"current");assert_eq!(row.change,"unchanged");
            assert!(row.warnings.is_empty());
        }
    }

    #[test]
    fn unchanged_orphan_warning_keeps_real_base_and_symmetric_outcome() {
        let f=Fixture::new();let link=Oid::from_str(&"c".repeat(40)).unwrap();let text=f.blob("text\n");
        let base=f.commit(&[("dep",link,GITLINK)],&[]);
        let ours=f.commit(&[("dep",link,GITLINK),("own",text,FILE)],&[base]);
        let theirs=f.commit(&[("dep",link,GITLINK),("incoming",text,FILE)],&[base]);f.branches(ours,theirs);
        for up_to_date in [false,true] {
            if up_to_date {f.branches(ours,base);}
            let before=f.state();let report=f.preview();assert_eq!(before,f.state());
            let row=report.items.iter().find(|i|i.path=="dep").unwrap();
            assert_eq!(row.base.as_ref().unwrap().oid,link.to_string());
            assert_eq!(row.current,row.base);assert_eq!(row.incoming,row.base);assert_eq!(row.result,row.base);
            assert_eq!(row.outcome,"both");assert_eq!(row.change,"unchanged");assert_eq!(row.warnings.len(),1);
        }
    }

    #[test]
    fn result_contents_preserve_generated_merge_and_all_other_comparison_pairs() {
        let f=Fixture::new();let base_text="a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let our_text="OUR\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";let their_text="a\nb\nc\nd\ne\nf\ng\nh\ni\nTHEIR\n";
        let base=f.commit(&[("file",f.blob(base_text),FILE)],&[]);
        let ours=f.commit(&[("file",f.blob(our_text),FILE)],&[base]);
        let theirs=f.commit(&[("file",f.blob(their_text),FILE)],&[base]);f.branches(ours,theirs);
        let before=f.state();let report=f.preview();let row=report.items.iter().find(|i|i.path=="file").unwrap();
        let versions=vec![row.base.clone(),row.current.clone(),row.incoming.clone(),row.result.clone()];
        let contents=tauri::async_runtime::block_on(merge_preview_contents(f.root(),versions)).unwrap();
        assert_eq!(contents[0].content.as_deref(),Some(base_text));assert_eq!(contents[1].content.as_deref(),Some(our_text));
        assert_eq!(contents[2].content.as_deref(),Some(their_text));
        assert_eq!(contents[3].content.as_deref(),Some("OUR\nb\nc\nd\ne\nf\ng\nh\ni\nTHEIR\n"));
        for (a,b) in [(1,3),(1,2),(0,1),(0,2)] {assert_ne!(contents[a].content,contents[b].content);}
        assert_eq!(before,f.state());
    }

    #[test]
    fn mode_only_and_empty_file_changes_have_real_effects_without_text_differences() {
        let f=Fixture::new();let text=f.blob("unchanged text\n");let empty=f.blob("");
        let base=f.commit(&[("file",text,FILE),("removed-empty",empty,FILE)],&[]);
        let ours=f.commit(&[("file",text,FILE),("removed-empty",empty,FILE),("own",text,FILE)],&[base]);
        let theirs=f.commit(&[("file",text,0o100755),("added-empty",empty,FILE)],&[base]);f.branches(ours,theirs);
        let report=f.preview();
        for (path,change) in [("file","modified"),("added-empty","added"),("removed-empty","removed")] {
            let row=report.items.iter().find(|i|i.path==path).unwrap();assert_eq!(row.change,change);assert_ne!(row.current,row.result);
            let contents=tauri::async_runtime::block_on(merge_preview_contents(f.root(),vec![row.current.clone(),row.result.clone()])).unwrap();
            assert_eq!(contents[0].content,contents[1].content);
        }
    }

    #[test]
    fn current_deletion_is_kept_but_incoming_edit_makes_it_a_conflict() {
        let f=Fixture::new();let text=f.blob("base\n");let edited=f.blob("edited\n");
        let base=f.commit(&[("kept-deletion",text,FILE),("conflicted-deletion",text,FILE)],&[]);
        let ours=f.commit(&[("own",f.blob("unrelated new file\n"),FILE)],&[base]);
        let theirs=f.commit(&[("kept-deletion",text,FILE),("conflicted-deletion",edited,FILE)],&[base]);f.branches(ours,theirs);
        let report=f.preview();let kept=report.items.iter().find(|i|i.path=="kept-deletion").unwrap();
        assert!(kept.current.is_none());assert!(kept.result.is_none());assert_eq!(kept.base,kept.incoming);
        assert_eq!(kept.outcome,"current");assert_eq!(kept.change,"unchanged");
        let conflict=report.items.iter().find(|i|i.path=="conflicted-deletion").unwrap();
        assert_eq!(conflict.outcome,"conflict");assert!(conflict.result.is_none());
    }

    #[test]
    fn folder_submodule_conversions_report_directories_not_absence_in_both_directions() {
        for current_converts in [true,false] {
            for base_is_folder in [true,false] {
                let f=Fixture::new();let text=f.blob("folder file\n");let link=Oid::from_str(&"c".repeat(40)).unwrap();
                let config=f.blob("[submodule \"dep\"]\npath=dep\nurl=../dep.git\n");
                let folder=vec![("dep/file",text,FILE)];let sub=vec![("dep",link,GITLINK),(".gitmodules",config,FILE)];
                let (initial,converted)=if base_is_folder {(&folder,&sub)} else {(&sub,&folder)};
                let base=f.commit(initial,&[]);let mut unchanged=initial.clone();unchanged.push(("independent",f.blob("unrelated\n"),FILE));
                let converts=f.commit(converted,&[base]);let other=f.commit(&unchanged,&[base]);
                let (ours,theirs)=if current_converts {(converts,other)} else {(other,converts)};f.branches(ours,theirs);
                let before=f.state();let report=f.preview();assert_eq!(before,f.state());assert_eq!(report.status,"clean");
                let row=report.items.iter().find(|i|i.path=="dep").unwrap();
                let has=|side:&str|row.directory_sides.iter().any(|s|s==side);
                assert_eq!(has("base"),base_is_folder);
                assert_eq!(has("current"),base_is_folder!=current_converts);
                assert_eq!(has("incoming"),base_is_folder==current_converts);
                assert_eq!(has("result"),!base_is_folder);
                assert_eq!(row.outcome,if current_converts {"current"} else {"incoming"});
            }
        }
    }

    #[test]
    fn diverged_submodule_pointers_are_not_guessed_when_child_history_is_missing() {
        let f=Fixture::new();let a=Oid::from_str(&"1".repeat(40)).unwrap();let b=Oid::from_str(&"2".repeat(40)).unwrap();let c=Oid::from_str(&"3".repeat(40)).unwrap();
        let config=f.blob("[submodule \"dep\"]\npath = dep\nurl = ../dep.git\n");
        let base=f.commit(&[("dep",a,GITLINK),(".gitmodules",config,FILE)],&[]);
        let ours=f.commit(&[("dep",b,GITLINK),(".gitmodules",config,FILE)],&[base]);let theirs=f.commit(&[("dep",c,GITLINK),(".gitmodules",config,FILE)],&[base]);
        f.branches(ours,theirs);let report=f.preview();let item=report.items.iter().find(|i|i.path=="dep").unwrap();
        assert_eq!(item.outcome,"conflict");assert!(item.result.is_none());assert_eq!(report.status,"conflicts");
    }

    #[test]
    fn moved_source_and_unrelated_history_fail_without_starting_merge() {
        let f=Fixture::new();let a=f.blob("base");let base=f.commit(&[("f",a,FILE)],&[]);f.branches(base,base);
        assert!(preview(f.root(),String::new(),"incoming".into(),Some("0".repeat(40))).unwrap_err().contains("moved"));
        let other=f.commit(&[],&[]);f.repo.reference("refs/heads/incoming",other,true,"fixture").unwrap();
        assert!(preview(f.root(),String::new(),"incoming".into(),None).unwrap_err().contains("ancestry"));assert_eq!(f.repo.state(),git2::RepositoryState::Clean);
    }

    #[test]
    fn content_reads_are_bounded_and_report_binary_absent_and_wrong_object_types() {
        let f=Fixture::new();let text=f.blob("text\n");let binary=f.repo.blob(b"a\0b").unwrap();let big=f.repo.blob(&vec![b'x';MAX_TEXT+1]).unwrap();
        let versions=vec![Some(version("f",text,FILE)),Some(version("f",binary,FILE)),Some(version("f",big,FILE)),None];
        let result=tauri::async_runtime::block_on(merge_preview_contents(f.root(),versions)).unwrap();
        assert_eq!(result[0].content.as_deref(),Some("text\n"));assert!(result[1].note.contains("Binary"));assert!(result[2].note.contains("limit"));assert_eq!(result[3].content.as_deref(),Some(""));
        let commit=f.commit(&[("f",text,FILE)],&[]);
        assert!(tauri::async_runtime::block_on(merge_preview_contents(f.root(),vec![Some(version("f",commit,FILE))])).is_err());
    }

    #[test]
    fn clean_rename_and_submodule_to_folder_conversion_match_actual_merge() {
        let f=Fixture::new();let a=f.blob("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");let edit=f.blob("a\nb\nc\nd\ne\nf\ng\nh\ni\nEDIT\n");
        let link=Oid::from_str(&"1".repeat(40)).unwrap();let config=f.blob("[submodule \"dep\"]\npath=dep\nurl=../dep.git\n");
        let base=f.commit(&[("old",a,FILE),("dep",link,GITLINK),(".gitmodules",config,FILE)],&[]);
        let ours=f.commit(&[("renamed",a,FILE),("dep",link,GITLINK),(".gitmodules",config,FILE)],&[base]);
        let theirs=f.commit(&[("old",edit,FILE),("dep/file",a,FILE)],&[base]);f.branches(ours,theirs);
        let before=f.state();let report=f.preview();assert_eq!(before,f.state());assert_eq!(report.status,"clean");
        native::text(&f.root(),&["merge","--no-ff","--no-commit","-s","ort","incoming"]).unwrap();
        let actual=native::index(&f.root()).unwrap();assert!(!actual.conflicted());
        for item in &report.items {assert_eq!(actual.get(&item.path,0).map(|e| version(&e.path,e.id,e.mode)),item.result,"{}",item.path);}
        assert_eq!(report.items.iter().find(|i|i.path=="dep").unwrap().change,"removed");
        assert_eq!(report.items.iter().find(|i|i.path=="dep/file").unwrap().change,"added");
        assert_eq!(actual.get("renamed",0).unwrap().id,edit);
    }

    #[test]
    fn conflicting_gitmodules_is_not_reported_as_a_verified_registration() {
        let f=Fixture::new();let link=Oid::from_str(&"1".repeat(40)).unwrap();
        let config=|url|f.blob(&format!("[submodule \"dep\"]\npath=dep\nurl={url}\n"));
        let base=f.commit(&[("dep",link,GITLINK),(".gitmodules",config("old"),FILE)],&[]);
        let ours=f.commit(&[("dep",link,GITLINK),(".gitmodules",config("ours"),FILE)],&[base]);
        let theirs=f.commit(&[("dep",link,GITLINK),(".gitmodules",config("theirs"),FILE)],&[base]);f.branches(ours,theirs);
        let report=f.preview();assert_eq!(report.status,"conflicts");
        assert!(report.warnings.iter().any(|w|w.contains("registration could not be verified")));
        assert_eq!(report.items.iter().find(|i|i.path==".gitmodules").unwrap().outcome,"conflict");
    }

    #[test]
    fn explicit_submodule_target_uses_child_history_without_touching_parent() {
        let parent=Fixture::new();
        let config=parent.blob("[submodule \"dep\"]\npath=dep\nurl=../dep.git\n");
        let parent_tip=parent.commit(&[("dep",Oid::from_str(&"1".repeat(40)).unwrap(),GITLINK),(".gitmodules",config,FILE)],&[]);parent.branches(parent_tip,parent_tip);
        // Create the child after the parent fixture checkout so fixture setup
        // cannot disturb an already-open child's metadata.
        let root=parent.root.join("dep");fs::create_dir_all(&root).unwrap();
        let child=Fixture{repo:Repository::init(&root).unwrap(),root};let text=child.blob("old");let updated=child.blob("new");
        let base=child.commit(&[("inside",text,FILE)],&[]);let tip=child.commit(&[("inside",updated,FILE)],&[base]);child.branches(base,tip);
        assert!(child.repo.head().is_ok(),"fixture lost child HEAD before preview");
        let parent_before=parent.state();let child_before=child.state();
        let report=preview(parent.root(),"dep".into(),"incoming".into(),Some(tip.to_string())).unwrap();
        assert_eq!(report.repository_path,child.root());assert_eq!(report.current_revision,base.to_string());assert_eq!(report.status,"fast_forward");
        assert_eq!(report.items[0].path,"inside");assert_eq!(parent_before,parent.state());assert_eq!(child_before,child.state());
        assert!(preview(parent.root(),"missing".into(),"incoming".into(),None).is_err());
    }

    #[test]
    fn multiple_bases_are_explicit_and_still_use_the_real_engine_result() {
        let f=Fixture::new();let a=f.blob("a");let b=f.blob("b");
        let base=f.commit(&[("one",a,FILE),("two",a,FILE)],&[]);
        let a1=f.commit(&[("one",b,FILE),("two",a,FILE)],&[base]);let b1=f.commit(&[("one",a,FILE),("two",b,FILE)],&[base]);
        let ours=f.commit(&[("one",b,FILE),("two",b,FILE),("ours",a,FILE)],&[a1,b1]);
        let theirs=f.commit(&[("one",b,FILE),("two",b,FILE),("theirs",a,FILE)],&[b1,a1]);f.branches(ours,theirs);
        let before=f.state();let report=f.preview();assert_eq!(before,f.state());assert_eq!(report.merge_bases.len(),2);assert_eq!(report.status,"clean");
        assert!(report.items.iter().all(|i|i.base.is_none()));assert!(report.warnings.iter().any(|w|w.contains("Multiple merge bases")));
        native::text(&f.root(),&["merge","--no-ff","--no-commit","-s","ort","incoming"]).unwrap();
        let actual=native::index(&f.root()).unwrap();for item in report.items {assert_eq!(actual.get(&item.path,0).map(|e| version(&e.path,e.id,e.mode)),item.result);}
    }

    #[test]
    fn detached_preview_is_explicit_and_in_progress_or_shallow_history_is_rejected() {
        let f=Fixture::new();let a=f.blob("a");let b=f.blob("b");
        let base=f.commit(&[("f",a,FILE)],&[]);let tip=f.commit(&[("f",b,FILE)],&[base]);f.branches(base,tip);
        f.repo.set_head_detached(base).unwrap();let before=f.state();let report=f.preview();assert_eq!(before,f.state());
        assert_eq!(report.current_branch,"Detached HEAD");assert!(report.warnings.iter().any(|w|w.contains("local branch")));
        fs::write(f.repo.path().join("MERGE_HEAD"),format!("{tip}\n")).unwrap();
        assert!(preview(f.root(),String::new(),"incoming".into(),None).unwrap_err().contains("already in progress"));
        fs::remove_file(f.repo.path().join("MERGE_HEAD")).unwrap();
        fs::write(f.repo.path().join("shallow"),format!("{base}\n")).unwrap();
        assert!(preview(f.root(),String::new(),"incoming".into(),None).unwrap_err().contains("complete local ancestry"));
    }
}
