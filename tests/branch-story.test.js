'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { buildBranchStory, buildGraphModel, branchStoryIncomingPath } = require('../frontend/graph-model.js');
const c = (id, parents = [], refs = []) => ({ id, parents, refs });
const ref = (name, kind = 'local_branch') => ({ name, kind });
const ids = story => story.commits.map(commit => commit.id);
const { branchStoryStart } = require('../frontend/graph-model.js');
const { lastFirstParentMerge } = require('../frontend/graph-model.js');

test('Last merge follows first parents, ignoring imported merges and commit timestamps', () => {
  const commits = [c('tip', ['linear']), c('linear', ['merge']), c('merge', ['olderMerge', 'imported']),
    c('imported', ['a', 'b']), c('olderMerge', ['root', 'c']), c('root')];
  commits[3].timestamp = 999999;
  assert.equal(lastFirstParentMerge(commits, 'tip').id, 'merge');
  assert.equal(lastFirstParentMerge(commits, 'merge').id, 'merge');
  assert.equal(lastFirstParentMerge([c('tip', ['missing']), c('other', ['a','b'])], 'tip').id, null);
  assert.match(lastFirstParentMerge([c('tip', ['missing'])], 'tip').reason, /load older/);
  assert.equal(lastFirstParentMerge([c('linear', ['root']), c('root')], 'linear').id, null);
  assert.equal(lastFirstParentMerge([], null).id, null);
  assert.equal(lastFirstParentMerge([c('a', ['b']), c('b', ['a'])], 'a').id, null);
});

test('Branch Start uses creation evidence and remains B after main merges, not current base D', () => {
  const commits = originalForkFixture();
  const story = buildBranchStory(commits, 'F1');
  const before = JSON.stringify(commits);
  assert.equal(story.baseId, 'D');
  assert.equal(branchStoryStart(commits, story, 'B').id, 'B');
  assert.equal(branchStoryStart(commits, buildBranchStory(commits, 'D1'), 'B').id, 'B');
  assert.equal(JSON.stringify(commits), before);
  assert.deepEqual(ids(story), commits.map(c => c.id));
  assertRealEdges(story);
});

test('Branch Start refuses missing evidence, recently created tracking branch and partial ancestry', () => {
  const commits = originalForkFixture();
  const story = buildBranchStory(commits, 'F1');
  for (const birth of [null, 'F1', 'C1', 'D']) assert.equal(branchStoryStart(commits, story, birth).id, null);
  assert.equal(branchStoryStart(commits.slice(0, 6), buildBranchStory(commits.slice(0, 6), 'F1'), 'B').id, null);
  const ff = [c('tip', ['B'], [ref('feature'), ref('origin/main', 'remote_branch')]), c('B')];
  assert.equal(branchStoryStart(ff, buildBranchStory(ff, 'tip'), 'B').id, null);
});

test('Branch Start can be confirmed without loading ancestors older than the recorded start', () => {
  const commits = originalForkFixture().filter(commit => commit.id !== 'A');
  const story = buildBranchStory(commits, 'F1');
  assert.equal(story.incomplete, true);
  assert.equal(branchStoryStart(commits, story, 'B').id, 'B');
  assert.equal(story.commits.find(commit => commit.id === 'B').parents[0], 'A');
});
function assertRealEdges(story) {
  const model = buildGraphModel(story.commits, story.tipId);
  for (const node of model) {
    const original = story.commits.find(commit => commit.id === node.commitId);
    assert.deepEqual(new Set(node.parents.map(parent => parent.commitId)), new Set(original.parents));
  }
}

test('Branch Story: linear selected history excludes unrelated tips without modifying Full Graph input', () => {
  const commits = [c('unrelated', ['root']), c('tip', ['a'], [ref('feature')]), c('a', ['root']), c('root')];
  const original = JSON.stringify(commits);
  const before = buildGraphModel(commits, 'tip');
  const story = buildBranchStory(commits, 'tip');
  assert.deepEqual(ids(story), ['tip', 'a', 'root']);
  assert.deepEqual([...story.spine], ['tip', 'a', 'root']);
  assert.equal(story.incoming.size, 0);
  assertRealEdges(story);
  assert.equal(JSON.stringify(commits), original);
  assert.deepEqual(buildGraphModel(commits, 'tip'), before);
  assert.strictEqual(story.commits[0], commits[1]);
});

test('Branch Story: multiple incoming merges keep real parents and backend order', () => {
  const commits = [c('other'), c('m2', ['f2', 'b2']), c('f2', ['m1']), c('b2', ['b1'], [ref('origin/main', 'remote_branch')]), c('m1', ['f1', 'b1']), c('f1', ['root']), c('b1', ['root']), c('root')];
  const story = buildBranchStory(commits, 'm2');
  assert.deepEqual(ids(story), commits.slice(1).map(commit => commit.id));
  assert.deepEqual([...story.spine], ['m2', 'f2', 'm1', 'f1', 'root']);
  assert.deepEqual([...story.incoming.keys()], ['b2', 'b1']);
  assert.equal(story.incoming.get('b1').refs.length, 0); // Historical Path
  assert.equal(story.incoming.get('b1').onBaseHistory, 'origin/main');
  assert.equal(story.baseId, 'b2'); // latest shared base, NOT branch creation
  assertRealEdges(story);
});

// The original fork B and today's merge-base D are different facts. Importing
// main must not trim B (or its older ancestry) out of the selectable graph.
function originalForkFixture() {
  return [
    c('F1', ['M'], [ref('feature')]), c('M', ['D1', 'D']),
    c('D1', ['C1']), c('C1', ['B']),
    c('D', ['C'], [ref('origin/main', 'remote_branch')]),
    c('C', ['B']), c('B', ['A']), c('A'),
  ];
}

test('Branch Story: original fork B stays on the development path before and after main merges', () => {
  const commits = originalForkFixture();
  const before = buildBranchStory(commits, 'D1');
  assert.deepEqual(ids(before), ['D1', 'C1', 'B', 'A']);
  assert.equal(before.baseId, 'B');
  assertRealEdges(before);

  const after = buildBranchStory(commits, 'F1');
  assert.deepEqual(ids(after), commits.map(commit => commit.id));
  assert.deepEqual([...after.spine], ['F1', 'M', 'D1', 'C1', 'B', 'A']);
  assert.equal(after.baseId, 'D', 'Shared base advances, not the original fork');
  assert.deepEqual(after.incoming.get('D').merges, ['M']);
  assert.strictEqual(after.commits.find(commit => commit.id === 'B'), commits[6]);
  assertRealEdges(after);

  const later = [c('M2', ['F1', 'E']), c('E', ['D'], [ref('origin/main', 'remote_branch')]),
    ...commits.map(commit => commit.id === 'D' ? { ...commit, refs: [] } : commit)];
  const again = buildBranchStory(later, 'M2');
  assert.equal(again.baseId, 'E');
  assert.ok(again.spine.has('B'));
  assert.deepEqual(again.incoming.get('D').merges, ['M']);
  assert.deepEqual(again.incoming.get('E').merges, ['M2']);
  assertRealEdges(again);
});

test('Branch Story: an original fork outside loaded history appears when older nodes are loaded', () => {
  const commits = originalForkFixture();
  const partial = buildBranchStory(commits.slice(0, 6), 'F1');
  assert.equal(partial.incomplete, true);
  assert.equal(partial.baseId, null);
  assert.ok(!ids(partial).includes('B'), 'Never synthesize an unloaded node');
  const complete = buildBranchStory(commits, 'F1');
  assert.equal(complete.incomplete, false);
  assert.ok(ids(complete).includes('B'));
  assertRealEdges(complete);
});

test('Branch Story: fast-forward and squash histories never invent a merge or deleted branch', () => {
  const commits = [c('tip', ['squash'], [ref('feature'), ref('origin/main', 'remote_branch')]), c('squash', ['root']), c('root')];
  const story = buildBranchStory(commits, 'tip');
  assert.equal(story.incoming.size, 0);
  assert.equal(story.baseId, null);
  assert.deepEqual(ids(story), ['tip', 'squash', 'root']);
  assertRealEdges(story);
});

test('Branch Story: every merge exposes incoming parents, including missing and nested historical paths', () => {
  const commits = [c('tip', ['own', 'side', 'unloaded']), c('own', ['root']), c('side', ['root', 'nested'], [ref('origin/topic', 'remote_branch')]), c('nested', ['root']), c('root')];
  const story = buildBranchStory(commits, 'tip');
  assert.deepEqual(story.mergeParents.get('tip'), [
    {id:'side',loaded:true,refs:[ref('origin/topic', 'remote_branch')]},
    {id:'unloaded',loaded:false,refs:[]},
  ]);
  assert.deepEqual(story.mergeParents.get('side'), [{id:'nested',loaded:true,refs:[]}]);
  assert.equal(story.incomplete, true);
  assertRealEdges(story);
});

test('Branch Story UI: missing merge source is explicit; loaded sources can be selected; Full Graph is unchanged', () => {
  const h = rendererHarness();
  h.graph.commits = [c('tip', ['a','side'], [ref('feature')]),c('a')];
  h.context.renderGraph();
  const full = h.refs.graph.innerHTML;
  h.context.activeBranchStoryView().mode = 'story'; h.context.renderGraph();
  assert.match(h.refs.graph.innerHTML, /data-story-parent="side"[^>]*><code>side<\/code><span>Load history<\/span>/);
  assert.match(h.refs.graph.innerHTML, /Not loaded yet/);
  assert.match(h.refs.graph.innerHTML, /Historical Path/);
  h.context.activeBranchStoryView().mode = 'full'; h.context.renderGraph();
  assert.equal(h.refs.graph.innerHTML, full);
  h.graph.commits = [...h.graph.commits, c('side', [], [ref('origin/topic','remote_branch')])];
  h.context.activeBranchStoryView().mode = 'story'; h.context.renderGraph();
  assert.match(h.refs.graph.innerHTML, /Current refs: origin\/topic/);
  assert.match(h.refs.graph.innerHTML, /data-story-parent="side"[^>]*><code>side<\/code><span>Show commit<\/span>/);
  assert.doesNotMatch(h.refs.graph.innerHTML, /Not loaded yet/);
});

test('Branch Story incoming navigation reuses bounded paging and stops after context changes', async () => {
  const h = rendererHarness();
  h.state.view = 'graph';
  const view = h.context.activeBranchStoryView(); view.mode = 'story';
  let pages = 0, selected = null, visible = false, messages = [];
  h.graph.commitsTruncated = true;
  h.context.graphCommitRow = () => visible ? {scrollIntoView(){}} : null;
  h.context.selectCommit = id => {selected = id;};
  h.context.status = text => messages.push(text);
  h.context.loadOlderGraphCommits = async () => {pages++; visible = pages === 2; return true;};
  vm.runInContext('async ' + appFunction('showStoryMergeParent'), h.context);
  await h.context.showStoryMergeParent('side');
  assert.equal(pages,2); assert.equal(selected,'side'); assert.equal(view.loadingParent,null);
  await h.context.showStoryMergeParent('side'); assert.equal(pages,2,'Already loaded: no Git call');
  visible = false; pages = 0;
  h.context.loadOlderGraphCommits = async () => {pages++; return true;};
  await h.context.showStoryMergeParent('far');
  assert.equal(pages,5); assert.match(messages.pop(), /beyond the loaded pages/);
  pages = 0; selected = null;
  h.context.loadOlderGraphCommits = async () => {pages++; view.selection = 'branch:another'; return true;};
  await h.context.showStoryMergeParent('stale');
  assert.equal(pages,1); assert.equal(selected,null);
  h.context.loadOlderGraphCommits = async () => false;
  await h.context.showStoryMergeParent('failed');
  assert.equal(view.loadingParent,null);
});

test('Branch Story SVG marks unloaded parents instead of silently dropping the incoming edge', () => {
  const h = rendererHarness();
  const commits = [c('merge',['own','missing']),c('own')];
  const model = buildGraphModel(commits,'merge');
  const svg = {setAttribute(){},innerHTML:''};
  h.refs.graph.querySelector = () => svg;
  h.refs.graph.querySelectorAll = () => commits.map((commit,i) => ({offsetTop:i*70,offsetHeight:60,classList:{contains:()=>false}}));
  h.context.palette = ['blue','gold']; h.context.laneX = lane => lane*24+12;
  vm.runInContext(appFunction('drawGraphOverlay'),h.context);
  h.context.drawGraphOverlay(model,72,{branchStory:true});
  assert.match(svg.innerHTML, /graph-missing-parent/);
  assert.match(svg.innerHTML, /Parent missing is outside loaded history/);
  h.context.drawGraphOverlay(model,72,{});
  assert.doesNotMatch(svg.innerHTML, /graph-missing-parent/);
});

test('Story incoming edge leaves the main lane within 24px even for a distant parent', () => {
  for (const y of [200, 6000, 50000]) {
    const from = {x:15,y:100}, to = {x:45,y};
    const path = branchStoryIncomingPath(from,to,45,30,90);
    assert.ok(path.startsWith('M15 100 C15 112 45 112 45 124 L45 '));
    assert.ok(path.endsWith(`45 ${y}`), 'Connect to the real parent, not an artificial endpoint');
    assert.deepEqual(from,{x:15,y:100}); assert.deepEqual(to,{x:45,y});
  }
});

test('Story incoming edge handles short, same-column and reversed edges inside the gutter', () => {
  assert.equal(branchStoryIncomingPath({x:15,y:100},{x:45,y:118},45,30,90),
    'M15 100 C15 103 45 103 45 106 L45 112 C45 115 45 115 45 118');
  assert.equal(branchStoryIncomingPath({x:15,y:100},{x:15,y:6000},15,30,90),
    'M15 100 C15 112 30 112 30 124 L30 5976 C30 5988 15 5988 15 6000');
  assert.match(branchStoryIncomingPath({x:75,y:100},{x:75,y:6000},75,30,90), /C75 112 60 112 60 124/);
  assert.equal(branchStoryIncomingPath({x:45,y:118},{x:15,y:100},15,30,90),
    'M45 118 C45 115 15 115 15 112 L15 106 C15 103 15 103 15 100');
});

test('Story SVG separates incoming parents, retains every edge and leaves Full Graph rendering unchanged', () => {
  const h = rendererHarness();
  const commits = [c('merge',['own','incoming','another']),c('own'),c('incoming'),c('another')];
  const model = buildGraphModel(commits,'merge');
  const originalModel = JSON.stringify(model);
  const svg = {setAttribute(){},innerHTML:''};
  h.refs.graph.querySelector = () => svg;
  h.refs.graph.querySelectorAll = () => commits.map((commit,i) => ({offsetTop:[0,70,6000,6500][i],offsetHeight:60,classList:{contains:()=>false}}));
  h.context.palette = ['blue','gold','green']; h.context.laneX = lane => lane*24+12;
  vm.runInContext(appFunction('drawGraphOverlay'),h.context);
  h.context.drawGraphOverlay(model,72,{});
  const full = svg.innerHTML;
  assert.match(full,/M12 30 C12 3030 36 3030 36 6030/,'Original Full Graph Bézier');
  h.context.drawGraphOverlay(model,72,{branchStory:true});
  assert.match(svg.innerHTML,/M12 30 C12 42 36 42 36 54 L36 6006/);
  assert.match(svg.innerHTML,/M12 30 C12 42 60 42 60 54 L60 6506/);
  assert.equal((svg.innerHTML.match(/class="graph-edge graph-incoming-edge"/g)||[]).length,2);
  assert.match(svg.innerHTML, /x1="12" y1="30" x2="12" y2="100"/, 'First parent unchanged');
  assert.equal(JSON.stringify(model),originalModel,'Routing cannot alter lanes, nodes or parent IDs');
  h.context.drawGraphOverlay(model,72,{});
  assert.equal(svg.innerHTML,full,'Switching back restores exact Full Graph SVG');
  h.context.drawGraphOverlay(model,72,{branchStory:true,disconnected:true});
  assert.doesNotMatch(svg.innerHTML,/graph-incoming-edge/,'No edges in disconnected search results');
});

test('Incoming history pagination cannot append old results after a same-repository refresh', async () => {
  const h = rendererHarness();
  h.state.view = 'graph'; h.state.submoduleGraph = null;
  h.context.GRAPH_COMMIT_WINDOW = 500;
  h.context.appendOlderGraphRows = () => {};
  let complete;
  h.context.invoke = () => new Promise(resolve => {complete = resolve;});
  vm.runInContext('async ' + appFunction('loadOlderGraphCommits'), h.context);
  const pending = h.context.loadOlderGraphCommits();
  const refreshed = [c('new-head')];
  h.graph.commits = refreshed; h.state.commits = refreshed;
  complete({commits:[c('old-page')],has_more:false});
  assert.equal(await pending,false);
  assert.strictEqual(h.state.commits,refreshed);
  assert.equal(h.context.loadingOlderCommits,false);
});

test('Branch Story: historical incoming paths keep multiple labels at different nodes', () => {
  const refs = [ref('release'), ref('origin/release', 'remote_branch'), ref('v1', 'tag')];
  const commits = [c('m', ['a', 'incoming']), c('incoming', ['older']), c('a', ['root']), c('older', ['root'], refs), c('root')];
  const story = buildBranchStory(commits, 'm');
  assert.equal(story.incoming.get('incoming').refs.length, 0);
  assert.deepEqual(story.commits.find(commit => commit.id === 'older').refs, refs);
  assert.equal(story.commits.find(commit => commit.id === 'incoming').refs.length, 0);
  assertRealEdges(story);
});

test('Branch Story: a reliable divergence/shared base is identified from a complete DAG', () => {
  const commits = [c('feature', ['base']), c('main', ['base'], [ref('origin/main', 'remote_branch')]), c('base', ['root']), c('root')];
  const story = buildBranchStory(commits, 'feature');
  assert.equal(story.baseId, 'base');
  assert.equal(story.baseRef, 'origin/main');
});

test('Branch Story: partial histories never invent a base or connect across missing parents', () => {
  const commits = [c('m', ['feature', 'main']), c('feature', ['unknown']), c('main', ['unknown'], [ref('origin/main', 'remote_branch')])];
  const story = buildBranchStory(commits, 'm');
  assert.equal(story.baseId, null);
  assert.equal(story.incomplete, true);
  assert.equal(story.incoming.get('main').onBaseHistory, 'origin/main');
  assertRealEdges(story);
});

test('Branch Story: ambiguous criss-cross common ancestors are not labelled as a unique start', () => {
  const commits = [c('f', ['a', 'b']), c('m', ['b', 'a'], [ref('origin/main', 'remote_branch')]), c('a', ['root']), c('b', ['root']), c('root')];
  assert.equal(buildBranchStory(commits, 'f').baseId, null);
});

test('Branch Story: octopus merges preserve every incoming path', () => {
  const commits = [c('m', ['a', 'b', 'c']), c('a', ['root']), c('b', ['root']), c('c', ['root']), c('root')];
  const story = buildBranchStory(commits, 'm');
  assert.equal(story.incoming.size, 2);
  assertRealEdges(story);
});

test('Branch Story: an unloaded tip is empty, never replaced with a different branch', () => {
  const story = buildBranchStory([c('other')], 'missing');
  assert.equal(story.tipId, null);
  assert.deepEqual(story.commits, []);
});

test('Branch Story: repeated imports of the same parent preserve both merge links', () => {
  const story = buildBranchStory([c('m2', ['m1', 'b']), c('m1', ['a', 'b']), c('a'), c('b')], 'm2');
  assert.deepEqual(story.incoming.get('b').merges, ['m2', 'm1']);
  assertRealEdges(story);
});

test('Branch Story: loading more history recomputes the projection without mutating earlier commits', () => {
  const partial = [c('tip', ['a'])];
  assert.equal(buildBranchStory(partial, 'tip').incomplete, true);
  const complete = [...partial, c('a')];
  const story = buildBranchStory(complete, 'tip');
  assert.equal(story.incomplete, false);
  assert.deepEqual(ids(story), ['tip', 'a']);
  assert.deepEqual(partial[0].parents, ['a']);
});

// Render the real Branch Map function with small DOM/IPC stand-ins. This
// checks mode isolation as well as the projection's topology above.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
function appFunction(name) {
  const start = app.indexOf(`function ${name}(`);
  const firstLine = app.slice(start, app.indexOf('\n', start));
  if (firstLine.endsWith('}')) return firstLine;
  const end = app.indexOf('\n}', start);
  assert.ok(start >= 0 && end > start, `missing real app function ${name}`);
  return app.slice(start, end + 2);
}
function rendererHarness() {
  const element = () => ({ innerHTML: '', value: '', style: { setProperty() {} }, querySelector: () => null, querySelectorAll: () => [], addEventListener() {} });
  const commits = [c('unrelated', ['root'], [ref('origin/main', 'remote_branch')]), c('tip', ['a'], [ref('feature')]), c('a', ['root']), c('root')];
  const graph = { path: '/fixture', name: 'fixture', currentBranch: 'feature', headOid: 'tip', headDetached: false, commits, branches: [{name:'feature'}, {name:'origin/main',remote:true}], stashes: [] };
  const state = { repository: {}, graphPrimaryBranch:'branch:feature', graphRefFilter:'all', graphOnlySearchMatches:false, historyKind:'', historyScope:'' };
  const refs = { search:element(), graphView:element(), laneLegend:element(), graph:element() };
  let backendAnnotations = 0, interactions = 0;
  const context = {
    GitActivity: require('../frontend/git-activity.js'),
    state, refs, ...require('../frontend/graph-model.js'), activeGraphData:()=>graph,
    performance, jsPerfLog:()=>{}, LANE_WIDTH:24, headMainMergeBaseFetchKey:null,
    ensureStoryCreation:()=>null,
    ensureHeadMainMergeBase:()=>{ backendAnnotations++; return null; },
    ensureBranchDivergence:()=>{ backendAnnotations++; return null; },
    esc:value=>String(value ?? ''), commitSubjectHtml:value=>String(value ?? ''),
    refsBadges:refs=>JSON.stringify(refs), graphCommitCompareAnchorMatches:()=>false,activeRepositoryContext:()=>({}),
    graphHeadBannerHtml:()=>'', loadingOlderCommits:false, GRAPH_LEGEND_HTML:'<details class="graph-legend"></details>',
    $:()=>null, requestAnimationFrame:()=>{}, wireGraphRowInteractions:()=>{interactions++;},
    loadOlderGraphCommits:()=>{}, jumpToGraphHead:()=>{}, graphCommitRow:()=>null, status:()=>{},
    lastGraphRenderContext:null, lastGraphModel:null,lastGraphLanesWidth:0,lastGraphOnlySearchMatches:false,
  };
  vm.createContext(context);
  const names = ['activeGraphPrimaryBranch','setActiveGraphPrimaryBranch','activeGraphRefFilter','setActiveGraphRefFilter','activeBranchStoryView','traceGraphStart','resolvePrimarySelection','graphNavigationTargets','jumpToGraphNavigation','jumpToGraphBranchStart','jumpToGraphLastMerge','mergeCommitPresentation','buildCommitRowHtml','graphTruncationStubHtml','renderGraph','appendOlderGraphRows'];
  vm.runInContext('const branchStoryViews = new WeakMap();\n'+names.map(appFunction).join('\n'),context);
  return {context,state,refs,graph,annotationCalls:()=>backendAnnotations,interactions:()=>interactions};
}

test('Branch Story UI: Full Graph rows and settings are exactly restored after changing story branch/filters', () => {
  const h = rendererHarness();
  h.context.renderGraph();
  const fullHtml = h.refs.graph.innerHTML;
  const fullModel = JSON.stringify(h.context.lastGraphModel);
  const view = h.context.activeBranchStoryView();
  view.mode = 'story'; view.selection = 'remote:origin/main'; view.refFilter = 'releases';
  const callsBefore = h.annotationCalls();
  h.context.renderGraph();
  assert.equal(h.annotationCalls(), callsBefore, 'Story must add no backend annotation queries');
  assert.match(h.refs.laneLegend.innerHTML, /Story branch/);
  assert.doesNotMatch(h.refs.graph.innerHTML, /data-id="tip"/);
  view.mode = 'full'; h.context.renderGraph();
  assert.equal(h.refs.graph.innerHTML, fullHtml);
  assert.equal(JSON.stringify(h.context.lastGraphModel), fullModel);
  assert.equal(h.state.graphPrimaryBranch, 'branch:feature');
  assert.equal(h.state.graphRefFilter, 'all');
  assert.equal(h.interactions(), 3, 'Both modes keep existing row interaction wiring');
});

test('both graph modes show real local date and time without changing commit selection', () => {
  const h = rendererHarness();
  const timestamp = new Date(2026, 9, 20, 9, 14, 5).getTime() / 1000;
  h.graph.commits.forEach(commit => { commit.timestamp = timestamp; });
  for (const mode of ['full', 'story']) {
    h.context.activeBranchStoryView().mode = mode;
    h.context.renderGraph();
    assert.match(h.refs.graph.innerHTML, /class="commit-date"[^>]*><span>2026-10-20<\/span><time datetime="[^"]+">09:14<\/time>/);
    assert.match(h.refs.graph.innerHTML, /data-id="tip"/);
  }
});

test('Branch Story UI: parent, submodule and reopened repository contexts are isolated', () => {
  const h = rendererHarness();
  const parent = h.context.activeBranchStoryView(); parent.mode = 'story';
  h.state.submoduleGraph = {};
  assert.equal(h.context.activeBranchStoryView().mode, 'full');
  h.state.submoduleGraph = null;
  assert.strictEqual(h.context.activeBranchStoryView(), parent);
  h.state.repository = {};
  assert.equal(h.context.activeBranchStoryView().mode, 'full');
  h.state.historyScope = 'src';
  assert.equal(h.context.activeBranchStoryView(), null, 'Path histories must not gain Story controls');
});

test('Branch Story UI: search keeps connected real nodes without changing Full Graph search preference', () => {
  const h = rendererHarness();
  h.context.activeBranchStoryView().mode = 'story';
  h.refs.search.value = 'tip'; h.state.graphOnlySearchMatches = true;
  h.context.renderGraph();
  assert.deepEqual(Array.from(h.context.lastGraphModel, node=>node.commitId), ['tip','a','root']);
  assert.ok(h.context.lastGraphModel[0].parents.length);
  assert.equal(h.state.graphOnlySearchMatches,true);
  h.context.activeBranchStoryView().mode = 'full'; h.context.renderGraph();
  assert.equal(h.context.lastGraphOnlySearchMatches,true);
});

test('Branch Story UI: loading older commits uses a full projection render, not the Full Graph append path', () => {
  const h = rendererHarness();
  h.context.activeBranchStoryView().mode = 'story';
  h.context.renderGraph();
  const before = h.interactions();
  h.context.appendOlderGraphRows(1);
  assert.equal(h.interactions(),before+1);
  assert.ok(h.context.lastGraphRenderContext.story);
});

test('Branch Story UI: original fork is a normal selectable row, distinct from the current shared base', () => {
  const h = rendererHarness();
  h.graph.commits = originalForkFixture(); h.graph.headOid = 'F1';
  h.context.activeBranchStoryView().mode = 'story';
  h.context.renderGraph();
  const html = h.refs.graph.innerHTML;
  const rowFor = id => html.match(new RegExp(`<article[^>]*data-id="${id}"[\\s\\S]*?</article>`))?.[0];
  assert.match(rowFor('B'), /data-story-path="first-parent"/);
  assert.doesNotMatch(rowFor('B'), />Shared base</);
  assert.match(rowFor('D'), />Shared base</);
  assert.match(rowFor('D'), /not necessarily the original branch creation point/);
  assert.doesNotMatch(rowFor('D'), />Branch start</);
  assert.equal(h.annotationCalls(), 0, 'No extra Git lookup to reconstruct a branch name or creation event');
});

test('Branch Story UI: verified Branch Start marker and jump select the original commit, not the merge-base', () => {
  const h = rendererHarness();
  h.graph.commits = originalForkFixture(); h.graph.headOid = 'F1';
  h.context.activeBranchStoryView().mode = 'story';
  h.context.ensureStoryCreation = () => 'B';
  let jump, selected, scrolled = false;
  h.refs.laneLegend.querySelectorAll = selector => selector === '[data-jump-branch-start]'
    ? [{ addEventListener: (_name, handler) => { jump = handler; } }] : [];
  h.context.graphCommitRow = id => { assert.equal(id, 'B'); return { scrollIntoView: () => { scrolled = true; } }; };
  h.context.selectCommit = id => { selected = id; };
  h.context.renderGraph();
  const html = h.refs.graph.innerHTML;
  const rowFor = id => html.match(new RegExp(`<article[^>]*data-id="${id}"[\\s\\S]*?</article>`))?.[0];
  assert.match(rowFor('B'), />Branch Start</);
  assert.doesNotMatch(rowFor('D'), />Branch Start</);
  assert.match(h.refs.laneLegend.innerHTML, /Go to start/);
  jump(); assert.equal(selected, 'B'); assert.equal(scrolled, true);
  assert.deepEqual(Array.from(h.context.lastGraphRenderContext.story.commits, c => c.id), h.graph.commits.map(c => c.id));
});

test('Branch Story creation lookup is async, cached and ignores completion after selection changes', async () => {
  const view = {mode:'story'};
  const pending = [];
  let rendered = 0;
  let retained;
  const context = { state: {view:'graph', selectedCommit:{id:'C'}},
    traceGraphStart: () => {}, diagnosticErrorCategory: () => 'unclassified',
    graphCommitRow: id => ({classList:{add: () => { retained = id; }}}),
    invoke: (_command, args) => new Promise(resolve => pending.push({args, resolve})),
    activeBranchStoryView: () => view, renderGraph: () => { rendered++; } };
  vm.createContext(context); vm.runInContext(appFunction('ensureStoryCreation'), context);
  const graph = {path:'/repo', commits:[]};
  assert.equal(context.ensureStoryCreation(graph, view, 'branch', 'feature', 'F1'), null);
  context.ensureStoryCreation(graph, view, 'branch', 'feature', 'F1');
  assert.equal(pending.length, 1);
  context.ensureStoryCreation(graph, view, 'branch', 'other', 'G1');
  pending[0].resolve('B'); await new Promise(resolve => setImmediate(resolve));
  assert.equal(rendered, 0);
  pending[1].resolve('C'); await new Promise(resolve => setImmediate(resolve));
  assert.equal(rendered, 1);
  assert.equal(retained, 'C');
  assert.equal(context.ensureStoryCreation(graph, view, 'branch', 'other', 'G1'), 'C');
  assert.equal(pending.length, 2);
  graph.commits = [{id:'older'}]; view.mode = 'full';
  assert.equal(context.ensureStoryCreation(graph, view, 'branch', 'other', 'G1'), 'C');
  assert.equal(pending.length, 2, 'pagination or view switch must not reread the same reflog');
  assert.equal(context.ensureStoryCreation(graph, view, 'remote', 'origin/other', 'G1'), null);
  assert.equal(view.creation, null);
  context.ensureStoryCreation(graph, view, 'branch', 'next', 'H1');
  context.state.view = 'explorer';
  pending[2].resolve('D'); await new Promise(resolve => setImmediate(resolve));
  assert.equal(rendered, 1, 'Do not redraw a graph after leaving its page');
});

test('both graph views navigate to original B, not advanced shared base D or merge M', () => {
  const h = rendererHarness(), lines = [], selected = [];
  h.state.view = 'graph'; h.graph.commits = originalForkFixture(); h.graph.headOid = 'F1';
  h.context.jsPerfLog = text => { if (text.startsWith('graph_start: ')) lines.push(JSON.parse(text.slice('graph_start: '.length))); };
  h.context.ensureHeadMainMergeBase = () => ({oid:'D',base_ref:'origin/main'});
  h.context.ensureStoryCreation = () => 'B';
  h.context.renderGraph(); h.context.renderGraph();
  const existingAnnotationCalls = h.annotationCalls();
  assert.equal(lines.filter(line => line.event === 'candidate').length, 1, 'identical redraws must not flood logs');
  assert.equal(lines[0].source, 'creation_and_first_parent'); assert.equal(lines[0].target, 'B');
  h.context.graphCommitRow = () => ({scrollIntoView(){}});
  h.context.selectCommit = id => selected.push(id);
  h.context.jumpToGraphBranchStart(); h.context.jumpToGraphLastMerge();
  assert.deepEqual(selected, ['B', 'M']);
  h.context.activeBranchStoryView().mode = 'story';
  let jump;
  h.refs.laneLegend.querySelectorAll = selector => selector === '[data-jump-branch-start]' ? [{addEventListener: (_name, fn) => {jump = fn;}}] : [];
  h.context.graphCommitRow = () => ({scrollIntoView(){}});
  h.context.selectCommit = id => selected.push(id);
  h.context.renderGraph();
  const story = lines.filter(line => line.event === 'candidate').at(-1);
  assert.equal(story.target, 'B'); assert.equal(story.source, 'creation_and_first_parent');
  assert.equal(story.story_shared_base, 'D'); assert.equal(story.head, 'F1');
  jump(); assert.deepEqual(selected, ['B', 'M', 'B']);
  assert.equal(lines.at(-1).event, 'jump_selected'); assert.equal(lines.at(-1).target, 'B');
  assert.equal(h.annotationCalls(), existingAnnotationCalls, 'logging must not add Git or status queries');
  assert.ok(lines.every(line => !JSON.stringify(line).includes('/fixture')));

  h.context.ensureStoryCreation = () => null;
  h.context.jumpToGraphBranchStart();
  assert.deepEqual(selected, ['B', 'M', 'B'], 'missing evidence must not fall back to shared base');
  assert.equal(lines.at(-1).target, null);
});

test('start diagnostics expose a Primary selection different from HEAD and unavailable story evidence', () => {
  const h = rendererHarness(), lines = [];
  h.graph.commits = originalForkFixture(); h.graph.headOid = 'F1';
  h.context.jsPerfLog = text => { if (text.startsWith('graph_start: ')) lines.push(JSON.parse(text.slice('graph_start: '.length))); };
  h.context.ensureHeadMainMergeBase = () => ({oid:'D',base_ref:'origin/main'});
  h.state.graphPrimaryBranch = 'remote:origin/main';
  h.context.renderGraph();
  assert.equal(lines.at(-1).selected_kind, 'remote'); assert.equal(lines.at(-1).selected_tip, 'D');
  assert.equal(lines.at(-1).selected_is_checkout, false); assert.equal(lines.at(-1).head, 'F1');
  assert.equal(lines.at(-1).target, null, 'Full Graph also refuses unproven remote branch starts');
  h.context.activeBranchStoryView().mode = 'story'; h.context.renderGraph();
  assert.equal(lines.at(-1).target, null); assert.match(lines.at(-1).reason, /creation record is missing/);
});

test('navigation is selection-scoped, reveals filtered targets and never invents a start', () => {
  const h = rendererHarness(), selected = [];
  h.graph.commits = originalForkFixture(); h.graph.headOid = 'F1';
  h.context.ensureStoryCreation = () => 'B';
  h.context.graphCommitRow = () => ({scrollIntoView(){}});
  h.context.selectCommit = id => selected.push(id);
  h.state.graphOnlySearchMatches = true;
  h.context.jumpToGraphBranchStart();
  assert.equal(h.state.graphOnlySearchMatches, false);
  assert.deepEqual(selected, ['B']);
  h.state.graphPrimaryBranch = 'remote:origin/main';
  h.context.ensureStoryCreation = () => null;
  h.context.jumpToGraphBranchStart(); h.context.jumpToGraphLastMerge();
  assert.deepEqual(selected, ['B'], 'neither action may silently use checkout history');
  h.graph.headDetached = true; h.state.graphPrimaryBranch = 'detached';
  h.context.jumpToGraphBranchStart(); h.context.jumpToGraphLastMerge();
  assert.deepEqual(selected, ['B', 'M']);
});

test('navigation toolbar has one HEAD action and three matching buttons in both views', () => {
  const h = rendererHarness();
  h.graph.commits = originalForkFixture(); h.graph.headOid = 'F1';
  h.context.ensureStoryCreation = () => 'B';
  h.context.graphHeadBannerHtml = vm.runInContext('(' + appFunction('graphHeadBannerHtml') + ')', h.context);
  for (const mode of ['full', 'story']) {
    h.context.activeBranchStoryView().mode = mode;
    h.context.renderGraph();
    const html = h.refs.laneLegend.innerHTML;
    assert.equal((html.match(/data-jump-head/g) || []).length, 1);
    assert.equal((html.match(/class="graph-nav-button"/g) || []).length, 3);
    assert.match(html, /data-jump-last-merge/);
    assert.doesNotMatch(html, /data-jump-story-start/);
    assert.equal(h.context.graphNavigationTargets().start.id, 'B');
  }
});

test('navigation retains missing-history reasons and enables Start when older evidence is loaded', () => {
  const h = rendererHarness();
  h.graph.commits = originalForkFixture().slice(0, 6); h.graph.headOid = 'F1';
  h.context.ensureStoryCreation = () => 'B';
  h.context.renderGraph();
  assert.equal(h.context.graphNavigationTargets().start.id, null);
  assert.match(h.refs.laneLegend.innerHTML, /data-jump-branch-start disabled/);
  const before = h.graph.commits.length;
  h.graph.commits = originalForkFixture();
  h.context.appendOlderGraphRows(before);
  assert.equal(h.context.graphNavigationTargets().start.id, 'B');
  assert.doesNotMatch(h.refs.laneLegend.innerHTML, /data-jump-branch-start disabled/);
  assert.match(h.refs.graph.innerHTML, />Branch Start</);
});

test('graph annotation commands use the worker and do not add Git operations or status scans', () => {
  const rust = fs.readFileSync(path.join(__dirname, '../src-tauri/src/repository/branches.rs'), 'utf8');
  for (const name of ['graph_branch_divergence', 'graph_head_main_merge_base']) {
    const start = rust.indexOf(`pub async fn ${name}(`), end = rust.indexOf(`fn ${name}_inner(`, start);
    assert.ok(start >= 0 && end > start);
    const wrapper = rust.slice(start, end);
    assert.match(wrapper, /off_main_thread\(move \|\|/);
    assert.doesNotMatch(wrapper, /repo_write_lock|invalidate|refresh_status|git\(/);
  }
});

test('shared-base diagnostics report rejected late results, cache hits and classified errors without extra requests', async () => {
  let current = {path:'/repo',headOid:'old',commits:[]}, renders = 0;
  const pending = [], lines = [];
  const context = {state:{view:'graph'}, headMainMergeBaseCache:{key:null,data:null}, headMainMergeBaseFetchKey:null,
    activeGraphData: () => current, renderGraph: () => {renders++;},
    traceGraphStart: (event, _graph, facts) => lines.push({event,...facts}), diagnosticErrorCategory: () => 'timeout',
    invoke: (command, args) => new Promise((resolve,reject) => pending.push({command,args,resolve,reject}))};
  vm.createContext(context); vm.runInContext(appFunction('ensureHeadMainMergeBase'),context);
  context.ensureHeadMainMergeBase(current); context.ensureHeadMainMergeBase(current);
  assert.equal(pending.length, 1);
  current = {...current,headOid:'new'}; context.ensureHeadMainMergeBase(current);
  pending[0].resolve({oid:'old-base',base_ref:'origin/main'}); await new Promise(resolve => setImmediate(resolve));
  assert.equal(lines.at(-1).accepted, false); assert.equal(renders, 0);
  pending[1].resolve({oid:'new-base',base_ref:'origin/main'}); await new Promise(resolve => setImmediate(resolve));
  assert.equal(lines.at(-1).accepted, true); assert.equal(renders, 1);
  assert.equal(context.ensureHeadMainMergeBase(current).oid, 'new-base'); assert.equal(pending.length, 2);
  current = {...current,headOid:'next'}; context.ensureHeadMainMergeBase(current);
  pending[2].reject('timeout with sensitive path'); await new Promise(resolve => setImmediate(resolve));
  assert.equal(lines.at(-1).category, 'timeout'); assert.equal(context.headMainMergeBaseCache.data.oid, 'new-base');
  assert.ok(!JSON.stringify(lines).includes('sensitive path'));
});

test('Branch Story UI: the original fork supports inspection, compare with HEAD, and compare with any commit', async () => {
  const h = rendererHarness();
  const calls = [], handlers = {};
  const context = { path: '/fixture', isSubmodule: false, name: 'fixture' };
  h.state.repository.path = context.path;
  Object.assign(h.context, {
    activeRepositoryContext: () => context,
    setCompareOrigin: () => {},
    repoOpenGeneration: 1,
    graphBranchRefsForCommit: () => [],
    showFloatingMenu: (_event, items) => { h.context.menuItems = items; },
    selectCommit: id => calls.push(['inspect', id]), status: () => {},
    closeSubmoduleGraph: () => {}, render: () => {},
    openCommanderDirectory: async scope => calls.push(['compare', scope, h.state.branchCompareLeftRef, h.state.branchCompareRightRef]),
  });
  const names = ['graphCommitCompareAnchorMatches', 'selectedGraphCommitIds', 'refreshGraphCommitSelection', 'handleGraphCommitRowClick', 'setGraphCommitCompareStart',
    'openRepositoryRevisionCompare', 'openGraphRevisionCompare', 'showGraphCommitContextMenu', 'wireGraphRowInteractions'];
  vm.runInContext(names.map(name => `${['openRepositoryRevisionCompare', 'openGraphRevisionCompare'].includes(name) ? 'async ' : ''}${appFunction(name)}`).join('\n'), h.context);
  h.graph.commits = originalForkFixture(); h.graph.headOid = 'F1';
  h.context.activeBranchStoryView().mode = 'story';
  h.context.renderGraph();
  h.context.wireGraphRowInteractions([{
    dataset: { id: 'B' }, querySelectorAll: () => [],
    addEventListener: (name, handler) => { handlers[name] = handler; },
  }]);
  handlers.click();
  assert.deepEqual(calls.pop(), ['inspect', 'B']);
  handlers.contextmenu({});
  await h.context.menuItems.find(item => item.id === 'compare-commit-head').run();
  assert.deepEqual(calls.pop(), ['compare', '', 'B', 'HEAD']);
  assert.equal(h.state.gitCompareMode, 'refs', 'Use existing snapshot comparison, not a merge-base diff');
  h.context.menuItems.find(item => item.id === 'compare-commit-start').run();
  assert.equal(h.state.graphCommitCompareAnchor.commitId, 'B');
  h.context.showGraphCommitContextMenu({}, 'C1');
  h.context.menuItems.find(item => item.id === 'compare-commit-with-start').run();
  assert.deepEqual(calls.pop(), ['compare', '', 'B', 'C1']);
  assert.equal(h.state.graphCommitCompareAnchor, null);
});
