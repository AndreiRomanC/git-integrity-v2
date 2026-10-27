const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
const css = fs.readFileSync(path.join(__dirname, '../frontend/styles.css'), 'utf8');
function source(name) {
  const start = app.indexOf(`function ${name}(`);
  assert.ok(start >= 0, name);
  return app.slice(start, app.indexOf('\n}', start) + 2);
}
const plain = value => JSON.parse(JSON.stringify(value));

function graphHarness() {
  const state = { repository: { path: '/parent' } };
  const rows = ['A', 'B', 'C'].map(id => ({
    dataset: { id }, handlers: {}, selected: false, pills: [],
    classList: { toggle(name, value) { if (name === 'selected') rows.find(row => row.dataset.id === id).selected = value; } },
    addEventListener(name, handler) { this.handlers[name] = handler; },
    querySelectorAll(selector) { return selector === '[data-graph-ref-name]' ? this.pills : []; },
  }));
  const calls = [], messages = [], details = { innerHTML: '', closest: () => null };
  const context = {
    state, repoOpenGeneration: 1,
    current: { path: '/parent', isSubmodule: false },
    activeRepositoryContext: () => context.current,
    activeGraphData: () => ({ commits: ['A', 'B', 'C'].map(id => ({ id, subject: id, author: 'A', refs: [], parents: [] })) }),
    refs: { graph: { querySelectorAll: () => rows }, details },
    esc: value => String(value ?? ''), commitSubjectHtml: String,
    GitActivity: { commitTime: () => ({ date: '2026-10-20', time: '10:00', title: '' }) },
    status: message => messages.push(message),
    graphBranchRefsForCommit: () => [],
    showFloatingMenu: (_event, items) => { context.menu = items; },
    openGraphRevisionCompare: (...args) => calls.push(args),
    handleGraphBranchCompareSelection: async name => calls.push(['branch', name]),
    handleError: error => { throw error; },
  };
  vm.createContext(context);
  vm.runInContext([
    'graphCommitCompareAnchorMatches', 'selectedGraphCommitIds', 'refreshGraphCommitSelection',
    'handleGraphCommitRowClick', 'selectCommit', 'cleanUpstreamMenuItems', 'showGraphCommitContextMenu', 'wireGraphRowInteractions',
  ].map(source).join('\n'), context);
  context.wireGraphRowInteractions(rows);
  return { state, context, rows, calls, messages, selected: () => plain(context.selectedGraphCommitIds()) };
}

test('Ctrl/Cmd selects two graph rows without comparing; either selected context menu compares in selection order', () => {
  for (const modifier of ['ctrlKey', 'metaKey']) {
    const h = graphHarness();
    h.rows[1].handlers.click({ [modifier]: true });
    h.rows[0].handlers.click({ [modifier]: true });
    assert.deepEqual(h.selected(), ['B', 'A']);
    assert.deepEqual(h.rows.map(row => row.selected), [true, true, false]);
    assert.equal(h.calls.length, 0);
    for (const index of [0, 1]) {
      h.rows[index].handlers.contextmenu({});
      h.context.menu.find(item => item.id === 'compare-selected-commits').run();
      assert.deepEqual(h.calls.at(-1), ['B', 'A']);
      assert.deepEqual(h.selected(), ['B', 'A']);
    }
  }
});

test('normal click replaces selection, Ctrl toggles, and three selections do not offer a pair comparison', () => {
  const h = graphHarness();
  h.rows[0].handlers.click({});
  h.rows[1].handlers.click({ ctrlKey: true });
  h.rows[2].handlers.click({ ctrlKey: true });
  h.rows[0].handlers.contextmenu({});
  assert.ok(!h.context.menu.some(item => item.id === 'compare-selected-commits'));
  h.rows[2].handlers.click({ ctrlKey: true });
  assert.deepEqual(h.selected(), ['A', 'B']);
  h.rows[2].handlers.click({});
  assert.deepEqual(h.selected(), ['C']);
  h.rows[2].handlers.click({ metaKey: true });
  assert.deepEqual(h.selected(), []);
});

test('right-clicking an unselected commit resets the pair; a stale context menu cannot compare', () => {
  const h = graphHarness();
  h.rows[0].handlers.click({});
  h.rows[1].handlers.click({ ctrlKey: true });
  h.rows[0].handlers.contextmenu({});
  const compare = h.context.menu.find(item => item.id === 'compare-selected-commits');
  h.rows[2].handlers.contextmenu({});
  assert.deepEqual(h.selected(), ['C']);
  assert.ok(!h.context.menu.some(item => item.id === 'compare-selected-commits'));
  compare.run();
  assert.equal(h.calls.length, 0);
});

test('graph pair selection survives repaint but cannot leak between repositories, submodules, or reloads', () => {
  const h = graphHarness();
  h.rows[0].handlers.click({});
  h.rows[1].handlers.click({ ctrlKey: true });
  h.context.refreshGraphCommitSelection();
  assert.deepEqual(h.rows.map(row => row.selected), [true, true, false]);
  h.rows[0].handlers.contextmenu({});
  const compare = h.context.menu.find(item => item.id === 'compare-selected-commits');
  h.context.current = { path: '/parent/module', isSubmodule: true, relativePath: 'module' };
  assert.deepEqual(h.selected(), []);
  compare.run();
  assert.equal(h.calls.length, 0);
  h.rows[0].handlers.click({});
  h.rows[1].handlers.click({ ctrlKey: true });
  h.rows[0].handlers.contextmenu({});
  h.context.menu.find(item => item.id === 'compare-selected-commits').run();
  assert.deepEqual(h.calls.pop(), ['A', 'B']);
  h.context.repoOpenGeneration++;
  assert.deepEqual(h.selected(), []);
});

test('Ctrl on a branch label selects its commit instead of triggering the older automatic branch comparison', () => {
  const h = graphHarness(), handlers = {};
  const pill = { dataset: { graphRefName: 'origin/feature' }, addEventListener(name, handler) { handlers[name] = handler; } };
  h.rows[0].pills = [pill];
  h.context.wireGraphRowInteractions([h.rows[0]]);
  handlers.click({ ctrlKey: true, stopPropagation() {} });
  assert.deepEqual(h.selected(), ['A']);
  assert.equal(h.calls.length, 0);
  handlers.click({ stopPropagation() {} });
  assert.deepEqual(h.calls[0], ['branch', 'origin/feature']);
});

function remoteHarness(branches, repository = { current_branch: 'feature' }, remotes = [{ name: 'origin' }]) {
  const cards = { innerHTML: '', querySelectorAll: () => [], querySelector: () => null };
  const context = { state: { branches, repository, remotes }, refs: { remoteCards: cards }, esc: value => String(value ?? '') };
  vm.createContext(context);
  vm.runInContext(['remoteUrlKind', 'remoteTrackingBranches', 'remoteCurrentBranchCandidate', 'remoteTrackingSummary', 'renderRemoteUrlRow', 'renderRemotes'].map(source).join('\n'), context);
  context.renderRemotes();
  return { context, text: cards.innerHTML };
}
test('Remotes uses actual upstream, never a same-name remote branch as proof of tracking', () => {
  let h = remoteHarness([{ name: 'feature', upstream: null }, { name: 'origin/feature', remote: true }]);
  assert.match(h.text, /No upstream available/);
  assert.match(h.text, /nothing is pushed until you confirm/);
  assert.match(h.text, /not proof of tracking/);
  h = remoteHarness([{ name: 'feature', upstream: 'team/review' }, { name: 'origin/feature', remote: true }]);
  assert.match(h.text, /Tracks team\/review/);
  assert.doesNotMatch(h.text, /No upstream available/);
});
test('Remotes distinguishes missing metadata, detached HEAD, and missing remotes', () => {
  assert.match(remoteHarness([]).text, /Upstream information not loaded yet/);
  assert.match(remoteHarness([], { head_detached: true }).text, /Detached HEAD does not track/);
  const h = remoteHarness([{ name: 'feature', upstream: null }], undefined, []);
  assert.match(h.text, /No upstream available/);
  assert.match(h.text, /Add a remote before publishing/);
  assert.match(h.text, /data-fetch-all-remotes disabled/);
});

test('Terminal button and Enter use the same existing command runner; busy/empty/search modes do not submit', () => {
  const input = { value: 'git status', focus() {} }, calls = [];
  const context = {
    state: { consoleMode: 'console' }, $: () => input,
    setCommandInputValue: value => { input.value = value; },
    renderGitHints() {}, runTerminalFromConsole: command => calls.push(command),
  };
  vm.runInNewContext(source('submitTerminalCommand'), context);
  context.submitTerminalCommand();
  assert.deepEqual(calls, ['git status']);
  assert.equal(input.value, '');
  context.submitTerminalCommand();
  for (const changes of [{ consoleCommandRunning: true }, { consoleCommandRunning: false, consoleMode: 'saved' }, { consoleMode: 'commands' }]) {
    Object.assign(context.state, changes); input.value = 'git diff';
    context.submitTerminalCommand();
    assert.equal(input.value, 'git diff');
  }
  assert.equal(calls.length, 1);
  assert.match(app, /#commandRun'\)\.addEventListener\('click', submitTerminalCommand\)/);
  assert.match(app, /e\.key === 'Enter' && !e\.isComposing\) \{ e\.preventDefault\(\); submitTerminalCommand\(\)/);
  assert.match(html, /label id="commandInputLabel" for="commandInput"/);
  assert.match(html, /id="commandRun" type="button"/);
  assert.match(css, /\.command-entry-line:focus-within/);
  assert.match(app, /#commandRun'\)\.hidden = mode !== 'console'/);
});
