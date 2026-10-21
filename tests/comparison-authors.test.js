const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createComparisonAuthorCache, comparisonAuthorMatches, comparisonAuthorEmptyMessage, comparisonAuthorDiagnostic, comparisonAuthorEvidence, workspaceComparisonAuthorContext } = require('../frontend/comparison-authors.js');
const context = { repositoryPath: '/repo', leftRevision: 'a', rightRevision: 'b' };
const authors = [{ key: 'email:a@test', name: 'Name', email: 'a@test', paths: ['old', 'new', 'modified'] }];

test('Why included shows the witness commit date and handles older cached evidence honestly', () => {
  const app = fs.readFileSync(path.join(__dirname,'../frontend/app.js'),'utf8');
  const context = {esc:value=>String(value).replaceAll('<','&lt;')};
  vm.createContext(context);
  vm.runInContext(app.slice(app.indexOf('function comparisonEvidenceCommitHtml('),app.indexOf('function bindComparisonEvidence(')),context);
  const touch = {commit:'123456789abcdef',date:'2026-10-13'};
  const html = context.comparisonEvidenceCommitHtml(touch);
  assert.match(html,/12345678<\/code> · <time[^>]*>2026-10-13<\/time>/);
  assert.match(html,/UTC/);
  assert.match(context.comparisonEvidenceCommitHtml({commit:'old'}),/Date unavailable/);
  assert.match(context.comparisonEvidenceCommitHtml({...touch,date:'<invalid>'}),/&lt;invalid>/);
  assert.match(app.slice(app.indexOf('function bindComparisonEvidence('),app.indexOf('function renderComparisonAuthorFilter(')),/comparisonEvidenceCommitHtml\(touch\)/);
});

test('display diagnostics distinguish failure from empty results without logging identity', () => {
  const model = {context, selected:authors[0].key, authors, directOnly:true, paths:new Set(), strictResult:{error:'budget'}};
  const failed = comparisonAuthorDiagnostic(model, 500, 0, false);
  assert.match(failed, /phase=error left=a right=b author_index=0 total_rows=500 matched_paths=0 shown_rows=0/);
  assert.doesNotMatch(failed, /a@test|Name|email:/);
  model.strictResult = {result:{paths:[],ambiguous_paths:{f:'unknown'}}};
  assert.match(comparisonAuthorDiagnostic(model,500,0,false), /phase=complete/);
  assert.match(comparisonAuthorDiagnostic(model,500,0,true), /inconclusive=1 other_filter=true/);
  model.strictResult.pending = true;
  assert.match(comparisonAuthorDiagnostic(model,500,0,false), /phase=pending/);
});

test('strict failure and pending analysis are not presented as a successful empty filter', () => {
  const model = {selected:'email:a@test',directOnly:true};
  assert.match(comparisonAuthorEmptyMessage(model), /Analyzing/);
  model.strictResult = {error:'budget exceeded'};
  assert.match(comparisonAuthorEmptyMessage(model), /could not finish/);
  assert.doesNotMatch(comparisonAuthorEmptyMessage(model), /No changed files match/);
  model.strictResult = {result:{paths:[]}};
  assert.equal(comparisonAuthorEmptyMessage(model), 'No changed files match this filter');
  model.directOnly = false;
  model.strictResult = {error:'previous failure'};
  assert.equal(comparisonAuthorEmptyMessage(model), 'No changed files match this filter');
});

test('per-file evidence uses selected identity and active mode without including other paths', async () => {
  const direct = {commit:'direct',parent:'base',direct:true,change:'Modified'};
  const imported = {commit:'imported',parent:'other',direct:false,change:'Added'};
  const cache = createComparisonAuthorCache(async command => command === 'comparison_author_contributions' ? {paths:['modified'], evidence:{modified:[direct]}, ambiguous_paths:{}} : [
    {...authors[0],direct_paths:['modified'],evidence:{modified:[direct,imported],other:[imported]}},
    {key:'other',paths:['modified'],direct_paths:[],evidence:{modified:[imported]}},
  ]);
  const model = cache.get(context); await cache.load(model);
  const row = {relative_path:'modified',local:{relative_path:'modified'},remote:{relative_path:'modified'}};
  assert.deepEqual(comparisonAuthorEvidence(row,model),[]);
  cache.select(model,authors[0].key);
  assert.deepEqual(comparisonAuthorEvidence(row,model),[{...direct,path:'modified'},{...imported,path:'modified'}]);
  cache.setDirectOnly(model,true);
  await cache.loadStrict(model);
  assert.deepEqual(comparisonAuthorEvidence(row,model),[{...direct,path:'modified'}]);
  cache.select(model,'other');
  assert.deepEqual(comparisonAuthorEvidence(row,model),[]);
  cache.select(model,'');
  assert.deepEqual(comparisonAuthorEvidence(row,model),[]);
});

test('older metadata does not fabricate evidence', async () => {
  const cache = createComparisonAuthorCache(async () => authors);
  const model = cache.get(context); await cache.load(model); cache.select(model,authors[0].key);
  assert.deepEqual(comparisonAuthorEvidence({relative_path:'modified'},model),[]);
});

test('evidence reuses the read-only diff dialog with exact parent and repository context', async () => {
  const app = fs.readFileSync(path.join(__dirname,'../frontend/app.js'),'utf8');
  for (const [owner, command] of [
    [null,'compare_git_revision_file'],
    [{repositoryPath:'nested-repo'},'compare_git_revision_file'],
    [{repositoryPath:'parent-repo',submodulePath:'modules/a'},'compare_submodule_revision_file'],
    [{repositoryPath:'parent-repo',owner:'eng',repositoryName:'a'},'compare_github_module_revision_file'],
  ]) {
    const calls = [], rendered = [];
    const context = {
      state:{repository:{path:'current-repo'},branchCompareLeftRevision:'left',branchCompareRightRevision:'right'},
      refs:{compareTitle:{},compareSubtitle:{},localCompare:{},remoteCompare:{},compareDialog:{showModal(){}}},
      openFileCompareGuard: () => () => true, resetCompareDiffNavigation(){},
      commanderComparisonSpec: () => ({ repositoryPath: 'current-repo', leftRef: 'left', rightRef: 'right' }),
      setCompareReadOnly: value => assert.equal(value,true),
      renderComparisonContents: (...args) => rendered.push(args),
      invoke: async (name,args) => {calls.push({name,args});return {local_content:'before',remote_content:'after'};},
    };
    vm.createContext(context);
    vm.runInContext(app.slice(app.indexOf('async function openGitRevisionFileCompare('),app.indexOf('function previewCommanderRows(')),context);
    const evidence = owner ? {context:owner,touch:{parent:'parent-sha',commit:'touch-sha',path:'exact.c',change:'Modified'}} : null;
    await context.openGitRevisionFileCompare({name:'file.c',relative_path:'file.c',local:{},remote:{}},evidence);
    assert.equal(calls.length,1);
    assert.equal(calls[0].name,command);
    assert.equal(calls[0].args.repositoryPath,owner?.repositoryPath || 'current-repo');
    assert.equal(calls[0].args.leftRef,owner ? 'parent-sha' : 'left');
    assert.equal(calls[0].args.rightRef,owner ? 'touch-sha' : 'right');
    assert.equal(calls[0].args.relativePath,owner ? 'exact.c' : 'file.c');
    assert.deepEqual(rendered,[['before','after',{local_content:'before',remote_content:'after'}]]);
    if (owner?.submodulePath) assert.equal(calls[0].args.submodulePath,'modules/a');
    if (owner?.owner) assert.equal(calls[0].args.owner,'eng');
  }
});

test('strict checkbox loads attribution only on demand and caches each author independently', async () => {
  let calls = 0;
  const cache = createComparisonAuthorCache(async (command,args) => { calls++; if(command==='comparison_author_contributions') return {paths:args.authorKey===authors[0].key?['modified']:[],evidence:{},ambiguous_paths:{}}; return [
    { ...authors[0], direct_paths: ['modified'] },
    { key: 'side', paths: ['new'], direct_paths: [] },
  ]; });
  const model = cache.get(context);
  await cache.load(model);
  cache.select(model, authors[0].key);
  const rows = ['old', 'new', 'modified'].map(relative_path => ({relative_path}));
  cache.setDirectOnly(model, true);
  assert.equal(calls,1);
  assert.equal(rows.filter(row => comparisonAuthorMatches(row,model)).length,0);
  await cache.loadStrict(model);
  assert.equal(model.selected, authors[0].key);
  assert.deepEqual(rows.filter(row => comparisonAuthorMatches(row, model)), [rows[2]]);
  cache.select(model, 'side');
  await cache.loadStrict(model);
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 0);
  cache.select(model, '');
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 3);
  cache.select(model, authors[0].key);
  cache.setDirectOnly(model, false);
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 3);
  assert.equal(calls, 3);
  assert.equal(cache.get({...context, rightRevision:'other'}).directOnly, false);
});

test('missing strict backend fails closed rather than reusing first-parent metadata', async () => {
  const cache = createComparisonAuthorCache(async command => { if(command==='comparison_author_contributions') throw new Error('unsupported command'); return authors; });
  const model = cache.get(context);
  await cache.load(model);
  cache.select(model, authors[0].key);
  cache.setDirectOnly(model, true);
  await cache.loadStrict(model);
  assert.equal(model.directOnly, true);
  assert.equal(model.paths.size, 0);
  assert.match(model.strictResult.error,/unsupported/);
});

test('selecting a merge-only author with no personal touches shows no files, not all files', async () => {
  const cache = createComparisonAuthorCache(async () => [
    ...authors, { key: 'email:merger@test', name: 'Integrator', email: 'merger@test', paths: [] },
  ]);
  const model = cache.get(context);
  await cache.load(model);
  const rows = [{ relative_path: 'modified' }, { relative_path: 'imported' }];
  cache.select(model, 'email:a@test');
  assert.deepEqual(rows.filter(row => comparisonAuthorMatches(row, model)), [rows[0]]);
  cache.select(model, 'email:merger@test');
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 0);
  cache.select(model, '');
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 2);
});

test('workspace author context uses resolved owner and revisions, and the actual list paths', async () => {
  const result = { repository_path: '/repo/modules/a', local_revision: 'head-sha', remote_revision: 'remote-sha',
    rows: [{ relative_path: 'new', local: { relative_path: 'new' }, remote: { relative_path: 'old' } }, { relative_path: 'untracked' }] };
  const context = workspaceComparisonAuthorContext(result);
  assert.deepEqual(context, { repositoryPath: '/repo/modules/a', leftRevision: 'head-sha', rightRevision: 'remote-sha', comparisonPaths: ['new', 'old', 'untracked'] });
  const cache = createComparisonAuthorCache(async () => authors);
  const model = cache.get(context);
  await cache.load(model); cache.select(model, authors[0].key);
  assert.equal(comparisonAuthorMatches(result.rows[0], model), true);
  assert.equal(comparisonAuthorMatches(result.rows[1], model), false);
  const changed = workspaceComparisonAuthorContext({ ...result, rows: [{ relative_path: 'other' }] });
  assert.notEqual(cache.get(changed), model);
  assert.equal(cache.get(workspaceComparisonAuthorContext(result)), model);
  cache.select(model, '');
  assert.equal(comparisonAuthorMatches(result.rows[1], model), true);
  assert.equal(workspaceComparisonAuthorContext({ ...result, local_revision: null }), null);
});

test('author metadata is opt-in, cached and does not change comparison rows', async () => {
  let calls = 0;
  const cache = createComparisonAuthorCache(async (command, args) => {
    calls++; assert.equal(command, 'comparison_authors'); assert.deepEqual(args, context); return authors;
  });
  const model = cache.get(context);
  assert.equal(calls, 0);
  await Promise.all([cache.load(model), cache.load(model)]);
  assert.equal(calls, 1);
  const rows = [{ relative_path:'old', local:{relative_path:'old'} }, { relative_path:'new', remote:{relative_path:'new'} },
    { relative_path:'modified', local:{relative_path:'modified'}, remote:{relative_path:'modified'} }, { relative_path:'other' }];
  const before = JSON.stringify(rows);
  cache.select(model, authors[0].key);
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 3);
  assert.equal(comparisonAuthorMatches({ relative_path:'renamed', local:{relative_path:'old'} }, model), true);
  cache.select(model, '');
  assert.equal(rows.filter(row => comparisonAuthorMatches(row, model)).length, 4);
  await cache.load(model);
  assert.equal(calls, 1);
  assert.equal(JSON.stringify(rows), before);
});

test('late author results never enter another repository/revision/submodule context', async () => {
  let finish;
  const cache = createComparisonAuthorCache(() => new Promise(resolve => { finish = resolve; }));
  const old = cache.get(context);
  const request = cache.load(old);
  await Promise.resolve();
  const next = cache.get({ ...context, rightRevision:'c' });
  const sub = cache.get({ ...context, submodulePath:'modules/a' });
  const external = cache.get({ ...context, owner:'org', repositoryName:'a' });
  finish(authors); await request;
  assert.equal(next.authors, null); assert.equal(sub.authors, null); assert.equal(external.authors, null);
  assert.equal(next.paths, null); assert.equal(old.authors, authors);
});

test('author and strict preference survive revision changes without falling back to all files', async () => {
  const cache = createComparisonAuthorCache(async command => command==='comparison_author_contributions'?{paths:['modified'],evidence:{},ambiguous_paths:{}}:[{...authors[0], direct_paths:['modified']}]);
  const old = cache.get(context); await cache.load(old);
  cache.select(old, authors[0].key); cache.setDirectOnly(old, true);
  const next = cache.get({...context, rightRevision:'next'});
  assert.equal(next.selected, authors[0].key);
  assert.equal(next.directOnly, true);
  assert.equal(comparisonAuthorMatches({relative_path:'modified'},next), false);
  await cache.load(next);
  await cache.loadStrict(next);
  assert.equal(comparisonAuthorMatches({relative_path:'modified'},next), true);
  assert.equal(comparisonAuthorMatches({relative_path:'new'},next), false);
  assert.equal(cache.get({...context,repositoryPath:'/different'}).selected,'');
});

test('rapid author changes cannot install a previous author strict result; toggle restores normal paths', async () => {
  const pending = new Map();
  const cache = createComparisonAuthorCache(async (command,args) => command==='comparison_authors'
    ? [authors[0],{key:'b',paths:['other']}]
    : new Promise(resolve=>pending.set(args.authorKey,resolve)));
  const model = cache.get(context); await cache.load(model);
  cache.select(model,authors[0].key); cache.setDirectOnly(model,true);
  const a = cache.loadStrict(model); await Promise.resolve();
  cache.select(model,'b'); const b = cache.loadStrict(model); await Promise.resolve();
  pending.get(authors[0].key)({paths:['modified'],evidence:{},ambiguous_paths:{}}); await a;
  assert.equal(model.paths.size,0);
  pending.get('b')({paths:['other'],evidence:{},ambiguous_paths:{}}); await b;
  assert.deepEqual([...model.paths],['other']);
  cache.select(model,authors[0].key); await cache.loadStrict(model);
  assert.deepEqual([...model.paths],['modified']);
  cache.setDirectOnly(model,false);
  assert.deepEqual([...model.paths],authors[0].paths);
});

test('strict errors and empty results never expose all files, and retry is explicit', async () => {
  let attempts=0;
  const cache = createComparisonAuthorCache(async command => {
    if(command==='comparison_authors')return authors;
    if(++attempts===1)throw new Error('budget exceeded');
    return {paths:[],evidence:{},ambiguous_paths:{modified:'inconclusive'}};
  });
  const model=cache.get(context);await cache.load(model);cache.select(model,authors[0].key);cache.setDirectOnly(model,true);
  await cache.loadStrict(model);await cache.loadStrict(model);
  assert.equal(attempts,1);assert.equal(comparisonAuthorMatches({relative_path:'modified'},model),false);
  await cache.loadStrict(model,true);assert.equal(attempts,2);assert.equal(model.paths.size,0);
  assert.equal(model.strictResult.result.ambiguous_paths.modified,'inconclusive');
});

test('late strict response stays in its SHA context and empty author selection is explicit', async () => {
  let finish;
  const cache=createComparisonAuthorCache(async command=>command==='comparison_authors'?authors:new Promise(resolve=>{finish=resolve;}));
  const old=cache.get(context);await cache.load(old);cache.select(old,authors[0].key);cache.setDirectOnly(old,true);
  const request=cache.loadStrict(old);await Promise.resolve();
  const next=cache.get({...context,rightRevision:'different-sha'});
  finish({paths:['modified'],evidence:{},ambiguous_paths:{}});await request;
  assert.equal(next.paths.size,0);assert.equal(next.strictResult,null);
  assert.equal(comparisonAuthorMatches({relative_path:'modified'},next),false);
  cache.select(next,'');assert.equal(comparisonAuthorMatches({relative_path:'modified'},next),true);
});

test('failed history reports an error, keeps full list and allows retry; cache is bounded', async () => {
  let calls = 0;
  const cache = createComparisonAuthorCache(async () => { if (++calls === 1) throw new Error('incomplete history'); return authors; }, 2);
  const model = cache.get(context);
  await cache.load(model);
  assert.match(model.error, /incomplete history/);
  assert.equal(comparisonAuthorMatches({relative_path:'anything'}, model), true);
  await cache.load(model);
  assert.equal(model.error, ''); assert.equal(model.authors, authors);
  cache.get({ ...context, rightRevision:'c' }); cache.get({ ...context, rightRevision:'d' });
  assert.notEqual(cache.get(context), model);
});
