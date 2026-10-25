const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ComparisonNavigation = require('../frontend/comparison-navigation.js');
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
const source = name => {
  const code = app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))?.[0];
  assert.ok(code, name); return code;
};
const plain = value => JSON.parse(JSON.stringify(value));
function element() {
  return { value: '', textContent: '', innerHTML: '', hidden: false, disabled: false, dataset: {}, handlers: {},
    addEventListener(name, fn) { this.handlers[name] = fn; },
    classList: { toggle() {} }, setAttribute() {}, closest() { return this; },
  };
}
function harness() {
  const nodes = new Map();
  const $ = id => { if (!nodes.has(id)) nodes.set(id, element()); return nodes.get(id); };
  const calls = [];
  let selection = { left: 'Working tree', right: 'HEAD' };
  const state = { repository: { path: '/parent', name: 'parent', head_oid: 'parent-head' }, branches: [{ name: 'parent-only', current: true }],
    view: 'commander', compareMode: 'submodule', gitCompareMode: 'refs', commanderPath: 'src/deep', currentPath: 'modules',
    submoduleCompare: { submodulePath: 'modules/dep', name: 'dep' }, compareFlatMode: false, commanderRows: [],
  };
  const context = {
    state, ComparisonNavigation, $,
    refs: { search: $('search'), commanderRows: $('rows'), gitCompareFlatControls: $('author') },
    render() {}, renderCommander() {}, status() {}, renderComparisonAuthorFilter() {},
    folderHistoryComparison: { activate() {}, selection: () => selection },
    localDriveWorkspace: { openRepositoryLocation: (...args) => calls.push(['drive', ...args]) },
    submoduleEntryForPath: path => ({ kind: 'submodule', relative_path: path, name: 'dep' }),
    openSubmoduleCompareFromEntry: (entry, opts) => calls.push(['submodule', entry.relative_path, opts.innerPath]),
    invoke: async (name, args) => {
      calls.push([name, args]);
      if (name === 'comparison_context') return { repository: { path: args.repositoryPath, name: 'dep', head_oid: 'sub-head', current_branch: 'sub-branch' }, branches: [{ name: 'sub-branch', current: true, upstream: 'origin/sub-branch' }, { name: 'origin/sub-branch', remote: true }] };
      return { rows: [], left_revision: 'left-exact', right_revision: 'right-exact', remote_revision: 'remote-exact', local_revision: 'sub-head' };
    },
    workspaceComparisonAuthorContext: () => null,
    esc: String,
  };
  vm.createContext(context);
  const names = ['comparisonRepository', 'comparisonBranches', 'setCompareOrigin', 'ensureComparisonContext', 'commanderComparisonSpec', 'createRequestGuard',
    'branchCompareRefOptions', 'defaultBranchCompareLeftRef', 'defaultBranchCompareRightRef', 'ensureBranchCompareRefs', 'commanderDownloadSnapshot',
    'syncLocalDriveToCommanderPath', 'switchComparisonMode', 'openCommanderDirectory'];
  vm.runInContext(names.map(source).join('\n') + '\nconst openCommanderDirectoryGuard = createRequestGuard();', context);
  context.setCompareOrigin('src/deep', 'modules/dep');
  return { context, state, calls, nodes, selection: value => { selection = value; } };
}

test('origin keeps inner paths separate from project paths on macOS and Windows', () => {
  for (const root of ['/parent', 'D:\\project']) {
    const c = ComparisonNavigation.origin({ path: root, name: 'parent' }, 'src\\deep', 'modules\\dep');
    assert.equal(c.originPath, 'src/deep'); assert.equal(c.submodulePath, 'modules/dep');
    assert.equal(ComparisonNavigation.projectPath(c, 'src/deep'), 'modules/dep/src/deep');
    assert.equal(c.repositoryPath.replace(/\\/g, '/'), `${root.replace(/\\/g, '/')}/modules/dep`);
  }
});

test('all four tabs preserve the selected submodule repository and inner folder', async () => {
  const h = harness();
  await h.context.switchComparisonMode('local-drive');
  assert.deepEqual(h.calls.at(-1), ['drive', '/parent/modules/dep', 'src/deep']);
  await h.context.switchComparisonMode('git');
  const compare = h.calls.find(c => c[0] === 'compare_git_revisions_directory');
  assert.deepEqual(plain(compare[1]), { repositoryPath: '/parent/modules/dep', relativePath: 'src/deep', leftRef: 'sub-branch', rightRef: 'origin/sub-branch' });
  await h.context.switchComparisonMode('history');
  const history = h.calls.find(c => c[0] === 'compare_workspace_revision');
  assert.deepEqual(plain(history[1]), { repositoryPath: '/parent/modules/dep', relativePath: 'src/deep', revision: 'HEAD', flat: false });
  await h.context.switchComparisonMode('submodule');
  assert.deepEqual(h.calls.at(-1), ['submodule', 'modules/dep', 'src/deep']);
  assert.equal(h.calls.filter(c => c[0] === 'comparison_context').length, 1);
  assert.equal(h.state.repository.path, '/parent'); // The app was never switched/reopened.
});

test('switching from a normal folder does not resurrect an unrelated previous submodule', async () => {
  const h = harness();
  h.context.setCompareOrigin('folder'); h.state.commanderPath = 'folder'; h.state.compareMode = 'local-drive';
  await h.context.switchComparisonMode('submodule');
  assert.equal(h.state.submoduleCompare, null);
  assert.equal(h.state.commanderPath, 'folder');
  assert.equal(h.calls.length, 0);
});

test('Folder Compare is initialized at the exact scope before rendering, never reset to the parent repository', async () => {
  const h = harness();
  h.context.refs = new Proxy(h.context.refs, { get(object, key) { return object[key] ||= element(); } });
  h.context.localDriveWorkspace.activate = path => h.calls.push(['activate', path]);
  h.context.localDriveWorkspace.deactivate = () => {};
  vm.runInContext(source('renderComparisonLocation') + '\n' + source('renderCommander'), h.context);
  h.context.render = () => h.context.renderCommander();
  await h.context.switchComparisonMode('local-drive');
  assert.deepEqual(h.calls, [['drive', '/parent/modules/dep', 'src/deep'], ['activate', '/parent/modules/dep']]);
  h.context.renderCommander();
  assert.deepEqual(h.calls.at(-1), ['activate', '/parent/modules/dep']);
});

test('compact comparison header preserves layout toggles, history controls, metadata and loading errors', () => {
  const h = harness();
  h.context.localDriveWorkspace.deactivate = () => {};
  h.context.refs = new Proxy(h.context.refs, { get(object, key) { return object[key] ||= element(); } });
  h.context.renderCommanderBreadcrumbs = () => {};
  h.context.renderBranchCompareSelectors = () => {};
  vm.runInContext(source('renderComparisonLocation') + '\n' + source('renderCommander'), h.context);
  for (const mode of ['git', 'history']) {
    for (const flat of [false, true]) {
      h.state.compareMode = mode;
      h.state.compareFlatMode = flat;
      h.state.comparisonContext.ready = true;
      h.state.commanderComparisonLoading = {};
      h.state.commanderRows = ['old'];
      h.context.renderCommander();
      assert.equal(h.nodes.get('#compareGitInfo').hidden, false);
      assert.equal(h.nodes.get('#gitRevisionToolbar').hidden, mode === 'history');
      assert.equal(h.nodes.get('#folderHistoryControls').hidden, mode !== 'history');
      assert.equal(h.context.refs.gitCompareFlatControls.hidden, !flat);
      assert.equal(h.context.refs.gitCompareFlatToggle.hidden, false, 'layout switch is not in the hidden filter row');
      assert.match(h.context.refs.commanderRows.innerHTML, /Loading comparison/);
      assert.doesNotMatch(h.context.refs.commanderRows.innerHTML, /old/);
      assert.match(h.nodes.get('#compareSnapshotSummary').textContent, /Resolving/);
      h.state.commanderComparisonLoading = null;
      h.state.commanderComparisonError = 'Unknown revision';
      h.state.commanderComparisonResult = { left_revision: 'left-exact', right_revision: 'right-exact' };
      h.context.renderCommander();
      assert.match(h.context.refs.commanderRows.innerHTML, /Unknown revision/, 'errors are not hidden in Details');
      assert.match(h.nodes.get('#compareSnapshotSummary').textContent, /right-exact/);
    }
  }
  for (const mode of ['local-drive', 'submodule']) {
    h.state.compareMode = mode;
    h.context.renderComparisonLocation();
    assert.equal(h.nodes.get('#compareGitInfo').hidden, true, 'other tabs never display stale branch snapshot metadata');
    assert.match(h.nodes.get('#compareOrigin').textContent, /\/parent\/modules\/dep/);
  }
  assert.equal(h.calls.length, 0, 'rearranging controls does not request Git or server work');
});

test('history compares either local files or two exact refs with the same existing renderer', async () => {
  const h = harness(); await h.context.switchComparisonMode('history'); h.calls.length = 0;
  h.selection({ left: 'old-sha', right: 'new-sha' });
  await h.context.openCommanderDirectory('src/deep');
  assert.equal(h.calls[0][0], 'compare_git_revisions_directory');
  assert.equal(h.calls[0][1].leftRef, 'old-sha'); assert.equal(h.calls[0][1].rightRef, 'new-sha');
  h.state.compareFlatMode = true; await h.context.openCommanderDirectory('src/deep');
  assert.equal(h.calls.at(-1)[0], 'compare_git_revisions_file_list');
  h.selection({ left: 'Working tree', right: 'old-sha' });
  await h.context.openCommanderDirectory('src/deep');
  assert.equal(h.calls.at(-1)[0], 'compare_workspace_revision'); assert.equal(h.calls.at(-1)[1].flat, true);
  assert.equal(h.calls.some(c => /fetch|status|checkout|restore_folder/.test(c[0])), false);
});

test('invalid revisions and errors clear old rows, survive repaint, and never show an old comparison', async () => {
  const h = harness(); await h.context.switchComparisonMode('history');
  h.state.commanderRows = ['old']; h.selection({ left: '', right: 'HEAD' });
  await h.context.openCommanderDirectory('src/deep');
  assert.equal(h.state.commanderRows.length, 0); assert.match(h.state.commanderComparisonError, /Choose both/);
  h.selection({ left: 'Working tree', right: 'missing-ref' });
  h.context.invoke = async () => { throw new Error('Unknown revision'); };
  await h.context.openCommanderDirectory('src/deep');
  assert.match(h.state.commanderComparisonError, /Unknown revision/);
  assert.equal(h.state.commanderComparisonLoading, null); assert.equal(h.state.commanderExportSnapshot, null);
});

test('late snapshot and metadata responses cannot replace newer selections or an away-and-back tab', async () => {
  const h = harness(); await h.context.switchComparisonMode('history');
  const pending = [];
  h.context.invoke = (name, args) => new Promise(resolve => pending.push({ name, args, resolve }));
  h.selection({ left: 'Working tree', right: 'A' }); const old = h.context.openCommanderDirectory('src/deep');
  h.selection({ left: 'Working tree', right: 'B' }); const fresh = h.context.openCommanderDirectory('src/deep');
  pending[1].resolve({ rows: ['B'], remote_revision: 'B' }); await fresh;
  pending[0].resolve({ rows: ['A'], remote_revision: 'A' }); await old;
  assert.deepEqual(plain(h.state.commanderRows), ['B']);
  const late = h.context.openCommanderDirectory('src/deep');
  await h.context.switchComparisonMode('local-drive');
  const back = h.context.switchComparisonMode('history');
  await Promise.resolve();
  pending[2].resolve({ rows: ['old tab'], remote_revision: 'B' }); await late;
  assert.equal(h.state.commanderRows.length, 0);
  pending[3].resolve({ rows: ['new tab'], remote_revision: 'B' }); await back;
  assert.deepEqual(plain(h.state.commanderRows), ['new tab']);

  const g = harness(); let finish;
  g.context.invoke = () => new Promise(resolve => { finish = resolve; });
  const loading = g.context.switchComparisonMode('git'); await Promise.resolve();
  await g.context.switchComparisonMode('local-drive');
  finish({ repository: { path: '/parent/modules/dep' }, branches: [] }); await loading;
  assert.equal(g.state.compareMode, 'local-drive'); assert.equal(g.state.commanderComparisonLoading, null);
});

function historyHarness() {
  const nodes = new Map(), pending = [], changes = [], menus = [];
  const root = { querySelector: id => { if (!nodes.has(id)) nodes.set(id, element()); return nodes.get(id); } };
  const history = ComparisonNavigation.createHistory({ root, invoke: (name, args) => new Promise((resolve, reject) => pending.push({ name, args, resolve, reject })), onChange: () => changes.push(history.selection()), onContextMenu: (event, items) => menus.push(items) });
  return { history, pending, changes, menus, picker: root.querySelector('.folder-history-picker'), get: id => root.querySelector(`[data-history-${id}]`) };
}
const page = (id, more = false, head = 'pinned') => ({ commits: [{ id, subject: 'Complete long message', author: 'A', date: '2026-10-21' }], head_id: head, next_offset: 500, has_more: more });
const tick = () => new Promise(resolve => setImmediate(resolve));

test('folder history uses bounded explicit pages, pinned HEAD, local search and keeps selected refs on refresh', async () => {
  const h = historyHarness(), context = { repositoryPath: '/repo' };
  h.history.activate(context, 'src'); h.history.activate(context, 'src');
  assert.equal(h.pending.length, 1);
  assert.deepEqual(h.pending[0].args, { repositoryPath: '/repo', relativePath: 'src', headId: null, offset: 0 });
  h.pending[0].resolve(page('old', true)); await tick();
  h.get('rows').handlers.click({ target: { closest: () => ({ dataset: { historyPick: 'old', side: 'left' } }) } });
  assert.equal(h.history.selection().left, 'old');
  h.get('search').value = 'missing'; h.get('search').handlers.input();
  assert.equal(h.pending.length, 1); assert.match(h.get('rows').innerHTML, /No matching/);
  const more = h.history.load(true);
  assert.equal(h.pending[1].args.headId, 'pinned'); assert.equal(h.pending[1].args.offset, 500);
  h.pending[1].resolve(page('older')); await more;
  const refresh = h.history.load(); h.pending[2].resolve(page('latest', false, 'new-head')); await refresh;
  assert.equal(h.history.selection().left, 'old');
  assert.match(h.get('note').textContent, /new-head/);
});

test('history rejects old folder pages and reports errors without dropping the current selection', async () => {
  const h = historyHarness(), context = { repositoryPath: '/repo' };
  h.history.activate(context, 'one'); h.history.activate(context, 'two');
  h.pending[0].resolve(page('wrong-folder')); await tick();
  assert.doesNotMatch(h.get('rows').innerHTML, /wrong-folder/);
  h.pending[1].reject('read failed'); await tick();
  assert.match(h.get('note').textContent, /read failed/);
  assert.deepEqual(h.history.selection(), { left: 'Working tree', right: 'HEAD' });
  const retry = h.history.load(); h.pending[2].resolve(page('correct-folder')); await retry;
  assert.match(h.get('rows').innerHTML, /correct-folder/);
});

test('working tree is explicit and stays on the left; two committed refs can be swapped', async () => {
  const h = historyHarness(); h.history.activate({ repositoryPath: '/repo' }, ''); h.pending[0].resolve(page('a')); await tick();
  h.get('swap').handlers.click(); assert.equal(h.changes.length, 0);
  h.get('left').value = 'a'; h.get('left').handlers.change();
  h.get('swap').handlers.click(); assert.deepEqual(h.history.selection(), { left: 'HEAD', right: 'a' });
  h.get('worktree').handlers.click(); assert.deepEqual(h.history.selection(), { left: 'Working tree', right: 'a' });
});

const selectHistoryEvent = (id, modifiers = {}) => ({ ...modifiers,
  target: { closest: selector => selector === '[data-history-select]' ? { dataset: { historySelect: id } }
    : selector === '[data-history-row]' ? { dataset: { historyRow: id } } : null },
});
const historyPageWith = (...ids) => ({ ...page(ids[0]), commits: ids.map(id => ({ ...page(id).commits[0] })) });

test('Ctrl/Cmd history selection is local until Compare selected and uses the selected order', async () => {
  for (const modifier of ['ctrlKey', 'metaKey']) {
    const h = historyHarness();
    h.history.activate({ repositoryPath: '/repo' }, 'folder');
    h.pending[0].resolve(historyPageWith('A', 'B', 'C')); await tick();
    const click = (id, extend = true) => h.get('rows').handlers.click(selectHistoryEvent(id, { [modifier]: extend }));
    click('A'); assert.equal(h.get('compare-selected').disabled, true);
    click('B'); assert.equal(h.get('compare-selected').disabled, false);
    assert.match(h.get('rows').innerHTML, /LEFT/); assert.match(h.get('rows').innerHTML, /RIGHT/);
    click('C'); // The most recent two, matching the version-picker convention.
    assert.match(h.get('selected-note').textContent, /B → C/);
    assert.equal(h.changes.length, 0); assert.equal(h.pending.length, 1);
    h.picker.open = true;
    h.get('compare-selected').handlers.click();
    assert.equal(h.picker.open, false, 'make room for the comparison without losing the selected pair');
    assert.deepEqual(h.changes, [{ left: 'B', right: 'C' }]);
    click('C'); assert.equal(h.get('compare-selected').disabled, true);
    h.get('compare-selected').handlers.click(); assert.equal(h.changes.length, 1);
    click('A', false); assert.match(h.get('selected-note').textContent, /A · 1\/2/);
    h.get('clear-selected').handlers.click(); assert.equal(h.get('clear-selected').hidden, true);
    assert.deepEqual(h.history.selection(), { left: 'B', right: 'C' }, 'clearing selection does not alter the displayed comparison');
    assert.equal(h.pending.length, 1, 'no refresh, fetch or write on selection');
  }
});

test('history context menu compares the pair and ignores actions from an older folder or selection', async () => {
  const h = historyHarness(), context = { repositoryPath: '/repo' };
  h.history.activate(context, 'folder'); h.pending[0].resolve(historyPageWith('A', 'B')); await tick();
  h.get('rows').handlers.click(selectHistoryEvent('A'));
  h.get('rows').handlers.click(selectHistoryEvent('B', { metaKey: true }));
  h.get('rows').handlers.contextmenu(selectHistoryEvent('A'));
  assert.equal(h.menus[0][0].disabled, false);
  h.menus[0][0].run(); assert.deepEqual(h.changes, [{ left: 'A', right: 'B' }]);
  h.get('rows').handlers.click(selectHistoryEvent('B'));
  h.menus[0][0].run(); assert.equal(h.changes.length, 1, 'old pair menu cannot compare a different selection');
  h.get('rows').handlers.contextmenu(selectHistoryEvent('B'));
  assert.equal(h.menus[1][0].disabled, true);
  h.get('rows').handlers.click(selectHistoryEvent('A', { ctrlKey: true }));
  h.get('rows').handlers.contextmenu(selectHistoryEvent('A'));
  h.history.activate(context, 'other-folder');
  h.menus[2][0].run(); assert.equal(h.changes.length, 1, 'old folder menu is inert');
  h.pending[1].resolve(historyPageWith('C', 'D')); await tick();
  assert.equal(h.get('compare-selected').disabled, true);
  assert.deepEqual(h.pending.map(p => p.name), ['restore_path_history', 'restore_path_history']);
});

test('history supports keyboard pairs; pagination preserves selection and refresh prunes missing commits', async () => {
  const h = historyHarness(); h.history.activate({ repositoryPath: '/repo' }, 'folder');
  h.pending[0].resolve({ ...historyPageWith('A', 'B'), has_more: true }); await tick();
  let prevented = 0;
  for (const [id, key, modifier] of [['A', 'Enter', 'ctrlKey'], ['B', ' ', 'metaKey']]) {
    h.get('rows').handlers.keydown(selectHistoryEvent(id, { key, [modifier]: true, preventDefault: () => prevented++ }));
  }
  assert.equal(prevented, 2); assert.equal(h.get('compare-selected').disabled, false);
  const more = h.history.load(true); h.pending[1].resolve(historyPageWith('C')); await more;
  assert.match(h.get('selected-note').textContent, /A → B/);
  h.get('search').value = 'does not match'; h.get('search').handlers.input();
  assert.equal(h.get('compare-selected').disabled, false, 'search does not discard selected commits');
  const refresh = h.history.load(); h.pending[2].resolve(historyPageWith('B', 'C')); await refresh;
  assert.match(h.get('selected-note').textContent, /B · 1\/2/);
  assert.equal(h.get('compare-selected').disabled, true);
  h.get('rows').handlers.click(selectHistoryEvent('not-loaded', { ctrlKey: true }));
  assert.match(h.get('selected-note').textContent, /B · 1\/2/);
});

test('right-click a folder opens its history picker directly, without a menu or details query', async () => {
  const list = element(), opened = [], menus = [], submodules = [];
  const folder = { kind: 'folder', relative_path: 'folder' }, submodule = { kind: 'submodule', relative_path: 'dep' };
  const context = { refs: { fileList: list }, state: { repository: { path: '/repo' }, entries: [folder, submodule] },
    showFloatingMenu: (event, items) => menus.push(items), handleError: assert.fail,
    openFolderComparisonHistory: async entry => opened.push(entry), selectEntry: id => submodules.push(id),
    openSubmoduleMenu: entry => submodules.push(entry),
  };
  vm.runInNewContext('let fileListDelegationAttached = false;\n' + source('attachFileListDelegation'), context);
  context.attachFileListDelegation();
  const event = path => ({ target: { closest: () => ({ dataset: { entry: path } }) }, preventDefault() {}, stopPropagation() {} });
  list.handlers.contextmenu(event('folder'));
  assert.equal(menus.length, 0); assert.equal(submodules.length, 0);
  await tick(); assert.deepEqual(opened, [folder]);
  list.handlers.contextmenu(event('dep'));
  assert.deepEqual(submodules, ['dep', submodule]); assert.equal(menus.length, 0);
});

function pickerHarness(insideSubmodule = false) {
  const nodes = new Map(), pending = [], comparisons = [];
  const $ = id => { if (!nodes.has(id)) nodes.set(id, element()); return nodes.get(id); };
  const dialog = $('#folderHistoryDialog');
  dialog.open = false;
  dialog.showModal = () => { dialog.open = true; };
  dialog.close = () => { dialog.open = false; };
  dialog.querySelector = id => /data-history-(left|right|options|compare\]|worktree|swap)/.test(id) ? null : $(id);
  const state = { view: 'explorer', repository: { path: '/repo', name: 'repo', current_branch: 'feature', head_oid: 'head' }, branches: [],
    compareMode: 'submodule', commanderPath: 'previous', comparisonContext: { previous: true } };
  const originalContext = state.comparisonContext;
  const context = { state, $, ComparisonNavigation, status() {},
    submoduleBoundaryFor: () => insideSubmodule ? 'modules/dep' : null,
    innerPathForSubmodule: (sub, path) => path.slice(sub.length + 1),
    openRepositoryRevisionCompare: async (...args) => { comparisons.push(args); state.view = 'commander'; },
  };
  vm.createContext(context);
  vm.runInContext(['openFolderComparisonHistory', 'compareSelectedFolderHistory', 'closeFolderComparisonHistory'].map(source).join('\n'), context);
  context.folderHistoryPicker = ComparisonNavigation.createHistory({ root: dialog,
    invoke: (name, args) => new Promise((resolve, reject) => pending.push({ name, args, resolve, reject })),
    onCompare: selection => context.compareSelectedFolderHistory(selection),
  });
  return { context, state, dialog, pending, comparisons, originalContext, get: id => $(`[data-history-${id}]`),
    open: path => context.openFolderComparisonHistory({ kind: 'folder', relative_path: path }),
    click: (id, key = 'ctrlKey') => $('[data-history-rows]').handlers.click(selectHistoryEvent(id, { [key]: true })),
  };
}

test('folder picker stays in Explorer until a complete pair is explicitly compared, including submodule scope', async () => {
  for (const inside of [false, true]) {
    const h = pickerHarness(inside);
    await h.open(inside ? 'modules/dep/src' : 'src');
    assert.equal(h.dialog.open, true); assert.equal(h.state.view, 'explorer');
    assert.equal(h.state.comparisonContext, h.originalContext); assert.equal(h.state.commanderPath, 'previous');
    assert.equal(h.pending.length, 1);
    assert.deepEqual(h.pending[0].args, { repositoryPath: inside ? '/repo/modules/dep' : '/repo', relativePath: 'src', headId: null, offset: 0 });
    h.pending[0].resolve(historyPageWith('A', 'B')); await tick();
    assert.doesNotMatch(h.get('rows').innerHTML, /Set left|Set right|data-history-pick=/);
    assert.equal(h.get('compare-selected').disabled, true);
    h.click('A'); h.get('compare-selected').handlers.click();
    assert.equal(h.comparisons.length, 0);
    h.click('B', 'metaKey');
    assert.equal(h.state.view, 'explorer'); assert.equal(h.comparisons.length, 0);
    h.get('compare-selected').handlers.click(); await tick();
    assert.equal(h.comparisons.length, 1); assert.equal(h.dialog.open, false);
    assert.equal(h.state.view, 'commander'); assert.equal(h.state.folderHistoryPicker, null);
    assert.deepEqual(h.comparisons[0].slice(0, 4), ['A', 'B', 'src', '']);
    assert.equal(h.comparisons[0][4].repositoryPath, inside ? '/repo/modules/dep' : '/repo');
    assert.deepEqual(h.pending.map(p => p.name), ['restore_path_history']);
  }
});

test('closing or replacing the folder picker ignores late history and never opens Compare', async () => {
  const h = pickerHarness(); await h.open('one');
  h.context.closeFolderComparisonHistory();
  h.pending[0].resolve(historyPageWith('old-closed')); await tick();
  assert.equal(h.dialog.open, false); assert.equal(h.state.view, 'explorer');
  assert.doesNotMatch(h.get('rows').innerHTML, /old-closed/);
  await h.open('two'); await h.open('three');
  h.pending[1].reject('old-folder-failure'); await tick();
  assert.doesNotMatch(h.get('note').textContent, /old-folder-failure/);
  h.pending[2].resolve(historyPageWith('new')); await tick();
  assert.match(h.get('rows').innerHTML, /new/); assert.equal(h.comparisons.length, 0);
});

test('folder picker rejects repository/branch changes and supports explicit paginated history', async () => {
  for (const field of ['path', 'current_branch', 'head_oid']) {
    const h = pickerHarness(); await h.open('src');
    h.pending[0].resolve({ ...historyPageWith('A'), has_more: true }); await tick();
    h.click('A');
    h.get('more').handlers.click();
    assert.equal(h.pending[1].args.headId, 'pinned'); assert.equal(h.pending[1].args.offset, 500);
    h.pending[1].resolve(historyPageWith('B')); await tick(); h.click('B');
    h.state.repository[field] = 'changed';
    h.get('compare-selected').handlers.click(); await tick();
    assert.equal(h.comparisons.length, 0); assert.equal(h.dialog.open, false);
    assert.equal(h.state.view, 'explorer');
  }
});
