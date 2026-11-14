'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.join(__dirname, '..');
const app = fs.readFileSync(path.join(root, 'frontend', 'app.js'), 'utf8');
const html = fs.readFileSync(path.join(root, 'frontend', 'index.html'), 'utf8');
const css = fs.readFileSync(path.join(root, 'frontend', 'styles.css'), 'utf8');

function extractFunction(name) {
  const start = app.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `${name} must exist`);
  const brace = app.indexOf('{', start);
  let depth = 0;
  for (let index = brace; index < app.length; index++) {
    if (app[index] === '{') depth++;
    else if (app[index] === '}' && --depth === 0) return app.slice(start, index + 1);
  }
  throw new Error(`Could not extract ${name}`);
}

function loadFunction(name) {
  const context = { result: null };
  vm.runInNewContext(`${extractFunction(name)}; result = ${name};`, context);
  return context.result;
}

test('Repository History Enter navigation cycles loaded matches in both directions', () => {
  const cycle = loadFunction('cyclicSearchMatchId');
  assert.equal(cycle(['a', 'b', 'c'], null, 1), 'a');
  assert.equal(cycle(['a', 'b', 'c'], 'a', 1), 'b');
  assert.equal(cycle(['a', 'b', 'c'], 'c', 1), 'a');
  assert.equal(cycle(['a', 'b', 'c'], 'a', -1), 'c');
  assert.match(app, /event\.key === 'Enter' && state\.view === 'graph'/);
  assert.match(app, /goToGraphSearchMatch\(event\.shiftKey \? -1 : 1\)/);
});

test('general file comparison groups consecutive changed rows into navigable blocks', () => {
  const blocks = loadFunction('comparisonBlocks');
  assert.deepEqual(JSON.parse(JSON.stringify(blocks([
    { same: true }, { same: false }, { same: false }, { same: true }, { same: false },
  ]))), [{ start: 1, end: 2 }, { start: 4, end: 4 }]);
  assert.match(html, /id="compareSearch"/);
  assert.match(html, /id="previousCompareDifference"[^>]*disabled><svg[^]*?<\/svg>Previous block<\/button>/);
  assert.match(app, /for \(let i = active\.start; i <= active\.end; i\+\+\) comparePaneRow\(pane, i\)\.classList\.add\('active-compare-difference'\)/);
});

test('detached dirty submodule guidance keeps one warning with a readable separate action', () => {
  assert.match(app, /class="submodule-guidance"/);
  assert.doesNotMatch(app, /const detachedDirtyBanner/);
  assert.match(app, /Local changes · Detached HEAD/);
  assert.match(css, /\.submodule-guidance > div \{[^}]*flex-direction: column/);
  assert.match(css, /\.submodule-guidance span \{[^}]*white-space: normal/);
  assert.match(css, /\.submodule-guidance button \{[^}]*grid-column: 2;[^}]*white-space: normal/);
  assert.doesNotMatch(css, /\.submodule-guidance (?:strong|span|> div) \{[^}]*overflow: hidden/);
});

test('search labels remain short, with full scope and keyboard help in accessible text', () => {
  const copy = loadFunction('viewSearchCopy');
  assert.equal(copy('explorer').placeholder, 'Filter this folder');
  for (const mode of ['local-drive', 'git', 'history', 'submodule']) {
    assert.equal(copy('commander', mode).placeholder, 'Filter files…');
    assert.match(copy('commander', mode).description, mode === 'local-drive' ? /both local folders/ : /current comparison/);
  }
  assert.equal(copy('graph').placeholder, 'Find commit…');
  assert.match(copy('graph').description, /author.*Enter: next.*Shift\+Enter: previous/);
  assert.match(app, /refs\.search\.title = searchCopy\.description/);
  assert.match(app, /refs\.search\.setAttribute\('aria-label', searchCopy\.description\)/);
  assert.match(css, /\.search \{[^}]*flex: 0 0 170px/);
  assert.match(css, /\.search input \{[^}]*min-width: 0/);
  assert.match(css, /\.file-grid \{[^}]*minmax\(160px, \.45fr\)/);
});

test('Compare uses two columns even after an empty or collapsed details panel, including narrow windows', () => {
  // The collapsed rule used !important; both persisted panel states must be
  // excluded, not just overridden by a lower-specificity Compare selector.
  assert.match(css, /body:not\(\.commander-mode\) \.main-layout\.details-collapsed \{[^}]*grid-template-columns/);
  assert.match(css, /body:not\(\.commander-mode\) \.main-layout\.details-empty-state:not\(\.details-collapsed\) \{[^}]*grid-template-columns/);
  assert.match(css, /\.commander-mode \.main-layout \{ grid-template-columns: 244px minmax\(0, 1fr\); \}/);
  assert.match(css, /@media \(max-width: 1180px\) \{ \.commander-mode \.main-layout \{ grid-template-columns: 220px minmax\(0, 1fr\); \} \}/);
  assert.match(css, /\.commander-mode \.details-panel-wrap \{ display: none; \}/);
  assert.match(css, /\.content-head > div:first-child \{[^}]*flex: 1 1 200px/);
  assert.match(css, /\.view-tools \{[^}]*flex: 0 1 auto/);
});

test('visual polish keeps branch names, graph controls and empty details responsive', () => {
  assert.match(css, /\.branch-row\.local-branch:not\(\.active\) \.branch-row-actions \{[^}]*position: absolute/);
  assert.match(css, /\.main-layout\.details-empty-state:not\(\.details-collapsed\)/);
  assert.match(css, /\.graph-toolbar-row \{/);
  assert.match(app, /class="graph-toolbar-row graph-toolbar-context"/);
  assert.match(app, /class="graph-toolbar-row graph-toolbar-controls"/);
  assert.match(html, /<main class="main-layout details-empty-state">/);
});

test('Full Graph and Branch Story keep one stable toolbar and lane gutter', () => {
  assert.match(app, /const fullGraphPrimary = resolvePrimarySelection\(g\)/);
  assert.match(app, /const widthModel = storyMode \|\| onlySearchMatches/);
  assert.match(app, /class="graph-toolbar-meta"/);
  assert.match(app, /class="graph-toolbar-head"/);
  assert.match(app, /class="graph-toolbar-actions"/);
  assert.match(css, /\.graph-toolbar-context \{[^}]*display: grid;[^}]*grid-template-columns:/);
  assert.match(css, /\.primary-branch-picker \{[^}]*grid-template-columns: 72px minmax\(0, 1fr\)/);
});

test('recent repositories disambiguate duplicate names with their parent folder', () => {
  assert.match(app, /const nameCounts = new Map\(\)/);
  assert.match(app, /\$\{parentName\} \/ \$\{repo\.name\}/);
  assert.match(css, /\.recent-repo-btn \{[^}]*max-width: 156px/);
});

test('Explorer initial cycles filtered entries and crosses the display cap without expanding it', () => {
  const names = ['ape', 'beta', 'apple', 'apricot'];
  const entries = names.map(name => ({ name, relative_path: name }));
  const state = { view: 'explorer', repository: {}, entries, selectedEntry: null };
  const row = { focus() {}, scrollIntoView() {} };
  const inside = area => ({ closest: selector => selector.includes(area) ? {} : null });
  const target = inside('#explorerView'), pages = [], renderState = { offset: 0, showAll: false };
  const refs = { search: { value: 'ap' }, fileList: { querySelector: () => row } };
  let overlay = null;
  const ctx = vm.createContext({ state, refs, document: { body: {}, documentElement: {}, querySelector: () => overlay },
    CSS: { escape: s => s }, EXPLORER_DOM_ROW_CAP: 2, explorerRenderState: renderState,
    renderExplorer(offset) { pages.push(offset); renderState.offset = offset; },
    selectEntry(path) { state.selectedEntry = entries.find(e => e.relative_path === path); }, lastExplorerClick: {} });
  vm.runInContext(extractFunction('jumpToExplorerInitial'), ctx);
  const press = (options = {}) => ctx.jumpToExplorerInitial({ key: 'A', target, preventDefault() {}, ...options });
  press({ target: inside('#contentHead') }); assert.equal(state.selectedEntry.name, 'ape');
  press({ ctrlKey: true }); press({ metaKey: true }); press({ target: inside('.sidebar') }); press({ isComposing: true });
  press({ target: inside('input') }); press({ target: { isContentEditable: true } });
  overlay = {}; press(); overlay = null;
  assert.equal(state.selectedEntry.name, 'ape');
  press({ target: inside('#recentReposBar') }); assert.equal(state.selectedEntry.name, 'apple');
  press(); assert.equal(state.selectedEntry.name, 'apricot');
  press({ target: ctx.document.body }); assert.equal(state.selectedEntry.name, 'ape');
  assert.deepEqual(pages, [2, 0]);
});

test('History Enter reuses painted matches and refreshes pending searches or replaced history', () => {
  const graph = {path:'/parent', commits:['a','b','c']};
  let rows = [], paints = 0, selected, marked;
  const state = {view:'graph', graphSearchCurrentId:null};
  const refs = {search:{value:'a'}, graph:{
    querySelectorAll: () => rows,
    querySelector: () => rows.find(row => row.dataset.id === marked),
  }};
  const ctx = vm.createContext({state, refs, lastGraphRenderContext:null, searchTimeout:null,
    activeGraphData: () => graph, clearTimeout(){}, status(){}, selectCommit: id => { selected = id; },
    renderGraph(){
      paints++;
      const query = refs.search.value.toLowerCase();
      ctx.lastGraphRenderContext = {query, sourceCommits:graph.commits, repositoryPath:graph.path};
      rows = graph.commits.filter(id => id.includes(query)).map(id => ({
        dataset:{id}, classList:{add(){marked=id;},remove(){marked=null;}},scrollIntoView(){},
      }));
    },
  });
  vm.runInContext(extractFunction('cyclicSearchMatchId') + '\n' + extractFunction('goToGraphSearchMatch'), ctx);
  graph.commits = ['abc', 'abd', 'cd'];
  ctx.goToGraphSearchMatch(1); assert.equal(selected,'abc');
  ctx.goToGraphSearchMatch(1); assert.equal(selected,'abd');
  ctx.goToGraphSearchMatch(1); assert.equal(selected,'abc');
  ctx.goToGraphSearchMatch(-1); assert.equal(selected,'abd'); assert.equal(paints,1);
  refs.search.value='C'; ctx.goToGraphSearchMatch(-1); assert.equal(selected,'cd'); assert.equal(paints,2);
  graph.commits=['cc']; ctx.goToGraphSearchMatch(1); assert.equal(selected,'cc'); assert.equal(paints,3);
  graph.path='/child'; ctx.goToGraphSearchMatch(1); assert.equal(paints,4);
  refs.search.value='absent'; selected=null; ctx.goToGraphSearchMatch(1); assert.equal(selected,null);
});
