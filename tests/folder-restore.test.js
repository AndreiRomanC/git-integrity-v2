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
const historyPage = (commits, hasMore = false, offset = commits.length, head = 'C1') => ({ commits, has_more: hasMore, next_offset: offset, head_id: head });

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
  assert.deepEqual({ ...h.calls[0].args }, { repositoryPath: 'repo', relativePath: 'folder/a.txt', headId: null, offset: 0 });
  h.calls[0].resolve(historyPage([{ id: 'C1', subject: 'new file version' }, { id: 'C2', subject: 'older' }]));
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
    assert.equal(h.calls[i].command, 'restore_path_history');
    assert.deepEqual({ ...h.calls[i].args }, { repositoryPath: 'repo', relativePath: 'folder', headId: null, offset: 0 });
    h.calls[i].resolve(historyPage([{ id: 'C1', subject: 'new' }, { id: 'C2', subject: 'old' }]));
    await pending;
    assert.deepEqual(Array.from(h.state.folderRestore.commits, commit => commit.id), ['C1', 'C2']);
  }
});

test('restore history pages are explicit, pin HEAD and preserve selection when appending', async () => {
  const h = harness();
  const first = h.context.loadFolderRestoreCommits();
  h.calls[0].resolve(historyPage([], true, 500));
  await first;
  assert.equal(h.calls.length, 1, 'no automatic older-history scan');
  assert.match(h.refs.folderRestoreCommitList.innerHTML, /No matching commits in the loaded portion/);
  assert.equal(h.refs.loadOlderFolderRestoreCommits.hidden, false);
  const older = h.context.loadFolderRestoreCommits(true);
  assert.deepEqual({ ...h.calls[1].args }, { repositoryPath: 'repo', relativePath: 'folder', headId: 'C1', offset: 500 });
  h.calls[1].resolve(historyPage([{ id: 'OLD', subject: 'older file' }], true, 1000));
  await older;
  assert.equal(h.state.folderRestore.selectedCommit, 'OLD');
  const oldest = h.context.loadFolderRestoreCommits(true);
  h.calls[2].resolve(historyPage([{ id: 'ROOT', subject: 'created' }], false, 1001));
  await oldest;
  assert.deepEqual(Array.from(h.state.folderRestore.commits, c => c.id), ['OLD', 'ROOT']);
  assert.equal(h.state.folderRestore.selectedCommit, 'OLD');
  assert.equal(h.refs.loadOlderFolderRestoreCommits.hidden, true);
  assert.match(h.refs.folderRestoreHistoryStatus.textContent, /End of history/);
});

test('restore history errors preserve loaded pages and support retry without skipping a page', async () => {
  const h = harness();
  const first = h.context.loadFolderRestoreCommits();
  h.calls[0].resolve(historyPage([{ id: 'C1', subject: 'latest' }], true, 500)); await first;
  const older = h.context.loadFolderRestoreCommits(true);
  assert.equal(h.refs.loadOlderFolderRestoreCommits.disabled, true);
  await h.context.loadFolderRestoreCommits(true);
  assert.equal(h.calls.length, 2, 'no concurrent duplicate request');
  h.calls[1].reject(new Error('read failed')); await older;
  assert.equal(h.state.folderRestore.commits[0].id, 'C1');
  assert.equal(h.state.folderRestore.historyOffset, 500);
  assert.match(h.refs.folderRestoreHistoryStatus.textContent, /Retry with Load older/);
  const retry = h.context.loadFolderRestoreCommits(true);
  assert.equal(h.calls[2].args.offset, 500);
  h.calls[2].resolve(historyPage([], false, 600)); await retry;
});

test('restore history from a closed dialog or another repository cannot replace the current list', async () => {
  for (const switchRepository of [false, true]) {
    const h = harness();
    const pending = h.context.loadFolderRestoreCommits();
    if (switchRepository) h.state.repository.path = 'another-repo';
    else { h.refs.folderRestoreDialog.listeners.cancel(); h.context.openFolderRestoreDialog({ kind: 'folder', name: 'other', relative_path: 'other' }); }
    h.calls[0].resolve(historyPage([{ id: 'STALE', subject: 'old response' }])); await pending;
    assert.equal(h.state.folderRestore.commits.length, 0);
  }
});

test('restore commit rows keep full long messages, separate metadata and accessible selection', () => {
  const h = harness();
  const message = 'A long commit subject — '.repeat(40);
  h.state.folderRestore.commits = [{ id: '1234567890abcdef', subject: message, author: 'Long author', date: '2026-10-09' }];
  h.state.folderRestore.selectedCommit = '1234567890abcdef';
  h.context.renderFolderRestoreCommits();
  const html = h.refs.folderRestoreCommitList.innerHTML;
  assert.ok(html.includes(message));
  assert.match(html, /aria-pressed="true"/);
  assert.match(html, /<code title="1234567890abcdef">12345678<\/code>/);
  assert.match(html, /<time>2026-10-09<\/time>/);
  const css = fs.readFileSync(path.join(__dirname, '../frontend/styles.css'), 'utf8');
  const subjectStyle = css.match(/\.folder-restore-commit strong \{([^}]+)\}/)[1];
  assert.match(subjectStyle, /white-space: normal/);
  assert.match(subjectStyle, /overflow-wrap: anywhere/);
  assert.doesNotMatch(subjectStyle, /ellipsis|nowrap/);
});
