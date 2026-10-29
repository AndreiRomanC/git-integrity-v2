const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const source = name => app.match(new RegExp(`function ${name}\\([^]*?\\n}`))[0];
const change = (path, staged = true, status = 'M') => ({ path, staged, status });
function fixture(count = 75) {
  class Node {
    constructor(dataset = {}) { this.dataset = dataset; this.listeners = {}; this.value = ''; this.textContent = ''; this.disabled = false; }
    set disabled(value) { this._disabled = Boolean(value); }
    get disabled() { return this._disabled; }
    addEventListener(event, listener) { this.listeners[event] = listener; }
    emit(event, detail = {}) { return this.listeners[event]?.(detail); }
  }
  const metrics = { renders: 0, submoduleScans: 0, polarionSyncs: 0, badges: 0 };
  const elements = new Map();
  const $ = id => { if (!elements.has(id)) elements.set(id, new Node()); return elements.get(id); };
  const refs = Object.fromEntries(['changes', 'drawerScopeTitle', 'changesSummary', 'selectionText', 'commitButton', 'commitMessage', 'defaultCommitMessage', 'changesDrawer'].map(name => [name, $('#' + name)]));
  refs.changesDrawer.classList = { contains: () => true };
  let rows = [], inputs = [], buttons = [];
  Object.defineProperty(refs.changes, 'innerHTML', { set(html) {
    metrics.renders++;
    const paths = [...html.matchAll(/data-change-path="([^"]+)"/g)].map(match => match[1]);
    rows = paths.map(value => new Node({ changeComparePath: value }));
    inputs = paths.map(value => new Node({ changePath: value }));
    buttons = paths.map(value => new Node({ stashPath: value }));
  } });
  refs.changes.querySelectorAll = selector => selector === '[data-change-path]' ? inputs : selector === '[data-stash-path]' ? buttons : rows;
  const state = { repository: { path: '/fixture', head_detached: false }, currentPath: '', changesScope: 'global',
    changes: Array.from({ length: count }, (_, i) => change(`src/file_${i}.c`)) };
  const context = { state, refs, $, commitWriteRunning: false, drawerPolarionReferences: null, lastAppliedDefaultCommitMessage: '',
    knownSubmodulePaths() { metrics.submoduleScans++; return ['modules/a', 'modules/a/nested']; },
    syncDrawerPolarionReferences() { metrics.polarionSyncs++; }, updateChangeBadge() { metrics.badges++; },
    detachedHeadWorkMessage: () => 'Create or switch to a branch',
    toggleStage: (path, checked) => { context.lastToggle = { path, checked }; },
    stashOneFile: path => { context.lastStash = path; },
    openWorkingTreeChangeCompare: async value => { context.lastCompare = value; }, handleError: assert.fail,
  };
  vm.createContext(context);
  for (const name of ['esc', 'normalizeRepositoryRelativePath', 'changeDisplayState', 'updateCommitMessageControls', 'renderChanges', 'syncOpenDrawerDefaultCommitMessage']) {
    vm.runInContext(source(name), context);
  }
  vm.runInContext(app.match(/refs\.commitMessage\.addEventListener\('input', [^\n]+/)[0], context);
  const type = value => { refs.commitMessage.value = value; refs.commitMessage.emit('input'); };
  return { context, state, refs, metrics, $, type, rows: () => rows, inputs: () => inputs, buttons: () => buttons };
}

for (const count of [75, 750, 5000]) {
  test(`typing with ${count} staged items keeps rows and all staging controls untouched`, () => {
    const f = fixture(count); f.context.renderChanges();
    const rows = f.rows(), inputs = f.inputs(), before = { ...f.metrics };
    f.refs.changes.scrollTop = 250;
    for (let i = 0; i < 100; i++) f.type('Commit note ' + i);
    f.type('P:OEM-12 - Note\n\nOEM-101\nOEM-102\nOEM-103');
    assert.deepEqual(f.metrics, before, 'no list render, submodule lookup, context sync or badge update per keystroke');
    assert.strictEqual(f.rows(), rows); assert.strictEqual(f.inputs(), inputs);
    assert.equal(f.refs.changes.scrollTop, 250);
    assert.equal(f.refs.commitButton.disabled, false);
    assert.equal(f.refs.commitMessage.value, 'P:OEM-12 - Note\n\nOEM-101\nOEM-102\nOEM-103');
    assert.equal(f.refs.selectionText.textContent, `${count} files in staging area`);
    assert.equal(f.metrics.submoduleScans, 1, 'one shared submodule lookup per full list render');
  });
}

test('message validation retains all commit safety gates and uses current staging state', () => {
  const f = fixture();
  for (const text of ['', ' \t\n', 'message']) {
    f.type(text); assert.equal(f.refs.commitButton.disabled, !text.trim());
  }
  f.state.repository.head_detached = true; f.type('message');
  assert.equal(f.refs.commitButton.disabled, true); assert.match(f.refs.commitButton.title, /branch/);
  f.state.repository.head_detached = false; f.context.commitWriteRunning = true; f.type('message');
  assert.equal(f.refs.commitButton.disabled, true);
  f.context.commitWriteRunning = false;
  let allowed = false;
  f.context.drawerPolarionReferences = { canSubmit: () => allowed };
  f.type('message'); assert.equal(f.refs.commitButton.disabled, true);
  allowed = true; f.type('message'); assert.equal(f.refs.commitButton.disabled, false);
  f.state.changes.forEach(item => { item.staged = false; });
  f.type('message'); assert.equal(f.refs.commitButton.disabled, true, 'no cached staged count');
  f.state.changes.at(-1).staged = true; f.type('message');
  assert.equal(f.refs.commitButton.disabled, false); assert.equal(f.refs.commitButton.title, '');
});

test('folder-scoped validation does not accept staged files outside the folder or a common prefix', () => {
  const f = fixture(0); f.state.changesScope = 'folder'; f.state.currentPath = 'src/a';
  f.state.changes = [change('src/ab/file.c'), change('other/file.c'), change('src/a/file.c', false)];
  f.type('message'); assert.equal(f.refs.commitButton.disabled, true);
  f.state.changes[2].staged = true; f.type('message'); assert.equal(f.refs.commitButton.disabled, false);
  f.state.changes = [change('src/a')]; f.type('gitlink'); assert.equal(f.refs.commitButton.disabled, false);
  f.state.currentPath = 'other'; f.type('message'); assert.equal(f.refs.commitButton.disabled, true);
});

test('stage/status refresh still rebuilds rows, restores checked properties and updates the staged count', async () => {
  const f = fixture(0); f.state.changes = [change('src/a.c'), change('src/b.c', false)];
  f.type('Keep my draft'); f.context.renderChanges();
  assert.deepEqual(f.inputs().map(input => input.checked), [true, false]);
  const input = f.inputs()[1]; input.checked = true; input.emit('change');
  assert.deepEqual(f.context.lastToggle, { path: 'src/b.c', checked: true });
  f.buttons()[0].emit('click'); assert.equal(f.context.lastStash, 'src/a.c');
  f.rows()[1].emit('dblclick', { target: { closest: () => null } });
  assert.strictEqual(f.context.lastCompare, f.state.changes[1]);
  f.state.changes[1].staged = true; f.context.renderChanges();
  assert.deepEqual(f.inputs().map(input => input.checked), [true, true]);
  assert.equal(f.refs.selectionText.textContent, '2 files in staging area');
  assert.equal(f.refs.commitMessage.value, 'Keep my draft');
  f.state.changes = []; f.context.renderChanges();
  assert.equal(f.refs.commitButton.disabled, true); assert.equal(f.$('#stageAllButton').disabled, true);
  assert.equal(f.$('#unstageAllButton').disabled, true);
});

test('batched root lookup preserves submodule-root versus inner-file status semantics', () => {
  const f = fixture(0);
  for (const [item, expected] of [
    [change('modules/a'), 'Submodule version staged'],
    [change('modules/a', false), 'Submodule changed'],
    [change('modules/a/nested'), 'Submodule version staged'],
    [change('modules\\a'), 'Submodule version staged'],
    [change('modules/a/file.c'), 'Staged'],
    [change('modules/ab'), 'Staged'],
    [change('new.c', true, 'A'), 'Staged new file'],
    [change('deleted.c', true, 'D'), 'Staged deletion'],
    [change('renamed.c', true, 'R'), 'Staged rename'],
  ]) assert.equal(f.context.changeDisplayState(item), expected);
});

test('synchronizing a default message validates without rebuilding rows and never replaces a personal draft', () => {
  const f = fixture(); f.context.renderChanges(); const before = { ...f.metrics };
  f.refs.defaultCommitMessage.value = 'Default'; f.context.syncOpenDrawerDefaultCommitMessage();
  assert.equal(f.refs.commitMessage.value, 'Default'); assert.equal(f.refs.commitButton.disabled, false);
  f.refs.defaultCommitMessage.value = 'Updated'; f.context.syncOpenDrawerDefaultCommitMessage();
  assert.equal(f.refs.commitMessage.value, 'Updated');
  f.type('Personal draft'); f.refs.defaultCommitMessage.value = 'Another'; f.context.syncOpenDrawerDefaultCommitMessage();
  assert.equal(f.refs.commitMessage.value, 'Personal draft'); assert.deepEqual(f.metrics, before);
});

test('StructDoc callbacks revalidate pending/failed/ready references without redrawing staging rows', () => {
  const f = fixture(); let options, allowed = false;
  f.context.createPolarionCommitReferences = config => { options = config; return { setContext() {}, canSubmit: () => allowed }; };
  f.context.submoduleCommitReferenceContext = () => ({ repositoryPath: '/parent', moduleName: 'a' });
  f.context.invoke = () => assert.fail('message input must not trigger IPC'); f.context.jsPerfLog = () => {};
  vm.runInContext(source('syncDrawerPolarionReferences'), f.context);
  f.context.renderChanges(); const before = { ...f.metrics };
  f.type('Base description'); assert.equal(f.refs.commitButton.disabled, true);
  allowed = true; f.refs.commitMessage.value += '\n\nOEM-1\nOEM-2\nOEM-3'; options.onChange();
  assert.equal(f.refs.commitButton.disabled, false);
  allowed = false; options.onChange(); assert.equal(f.refs.commitButton.disabled, true);
  assert.match(f.refs.commitMessage.value, /Base description\n\nOEM-1/);
  assert.deepEqual(f.metrics, before);
});
