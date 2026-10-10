'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));

function harness() {
  const calls = [], events = [], timers = new Map();
  let id = 0;
  const state = { repository: { path: 'repo' }, currentPath: '', changes: [{ path: 'a', staged: false }] };
  const buttons = {};
  const context = { state, performance, jsPerfLog() {}, renderChanges() {}, status() {}, showOperationToast() {},
    handleError: error => String(error),
    setTimeout(fn, ms) { timers.set(++id, { fn, ms }); return id; }, clearTimeout(key) { timers.delete(key); },
    $: selector => buttons[selector] ||= { textContent: selector, disabled: false },
    invoke: (command, args) => new Promise((resolve, reject) => { events.push(command); calls.push({ command, args, resolve, reject }); }),
    refreshStatusAndFolder: async () => { events.push('refresh'); },
    refreshStatusAndFolderInBackground() { events.push('background-refresh'); },
    requestRepositoryStatusRefresh: async () => {},
    activeRepositoryContext: () => ({ path: state.repository.path, isSubmodule: false }),
    beginButtonOperation: () => () => {},
    loadRepository: async () => { events.push('load'); },
  };
  vm.createContext(context);
  vm.runInContext(app.slice(app.indexOf('let pendingToggles ='), app.indexOf("$('#openRepo').addEventListener")) +
    app.slice(app.indexOf('async function stageAllInScope('), app.indexOf('// Pre-fills the commit message box')) +
    '\nglobalThis.flush = flushPendingTogglesNow;', context);
  return { context, state, calls, events, timers };
}

test('rapid checkbox changes drain in order; Commit barrier waits for the final unstage', async () => {
  const h = harness();
  h.context.toggleStage('a', true);
  const ready = h.context.flush().then(() => h.events.push('commit-ready'));
  await tick();
  assert.deepEqual(h.events, ['stage_files']);
  h.context.toggleStage('a', false);
  h.calls[0].resolve({ staged_paths: ['a'], skipped_dirty_submodules: [] });
  await tick();
  assert.equal(h.calls[1].command, 'unstage_files');
  assert.ok(!h.events.includes('commit-ready'));
  h.calls[1].resolve();
  await ready;
  assert.equal(h.state.changes[0].staged, false);
  assert.equal(h.events.at(-1), 'commit-ready');
});

test('Stage all, a later checkbox and branch switching share the same ordering barrier', async () => {
  const h = harness();
  h.context.refreshStatusAndFolder = async () => { h.state.changes[0].staged = true; };
  const bulk = h.context.stageAllInScope('');
  await tick();
  h.context.toggleStage('a', false);
  const switching = h.context.switchBranch('feature');
  await tick();
  assert.deepEqual(h.calls.map(c => c.command), ['stage_all']);
  h.calls[0].resolve({ staged_paths: ['a'], skipped_dirty_submodules: [] });
  await tick();
  assert.deepEqual(h.calls.map(c => c.command), ['stage_all', 'unstage_files']);
  h.calls[1].resolve();
  await tick();
  assert.equal(h.calls[2].command, 'switch_branch');
  h.calls[2].resolve();
  await Promise.all([bulk, switching]);
  assert.equal(h.state.changes[0].staged, false, 'bulk refresh must not erase the later checkbox choice');
  assert.equal(h.events.at(-1), 'load');
});

test('Commit/repository-switch barrier includes bulk refresh, not just the backend write', async () => {
  const h = harness();
  let finishRefresh;
  h.context.refreshStatusAndFolder = () => new Promise(resolve => { finishRefresh = resolve; });
  const bulk = h.context.stageAllInScope('');
  await tick();
  h.calls[0].resolve({ staged_paths: ['a'], skipped_dirty_submodules: [] });
  await tick();
  let ready = false;
  const barrier = h.context.flush().then(() => { ready = true; });
  await tick();
  assert.equal(ready, false);
  finishRefresh();
  await Promise.all([bulk, barrier]);
  assert.equal(ready, true);
});

test('failed staging refreshes truth and prevents a waiting branch switch', async () => {
  const h = harness();
  h.context.toggleStage('a', true);
  const switching = h.context.switchBranch('feature');
  await tick();
  h.calls[0].reject(new Error('index.lock exists'));
  await switching;
  assert.deepEqual(h.calls.map(c => c.command), ['stage_files']);
  assert.ok(h.events.includes('refresh'));
  h.context.toggleStage('a', true);
  const retry = h.context.flush();
  await tick();
  h.calls[1].resolve({ staged_paths: ['a'], skipped_dirty_submodules: [] });
  await retry;
});

test('timeout refuses to proceed without cancelling or forgetting staging', async () => {
  const h = harness();
  h.context.toggleStage('a', true);
  const waiting = h.context.flush();
  const rejected = assert.rejects(waiting, /has not finished/);
  await tick();
  [...h.timers.values()].find(timer => timer.ms === 20000).fn();
  await rejected;
  let done = false;
  const retry = h.context.flush().then(() => { done = true; });
  await tick();
  assert.equal(done, false);
  assert.equal(h.calls.length, 1);
  h.calls[0].resolve({ staged_paths: ['a'], skipped_dirty_submodules: [] });
  await retry;
});

test('stage/unstage IPC wrappers reuse synchronous inner logic without nested workers', () => {
  const source = fs.readFileSync(path.join(__dirname, '../src-tauri/src/repository.rs'), 'utf8');
  assert.match(source, /pub async fn stage_files\([^]*?off_main_thread\(move \|\| stage_files_blocking\(path, files\)\)\.await/);
  assert.match(source, /pub async fn unstage_files\([^]*?off_main_thread\(move \|\| \{[^]*?unstage_files_blocking\(path.clone\(\), files\)/);
  assert.match(source, /let inner = stage_files_blocking\(sub_path, inner_files\)\?/);
  assert.match(source, /unstage_files_blocking\(sub_path, inner_files\)\?/);
});
