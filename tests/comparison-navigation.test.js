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
  const nodes = new Map(), pending = [], changes = [];
  const root = { querySelector: id => { if (!nodes.has(id)) nodes.set(id, element()); return nodes.get(id); } };
  const history = ComparisonNavigation.createHistory({ root, invoke: (name, args) => new Promise((resolve, reject) => pending.push({ name, args, resolve, reject })), onChange: () => changes.push(history.selection()) });
  return { history, pending, changes, get: id => root.querySelector(`[data-history-${id}]`) };
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
