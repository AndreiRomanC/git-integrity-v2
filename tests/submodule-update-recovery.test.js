'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const section = (start, end) => app.slice(app.indexOf(start), app.indexOf(end, app.indexOf(start)));

function harness() {
  const state = { repository: { path: 'parent' }, currentPath: 'vendor', consoleTranscript: [], consoleCmdHistory: [], consoleCommandRunning: false };
  const calls = [], refreshes = [], messages = [], events = [];
  const context = {
    state, performance, consoleHistoryPointer: -1, consoleHistoryDraft: '',
    setConsoleMode() {}, setCommandInputValue() {}, normalizeTerminalCommand: value => value,
    looksDestructiveTerminalCommand: () => false, consoleGitTarget: () => ({ label: 'Repository root', path: 'parent', displayPath: 'parent' }),
    invoke: (command, args) => new Promise((resolve, reject) => calls.push({ command, args, resolve, reject })),
    setInterval: () => 1, clearInterval: () => events.push('ticker-cleared'),
    renderConsoleTranscript() {}, refreshCommandHint() {}, jsPerfLog() {},
    status: text => messages.push(text),
    directoryCache: { clear: () => events.push('directories-cleared') },
    loadRepository: async (repositoryPath, options) => { refreshes.push({ repositoryPath, options }); },
    refreshStatusAndFolder: async (repositoryPath, folder) => { refreshes.push({ repositoryPath, folder }); },
    selectEntry: async () => {}, showOperationToast: text => messages.push(text),
  };
  vm.createContext(context);
  vm.runInContext(section('let consoleRunningTicker =', '// Live "what comes next" helper'), context);
  vm.runInContext(section('async function initAndUpdateSubmodulesFromActions()', 'function buildCommands()'), context);
  vm.runInContext(section('async function initializeSubmodule(', 'async function openSubmoduleAsFullRepository('), context);
  return { context, state, calls, refreshes, messages, events };
}

test('submodule update timeout refreshes once, keeps its error, and never claims success', async () => {
  const h = harness();
  const pending = h.context.initAndUpdateSubmodulesFromActions();
  assert.equal(h.calls[0].command, 'run_terminal_command');
  assert.equal(h.calls[0].args.commandText, 'git submodule update --init --recursive');
  h.calls[0].reject('Terminal command timed out after 60 minutes. Submodule update is incomplete.');
  const result = await pending;
  assert.equal(result.success, false);
  assert.match(result.stderr, /timed out after 60 minutes/);
  assert.equal(h.state.consoleTranscript[0].status, 'TIMED_OUT');
  assert.equal(h.state.consoleCommandRunning, false);
  assert.equal(h.refreshes.length, 1);
  assert.equal(h.refreshes[0].repositoryPath, 'parent');
  assert.equal(h.refreshes[0].options.force, true);
  assert.match(h.messages.at(-1), /did not complete.*may already have changed/);
  assert.equal(h.calls.length, 1, 'no reset, force update, stage, or clean is run');
});

test('nonzero update exit refreshes partial state and preserves stdout and stderr', async () => {
  const h = harness();
  const pending = h.context.runTerminalFromConsole('git submodule update --init --recursive');
  h.calls[0].resolve({ success: false, read_only: false, stdout: 'First module checked out', stderr: 'Second module failed', exit_code: 1 });
  const result = await pending;
  assert.equal(h.refreshes.length, 1);
  assert.equal(result.stdout, 'First module checked out');
  assert.equal(result.stderr, 'Second module failed');
  assert.equal(h.state.consoleTranscript[0].status, 'FAILED');
});

test('successful update still refreshes exactly once without changing its Git command', async () => {
  const h = harness();
  const pending = h.context.initAndUpdateSubmodulesFromActions();
  h.calls[0].resolve({ success: true, read_only: false, stdout: '', stderr: '', exit_code: 0 });
  const result = await pending;
  assert.equal(result.success, true);
  assert.equal(h.refreshes.length, 1);
  assert.equal(h.calls.length, 1);
  assert.match(h.messages.at(-1), /review any remaining local changes/);
});

test('successful and failed read-only terminal results still skip the repository reload', async () => {
  for (const success of [true, false]) {
    const h = harness();
    const pending = h.context.runTerminalFromConsole('git status --short');
    h.calls[0].resolve({ success, read_only: true, stdout: '', stderr: '', exit_code: success ? 0 : 1 });
    await pending;
    assert.equal(h.refreshes.length, 0);
    assert.ok(!h.events.includes('directories-cleared'));
  }
});

test('late failure cannot refresh the repository currently open in a different context', async () => {
  const h = harness();
  const pending = h.context.runTerminalFromConsole('git submodule update --init --recursive');
  h.state.repository = { path: 'other' };
  h.calls[0].reject('Terminal command timed out after 60 minutes');
  await pending;
  assert.equal(h.refreshes.length, 0);
  assert.equal(h.state.repository.path, 'other');
});

test('a failed refresh cannot hide the original timeout or turn it into success', async () => {
  const h = harness();
  h.context.loadRepository = async () => { throw new Error('status scan failed'); };
  const pending = h.context.runTerminalFromConsole('git submodule update --init --recursive');
  h.calls[0].reject('Terminal command timed out after 60 minutes');
  const result = await pending;
  assert.equal(result.success, false);
  assert.equal(result.refresh_failed, true);
  assert.match(result.stderr, /timed out after 60 minutes/);
  assert.match(result.stderr, /status could not be refreshed/);
  assert.equal(h.state.consoleTranscript[0].status, 'TIMED_OUT');
});

test('a status refresh failure is not misreported as failure of a successful Git update', async () => {
  const h = harness();
  h.context.loadRepository = async () => { throw new Error('status scan failed'); };
  const pending = h.context.initAndUpdateSubmodulesFromActions();
  h.calls[0].resolve({ success: true, read_only: false, stdout: 'checked out', stderr: '', exit_code: 0 });
  const result = await pending;
  assert.equal(result.success, true);
  assert.equal(result.refresh_failed, true);
  assert.equal(h.state.consoleTranscript[0].status, 'SUCCESS');
  assert.match(h.messages.at(-1), /completed, but status could not be refreshed/);
});

test('individual submodule initialization refreshes its partial state after failure without invoking cleanup', async () => {
  const h = harness();
  const pending = h.context.initializeSubmodule({ kind: 'submodule', relative_path: 'vendor/dep', name: 'dep' });
  assert.equal(h.calls[0].command, 'init_submodule');
  assert.equal(h.calls[0].args.repositoryPath, 'parent');
  h.calls[0].reject('Submodule update timed out. Initialization is incomplete.');
  await pending;
  assert.equal(h.refreshes.length, 1);
  assert.equal(h.refreshes[0].repositoryPath, 'parent');
  assert.equal(h.calls.length, 1);
  assert.ok(h.messages.some(message => /timed out/.test(message)));
});

test('individual submodule initialization cannot refresh a different repository after a late error', async () => {
  const h = harness();
  const pending = h.context.initializeSubmodule({ kind: 'submodule', relative_path: 'vendor/dep', name: 'dep' });
  h.state.repository = { path: 'other' };
  h.calls[0].reject('Submodule update is incomplete');
  await pending;
  assert.equal(h.refreshes.length, 0);
});
