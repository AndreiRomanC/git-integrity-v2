const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
function load(names, context) {
  vm.createContext(context);
  for (const name of names) {
    const code = app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))?.[0];
    assert.ok(code, name);
    vm.runInContext(code, context);
  }
  return context;
}
function escapeFixture() {
  const flags = new Set(['open']);
  let modal = false, floating = false;
  const picker = { hidden: true };
  const context = {
    state: { view: 'commander', draft: 'Keep draft', staged: ['a.txt'] },
    refs: { changesDrawer: { contains: () => true, classList: { contains: x => flags.has(x), remove: x => flags.delete(x) } }, submoduleMenu: { hidden: true } },
    document: { activeElement: {}, querySelector: s => s === 'dialog[open]' ? modal : floating ? { remove: () => { floating = false; } } : null },
    $: s => s === '#repoPickerMenu' ? picker : { focus: () => context.focused = true },
    closeSubmoduleMenu: () => { context.refs.submoduleMenu.hidden = true; },
    returnToProjectNavigator: () => { context.navigated = true; },
  };
  load(['closeWorkingTree', 'handleWorkspaceEscape'], context);
  const press = extras => {
    const event = { key: 'Escape', ...extras, preventDefault() { this.prevented = true; }, stopImmediatePropagation() { this.stopped = true; } };
    context.handleWorkspaceEscape(event); return event;
  };
  return { context, flags, picker, press, modal: value => { modal = value; }, floating: value => { floating = value; } };
}
test('Escape closes Working tree only, preserving draft/staging and restoring focus', () => {
  const f = escapeFixture();
  const before = JSON.stringify(f.context.state);
  const event = f.press();
  assert.equal(f.flags.has('open'), false);
  assert.equal(JSON.stringify(f.context.state), before);
  assert.equal(f.context.navigated, undefined);
  assert.equal(f.context.focused, true);
  assert.equal(event.stopped, true);
  f.press(); assert.equal(f.context.navigated, true);
});
test('Escape leaves native dialogs, handled keys and IME alone', () => {
  const f = escapeFixture(); f.modal(true);
  assert.equal(f.press().prevented, undefined);
  f.modal(false);
  for (const event of [{ defaultPrevented: true }, { isComposing: true }, { key: 'Enter' }]) assert.equal(f.press(event).prevented, undefined);
  assert.equal(f.flags.has('open'), true);
});
test('Escape closes menus before the Working tree underneath', () => {
  const f = escapeFixture(); f.floating(true); f.picker.hidden = false; f.context.refs.submoduleMenu.hidden = false;
  f.press(); assert.equal(f.picker.hidden, false);
  f.press(); assert.equal(f.picker.hidden, true);
  f.press(); assert.equal(f.context.refs.submoduleMenu.hidden, true);
  assert.equal(f.flags.has('open'), true);
  f.press(); assert.equal(f.flags.has('open'), false);
});
function navigationFixture(selected, submoduleContext = null, previousMode = 'submodule') {
  let click;
  const context = {
    state: { view: 'explorer', selectedEntry: selected, currentPath: 'folder', compareMode: previousMode, commanderPath: 'stale-submodule-path' },
    refs: { search: { value: 'old filter' } },
    $: () => ({ addEventListener: (_, fn) => { click = fn; } }),
    currentSubmoduleCompareContext: () => submoduleContext,
    closeSubmoduleGraph() {}, resetFooterForContextChange() {}, render() {},
    parentPathOf: p => p.split('/').slice(0, -1).join('/'),
    syncLocalDriveToCommanderPath: () => { context.openedDrive = context.state.commanderPath; },
    openSubmoduleCompareFromEntry: (entry, options) => { context.openedModule = { entry, options }; },
  };
  vm.runInNewContext(app.slice(app.indexOf("$('#navCommander').addEventListener"), app.indexOf('refs.compareModeGit.addEventListener')), context);
  return { context, click: () => click() };
}
test('Compare & Sync defaults to Local Drive for files/folders/root, independent of old tab', () => {
  for (const old of ['git', 'submodule', 'local-drive']) {
    for (const [selected, path] of [[{ kind: 'file', relative_path: 'folder/nested/a.txt' }, 'folder/nested'], [{ kind: 'folder', relative_path: 'folder/nested' }, 'folder/nested'], [null, 'folder']]) {
      const f = navigationFixture(selected, null, old); f.click();
      assert.equal(f.context.state.compareMode, 'local-drive');
      assert.equal(f.context.openedDrive, path);
    }
  }
});
test('Compare & Sync reuses submodule context and does not reset an already open comparison', () => {
  const selected = { kind: 'submodule', relative_path: 'modules/dep' };
  const f = navigationFixture(selected, { entry: selected, innerPath: 'src' }); f.click();
  assert.equal(f.context.state.compareMode, 'submodule');
  assert.equal(f.context.openedModule.options.innerPath, 'src');
  const g = navigationFixture(null); g.context.state.view = 'commander'; g.click();
  assert.equal(g.context.openedDrive, undefined);
});
test('fresh graph comparisons reset to drill-down; explicit List choice is idempotent', async () => {
  for (const name of ['openGraphBranchCompare', 'openGraphRevisionCompare']) {
    const c = load([name], { state: { compareFlatMode: true }, refs: { search: {} },
      activeRepositoryContext: () => ({ path: '/repo', isSubmodule: false }), closeSubmoduleGraph() {}, render() {}, openCommanderDirectory: async () => {},
    });
    await c[name]('main', 'feature'); assert.equal(c.state.compareFlatMode, false);
  }
  let calls = 0;
  const c = load(['setComparisonListMode'], { state: { compareFlatMode: false, subCompareFlatMode: false, commanderRows: ['stale'], commanderPath: 'src', submoduleCompare: { rows: ['stale'] } }, renderCommander() {}, openCommanderDirectory: () => calls++, openSubmoduleCompareDirectory: () => calls++ });
  c.setComparisonListMode(false, true); c.setComparisonListMode(false, true);
  assert.equal(calls, 1); assert.equal(c.state.commanderRows.length, 0);
  c.setComparisonListMode(false, false); assert.equal(calls, 2);
  c.setComparisonListMode(true, true); assert.equal(calls, 3); assert.equal(c.state.submoduleCompare.rows.length, 0);
});
test('late comparison response cannot install a list after switching tab or layout', async () => {
  for (const submodule of [false, true]) {
    for (const change of ['layout', 'tab', 'revision']) {
      let finish;
      const comparison = { submodulePath: 'module', leftRef: 'a', rightRef: 'b', rows: [] };
      const c = {
        state: { repository: { path: '/repo' }, view: 'commander', compareMode: submodule ? 'submodule' : 'git', gitCompareMode: 'refs', compareFlatMode: true, subCompareFlatMode: true, submoduleCompare: comparison, commanderRows: [], branchCompareLeftRef: 'a', branchCompareRightRef: 'b' },
        refs: { commanderRows: {}, subCompareRows: {} }, ensureBranchCompareRefs() {},
        openCommanderDirectoryGuard: () => () => true, openSubmoduleCompareDirectoryGuard: () => () => true,
        invoke: () => new Promise(resolve => { finish = resolve; }), render: () => { c.rendered = true; }, status() {}, revisionDisplay: String,
      };
      load([submodule ? 'openSubmoduleCompareDirectory' : 'openCommanderDirectory'], c);
      const pending = submodule ? c.openSubmoduleCompareDirectory('') : c.openCommanderDirectory('');
      if (change === 'tab') c.state.compareMode = 'local-drive';
      else if (change === 'layout') c.state[submodule ? 'subCompareFlatMode' : 'compareFlatMode'] = false;
      else if (submodule) comparison.rightRef = 'other'; else c.state.branchCompareRightRef = 'other';
      finish({ rows: ['stale'] }); await pending;
      assert.equal(c.rendered, undefined);
      assert.equal((submodule ? comparison.rows : c.state.commanderRows).length, 0);
    }
  }
});
