'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { createRefreshCoordinator } = require('../frontend/refresh-coordinator.js');

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function nextTurn() { return new Promise(resolve => setImmediate(resolve)); }

function controlledCoordinator(options = {}) {
  const runs = [];
  const applied = [];
  const events = [];
  const coordinator = createRefreshCoordinator({
    execute: (key, scope) => {
      const gate = deferred();
      runs.push({ key, scope, gate });
      return gate.promise;
    },
    apply: async (key, result, scope) => { applied.push({ key, result, scope }); },
    mergeScope: (left, right) => Math.max(left, right),
    describeScope: String,
    log: (event, details) => events.push({ event, details }),
    ...options,
  });
  return { coordinator, runs, applied, events };
}

test('one refresh request executes and applies exactly once', async () => {
  const { coordinator, runs, applied } = controlledCoordinator();
  const request = coordinator.request('repo-a', 0);
  assert.equal(runs.length, 1);
  runs[0].gate.resolve('fresh');
  assert.equal(await request, 'fresh');
  assert.deepEqual(applied, [{ key: 'repo-a', result: 'fresh', scope: 0 }]);
  assert.equal(coordinator.activeCount(), 0);
});

test('three equivalent requests during one active refresh collapse into one rerun', async () => {
  const { coordinator, runs, applied } = controlledCoordinator();
  const first = coordinator.request('repo-a', 0);
  const second = coordinator.request('repo-a', 0);
  const third = coordinator.request('repo-a', 0);
  assert.strictEqual(first, second);
  assert.strictEqual(second, third);
  assert.equal(runs.length, 1);
  runs[0].gate.resolve('old');
  await nextTurn();
  assert.equal(runs.length, 2);
  runs[1].gate.resolve('new');
  assert.deepEqual(await Promise.all([first, second, third]), ['new', 'new', 'new']);
  assert.deepEqual(applied.map(item => item.result), ['new']);
});

test('a repository mutation request during an active refresh forces a fresh rerun', async () => {
  const { coordinator, runs, applied, events } = controlledCoordinator();
  const initial = coordinator.request('repo-a', 0);
  const afterMutation = coordinator.request('repo-a', 1);
  runs[0].gate.resolve({ epoch: 15 });
  await nextTurn();
  runs[1].gate.resolve({ epoch: 16 });
  await Promise.all([initial, afterMutation]);
  assert.deepEqual(applied.map(item => item.result.epoch), [16]);
  assert.ok(events.some(item => item.event === 'rerun'));
});

test('a stale first result can never become the final applied UI state', async () => {
  const { coordinator, runs, applied } = controlledCoordinator();
  const request = coordinator.request('repo-a', 0);
  coordinator.request('repo-a', 0);
  runs[0].gate.resolve(['stale-file.txt']);
  await nextTurn();
  assert.deepEqual(applied, []);
  runs[1].gate.resolve(['current-file.txt']);
  await request;
  assert.deepEqual(applied.map(item => item.result), [['current-file.txt']]);
});

test('a stronger pending scope upgrades the rerun and final application', async () => {
  const { coordinator, runs, applied, events } = controlledCoordinator();
  const request = coordinator.request('repo-a', 0);
  coordinator.request('repo-a', 2);
  runs[0].gate.resolve('old');
  await nextTurn();
  assert.equal(runs[1].scope, 2);
  runs[1].gate.resolve('new');
  await request;
  assert.equal(applied[0].scope, 2);
  assert.ok(events.some(item => item.event === 'upgraded'));
});

test('a weaker pending scope cannot downgrade a stronger active refresh', async () => {
  const { coordinator, runs, applied } = controlledCoordinator();
  const request = coordinator.request('repo-a', 2);
  coordinator.request('repo-a', 0);
  runs[0].gate.resolve('old');
  await nextTurn();
  assert.equal(runs[1].scope, 2);
  runs[1].gate.resolve('new');
  await request;
  assert.equal(applied[0].scope, 2);
});

test('failure of the first refresh still runs and applies a pending recovery refresh', async () => {
  const { coordinator, runs, applied } = controlledCoordinator();
  const first = coordinator.request('repo-a', 0);
  const recovery = coordinator.request('repo-a', 1);
  runs[0].gate.reject(new Error('temporary failure'));
  await nextTurn();
  assert.equal(runs.length, 2);
  runs[1].gate.resolve('recovered');
  assert.deepEqual(await Promise.all([first, recovery]), ['recovered', 'recovered']);
  assert.deepEqual(applied.map(item => item.result), ['recovered']);
});

test('two repositories refresh independently', async () => {
  const { coordinator, runs, applied } = controlledCoordinator();
  const a = coordinator.request('repo-a', 0);
  const b = coordinator.request('repo-b', 0);
  assert.equal(runs.length, 2);
  assert.deepEqual(new Set(runs.map(item => item.key)), new Set(['repo-a', 'repo-b']));
  runs.find(item => item.key === 'repo-b').gate.resolve('b');
  runs.find(item => item.key === 'repo-a').gate.resolve('a');
  await Promise.all([a, b]);
  assert.deepEqual(new Set(applied.map(item => `${item.key}:${item.result}`)), new Set(['repo-a:a', 'repo-b:b']));
});

test('rapid stage then unstage ends on the final status without extra scans', async () => {
  const { coordinator, runs, applied } = controlledCoordinator();
  const stageRefresh = coordinator.request('repo-a', 1);
  const unstageRefresh = coordinator.request('repo-a', 1);
  runs[0].gate.resolve([{ path: 'file.c', staged: true }]);
  await nextTurn();
  assert.equal(runs.length, 2, 'one active scan plus one required rerun');
  runs[1].gate.resolve([{ path: 'file.c', staged: false }]);
  await Promise.all([stageRefresh, unstageRefresh]);
  assert.deepEqual(applied[0].result, [{ path: 'file.c', staged: false }]);
  assert.equal(runs.length, 2, 'coalescing must not add a third physical scan');
});

test('an isolated failure clears the active slot so a later request can recover', async () => {
  const { coordinator, runs } = controlledCoordinator();
  const failed = coordinator.request('repo-a', 0);
  runs[0].gate.reject(new Error('failed'));
  await assert.rejects(failed, /failed/);
  assert.equal(coordinator.isRefreshing('repo-a'), false);
  const retry = coordinator.request('repo-a', 0);
  assert.equal(runs.length, 2);
  runs[1].gate.resolve('ok');
  assert.equal(await retry, 'ok');
});
