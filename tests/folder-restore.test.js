'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const logic = app.slice(app.indexOf('function folderRestoreSourceRevision()'), app.indexOf('async function initializeSubmodule('));
const events = app.slice(app.indexOf("refs.folderRestoreModeHead.addEventListener"), app.indexOf("$('#initRepo').addEventListener"));

function harness() {
  const calls = [];
  const nodes = {};
  const refs = new Proxy({}, { get(target, name) {
    return target[name] ||= { checked: false, open: true, listeners: {},
      addEventListener(event, fn) { this.listeners[event] = fn; },
      showModal() { this.open = true; }, setAttribute() {},
      close() { this.open = false; },
      querySelector(selector) { return nodes[selector] ||= {}; },
      closest(selector) { return nodes[selector] ||= {}; },
    };
  } });
  const state = { repository: { path: 'repo' }, changes: [], entries: [], currentPath: '' };
  const context = { state, refs, esc: String, commitSubjectHtml: String, status() {},
    submoduleBoundaryFor: () => null, directoryCache: new Map(), performance, jsPerfLog() {},
    refreshStatusAndFolder: async () => {}, showOperationToast() {},
    beginButtonOperation: () => () => {}, handleError(error) { throw error; },
    invoke: (command, args) => new Promise((resolve, reject) => calls.push({ command, args, resolve, reject })),
  };
  vm.createContext(context);
  vm.runInContext(logic + '\n' + events, context);
  context.openFolderRestoreDialog({ kind: 'folder', name: 'folder', relative_path: 'folder' });
  return { context, state, refs, calls };
}

const preview = (source, paths = []) => ({ source_revision: source, source_id: source === 'HEAD' ? 'C1' : source,
  source_subject: 'snapshot', tracked_changes: paths.map(path => ({ status: 'M', path })), clean_candidates: [] });

test('folder restore preview ignores a result for the previous source', async () => {
  const h = harness();
  const pending = h.context.previewFolderRestore();
  h.refs.folderRestoreModeCommit.checked = true;
  h.state.folderRestore.selectedCommit = 'C2';
  h.refs.folderRestoreModeCommit.listeners.change();
  h.calls[0].resolve(preview('HEAD'));
  await pending;
  assert.equal(h.state.folderRestore.preview, null, 'HEAD preview must not be reused for C2');
});

test('tracked files reuse the folder dialog with file history and no clean operation', async () => {
  const h = harness();
  const entry = { kind: 'file', tracked: true, name: 'a.txt', relative_path: 'folder/a.txt' };
  h.context.openFolderRestoreDialog(entry);
  assert.equal(h.state.folderRestore.isFile, true);
  assert.equal(h.refs.folderRestoreDialog.querySelector('[data-restore-title]').textContent, 'Restore file');
  assert.equal(h.refs.folderRestoreClean.checked, false);
  assert.equal(h.refs.folderRestoreClean.closest('label').hidden, true);
  const history = h.context.loadFolderRestoreCommits();
  assert.deepEqual({ ...h.calls[0].args }, { repositoryPath: 'repo', relativePath: 'folder/a.txt' });
  h.calls[0].resolve([{ id: 'C1', subject: 'new file version' }, { id: 'C2', subject: 'older' }]);
  await history;
  const pending = h.context.previewFolderRestore();
  assert.equal(h.calls[1].command, 'preview_folder_restore');
  assert.equal(h.calls[1].args.itemKind, 'file');
  assert.equal(h.calls[1].args.cleanUntracked, false);
  h.calls[1].resolve(preview('HEAD', ['folder/a.txt']));
  await pending;
  assert.equal(h.refs.confirmFolderRestore.textContent, 'Restore file');
  assert.doesNotMatch(h.refs.folderRestorePreview.innerHTML, /Untracked clean preview/);
  h.context.customConfirm = async (message, options) => {
    assert.match(message, /staged and unstaged changes in this file will be overwritten/);
    assert.equal(options.title, 'Restore file');
    return true;
  };
  const restoring = h.context.confirmFolderRestore();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(h.calls[2].command, 'restore_folder');
  assert.equal(h.calls[2].args.relativePath, 'folder/a.txt');
  assert.equal(h.calls[2].args.sourceRevision, 'C1', 'use the exact previewed SHA');
  assert.equal(h.calls[2].args.itemKind, 'file');
  assert.equal(h.calls[2].args.cleanPaths.length, 0);
  h.calls[2].resolve();
  await restoring;
  h.context.openFolderRestoreDialog({ kind: 'folder', name: 'folder', relative_path: 'folder' });
  assert.equal(h.refs.folderRestoreClean.checked, true, 'folder defaults are preserved after using file mode');
  assert.equal(h.refs.folderRestoreClean.closest('label').hidden, false);
});

test('deleted tracked files are eligible, but untracked files and submodules are not', () => {
  const h = harness();
  h.context.openFolderRestoreDialog({ kind: 'deleted', tracked: true, relative_path: 'gone.txt', name: 'gone.txt' });
  assert.equal(h.state.folderRestore.isFile, true);
  const model = h.state.folderRestore;
  for (const entry of [{ kind: 'file', tracked: false }, { kind: 'submodule', tracked: true }]) {
    h.context.openFolderRestoreDialog({ ...entry, relative_path: 'other', name: 'other' });
    assert.equal(h.state.folderRestore, model);
  }
  h.context.submoduleBoundaryFor = () => 'vendor/dep';
  h.context.openFolderRestoreDialog({ kind: 'file', tracked: true, relative_path: 'vendor/dep/a.txt', name: 'a.txt' });
  assert.equal(h.state.folderRestore, model);
});

test('folder restore preview rejects an old response after switching away and back', async () => {
  const h = harness();
  const old = h.context.previewFolderRestore();
  h.refs.folderRestoreModeCommit.checked = true;
  h.state.folderRestore.selectedCommit = 'C2';
  h.refs.folderRestoreModeCommit.listeners.change();
  h.refs.folderRestoreModeCommit.checked = false;
  h.refs.folderRestoreModeHead.listeners.change();
  const fresh = h.context.previewFolderRestore();
  h.calls[2].resolve(preview('HEAD', ['folder/local.txt']));
  await fresh;
  h.calls[0].resolve(preview('HEAD'));
  await old;
  assert.equal(h.state.folderRestore.preview.tracked_changes.length, 1);
});

test('folder restore preview ignores responses and errors from a closed dialog', async () => {
  for (const fail of [false, true]) {
    const h = harness();
    const pending = h.context.previewFolderRestore();
    h.refs.folderRestoreDialog.listeners.cancel();
    h.context.openFolderRestoreDialog({ kind: 'folder', name: 'other', relative_path: 'other' });
    h.refs.folderRestoreStatus.textContent = 'new dialog';
    if (fail) h.calls[0].reject(new Error('old request failed'));
    else h.calls[0].resolve(preview('HEAD'));
    await pending;
    assert.equal(h.state.folderRestore.preview, null);
    assert.equal(h.refs.folderRestoreStatus.textContent, 'new dialog');
  }
});

test('Preview from HEAD cannot be replaced by a late empty preview of an older commit', async () => {
  const h = harness();
  h.refs.folderRestoreModeCommit.checked = true;
  h.state.folderRestore.selectedCommit = 'C2';
  const old = h.context.previewFolderRestore();
  h.refs.folderRestoreModeCommit.checked = false;
  h.refs.folderRestoreModeHead.listeners.change();
  const current = h.context.previewFolderRestore();
  assert.equal(h.calls[1].args.sourceRevision, 'HEAD');
  h.calls[1].resolve(preview('HEAD', ['folder/file.txt']));
  await current;
  h.calls[0].resolve(preview('C2'));
  await old;
  assert.equal(h.state.folderRestore.preview.source_revision, 'HEAD');
  assert.match(h.refs.folderRestorePreview.innerHTML, /folder\/file.txt/);
});

test('changing Clean invalidates an in-flight preview and its deletion candidates', async () => {
  const h = harness();
  const pending = h.context.previewFolderRestore();
  h.refs.folderRestoreClean.checked = false;
  h.refs.folderRestoreClean.listeners.change();
  h.calls[0].resolve({ ...preview('HEAD'), clean_candidates: ['folder/untracked.txt'] });
  await pending;
  assert.equal(h.state.folderRestore.preview, null);
});

test('confirmation cannot execute a preview invalidated while the user was deciding', async () => {
  const h = harness();
  h.state.folderRestore.preview = preview('HEAD');
  let decide;
  h.context.customConfirm = () => new Promise(resolve => { decide = resolve; });
  const pending = h.context.confirmFolderRestore();
  h.refs.folderRestoreClean.checked = false;
  h.refs.folderRestoreClean.listeners.change();
  decide(true);
  await pending;
  assert.equal(h.calls.length, 0, 'no Git restore may run for the invalidated preview');
});

test('folder restore history reloads from the repository and path, never the restored source', async () => {
  const h = harness();
  for (let i = 0; i < 2; i++) {
    h.state.folderRestore.selectedCommit = 'C2';
    const pending = h.context.loadFolderRestoreCommits();
    assert.equal(h.calls[i].command, 'path_history');
    assert.deepEqual({ ...h.calls[i].args }, { repositoryPath: 'repo', relativePath: 'folder' });
    h.calls[i].resolve([{ id: 'C1', subject: 'new' }, { id: 'C2', subject: 'old' }]);
    await pending;
    assert.deepEqual(h.state.folderRestore.commits.map(commit => commit.id), ['C1', 'C2']);
  }
});
