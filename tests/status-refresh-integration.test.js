'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const refresh = require('../frontend/refresh-coordinator.js');

// Exercise the real app scopes/application callback, not a simplified merge
// function. Only IPC, DOM repainting and repository-open events are simulated.
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const start = app.indexOf('const STATUS_REFRESH_CHANGES =');
const end = app.indexOf('async function refreshChangesLightweight()', start);
assert.ok(start >= 0 && end > start);
const integration = app.slice(start, end);
const nextTurn = () => new Promise(resolve => setImmediate(resolve));

function harness() {
  const runs = [], paints = [], events = [];
  let badges = 0;
  const state = { repository: { path: 'repo-a' }, changes: ['initial'], statusReady: false, view: 'explorer', currentPath: '' };
  const context = {
    state, repoOpenGeneration: 1,
    window: { GitDrillDownRefresh: { createRefreshCoordinator(options) {
      return refresh.createRefreshCoordinator({ ...options, log: (event, details) => events.push({ event, ...details }) });
    } } },
    refs: { changesDrawer: { classList: { contains: () => false } } },
    updateChangeBadge: () => { badges++; }, renderChanges: () => {},
    openDirectory: async folder => { paints.push(folder); }, jsPerfLog: () => {},
    invoke: (command, args) => new Promise((resolve, reject) => runs.push({ command, args, resolve, reject })),
  };
  vm.createContext(context);
  vm.runInContext(integration + '\nglobalThis.request = requestRepositoryStatusRefresh;', context);
  return { context, state, runs, paints, events, badges: () => badges };
}

test('an ordinary status+folder refresh still invokes once and repaints once', async () => {
  const h = harness();
  const pending = h.context.request('repo-a', { repaintFolder: true, folder: '' });
  h.runs[0].resolve(['fresh']);
  await pending;
  assert.equal(h.runs.length, 1);
  assert.equal(h.runs[0].command, 'refresh_status');
  assert.deepEqual(h.state.changes, ['fresh']);
  assert.equal(h.state.statusReady, true);
  assert.deepEqual(h.paints, ['']);
});

test('invalid optional drawer request cannot veto a mandatory mutation refresh', async () => {
  const h = harness();
  let valid = true;
  const first = h.context.request('repo-a', { reason: 'changes-drawer', isValid: () => valid });
  valid = false; // navigation invalidated the drawer request's context
  const second = h.context.request('repo-a', { reason: 'mutation', repaintFolder: true });
  h.runs[0].resolve(['old']);
  await nextTurn();
  assert.deepEqual(h.state.changes, ['initial']);
  h.runs[1].resolve(['fresh']);
  await Promise.all([first, second]);
  assert.deepEqual(h.state.changes, ['fresh']);
  assert.deepEqual(h.paints, ['']);
  assert.equal(h.runs.length, 2);
});

test('a repaint requested for root supersedes the earlier subfolder target', async () => {
  const h = harness();
  h.state.currentPath = 'src';
  const first = h.context.request('repo-a', { folder: 'src', repaintFolder: true });
  h.state.currentPath = '';
  const second = h.context.request('repo-a', { folder: '', repaintFolder: true });
  h.runs[0].resolve(['old']);
  await nextTurn();
  h.runs[1].resolve(['fresh']);
  await Promise.all([first, second]);
  assert.deepEqual(h.paints, ['']);
});

test('status-only requests do not erase a pending folder repaint', async () => {
  const h = harness();
  h.state.currentPath = 'src';
  const first = h.context.request('repo-a', { folder: 'src', repaintFolder: true });
  h.context.request('repo-a', { reason: 'changes-drawer' });
  h.runs[0].resolve([]); await nextTurn(); h.runs[1].resolve(['fresh']);
  await first;
  assert.deepEqual(h.paints, ['src']);
});

test('old lightweight status cannot overwrite a newer full repository reload', async () => {
  const h = harness();
  const pending = h.context.request('repo-a', { repaintFolder: true, isValid: () => true });
  h.context.repoOpenGeneration++;
  h.state.changes = ['newer reload'];
  h.runs[0].resolve(['old scan']);
  await pending;
  assert.deepEqual(h.state.changes, ['newer reload']);
  assert.equal(h.badges(), 0);
  assert.deepEqual(h.paints, []);
  assert.equal(h.events.at(-1).applied, false);
});

test('leaving and reopening the same repository does not revive its old request', async () => {
  const h = harness();
  const pending = h.context.request('repo-a', { isValid: () => h.state.repository.path === 'repo-a' });
  h.context.repoOpenGeneration++; h.state.repository = { path: 'repo-b' };
  h.context.repoOpenGeneration++; h.state.repository = { path: 'repo-a' };
  h.state.changes = []; h.state.statusReady = false;
  h.runs[0].resolve(['old scan']); await pending;
  assert.deepEqual(h.state.changes, []);
  assert.equal(h.state.statusReady, false);
});

test('new-session request can follow an old in-flight request without losing its scope', async () => {
  const h = harness();
  const first = h.context.request('repo-a', { folder: 'src', repaintFolder: true, isValid: () => false });
  h.context.repoOpenGeneration++;
  const second = h.context.request('repo-a', { folder: '', repaintFolder: true });
  h.runs[0].resolve(['old']); await nextTurn(); h.runs[1].resolve(['fresh']);
  await Promise.all([first, second]);
  assert.deepEqual(h.state.changes, ['fresh']);
  assert.deepEqual(h.paints, ['']);
  assert.equal(h.runs.length, 2);
});

test('a mandatory old-session request cannot validate an invalid new-session request', async () => {
  const h = harness();
  const first = h.context.request('repo-a', { repaintFolder: true });
  h.context.repoOpenGeneration++;
  h.context.request('repo-a', { isValid: () => false });
  h.runs[0].resolve(['old']); await nextTurn(); h.runs[1].resolve(['obsolete context']);
  await first;
  assert.deepEqual(h.state.changes, ['initial']);
  assert.deepEqual(h.paints, []);
});

test('all obsolete optional requests remain ignored', async () => {
  const h = harness();
  const pending = h.context.request('repo-a', { isValid: () => false });
  h.runs[0].resolve(['obsolete']); await pending;
  assert.deepEqual(h.state.changes, ['initial']);
  assert.equal(h.badges(), 0);
});

test('a failed superseded scan uses the existing pending rerun, without a third scan', async () => {
  const h = harness();
  const first = h.context.request('repo-a', { isValid: () => false });
  h.context.request('repo-a', { reason: 'mutation', repaintFolder: true });
  h.runs[0].reject(new Error('Repository changed while reading status'));
  await nextTurn(); h.runs[1].resolve(['fresh']); await first;
  assert.equal(h.runs.length, 2);
  assert.deepEqual(h.state.changes, ['fresh']);
});
