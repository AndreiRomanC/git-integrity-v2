const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const helpers = require('../frontend/comparison-authors.js');
const {createComparisonAuthorCache, comparisonAuthorMatches, comparisonAuthorMode, comparisonAuthorStrict,
  comparisonAuthorContributors, comparisonContributorEvidence, comparisonAuthorEmptyMessage,
  comparisonAuthorDiagnostic, scopedComparisonAuthorContext} = helpers;
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const context = {repositoryPath:'/repo', leftRevision:'start', rightRevision:'end'};
const row = relative_path => ({relative_path, local:{relative_path, kind:'file'}, remote:{relative_path, kind:'file'}});
const rows = [row('src/shared'), row('src/solo'), row('src/other'), row('src/unattributed')];
const witness = (commit, parent = 'base') => ({commit, parent, date:'2026-10-25', direct:true, change:'Modified'});
const authors = [
  {key:'email:a@test', name:'Alice', email:'a@test', paths:['src/shared','src/solo'], evidence:{'src/shared':[witness('alice')],'src/solo':[witness('solo')]}},
  {key:'email:b@test', name:'Bob', email:'b@test', paths:['src/shared','src/other'], evidence:{'src/shared':[witness('bob')],'src/other':[witness('other')]}},
];
const matching = model => rows.filter(r => comparisonAuthorMatches(r, model)).map(r => r.relative_path);
const esc = value => String(value).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;').replaceAll('"','&quot;').replaceAll("'",'&#39;');

function renderer(cache) {
  const controls = new Map();
  // A narrow DOM double: generated markup is asserted separately, while real
  // handlers are bound and exercised (selection, strict toggle, retry, clear).
  const element = {
    html:'',
    set innerHTML(html) { this.html = html; controls.clear(); },
    get innerHTML() { return this.html; },
    insertAdjacentHTML(_, html) { this.html += html; },
    querySelector(selector) {
      const token = selector.startsWith('[') ? selector.slice(1,-1) : 'comparison-author-help';
      if (!this.html.includes(token)) return null;
      if (!controls.has(selector)) controls.set(selector, {
        open:/class="comparison-author-help" open/.test(this.html),
        listeners:{}, value:'',
        addEventListener(name, fn) { this.listeners[name] = fn; },
        fire(name, target = this) { return this.listeners[name]({target}); },
        insertAdjacentHTML:(_, html) => { element.html += html; },
      });
      return controls.get(selector);
    },
  };
  const container = {classList:{toggle(){}}, querySelector:() => element};
  const h = {...helpers, comparisonAuthorCache:cache, esc, state:{view:'commander'}, renders:0, logs:[],
    renderCommander() { h.renders++; }, jsPerfLog(message) { h.logs.push(message); },
    flatCompareRowHtml:r => `<button data-row="${esc(r.relative_path)}">${esc(r.relative_path)}</button>`};
  vm.createContext(h);
  vm.runInContext(app.slice(app.indexOf('function comparisonEvidenceRowHtml('), app.indexOf('function logComparisonAuthorDisplay(')), h);
  return {h, element, render:(c = context, options = {}) => h.renderComparisonAuthorFilter(container,c,{scopeRows:rows,rows,...options})};
}

test('Shared uses distinct recorded identities, not number of commits or display names', async () => {
  const data = [
    {...authors[0], name:'Same name', paths:['src/shared','src/shared','src/solo'], evidence:{'src/shared':[witness('a1'),witness('a2')]}},
    {...authors[1], name:'Same name'},
  ];
  const before = JSON.stringify(data);
  const calls = [];
  const cache = createComparisonAuthorCache(async (...args) => { calls.push(args); return data; });
  const model = cache.get(context); await cache.load(model); cache.setMode(model,'shared');
  assert.deepEqual(matching(model), ['src/shared']);
  assert.deepEqual(comparisonAuthorContributors(rows[0],model).map(a=>a.key), ['email:a@test','email:b@test']);
  assert.equal(comparisonAuthorContributors(rows[1],model).length,1);
  for (let i=0;i<20;i++) { cache.setMode(model,'all'); cache.setMode(model,'shared'); await cache.load(model); }
  assert.equal(calls.length,1,'mode switches reuse the existing history response');
  assert.equal(JSON.stringify(data),before,'metadata/evidence remain unchanged');
});

test('row aliases union contributors and deduplicate each identity once', () => {
  const renamed = {relative_path:'new',local:{relative_path:'old'},remote:{relative_path:'new'}};
  const model = {mode:'shared',authors:[{key:'a',paths:['old','new','new']},{key:'b',paths:['new']}]};
  assert.equal(comparisonAuthorMatches(renamed,model),true);
  assert.equal(comparisonAuthorContributors(renamed,model).length,2);
  model.authors = [{key:'a',paths:['old','new','new']}];
  assert.equal(comparisonAuthorMatches(renamed,model),false);
  model.authors.push({key:'b',paths:['new']});
  model.authors[0].paths = ['old'];
  assert.equal(comparisonAuthorMatches(renamed,model),true,'two aliases of a single comparison item count together');
});

test('changing mode clears surviving attribution; selecting Touched never silently restores it', async () => {
  const calls = [];
  const cache = createComparisonAuthorCache(async command => {
    calls.push(command); return command === 'comparison_authors' ? authors : {paths:['src/solo'], evidence:{}, ambiguous_paths:{}};
  });
  const model = cache.get(context); await cache.load(model);
  cache.select(model,authors[0].key); cache.setDirectOnly(model,true); await cache.loadStrict(model);
  assert.deepEqual(matching(model),['src/solo']);
  cache.setMode(model,'shared');
  assert.equal(comparisonAuthorStrict(model),false);
  assert.deepEqual(matching(model),['src/shared']);
  assert.equal(await cache.loadStrict(model),null);
  cache.select(model,'');
  assert.deepEqual(matching(model),rows.map(r=>r.relative_path));
  assert.equal(comparisonAuthorStrict(model),false);
  assert.equal(model.directOnly,false,'surviving analysis resets on filter changes');
  cache.select(model,authors[0].key);
  assert.equal(comparisonAuthorStrict(model),false);
  assert.deepEqual(matching(model),['src/shared','src/solo']);
  assert.equal(await cache.loadStrict(model),null);
  cache.setDirectOnly(model,true); await cache.loadStrict(model);
  assert.equal(comparisonAuthorStrict(model),true);
  assert.deepEqual(matching(model),['src/solo']);
  assert.deepEqual(calls,['comparison_authors','comparison_author_contributions']);
});

test('late strict completion cannot replace a Shared or All result', async () => {
  for (const mode of ['shared','all']) {
    let resolve;
    const cache = createComparisonAuthorCache(command => command === 'comparison_authors' ? Promise.resolve(authors) : new Promise(done=>{resolve=done;}));
    const model = cache.get(context); await cache.load(model);
    cache.select(model,authors[0].key); cache.setDirectOnly(model,true);
    const pending = cache.loadStrict(model); await Promise.resolve();
    cache.setMode(model,mode);
    const before = matching(model);
    resolve({paths:['src/solo'], ambiguous_paths:{}}); await pending;
    assert.equal(comparisonAuthorMode(model),mode);
    assert.deepEqual(matching(model),before);
    assert.equal(comparisonAuthorStrict(model),false);
  }
});

test('Shared missing history fails closed, distinguishes errors from no matches, and allows retry', async () => {
  let count = 0;
  const cache = createComparisonAuthorCache(async () => { if (++count === 1) throw new Error('incomplete local history'); return authors; });
  const model = cache.get(context); cache.setMode(model,'shared');
  assert.deepEqual(matching(model),[]);
  assert.match(comparisonAuthorEmptyMessage(model),/Loading contributor history/);
  await cache.load(model);
  assert.deepEqual(matching(model),[]);
  assert.match(comparisonAuthorEmptyMessage(model),/unavailable.*retry/);
  assert.doesNotMatch(comparisonAuthorEmptyMessage(model),/No compared items/);
  cache.setMode(model,'all'); assert.equal(matching(model).length,4);
  cache.setMode(model,'shared'); await cache.load(model);
  assert.deepEqual(matching(model),['src/shared']);
  assert.equal(count,2);
  assert.match(comparisonAuthorDiagnostic(model,4,1,false),/mode=shared phase=complete.*author_index=-1/);
  assert.doesNotMatch(comparisonAuthorDiagnostic(model,4,1,false),/Alice|a@test|Bob|b@test/);
});

test('Shared preference survives folder/revision changes but its contributor results do not leak', async () => {
  const pending = new Map();
  const cache = createComparisonAuthorCache((_, args) => new Promise(resolve=>pending.set(args.comparisonPaths[0],resolve)));
  const a = cache.get(scopedComparisonAuthorContext(context,[row('a/file')])); cache.setMode(a,'shared');
  const loadA = cache.load(a); await Promise.resolve();
  const b = cache.get(scopedComparisonAuthorContext({...context,rightRevision:'new-tip'},[row('b/file')]));
  assert.equal(b.mode,'shared'); assert.equal(comparisonAuthorMatches(row('b/file'),b),false);
  const loadB = cache.load(b); await Promise.resolve();
  pending.get('b/file')([{key:'one',paths:['b/file']}]); await loadB;
  pending.get('a/file')([{key:'one',paths:['a/file']},{key:'two',paths:['a/file']}]); await loadA;
  assert.equal(comparisonAuthorMatches(row('a/file'),a),true);
  assert.equal(comparisonAuthorMatches(row('a/file'),b),false);
  assert.equal(comparisonAuthorMatches(row('b/file'),b),false);
  assert.match(comparisonAuthorEmptyMessage(b),/No compared items match Shared files.*search\/change-type filters/);
  assert.equal(cache.get(scopedComparisonAuthorContext(context,[row('a/file')])),a);
});

test('all/shared evidence is limited to row contributors and keeps exact witness paths/parents', async () => {
  const data = [...authors, {key:'outside',name:'Outside',paths:['elsewhere'],evidence:{elsewhere:[witness('outside')]}}];
  const cache = createComparisonAuthorCache(async()=>data);
  const model=cache.get(context); await cache.load(model);
  for (const mode of ['all','shared']) {
    cache.setMode(model,mode);
    const evidence = comparisonContributorEvidence(rows[0],model);
    assert.deepEqual(evidence.map(e=>[e.authorKey,e.commit,e.parent,e.path]),[
      ['email:a@test','alice','base','src/shared'],['email:b@test','bob','base','src/shared'],
    ]);
    assert.deepEqual(comparisonContributorEvidence(rows[3],model),[]);
  }
});

test('existing selector includes introduction history; strict is inside Details with a visible active state', async () => {
  const cache=createComparisonAuthorCache(async()=>authors);
  const ui=renderer(cache), model=ui.render(); await cache.load(model);
  ui.render();
  assert.equal((ui.element.innerHTML.match(/<select /g)||[]).length,1);
  assert.match(ui.element.innerHTML,/>All differences<.*Shared files · 2\+ authors.*optgroup label="Files touched by author"/);
  assert.match(ui.element.innerHTML,/optgroup label="Introduced by author"/);
  assert.doesNotMatch(ui.element.innerHTML,/Only files modified/);
  assert.match(ui.element.innerHTML,/<details[^>]*>[\s\S]*data-author-direct[\s\S]*<\/details>/);
  assert.match(ui.element.innerHTML,/data-author-direct\s+disabled/);
  const select=ui.element.querySelector('[data-author-select]'); select.value=authors[0].key; select.fire('change');
  assert.equal(model.mode,'touched'); assert.equal(ui.h.renders,1);
  ui.render();
  assert.equal(ui.element.querySelector('[data-author-select]').value,authors[0].key);
  assert.match(ui.element.innerHTML,/2 items shown · touched by this author/);
  model.strict.set(authors[0].key,{result:{paths:['src/solo'],no_contribution_paths:['src/shared']}});
  ui.element.querySelector('.comparison-author-help').open=true;
  ui.element.querySelector('[data-author-direct]').fire('change',{checked:true}); ui.render();
  assert.match(ui.element.innerHTML,/<details class="comparison-author-help" open/);
  assert.match(ui.element.innerHTML,/Details · surviving required/);
  assert.match(ui.element.innerHTML,/1 of 2 items shown · 1 excluded · surviving contribution required/);
  assert.equal(ui.h.renders,2);
  const shared=ui.element.querySelector('[data-author-select]'); shared.value='@shared'; shared.fire('change'); ui.render();
  assert.equal(model.mode,'shared'); assert.equal(model.directOnly,false);
  assert.match(ui.element.innerHTML,/1 items shown · touched by 2\+ recorded authors/);
  assert.doesNotMatch(ui.element.innerHTML,/Details · surviving required|data-author-direct checked/);
});

test('Shared summaries count current text/state candidates but keep the full folder metadata context', async () => {
  const calls=[];
  const cache=createComparisonAuthorCache(async(command,args)=>{calls.push([command,args]);return authors;});
  const ui=renderer(cache), model=ui.render(); await cache.load(model); cache.setMode(model,'shared');
  ui.render(context,{rows:[rows[1]]});
  assert.match(ui.element.innerHTML,/0 items shown/);
  ui.render(context,{rows:[rows[0]]});
  assert.match(ui.element.innerHTML,/1 items shown/);
  assert.deepEqual(calls,[['comparison_authors',{...context,comparisonPaths:rows.map(r=>r.relative_path).sort()}]]);
  assert.match(ui.element.innerHTML,/Alice.*Bob/,'available identities do not jump while typing');
});

test('errors offer retry or All and never silently expose unfiltered rows', async () => {
  const cache=createComparisonAuthorCache(async()=>{throw new Error('Missing <history>');});
  const ui=renderer(cache), model=ui.render(); cache.setMode(model,'shared'); await cache.load(model); ui.render();
  assert.match(ui.element.innerHTML,/Missing &lt;history&gt;.*No filtered results were applied/);
  assert.match(ui.element.innerHTML,/data-load-authors/);
  assert.deepEqual(matching(model),[]);
  ui.element.querySelector('[data-clear-author]').fire('click'); ui.render();
  assert.equal(model.mode,'all'); assert.equal(matching(model).length,4);
  assert.match(ui.element.innerHTML,/Complete comparison remains visible/);
});

test('contributor details escape identities, show date/evidence and distinguish pointer authorship', async () => {
  const data=authors.map((a,i)=>({...a,name:i ? 'Bob' : '<Alice>',email:i ? 'b@test' : 'a&test'}));
  const cache=createComparisonAuthorCache(async()=>data);
  const ui=renderer(cache), model=ui.render(); await cache.load(model); cache.setMode(model,'shared');
  const html=ui.h.comparisonEvidenceBodyHtml(rows[0],model,comparisonContributorEvidence(rows[0],model));
  assert.match(html,/Contributors in this comparison: &lt;Alice&gt; &lt;a&amp;test&gt;, Bob &lt;b@test&gt;/);
  assert.match(html,/Full file comparison shown/);
  assert.match(html,/do not prove that each author’s edits survived/);
  assert.match(html,/<time[^>]*>2026-10-25<\/time>/);
  assert.equal((html.match(/View change<\/button>/g)||[]).length,2);
  assert.doesNotMatch(html,/<Alice>/);
  const pointer={...rows[0],local:{...rows[0].local,kind:'submodule'}};
  const pointerHtml=ui.h.comparisonEvidenceBodyHtml(pointer,model,comparisonContributorEvidence(pointer,model));
  assert.match(pointerHtml,/Contributors to this submodule pointer/);
  assert.match(pointerHtml,/Internal file authors are not analyzed here/);
  assert.match(ui.h.comparisonEvidenceRowHtml(pointer,model),/Contributors · 2/);
});

test('All contributor disclosure and View change are lazy and preserve the main endpoint comparison', async () => {
  const cache=createComparisonAuthorCache(async()=>authors);
  const ui=renderer(cache), model=ui.render(); await cache.load(model);
  assert.match(ui.h.comparisonEvidenceRowHtml(rows[0],model),/Contributors · 2/);
  assert.equal(ui.h.comparisonEvidenceRowHtml(rows[3],model),'');
  const buttons=[0,1].map(index=>({dataset:{evidenceDiff:String(index)},addEventListener(_,fn){this.click=fn;}}));
  const body={innerHTML:'',querySelectorAll:()=>buttons};
  const details={open:false,dataset:{authorEvidence:'src/shared'},addEventListener(_,fn){this.toggle=fn;},querySelector:()=>body};
  const opened=[]; ui.h.openGitRevisionFileCompare=(r,args)=>opened.push([r,args]);
  ui.h.bindComparisonEvidence({querySelectorAll:()=>[details]},rows,model);
  assert.equal(body.innerHTML,''); details.toggle(); assert.equal(body.innerHTML,'');
  details.open=true; details.toggle(); assert.match(body.innerHTML,/Alice.*Bob/);
  buttons[1].click({preventDefault(){},stopPropagation(){}});
  assert.equal(opened.length,1); assert.equal(opened[0][0],rows[0]);
  assert.equal(opened[0][1].context,model.context);
  assert.equal(opened[0][1].touch.commit,'bob'); assert.equal(opened[0][1].touch.parent,'base');
  assert.equal(model.context.leftRevision,'start'); assert.equal(model.context.rightRevision,'end');
  assert.equal(model.mode,'all'); assert.equal(matching(model).length,4);
});

test('Shared ignores strict grouping/exclusions and renders only its matching endpoint rows', async () => {
  const cache=createComparisonAuthorCache(async()=>authors);
  const ui=renderer(cache), model=ui.render(); await cache.load(model);
  cache.select(model,authors[0].key); cache.setDirectOnly(model,true);
  model.strict.set(authors[0].key,{result:{paths:['src/solo'],structural_paths:{'src/solo':'src'},no_contribution_paths:['src/shared']}});
  cache.setMode(model,'shared');
  const html=ui.h.comparisonAuthorRowsHtml(rows.filter(r=>comparisonAuthorMatches(r,model)),rows,model,'data-row');
  assert.match(html,/src\/shared/); assert.doesNotMatch(html,/src\/solo|Reconfiguration|excluded/);
});

test('status dropdown behavior is retained and metadata remains opt-in without a retained filter', () => {
  const index=fs.readFileSync(path.join(__dirname,'../frontend/index.html'),'utf8');
  for (const id of ['gitCompareFlatFilter','subCompareFlatFilter']) {
    const select=index.match(new RegExp(`<select id="${id}">(.*?)</select>`))[1];
    assert.deepEqual([...select.matchAll(/value="([^"]+)"/g)].map(m=>m[1]),['all','modified','local-only','remote-only','type-changed']);
    assert.match(select,/Any change type/);
  }
  const cache=createComparisonAuthorCache(()=>assert.fail('no automatic metadata walk'));
  const ui=renderer(cache), model=ui.render();
  assert.equal(model.mode,'all'); assert.match(ui.element.innerHTML,/Contributor filters…/);
  assert.equal(ui.h.comparisonEvidenceRowHtml(rows[0],model),'');
});

test('a retained author awaiting metadata is not presented as no contribution', () => {
  const cache=createComparisonAuthorCache(()=>assert.fail('helper does not load history'));
  const model=cache.get(context); cache.select(model,authors[0].key);
  assert.equal(comparisonAuthorEmptyMessage(model),'Loading contributor history…');
  assert.deepEqual(matching(model),[]);
});

const introduced = [{key:'email:integrator@test',name:'Integrator',email:'integrator@test',paths:['src/shared'],evidence:{'src/shared':[
  {commit:'merge',parent:'first-parent',date:'2026-10-25',kind:'merge',witness_path:'src/shared'},
]}}];

test('Introduced is opt-in, cached independently and does not redefine touched/shared/strict', async () => {
  const calls=[];
  const cache=createComparisonAuthorCache(async(command,args)=>{calls.push([command,args]);return command==='comparison_authors' ? authors : introduced;});
  const model=cache.get(context); await cache.load(model);
  assert.equal(calls.length,1);
  cache.select(model,introduced[0].key,'introduced');
  assert.deepEqual(matching(model),[]);
  assert.match(comparisonAuthorEmptyMessage(model),/Loading introduction history/);
  await cache.loadIntroductions(model);
  assert.deepEqual(matching(model),['src/shared']);
  assert.equal(comparisonAuthorStrict(model),false);
  assert.equal(await cache.loadStrict(model),null);
  assert.deepEqual(comparisonAuthorContributors(rows[0],model).map(a=>a.name),['Alice','Bob']);
  cache.select(model,introduced[0].key);
  assert.deepEqual(matching(model),[],'integrating others is not a non-merge touch');
  cache.setMode(model,'shared'); assert.deepEqual(matching(model),['src/shared']);
  cache.select(model,''); assert.equal(matching(model).length,4);
  cache.select(model,introduced[0].key,'introduced'); await cache.loadIntroductions(model);
  assert.deepEqual(calls.map(c=>c[0]),['comparison_authors','comparison_introductions']);
});

test('parent context is sent only to introduction analysis, never to personal/strict requests', async () => {
  const parentComparison={repositoryPath:'/parent',relativePath:'modules/unit',leftRevision:'p0',rightRevision:'p1'};
  const calls=[];
  const cache=createComparisonAuthorCache(async(command,args)=>{calls.push([command,args]);
    return command==='comparison_authors' ? authors : command==='comparison_introductions' ? introduced : {paths:['src/solo']};});
  const model=cache.get(scopedComparisonAuthorContext({...context,parentComparison},rows));
  await cache.load(model); cache.select(model,authors[0].key); cache.setDirectOnly(model,true); await cache.loadStrict(model);
  cache.setMode(model,'introduced'); await cache.loadIntroductions(model);
  assert.equal(Object.hasOwn(calls[0][1],'parentComparison'),false);
  assert.equal(Object.hasOwn(calls[1][1],'parentComparison'),false);
  assert.deepEqual(calls[2][1].parentComparison,parentComparison);
  assert.deepEqual(calls[2][1].comparisonPaths,rows.map(r=>r.relative_path).sort());
  assert.equal(Object.hasOwn(calls[2][1],'scopePaths'),false);
});

test('late introductions cannot overwrite another mode, revision or parent integration interval', async () => {
  const pending=[];
  const cache=createComparisonAuthorCache((command,args)=>command==='comparison_authors' ? Promise.resolve(authors)
    : new Promise(resolve=>pending.push({args,resolve})));
  const a=cache.get(context); await cache.load(a); cache.select(a,introduced[0].key,'introduced');
  const load=cache.loadIntroductions(a); await Promise.resolve();
  cache.setMode(a,'shared');
  const b=cache.get({...context,parentComparison:{repositoryPath:'/parent',relativePath:'unit',leftRevision:'p0',rightRevision:'p1'}});
  pending[0].resolve(introduced); await load;
  assert.equal(a.mode,'shared'); assert.deepEqual(matching(a),['src/shared']);
  assert.equal(b.introductions,null); assert.equal(b.authors,null);
  assert.notEqual(a,b);
  cache.select(a,''); assert.equal(matching(a).length,4);
});

test('introduction failures are incomplete, never empty ownership; explicit retry and All recover', async () => {
  let count=0;
  const cache=createComparisonAuthorCache(async command=>{if(command==='comparison_authors') return authors;
    if(++count===1) throw new Error('Missing intermediate <object>'); return introduced;});
  const ui=renderer(cache), model=ui.render(); await cache.load(model);
  cache.select(model,introduced[0].key,'introduced'); await cache.loadIntroductions(model);
  ui.render();
  assert.match(ui.element.innerHTML,/Missing intermediate &lt;object&gt;/);
  assert.match(comparisonAuthorEmptyMessage(model),/incomplete or unavailable/);
  assert.deepEqual(matching(model),[]);
  await cache.loadIntroductions(model); assert.equal(count,1,'no retry loop on rerender');
  ui.element.querySelector('[data-retry-introductions]').fire('click'); await model.introductionPending;
  ui.render(); assert.equal(count,2); assert.deepEqual(matching(model),['src/shared']);
  assert.match(ui.element.innerHTML,/1 items shown · introduced by this author/);
  cache.select(model,''); ui.render(); assert.equal(matching(model).length,4);
});

test('the same dropdown loads integration authors and only then offers their scoped selection', async () => {
  const cache=createComparisonAuthorCache(async command=>command==='comparison_authors' ? authors : introduced);
  const ui=renderer(cache), model=ui.render(); await cache.load(model); ui.render();
  assert.match(ui.element.innerHTML,/Load introduction history/);
  const select=ui.element.querySelector('[data-author-select]'); select.value='@introduced'; select.fire('change'); ui.render();
  assert.match(ui.element.innerHTML,/Reading introduction history/);
  await model.introductionPending; ui.render();
  assert.match(ui.element.innerHTML,/Choose an author under Introduced by author/);
  assert.match(ui.element.innerHTML,/introduced:email:integrator@test/);
  const choose=ui.element.querySelector('[data-author-select]'); choose.value='introduced:email:integrator@test'; choose.fire('change'); ui.render();
  assert.equal(model.mode,'introduced'); assert.equal(model.selected,introduced[0].key);
  assert.equal((ui.element.innerHTML.match(/<select /g)||[]).length,1);
  assert.match(ui.element.innerHTML,/may include work by others/);
  assert.equal((ui.element.innerHTML.match(/data-author-direct checked/g)||[]).length,0);
});

test('integration evidence names original contributors and preserves the first-parent diff', async () => {
  const cache=createComparisonAuthorCache(async command=>command==='comparison_authors' ? authors : introduced);
  const ui=renderer(cache), model=ui.render(); await cache.load(model); cache.select(model,introduced[0].key,'introduced'); await cache.loadIntroductions(model);
  const evidence=helpers.comparisonAuthorEvidence(rows[0],model);
  assert.equal(evidence[0].path,'src/shared'); assert.equal(evidence[0].parent,'first-parent');
  const html=ui.h.comparisonEvidenceBodyHtml(rows[0],model,evidence);
  assert.match(html,/Introduced by Integrator/);
  assert.match(html,/File contributors in this comparison: Alice.*Bob/);
  assert.match(html,/merge \(result versus first parent\)/);
  assert.match(html,/data-evidence-diff="0"/);
  assert.match(html,/Full comparison shown/);
  assert.doesNotMatch(html,/A contribution from this author remains/);
  assert.match(comparisonAuthorDiagnostic(model,4,1,false),/mode=introduced phase=complete/);
});

test('parent pointer evidence never opens its commit IDs in a child file diff', async () => {
  const data=[{...introduced[0],evidence:{'src/shared':[{commit:'parent-merge',parent:'parent-before',date:'2026-10-25',kind:'parent-merge-submodule',
    witness_path:'modules/<unit>',parent_repository:'/parent',before_revision:'child-a',after_revision:'child-b'}]}}];
  const cache=createComparisonAuthorCache(async command=>command==='comparison_authors' ? authors : data);
  const ui=renderer(cache), model=ui.render(); await cache.load(model); cache.select(model,introduced[0].key,'introduced'); await cache.loadIntroductions(model);
  const evidence=helpers.comparisonAuthorEvidence(rows[0],model);
  const html=ui.h.comparisonEvidenceBodyHtml(rows[0],model,evidence);
  assert.match(html,/parent merge changed the submodule pointer/);
  assert.match(html,/modules\/&lt;unit&gt;/); assert.match(html,/child-a.*child-b/);
  assert.match(html,/Parent repository: \/parent/);
  assert.doesNotMatch(html,/<unit>/);
  assert.match(html,/data-evidence-diff="0"/);
  const button={dataset:{evidenceDiff:'0'},addEventListener(_,fn){this.click=fn;}};
  const body={innerHTML:'',querySelectorAll:()=>[button]};
  const details={open:true,dataset:{authorEvidence:'src/shared'},addEventListener(_,fn){this.toggle=fn;},querySelector:()=>body};
  const opened=[];
  ui.h.openGitRevisionFileCompare=()=>assert.fail('parent commits must not be read in the child repository');
  ui.h.openSubmoduleComparisonDetails=(r,nested,args)=>opened.push({r,nested,args});
  ui.h.state.compareMode='submodule';
  ui.h.bindComparisonEvidence({querySelectorAll:()=>[details]},rows,model); details.toggle();
  button.click({preventDefault(){},stopPropagation(){}});
  assert.equal(opened[0].r.relative_path,'modules/<unit>');
  assert.equal(opened[0].args.touch.parent_repository,'/parent');
  assert.equal(opened[0].args.touch.parent,'parent-before');
  assert.equal(opened[0].args.touch.commit,'parent-merge');
  assert.equal(opened[0].nested,true);
});

test('changing child refs drops parent provenance, reversing swaps both pairs', () => {
  const source={repositoryPath:'/parent',relativePath:'unit',leftRevision:'p0',rightRevision:'p1',childLeft:'a',childRight:'b'};
  const direct=helpers.comparisonParentContext(source,'a','b');
  assert.deepEqual(direct,{repositoryPath:'/parent',relativePath:'unit',leftRevision:'p0',rightRevision:'p1'});
  assert.deepEqual(helpers.comparisonParentContext(source,'b','a'),{...direct,leftRevision:'p1',rightRevision:'p0'});
  assert.equal(helpers.comparisonParentContext(source,'a','new-ref'),null);
  assert.equal(helpers.comparisonParentContext(null,'a','b'),null);
  assert.equal(source.leftRevision,'p0');
});

test('merge-only scoped differences can load introduction authors even with no personal touches', async () => {
  const cache=createComparisonAuthorCache(async command=>command==='comparison_authors' ? [] : introduced);
  const ui=renderer(cache), model=ui.render(); await cache.load(model);
  cache.select(model,introduced[0].key,'introduced'); await cache.loadIntroductions(model); ui.render();
  assert.deepEqual(matching(model),['src/shared']);
  assert.match(ui.element.innerHTML,/1 items shown · introduced by this author/);
  assert.doesNotMatch(ui.element.innerHTML,/No author touches found/);
  const html=ui.h.comparisonEvidenceBodyHtml(rows[0],model,helpers.comparisonAuthorEvidence(rows[0],model));
  assert.match(html,/No non-merge touches identified/);
  assert.match(html,/merge \(result versus first parent\)/);
});

test('a pending strict result cannot narrow the newly chosen introduction filter', async () => {
  let finishStrict;
  const cache=createComparisonAuthorCache(command=>command==='comparison_authors' ? Promise.resolve(authors)
    : command==='comparison_introductions' ? Promise.resolve(introduced)
    : new Promise(resolve=>{finishStrict=resolve;}));
  const model=cache.get(context); await cache.load(model); cache.select(model,authors[0].key); cache.setDirectOnly(model,true);
  const strict=cache.loadStrict(model); await Promise.resolve();
  cache.select(model,introduced[0].key,'introduced'); await cache.loadIntroductions(model);
  finishStrict({paths:['src/solo']}); await strict;
  assert.deepEqual(matching(model),['src/shared']);
  assert.equal(comparisonAuthorStrict(model),false);
});

test('conversion imports stay in All with structural evidence, but do not qualify Touched or Shared', async () => {
  const conversion={...witness('conversion'),kind:'folder-submodule-conversion',witness_path:'src',side:'right',repository_path:'/repo',repository_id:'repo-id'};
  const edit={...witness('edit'),kind:'file-change',side:'right',repository_path:'/repo',repository_id:'repo-id'};
  const data=[{...authors[0],paths:['src/solo'],evidence:{'src/shared':[conversion],'src/solo':[conversion,edit],'src/unattributed':[conversion]}},
    {...authors[1],paths:['src/shared']}, {key:'converter',name:'Only structure',paths:[],evidence:{'src/other':[conversion]}}];
  const calls=[];
  const cache=createComparisonAuthorCache(async (...args)=>{calls.push(args);return data;});
  const ui=renderer(cache), model=ui.render(); await cache.load(model); ui.render();
  assert.deepEqual(matching(model),rows.map(r=>r.relative_path));
  assert.deepEqual(comparisonAuthorContributors(rows[0],model).map(a=>a.name),['Bob']);
  assert.equal(comparisonAuthorContributors(rows[3],model).length,0);
  assert.match(ui.h.comparisonEvidenceRowHtml(rows[3],model),/History · reconfiguration/);
  assert.doesNotMatch(ui.element.innerHTML,/<option value="converter">/);
  const structure=helpers.comparisonStructuralEvidence(rows[3],model);
  const html=ui.h.comparisonEvidenceBodyHtml(rows[3],model,structure);
  assert.match(html,/This item differs between the selected versions/);
  assert.match(html,/Reconfiguration only · individual file edits not established/);
  assert.match(html,/Right history.*Repository: \/repo/);
  assert.doesNotMatch(html,/Introduced by|A contribution from this author remains/);
  cache.select(model,authors[0].key);
  assert.deepEqual(matching(model),['src/solo']);
  assert.deepEqual(helpers.comparisonAuthorEvidence(rows[1],model).map(e=>e.commit),['edit']);
  cache.setMode(model,'shared'); assert.deepEqual(matching(model),[]);
  cache.setMode(model,'all'); assert.equal(matching(model).length,4);
  assert.equal(calls.length,1,'classification and disclosure reuse the loaded response');
});

test('Touched, Introduced, Shared and All explanations follow the current selection, never a late result', async () => {
  let complete;
  const cache=createComparisonAuthorCache(command=>command==='comparison_authors' ? Promise.resolve(authors)
    : new Promise(resolve=>{complete=resolve;}));
  const ui=renderer(cache), model=ui.render(); await cache.load(model);
  cache.select(model,introduced[0].key,'introduced'); const pending=cache.loadIntroductions(model); await Promise.resolve();
  cache.setMode(model,'all'); complete(introduced); await pending;
  let html=ui.h.comparisonEvidenceBodyHtml(rows[0],model,comparisonContributorEvidence(rows[0],model));
  assert.match(html,/This item differs between the selected versions/); assert.doesNotMatch(html,/Introduced by/);
  cache.select(model,introduced[0].key,'introduced');
  html=ui.h.comparisonEvidenceBodyHtml(rows[0],model,helpers.comparisonAuthorEvidence(rows[0],model));
  assert.match(html,/Introduced by Integrator/);
  cache.select(model,authors[0].key);
  html=ui.h.comparisonEvidenceBodyHtml(rows[0],model,helpers.comparisonAuthorEvidence(rows[0],model));
  assert.match(html,/This author touched this item/); assert.doesNotMatch(html,/Introduced by/);
  cache.setMode(model,'shared');
  html=ui.h.comparisonEvidenceBodyHtml(rows[0],model,comparisonContributorEvidence(rows[0],model));
  assert.match(html,/At least two recorded authors/); assert.doesNotMatch(html,/Introduced by/);
});

test('diagnostics carry exact mode, witness and anonymous repository context without contents or identities', () => {
  const model={context,mode:'touched',selected:'email:alice@test',directOnly:true};
  const touch={...witness('commit'),path:'module/old\nname',kind:'file-change',side:'left',repository_id:'anonymous-id',repository_path:'/private-repo',authorName:'Alice',authorEmail:'alice@test'};
  const message=helpers.comparisonEvidenceDiagnostic(row('module/new'),model,touch);
  const data=JSON.parse(message.slice(message.indexOf(': ')+2));
  assert.equal(data.mode,'touched'); assert.equal(data.surviving,true);
  assert.equal(data.side,'left'); assert.equal(data.kind,'file-change'); assert.equal(data.repository_id,'anonymous-id');
  assert.equal(data.item,'module/new'); assert.equal(data.witness_path,'module/old\nname');
  assert.equal(data.commit,'commit'); assert.equal(data.parent,'base');
  assert.doesNotMatch(message,/Alice|alice@test|\/private-repo|\n/);
});

test('surviving attribution persists through rerenders only, not another author, scope or return navigation', async () => {
  const cache=createComparisonAuthorCache(async()=>authors);
  const a=cache.get(context); await cache.load(a); cache.select(a,authors[0].key); cache.setDirectOnly(a,true);
  assert.equal(comparisonAuthorStrict(cache.get(context)),true,'ordinary rerender preserves explicit choice');
  const b=cache.get({...context,rightRevision:'another'}); await cache.load(b);
  assert.equal(comparisonAuthorStrict(b),false);
  assert.equal(comparisonAuthorStrict(cache.get(context)),false,'returning to cached results cannot resurrect strict filtering');
  cache.setDirectOnly(a,true); cache.select(a,authors[1].key);
  assert.equal(comparisonAuthorStrict(a),false);
  assert.deepEqual(matching(a),['src/shared','src/other']);
});
