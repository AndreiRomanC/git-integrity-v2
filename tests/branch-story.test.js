'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { buildBranchStory, buildGraphModel } = require('../frontend/graph-model.js');
const c = (id, parents = [], refs = []) => ({ id, parents, refs });
const ref = (name, kind = 'local_branch') => ({ name, kind });
const ids = story => story.commits.map(commit => commit.id);
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
  const element = () => ({ innerHTML: '', value: '', style: { setProperty() {} }, querySelectorAll: () => [], addEventListener() {} });
  const commits = [c('unrelated', ['root'], [ref('origin/main', 'remote_branch')]), c('tip', ['a'], [ref('feature')]), c('a', ['root']), c('root')];
  const graph = { path: '/fixture', name: 'fixture', currentBranch: 'feature', headOid: 'tip', headDetached: false, commits, branches: [{name:'feature'}, {name:'origin/main',remote:true}], stashes: [] };
  const state = { repository: {}, graphPrimaryBranch:'branch:feature', graphRefFilter:'all', graphOnlySearchMatches:false, historyKind:'', historyScope:'' };
  const refs = { search:element(), graphView:element(), laneLegend:element(), graph:element() };
  let backendAnnotations = 0, interactions = 0;
  const context = {
    state, refs, ...require('../frontend/graph-model.js'), activeGraphData:()=>graph,
    performance, jsPerfLog:()=>{}, LANE_WIDTH:24, headMainMergeBaseFetchKey:null,
    ensureHeadMainMergeBase:()=>{ backendAnnotations++; return null; },
    ensureBranchDivergence:()=>{ backendAnnotations++; return null; },
    esc:value=>String(value ?? ''), commitSubjectHtml:value=>String(value ?? ''),
    refsBadges:refs=>JSON.stringify(refs), graphCommitCompareAnchorMatches:()=>false,activeRepositoryContext:()=>({}),
    graphHeadBannerHtml:()=>'', loadingOlderCommits:false, GRAPH_LEGEND_HTML:'<details class="graph-legend"></details>',
    $:()=>null, requestAnimationFrame:()=>{}, wireGraphRowInteractions:()=>{interactions++;},
    loadOlderGraphCommits:()=>{}, jumpToGraphHead:()=>{}, jumpToGraphBranchStart:()=>{},
    lastGraphRenderContext:null, lastGraphModel:null,lastGraphLanesWidth:0,lastGraphOnlySearchMatches:false,
  };
  vm.createContext(context);
  const names = ['activeGraphPrimaryBranch','setActiveGraphPrimaryBranch','activeGraphRefFilter','setActiveGraphRefFilter','activeBranchStoryView','resolvePrimarySelection','mergeCommitPresentation','buildCommitRowHtml','graphTruncationStubHtml','renderGraph','appendOlderGraphRows'];
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

test('Branch Story UI: the original fork supports inspection, compare with HEAD, and compare with any commit', async () => {
  const h = rendererHarness();
  const calls = [], handlers = {};
  const context = { path: '/fixture', isSubmodule: false, name: 'fixture' };
  Object.assign(h.context, {
    activeRepositoryContext: () => context,
    graphBranchRefsForCommit: () => [],
    showFloatingMenu: (_event, items) => { h.context.menuItems = items; },
    selectCommit: id => calls.push(['inspect', id]), status: () => {},
    closeSubmoduleGraph: () => {}, render: () => {},
    openCommanderDirectory: async scope => calls.push(['compare', scope, h.state.branchCompareLeftRef, h.state.branchCompareRightRef]),
  });
  const names = ['graphCommitCompareAnchorMatches', 'setGraphCommitCompareStart',
    'openGraphRevisionCompare', 'showGraphCommitContextMenu', 'wireGraphRowInteractions'];
  vm.runInContext(names.map(name => `${name === 'openGraphRevisionCompare' ? 'async ' : ''}${appFunction(name)}`).join('\n'), h.context);
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
