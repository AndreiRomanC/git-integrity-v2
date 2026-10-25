const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const helpers = require('../frontend/comparison-authors.js');
const {comparisonViewRows, createComparisonAuthorCache, scopedComparisonAuthorContext, loadComparisonLeafScope, comparisonAuthorPartition} = helpers;
const app = fs.readFileSync(require('node:path').join(__dirname,'../frontend/app.js'),'utf8');
const source = name => app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))[0];
const context = {repositoryPath:'/repo',leftRevision:'a',rightRevision:'b'};
const entry = (path,kind='file') => ({relative_path:path, name:path.split('/').at(-1), kind});
const row = (path,kind='file',status='modified') => ({relative_path:path,name:path.split('/').at(-1),local:entry(path,kind),remote:entry(path,kind),status});
const leaves = [row('src/deep/shared'),row('src/solo'),row('src2/other'),row('vendor/unit','submodule'),row('root')];
const directories = [row('src','folder'),row('src2','folder'),row('vendor','folder'),row('root'),row('unchanged','folder','same')];
const authors = [
  {key:'alice',paths:['src/deep/shared','src/solo','vendor/unit']},
  {key:'bob',paths:['src/deep/shared','src2/other']},
];
const paths = rows => Array.from(rows,r=>r.relative_path);

test('every mode projects the exact List results onto ancestors, with no prefix collisions', async () => {
  const cache=createComparisonAuthorCache(async command => command === 'comparison_introductions'
    ? [{key:'integrator',paths:['src/solo','vendor/unit']}] : authors);
  const model=cache.get(scopedComparisonAuthorContext(context,leaves)); await cache.load(model); await cache.loadIntroductions(model);
  for (const [mode,author,expected,folders] of [
    ['touched','alice',['src/deep/shared','src/solo','vendor/unit'],['src','vendor']],
    ['shared','', ['src/deep/shared'],['src']],
    ['introduced','integrator',['src/solo','vendor/unit'],['src','vendor']],
  ]) {
    if (author) cache.select(model,author,mode); else cache.setMode(model,mode);
    const list=comparisonViewRows(directories,leaves,model,{flat:true});
    const tree=comparisonViewRows(directories,leaves,model);
    assert.deepEqual(paths(list.rows),expected); assert.deepEqual(paths(tree.rows),folders);
    assert.deepEqual(paths(tree.matching),paths(list.rows));
    assert.equal(tree.descendants.get('src'), expected.filter(p=>p.startsWith('src/')).length);
    assert.ok(!tree.rows.includes(directories[1]),'src does not match src2');
  }
});

test('Shared never combines authors of DIFFERENT child files to qualify a folder', async () => {
  const cache=createComparisonAuthorCache(async()=>[{key:'a',paths:['src/one']},{key:'b',paths:['src/two']}]);
  const model=cache.get(context);await cache.load(model);cache.setMode(model,'shared');
  assert.deepEqual(comparisonViewRows([row('src','folder')],[row('src/one'),row('src/two')],model).rows,[]);
});

test('submodule pointers stay atomic; folder conversions retain containers for actual matching descendants', async () => {
  const cache=createComparisonAuthorCache(async()=>[{key:'a',paths:['unit/file','vendor/unit']}]);
  const model=cache.get(context);await cache.load(model);cache.select(model,'a');
  const conversion={...row('unit','submodule','type-changed'),remote:entry('unit','folder')};
  const tree=comparisonViewRows([conversion,row('vendor','folder')],[row('unit/file'),row('vendor/unit','submodule')],model);
  assert.deepEqual(paths(tree.rows),['unit','vendor']);
  assert.equal(tree.descendants.get('unit'),1);
  assert.equal(tree.descendants.has('vendor/unit'),false,'a pointer does not fabricate internal files');
});

test('strict uses the same leaf population and keeps excluded/unanalysed items distinct', async () => {
  const cache=createComparisonAuthorCache(async command=> command==='comparison_authors' ? authors : {
    paths:['src/solo'],no_contribution_paths:['src/deep/shared'],ambiguous_paths:{'vendor/unit':'Missing object'},
  });
  const model=cache.get(context);await cache.load(model);cache.select(model,'alice');cache.setDirectOnly(model,true);await cache.loadStrict(model);
  const tree=comparisonViewRows(directories,leaves,model), list=comparisonViewRows(directories,leaves,model,{flat:true});
  assert.deepEqual(paths(tree.rows),['src']);assert.deepEqual(paths(list.rows),['src/solo']);
  const counts=comparisonAuthorPartition(tree.candidates,model);
  assert.equal(counts.total,3);assert.equal(counts.shown,1);assert.equal(counts.excluded,2);
  assert.equal(counts.inconclusive[0].reason,'Missing object');
  assert.deepEqual(paths(counts.noContribution.map(x=>x.row)),['src/deep/shared']);
});

test('search and change type filter leaves before projection; counts are items, not folder rows', async () => {
  const cache=createComparisonAuthorCache(async()=>authors), model=cache.get(context);await cache.load(model);cache.select(model,'alice');
  const filteredLeaves=leaves.map((r,i)=>({...r,status:i===0?'remote-only':r.status}));
  const tree=comparisonViewRows(directories,filteredLeaves,model,{query:'shared',filter:'remote-only'});
  assert.deepEqual(paths(tree.rows),['src']);assert.deepEqual(paths(tree.matching),['src/deep/shared']);
  assert.deepEqual(comparisonViewRows(directories,filteredLeaves,model,{query:'shared',filter:'local-only'}).rows,[]);
  assert.deepEqual(paths(comparisonViewRows([row('src/deep','folder'),row('src/solo')],filteredLeaves,model).rows),['src/deep','src/solo']);
});

test('aliases count once per ancestor; Git path casing and component boundaries are preserved', () => {
  const alias={...row('src/new'),local:entry('src/old')};
  const model={mode:'touched',paths:new Set(['src/new','SRC/one'])};
  const tree=comparisonViewRows([row('src','folder'),row('src2','folder'),row('SRC','folder')],[alias,row('SRC/one'),row('src2/no')],model);
  assert.deepEqual(paths(tree.rows),['src','SRC']);assert.equal(tree.descendants.get('src'),1);
});

test('default tree preserves identical entries and requires no leaf enumeration', () => {
  assert.equal(comparisonViewRows(directories,null,null).rows,directories);
  const pending={mode:'shared',authors:null,paths:new Set()};
  assert.deepEqual(comparisonViewRows(directories,null,pending).rows,[]);
  assert.deepEqual(comparisonViewRows(directories,[],pending).rows,[]);
});

test('leaf loading is explicit, deduplicated, and late completion belongs only to its snapshot', async () => {
  const old={rows:null},current={rows:null};let resolve,calls=0;
  const load=()=>{calls++;return new Promise(done=>resolve=done);};
  const one=loadComparisonLeafScope(old,load),two=loadComparisonLeafScope(old,load);
  await Promise.resolve();assert.equal(calls,1);
  resolve({rows:leaves});await Promise.all([one,two]);
  assert.equal(current.rows,null);assert.equal(old.rows,leaves);
  await loadComparisonLeafScope(old,()=>assert.fail('must reuse snapshot'));assert.equal(calls,1);
});

test('leaf errors never become empty successful attribution and retry is explicit', async () => {
  const scope={rows:null};let calls=0;
  const load=async()=>{if(++calls===1)throw Error('missing tree');return {rows:[]};};
  await loadComparisonLeafScope(scope,load);assert.equal(scope.rows,null);assert.match(scope.error,/missing tree/);
  await loadComparisonLeafScope(scope,load);assert.equal(calls,1);
  await loadComparisonLeafScope(scope,load,true);assert.deepEqual(scope.rows,[]);assert.equal(scope.error,'');
});

function scopeRenderer(invoke) {
  const controls=new Map();
  const element={html:'',set innerHTML(value){this.html=value;controls.clear();},get innerHTML(){return this.html;},
    querySelector(selector){
      if(!this.html.includes(selector.slice(1,-1)))return null;
      if(!controls.has(selector)) controls.set(selector,{addEventListener(_,fn){this.click=fn;}});
      return controls.get(selector);
    }};
  const container={classList:{add(){}},querySelector:()=>element};
  const cache=createComparisonAuthorCache(invoke);
  const h={...helpers,comparisonAuthorCache:cache,invoke,esc:String,state:{view:'commander'},renderCommander(){},
    renderComparisonAuthorFilter(_,c,opts){
      const model=c?cache.get(scopedComparisonAuthorContext(c,opts.scopeRows)):null;
      if(opts.loadHistory && !model.authors && !model.pending && !model.error)cache.load(model);
      return model;
    }};
  vm.createContext(h);vm.runInContext(source('renderComparisonScopedFilter'),h);
  const render=(snapshot,opts={},c=context)=>h.renderComparisonScopedFilter(container,c,snapshot,{flat:false,query:'',filter:'all',path:'src',sourceRows:directories,...opts});
  return {h,cache,element,render};
}
const settle=()=>new Promise(resolve=>setImmediate(resolve));

test('tree controls load exact local leaf scope only on demand, then history once', async () => {
  const calls=[];const ui=scopeRenderer(async(cmd,args)=>{calls.push([cmd,args]);return cmd==='comparison_authors'?authors:{rows:leaves};});
  const snapshot={repository_path:'/repo'};
  ui.render(snapshot);ui.render(snapshot);assert.equal(calls.length,0);
  ui.element.querySelector('[data-load-scope]').click();await settle();
  const result=ui.render(snapshot);await settle();ui.render(snapshot);
  assert.deepEqual(calls.map(c=>c[0]),['compare_git_revisions_file_list','comparison_authors']);
  assert.deepEqual(JSON.parse(JSON.stringify(calls[0][1])),{repositoryPath:'/repo',relativePath:'src',leftRef:'a',rightRef:'b'});
  assert.deepEqual(calls[1][1].comparisonPaths,leaves.map(r=>r.relative_path).sort());
  assert.equal(result.scopeRows,leaves);
});

test('text and type filters enumerate leaves once, never start author/strict/integration scans', async () => {
  const calls=[];const ui=scopeRenderer(async(cmd)=>{calls.push(cmd);return {rows:leaves};}),snapshot={repository_path:'/repo'};
  const pending=ui.render(snapshot,{query:'deep'});assert.match(pending.scopeMessage,/Loading/);
  await settle();ui.render(snapshot,{query:'dee'});ui.render(snapshot,{filter:'remote-only'});await settle();
  assert.deepEqual(calls,['compare_git_revisions_file_list']);
});

test('submodule/bare scope enumeration uses the already resolved local repository, never fetch/cache-ensure', async () => {
  for(const c of [{...context,submodulePath:'unit'},{...context,owner:'org',repositoryName:'unit'}]) {
    const calls=[],ui=scopeRenderer(async(cmd,args)=>{calls.push([cmd,args]);return {rows:[]};});
    const snapshot={repositoryPath:'/local/child-or-bare'};ui.render(snapshot,{filter:'modified'},c);await settle();
    assert.equal(calls[0][0],'compare_git_revisions_file_list');assert.equal(calls[0][1].repositoryPath,'/local/child-or-bare');
    assert.equal(calls.length,1);
  }
});

test('retained filter fails closed during scope load and offers an explicit All escape on error', async () => {
  const ui=scopeRenderer(async()=>{throw Error('missing tree');});
  const model=ui.cache.get(context);ui.cache.setMode(model,'shared');
  const snapshot={repository_path:'/repo'};
  let result=ui.render(snapshot);assert.equal(result.model.mode,'shared');assert.equal(result.scopeRows,null);
  await settle();result=ui.render(snapshot);assert.match(result.scopeMessage,/could not be loaded/);
  assert.match(ui.element.innerHTML,/Retry contributor filters/);
  ui.element.querySelector('[data-clear-scope-filter]').click();result=ui.render(snapshot);
  assert.equal(result.model,null);assert.equal(result.scopeMessage,'');
});

test('workspace scope uses pinned right revision and rejects a changed HEAD', async () => {
  const calls=[],ui=scopeRenderer(async(cmd,args)=>{calls.push([cmd,args]);return {local_revision:'different-head',rows:leaves};});
  const snapshot={repository_path:'/repo'};ui.render(snapshot,{workspace:true,filter:'modified'});await settle();
  assert.equal(calls[0][0],'compare_workspace_revision');assert.equal(calls[0][1].revision,'b');assert.equal(calls[0][1].flat,true);
  assert.equal(snapshot.authorScope.rows,null);assert.match(snapshot.authorScope.error,/HEAD changed/);
});

test('List uses already loaded leaf rows and shares the same author model as tree', async () => {
  const ui=scopeRenderer(()=>assert.fail('no extra enumeration'));
  const a=ui.render({}, {flat:true,sourceRows:leaves});
  const b=ui.render({authorScope:{rows:leaves}});
  assert.equal(a.model,b.model);assert.equal(a.scopeRows,leaves);
});

test('large nested results project without quadratic directory-by-leaf scans', () => {
  const many=Array.from({length:20000},(_,i)=>row(`folder${i%100}/deep/file${i}`));
  const dirs=Array.from({length:100},(_,i)=>row(`folder${i}`,'folder'));
  const model={mode:'touched',paths:new Set(many.filter((_,i)=>i%2===0).map(r=>r.relative_path))};
  const view=comparisonViewRows(dirs,many,model);
  assert.equal(view.rows.length,50);assert.equal(view.matching.length,10000);
  assert.equal(view.descendants.get('folder0'),200);
});

test('real renderers use projected rows, keep parent navigation on empty results, and open excluded leaves', async () => {
  for(const submodule of [false,true]) {
    const cache=createComparisonAuthorCache(async cmd=>cmd==='comparison_authors'?authors:{paths:['src/solo'],no_contribution_paths:['src/deep/shared','vendor/unit']});
    const model=cache.get(context);await cache.load(model);cache.select(model,'alice');cache.setDirectOnly(model,true);await cache.loadStrict(model);
    const makeElement=()=>({value:'',innerHTML:'',dataset:{},classList:{toggle(){}},setAttribute(){},closest(){return this;},
      querySelector(){return null;},querySelectorAll(selector){
        if(!selector.includes('data-commander-entry')&&!selector.includes('data-sub-compare-entry')) return [];
        this.buttons=[...this.innerHTML.matchAll(/data-(commander|sub-compare)-entry="([^"]+)"/g)].map(m=>({
          dataset:m[1]==='commander'?{commanderEntry:m[2]}:{subCompareEntry:m[2]},handlers:{},addEventListener(event,fn){this.handlers[event]=fn;},
        }));return this.buttons;
      }});
    const nodes=new Map(),$=id=>{if(!nodes.has(id))nodes.set(id,makeElement());return nodes.get(id);};
    const refs=new Proxy({}, {get(o,k){return o[k] ||= makeElement();}});
    const state={repository:{path:'/repo'},view:'commander',compareMode:submodule?'submodule':'git',gitCompareMode:'refs',commanderPath:'src',commanderRows:[row('src/deep','folder'),row('src/solo')],compareFlatMode:false,subCompareFlatMode:false,compareFlatFilter:'all',subCompareFlatFilter:'all',
      branchCompareLeftRevision:'a',branchCompareRightRevision:'b',submoduleCompare:{submodulePath:'module',rows:[row('src/deep','folder'),row('src/solo')],leftRevision:'a',rightRevision:'b'}};
    const opened=[],h={...helpers,state,refs,$,esc:String,localDriveWorkspace:null,renderComparisonLocation(){},renderCommanderBreadcrumbs(){},renderBranchCompareSelectors(){},
      commanderComparisonSpec:()=>({workspace:false,repositoryPath:'/repo'}),comparisonRepository:()=>state.repository,comparisonBranches:()=>[],revisionDisplay:String,iconFor:()=>'',
      renderSubmoduleCompareOptions(){},renderSubmoduleCompareCommitList(){},logComparisonAuthorDisplay(){},bindComparisonEvidence(){},
      renderComparisonScopedFilter:()=>({model,scopeRows:leaves}),commanderSide:e=>e?.relative_path||'',compareStatusText:String,
      flatCompareRowHtml:(r,d)=>`<button ${d}="${r.relative_path}">${r.relative_path}</button>`,comparisonEvidenceRowHtml:()=>'',
      bindComparisonConversion:()=>false,SubmoduleCompareDetails:{bindRow(){}},openGitRevisionFileCompare:r=>opened.push(r.relative_path),openSubmoduleRevisionFileCompare:r=>opened.push(r.relative_path)};
    vm.createContext(h);vm.runInContext(source('comparisonExcludedRowsHtml')+'\n'+source(submodule?'renderSubmoduleCompare':'renderCommander'),h);
    h[submodule?'renderSubmoduleCompare':'renderCommander']();
    const rendered=submodule?refs.subCompareRows:refs.commanderRows;
    assert.match(rendered.innerHTML,/Parent folder/);
    assert.match(rendered.innerHTML,/src\/solo/);assert.doesNotMatch(rendered.innerHTML,/data-(?:commander|sub-compare)-entry="src\/deep"/,'nonmatching folder is hidden');
    assert.match(rendered.innerHTML,/2 excluded/);
    const excluded=rendered.buttons.find(b=>(b.dataset.commanderEntry||b.dataset.subCompareEntry)==='src/deep/shared');
    excluded.handlers.click();assert.deepEqual(opened,['src/deep/shared'],'excluded leaf uses its full path, not its parent folder');
    model.strictResult.result.paths=[];model.paths=new Set();h[submodule?'renderSubmoduleCompare':'renderCommander']();
    assert.match(rendered.innerHTML,/Parent folder/);assert.match(rendered.innerHTML,/No changed files match/,'up-row does not hide the empty-state explanation');
  }
});

test('immutable List leaf scope is reused when returning to tree, never across revisions or folders', async () => {
  const scope={rows:leaves};
  for(const change of ['none','revision','folder','repository']) {
    const result={repository_path:'/repo',relative_path:'src',left_revision:'a',right_revision:'b',rows:directories};
    const previous={...result,authorScope:scope};
    if(change==='revision')previous.right_revision='old';
    if(change==='folder')previous.relative_path='other';
    if(change==='repository')previous.repository_path='/other';
    const state={view:'commander',compareMode:'git',gitCompareMode:'refs',compareFlatMode:false,commanderComparisonResult:previous};
    const h={state,ComparisonNavigation:{WORKTREE:'Working tree'},ensureBranchCompareRefs(){},
      commanderComparisonSpec:()=>({workspace:false,repositoryPath:'/repo',leftRef:'a',rightRef:'b'}),
      openCommanderDirectoryGuard:()=>()=>true,$:()=>null,renderCommander(){},render(){},status(){},commanderDownloadSnapshot:()=>null,
      invoke:async()=>({...result})};
    vm.createContext(h);vm.runInContext(source('openCommanderDirectory'),h);await h.openCommanderDirectory('src');
    assert.equal(state.commanderComparisonResult.authorScope,change==='none'?scope:undefined);
  }
});

test('the direct-submodule scope is reused only for the same loaded folder and exact pair', async () => {
  for(const change of ['none','folder','revision']) {
    const scope={rows:leaves};
    const compare={submodulePath:'unit',relativePath:change==='folder'?'other':'src',leftRef:'a',rightRef:'b',leftRevision:'a',rightRevision:change==='revision'?'old':'b',authorScope:scope};
    const state={repository:{path:'/repo'},view:'commander',compareMode:'submodule',subCompareFlatMode:false,submoduleCompare:compare,commanderPath:'src'};
    const h={state,refs:{subCompareRows:{}},openSubmoduleCompareDirectoryGuard:()=>()=>true,render(){},status(){},revisionDisplay:String,
      invoke:async()=>({repository_path:'/repo/unit',relative_path:'src',left_revision:'a',right_revision:'b',rows:directories})};
    vm.createContext(h);vm.runInContext(source('openSubmoduleCompareDirectory'),h);await h.openSubmoduleCompareDirectory('src');
    assert.equal(compare.authorScope,change==='none'?scope:null);
  }
});

test('folder/gitlink and folder/file conversions stay navigable through the existing leaf comparison', () => {
  for(const submodule of [false,true]) for(const kind of ['submodule','file']) for(const reverse of [false,true]) {
    const calls=[],state={},h={state,SubmoduleCompareDetails:{bindRow(node,open){node.open=open;}},
      openCommanderDirectory:path=>calls.push(['git',path]),openSubmoduleCompareDirectory:path=>calls.push(['submodule',path])};
    vm.createContext(h);vm.runInContext(source('bindComparisonConversion'),h);
    const conversion={...row('converted','folder','type-changed'),remote:entry('converted',kind)};
    if(reverse)[conversion.local,conversion.remote]=[conversion.remote,conversion.local];
    const node={};assert.equal(h.bindComparisonConversion(node,conversion,submodule),true);
    assert.match(node.title,/List/);assert.equal(calls.length,0);node.open();
    assert.deepEqual(calls,[[submodule?'submodule':'git','converted']]);
    assert.equal(state[submodule?'subCompareFlatMode':'compareFlatMode'],true);
    assert.equal(h.bindComparisonConversion({},row('ordinary','folder'),submodule),false);
    assert.equal(h.bindComparisonConversion({},row('pointer','submodule'),submodule),false);
  }
});
