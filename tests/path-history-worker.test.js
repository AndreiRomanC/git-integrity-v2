'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.resolve(__dirname, '..');
const app = fs.readFileSync(path.join(root, 'frontend/app.js'), 'utf8');

function harness() {
  const calls = [], errors = [];
  const state = { repository: { path: 'repo', current_branch: 'main' }, currentPath: 'folder', view: 'explorer', commits: [] };
  const context = { state, refs: { search: { value: '' } }, render() {}, status() {},
    selectedScope: () => ({ path: state.currentPath, name: state.currentPath }),
    submoduleBoundaryFor: () => null, handleError: error => errors.push(error),
    invoke: (command, args) => new Promise((resolve, reject) => calls.push({ command, args, resolve, reject })),
  };
  vm.createContext(context);
  vm.runInContext(app.slice(app.indexOf('function createRequestGuard()'), app.indexOf('let explorerRequestSeq')) +
    app.slice(app.indexOf('const showSelectedHistoryGuard'), app.indexOf('const SPEC_ID_PATTERN')), context);
  return { context, state, calls, errors };
}

test('path history command dispatches the unchanged read-only query to the existing worker', () => {
  const source = fs.readFileSync(path.join(root, 'src-tauri/src/repository.rs'), 'utf8');
  const wrapper = source.slice(source.indexOf('pub async fn path_history('), source.indexOf('#[derive(Serialize)]', source.indexOf('pub async fn path_history(')));
  assert.match(wrapper, /off_main_thread\(move \|\| Ok\(path_history_page_inner\(repository_path, relative_path, None, 0\)\?\.commits\)\)\.await/);
  assert.doesNotMatch(wrapper, /repo_write_lock|status\(|invalidate|fetch|checkout/);
});

for (const kind of ['path', 'submodule-refs']) {
  const start = h => kind === 'path' ? h.context.showSelectedHistory() : h.context.showSubmoduleReferenceChanges({ relative_path: 'modules/dep', name: 'dep' });
  test(`${kind}: latest history wins when responses arrive out of order`, async () => {
    const h = harness();
    const old = start(h), latest = start(h);
    assert.equal(h.calls[0].command, 'path_history');
    h.calls[1].resolve([{ id: 'new' }]);
    await latest;
    h.calls[0].resolve([{ id: 'old' }]);
    await old;
    assert.equal(h.state.commits[0].id, 'new');
    assert.equal(h.state.historyKind, kind);
  });
  for (const field of ['repository', 'folder', 'branch']) {
    test(`${kind}: ignores late results and errors after changing ${field}`, async () => {
      for (const fail of [false, true]) {
        const h = harness();
        const pending = start(h);
        if (field === 'repository') h.state.repository = { path: 'other', current_branch: 'main' };
        if (field === 'folder') h.state.currentPath = 'other';
        if (field === 'branch') h.state.repository.current_branch = 'feature';
        if (fail) h.calls[0].reject(new Error('old failure'));
        else h.calls[0].resolve([{ id: 'old' }]);
        await pending;
        assert.equal(h.state.commits.length, 0);
        assert.equal(h.state.view, 'explorer');
        assert.equal(h.errors.length, 0);
      }
    });
  }
}
