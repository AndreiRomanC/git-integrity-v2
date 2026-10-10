'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');

function harness() {
  const calls = [], refreshes = [];
  const context = { state: { repository: { path: 'repo' }, statusReady: true }, repoOpenGeneration: 1,
    activeStagingOperation: null, pendingToggleFlight: null, pendingToggles: new Map(),
    performance, status() {}, jsPerfLog() {}, refs: { stashesDialog: { open: false } }, stashDialogContext: null,
    window: { __TAURI__: { core: { invoke: (command, args) => new Promise((resolve, reject) => calls.push({ command, args, resolve, reject })) } } },
    loadRepository: async repo => { refreshes.push(repo); },
    checkForMergeConflicts: async () => { refreshes.push('conflicts'); },
    refreshStashesList: async () => { refreshes.push('stashes'); },
  };
  vm.createContext(context);
  vm.runInContext(app.slice(0, app.indexOf('const $ =')) +
    app.slice(app.indexOf('async function refreshAfterMutationFailure('), app.indexOf('function parseChangedSubmodulePaths(')) +
    '\nglobalThis.call = invoke;', context);
  return { context, calls, refreshes };
}

test('Merge/Stash failure refreshes repository and conflicts but preserves original error', async () => {
  for (const command of ['merge_branch', 'resolve_conflict', 'open_merge_tool', 'pop_stash', 'restore_stash_paths', 'stash_changes', 'complete_merge']) {
    const h = harness();
    h.context.refs.stashesDialog.open = true;
    h.context.stashDialogContext = {};
    const pending = h.context.call(command, { repositoryPath: 'repo' });
    const rejected = assert.rejects(pending, /original failure/);
    h.calls[0].reject(new Error('original failure'));
    await rejected;
    assert.deepEqual(h.refreshes, ['repo', 'conflicts', 'stashes']);
  }
});

test('successful mutation does not introduce any recovery refresh', async () => {
  const h = harness();
  const pending = h.context.call('merge_branch', { repositoryPath: 'repo' });
  h.calls[0].resolve({ status: 'merged' });
  await pending;
  assert.deepEqual(h.refreshes, []);
});

test('late mutation failure cannot reload another repository or a reopened context', async () => {
  for (const differentRepo of [true, false]) {
    const h = harness();
    const pending = h.context.call('stash_changes', { repositoryPath: 'repo' });
    const rejected = assert.rejects(pending, /failure/);
    h.context.repoOpenGeneration++;
    if (differentRepo) h.context.state.repository.path = 'other';
    h.calls[0].reject(new Error('failure'));
    await rejected;
    assert.deepEqual(h.refreshes, []);
  }
});

test('recovery refresh failure never hides the original Git error', async () => {
  const h = harness();
  h.context.loadRepository = async () => { throw new Error('refresh failed'); };
  const pending = h.context.call('merge_branch', { repositoryPath: 'repo' });
  const rejected = assert.rejects(pending, /original failure/);
  h.calls[0].reject(new Error('original failure'));
  await rejected;
});

test('incompatible mutation and repository reload cannot overtake staging', async () => {
  const h = harness();
  h.context.activeStagingOperation = Promise.resolve();
  for (const command of ['checkout_commit', 'merge_branch', 'stash_changes', 'load_repository']) {
    await assert.rejects(h.context.call(command, {}), error => /Stage\/Unstage/.test(String(error)));
  }
  assert.equal(h.calls.length, 0);
  const read = h.context.call('path_history', {});
  h.calls[0].resolve([]);
  await read;
  const stage = h.context.call('stage_files', {});
  h.calls[1].resolve({});
  await stage;
});
