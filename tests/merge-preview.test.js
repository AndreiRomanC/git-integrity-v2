'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const api = require('../frontend/merge-preview.js');
const app = fs.readFileSync(require.resolve('../frontend/app.js'), 'utf8');
const source = name => app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))[0];
const version = (id, mode=0o100644, path='file') => ({oid:id.repeat(40),mode,path});
const item = (path,outcome='incoming',change='modified',kind='file') => ({path,kind,outcome,change,
  base:version('a',0o100644,path),current:version('a',0o100644,path),incoming:version('b',0o100644,path),result:version('b',0o100644,path),warnings:[]});
const report = (items=[item('file')]) => ({items,repository_path:'/repo',current_branch:'feature',source_ref:'origin/main',
  current_revision:'a'.repeat(40),incoming_revision:'b'.repeat(40),merge_bases:['c'.repeat(40)],status:'clean',warnings:[],elapsed_ms:20});
function node() {
  return {handlers:{},open:false,innerHTML:'',textContent:'',value:'',isConnected:true,disabled:false,
    addEventListener(name,fn){this.handlers[name]=fn;},replaceChildren(){this.innerHTML='';},
    showModal(){this.open=true;},close(){this.open=false;this.handlers.close?.();},querySelectorAll(){return [];},
    querySelector(selector){this.children ||= {};return this.children[selector] ||= node();}};
}
function harness() {
  const nodes={},pending=[],files=[],dialog=node(); let active=true;
  dialog.querySelector=selector=>nodes[selector] ||= node();
  const controller=api.create({dialog,invoke:(command,args)=>new Promise((resolve,reject)=>pending.push({command,args,resolve,reject})),
    current:()=>active,onFile:(...args)=>files.push(args)});
  const el=name=>dialog.querySelector(`[data-preview-${name}]`);
  return {controller,pending,files,dialog,el,setActive:value=>{active=value;}};
}
const target={repositoryPath:'/repo',targetPath:'modules/dep',sourceRef:'origin/main',expectedIncoming:'b'.repeat(40)};
const click=(selector,element)=>({target:{closest:value=>value===selector?element:null}});

test('report totals reconcile; automatic combinations and submodules are subsets, not extra items',()=>{
  const items=[item('in'),item('auto','combined'),item('own','current','unchanged'),item('conflict','conflict','unresolved'),item('dep','incoming','added','submodule')];
  items[4].warnings=['Missing registration'];
  const counts=api.counts(items);
  assert.equal(counts.changes+counts.kept+counts.conflicts,items.length);
  assert.equal(counts.combined,1);assert.equal(counts.submodules,1);assert.equal(counts.review,2);
  assert.deepEqual(api.filtered(items,'','changes').map(i=>i.path),['in','auto','dep']);
  assert.deepEqual(api.filtered(items,'','review').map(i=>i.path),['conflict','dep']);
  assert.deepEqual(api.filtered(items,'DEP','submodules').map(i=>i.path),['dep']);
  assert.deepEqual(api.filtered(items,'','conflicts').map(i=>i.path),['conflict']);
});
test('reasons explain older unchanged versions without attributing authorship',()=>{
  assert.match(api.reason(item('f'),report()),/did not change.*common base.*incoming version/);
  const keep=item('f','current','unchanged');keep.incoming=keep.base;keep.current=keep.result;
  assert.match(api.reason(keep,report()),/Incoming branch did not change.*current version is kept/);
  const removed=item('f');removed.result=null;
  assert.match(api.reason(removed,report()),/removes this path/);
  assert.doesNotMatch(api.reason(item('f'),{merge_bases:['one','two']}),/did not change/);
  assert.match(api.reason(item('f','combined'),report()),/does not guarantee functional correctness/);
});
test('conflicts never show a fictitious chosen version; paths, refs and warnings are escaped',()=>{
  const row=item('<script>','conflict','unresolved','submodule');row.result=null;row.conflict='submodule pointer';row.warnings=['<unsafe>'];
  const html=api.rowHtml(row,0,report());
  assert.match(html,/Unresolved/);assert.match(html,/No final version chosen/);
  assert.match(html,/parent’s recorded pointer/);assert.match(html,/Inspect current \/ incoming/);
  assert.match(html,/&lt;script&gt;/);assert.match(html,/&lt;unsafe&gt;/);assert.doesNotMatch(html,/<script>|View result diff/);
  assert.match(api.rowHtml(item('f'),2,report()),/data-preview-file="2" data-pair="result"/);
});
test('rename aliases are searchable and multiple merge bases are not presented as absent',()=>{
  const row=item('new');row.base.path='old/path';
  assert.equal(api.filtered([row],'OLD/PATH','all').length,1);
  assert.match(api.rowHtml(row,0,{...report(),merge_bases:['one','two']}),/Multiple bases/);
});
test('only explicit opening/recalculation requests the backend; filters use the existing report',async()=>{
  const h=harness();assert.equal(h.pending.length,0);
  const opened=h.controller.open(target,{});assert.equal(h.pending[0].command,'preview_merge');assert.deepEqual(h.pending[0].args,target);
  h.pending[0].resolve(report([item('one'),item('two')]));await opened;
  h.el('search').value='two';h.el('search').handlers.input();
  assert.match(h.el('list').innerHTML,/data-preview-file="1"/);assert.doesNotMatch(h.el('list').innerHTML,/data-preview-file="0"/);
  h.el('filter').value='conflicts';h.el('filter').handlers.change();assert.equal(h.pending.length,1);
  const refresh=h.el('refresh').handlers.click();assert.equal(h.pending[1].args.expectedIncoming,null);
  h.pending[1].resolve(report());await refresh;
});
test('closed, replaced or navigated-away requests cannot install stale results',async()=>{
  const h=harness();const first=h.controller.open(target,{}),second=h.controller.open(target,{});
  h.pending[1].resolve(report([item('latest')]));await second;h.pending[0].resolve(report([item('stale')]));await first;
  assert.match(h.el('list').innerHTML,/latest/);assert.doesNotMatch(h.el('list').innerHTML,/stale/);
  const next=h.controller.open(target,{});h.dialog.close();h.pending[2].resolve(report([item('closed')]));await next;
  assert.doesNotMatch(h.el('list').innerHTML,/closed/);
  const changed=h.controller.open(target,{});h.setActive(false);h.pending[3].resolve(report([item('wrong repo')]));await changed;
  assert.match(h.el('list').innerHTML,/context changed/);assert.equal(h.el('search').disabled,true);
});
test('backend errors are explicit, escaped and retryable',async()=>{
  const h=harness();const opened=h.controller.open(target,{});h.pending[0].reject('Missing <object>');await opened;
  assert.match(h.el('list').innerHTML,/Missing &lt;object&gt;/);assert.equal(h.el('refresh').disabled,false);
  assert.equal(h.el('filter').disabled,true);assert.match(h.el('summary').textContent,/no result applied/);
});
test('long reports render a bounded first page while searching all items',async()=>{
  const h=harness();const opened=h.controller.open(target,{});
  h.pending[0].resolve(report(Array.from({length:2300},(_,i)=>item(`path-${i}`))));await opened;
  assert.equal((h.el('list').innerHTML.match(/<details/g)||[]).length,200);assert.match(h.el('list').innerHTML,/data-preview-more/);
  await h.el('list').handlers.click(click('[data-preview-more]',{}));assert.equal((h.el('list').innerHTML.match(/<details/g)||[]).length,400);
  h.el('search').value='path-2299';h.el('search').handlers.input();assert.match(h.el('list').innerHTML,/data-preview-file="2299"/);
  assert.equal(h.pending.length,1);
});
test('file inspection preserves snapshot identity and becomes invalid when closed',async()=>{
  const h=harness();const opened=h.controller.open(target,{});h.pending[0].resolve(report());await opened;
  await h.el('list').handlers.click(click('[data-preview-file]',{dataset:{previewFile:'0',pair:'result'}}));
  assert.equal(h.files.length,1);assert.equal(h.files[0][2],'result');assert.equal(h.files[0][3](),true);
  h.dialog.close();assert.equal(h.files[0][3](),false);
});
test('submodule metadata is opt-in and uses pinned parent commits, not moving branch names',async()=>{
  const h=harness();const opened=h.controller.open(target,{});h.pending[0].resolve(report([item('dep','incoming','modified','submodule')]));await opened;
  assert.equal(h.pending.length,1);
  global.SubmoduleCompareDetails={detailsHtml:()=>'<p>metadata</p>'};
  const button={dataset:{previewSubmodule:'0'}};
  const clickPromise=h.el('list').handlers.click(click('[data-preview-submodule]',button));
  assert.equal(h.pending[1].command,'submodule_comparison_details');assert.equal(h.pending[1].args.relativePath,'dep');
  assert.equal(h.pending[1].args.leftRef,'a'.repeat(40));assert.equal(h.pending[1].args.rightRef,'b'.repeat(40));
  h.pending[1].resolve({});await clickPromise;assert.equal(button.disabled,false);
});
test('graph preview keeps submodule scope and rejects stale menu actions',()=>{
  const c={state:{repository:{path:'/parent'}},repoOpenGeneration:4,activeGraphMergeTarget:()=>({targetPath:'dep',label:'dep'}),
    activeGraphData:()=>({path:'/parent/dep'}),openMergePreview:(...args)=>{c.args=args;}};
  vm.createContext(c);vm.runInContext(source('graphMergePreviewMenuItem'),c);
  const menu=c.graphMergePreviewMenuItem('origin/main','preview-1','sha');menu.run();assert.equal(c.args[0].targetPath,'dep');assert.equal(c.args[2],'sha');
  c.args=null;c.repoOpenGeneration++;menu.run();assert.equal(c.args,null);
  assert.match(app,/graphMergeMenuItem\(ref.name, `merge-\$\{index\}`\), graphMergePreviewMenuItem/);
});
test('preview file comparison has no write actions, reads immutable objects and rejects stale loads',async()=>{
  let resolve;const c={state:{},refs:{compareDialog:node(),compareTitle:node(),compareSubtitle:node(),localCompare:node(),remoteCompare:node()},
    openFileCompareGuard:()=>()=>true,resetCompareDiffNavigation:message=>{c.diffStatus=message;},setCompareHeadLabels(){},
    setCompareReadOnly:value=>{c.readOnly=value;},invoke:(command,args)=>{c.command=command;c.args=args;return new Promise(r=>{resolve=r;});},
    renderComparisonContents:(...contents)=>{c.rendered=contents;}};
  vm.createContext(c);vm.runInContext(source('openMergePreviewFile'),c);let active=true;
  const first=c.openMergePreviewFile(report(),item('f'),'result',()=>active);
  assert.equal(c.readOnly,true);assert.equal(c.command,'merge_preview_contents');assert.equal(c.args.versions[1].oid,'b'.repeat(40));
  active=false;resolve([{content:'old'},{content:'new'}]);await first;assert.equal(c.rendered,undefined);
  active=true;const second=c.openMergePreviewFile(report(),item('f'),'result',()=>active);resolve([{content:'old'},{content:'new'}]);await second;
  assert.deepEqual(Array.from(c.rendered),['old','new']);
  c.rendered=null;const binary=c.openMergePreviewFile(report(),item('f'),'result',()=>active);resolve([{content:null,note:'Binary file'},{content:'text',note:''}]);await binary;
  assert.match(c.diffStatus,/unavailable/);assert.equal(c.rendered,null);assert.equal(c.refs.localCompare.textContent,'Binary file');
});
