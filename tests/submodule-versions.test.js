// Automated tests for frontend/submodule-versions.js — run with:
//   node --test
// Same zero-dependency setup as tests/graph-model.test.js — see its own
// banner comment for why this never touches the shipped app's own
// "no Node.js/npm runtime or build step" promise.
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { groupSubmoduleBranchVersions, submoduleVersionRowHtml, submodulePushDialogState } = require('../frontend/submodule-versions.js');

function branch(name, upstream) { return { name, kind: 'branch', upstream: upstream || null }; }
function remote(name) { return { name, kind: 'remote' }; }

test('a local branch with a tracked upstream hides the matching remote-tracking entry', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('develop', 'origin/develop'),
    remote('origin/develop'),
  ]);
  assert.equal(local.length, 1);
  assert.equal(local[0].name, 'develop');
  assert.deepEqual(remoteOnly, [], 'origin/develop is already represented via develop\'s own upstream');
});

test('a remote-tracking branch with no local counterpart lands in remoteOnly', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('main', 'origin/main'),
    remote('origin/main'),
    remote('origin/feature/never-checked-out'),
  ]);
  assert.equal(local.length, 1);
  assert.equal(remoteOnly.length, 1);
  assert.equal(remoteOnly[0].name, 'origin/feature/never-checked-out');
});

test('a local branch with no upstream configured never hides any remote entry by name coincidence', () => {
  // Same shorthand name locally and on the remote, but *not* actually
  // tracked (upstream is null) — must not be matched by name alone.
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('release', null),
    remote('origin/release'),
  ]);
  assert.equal(local.length, 1);
  assert.equal(remoteOnly.length, 1, 'an untracked local branch must never suppress a same-named remote entry — only a real configured upstream can');
});

test('a local branch can track a differently-named remote branch', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('work', 'release/main'),
    remote('release/main'),
  ]);
  assert.equal(local.length, 1);
  assert.deepEqual(remoteOnly, []);
});

test('tags and commits are ignored entirely — this only ever groups branch/remote entries', () => {
  const { local, remoteOnly } = groupSubmoduleBranchVersions([
    branch('main', 'origin/main'),
    remote('origin/main'),
    { name: 'v1.0', kind: 'tag' },
    { name: 'abc1234', kind: 'commit' },
  ]);
  assert.equal(local.length, 1);
  assert.deepEqual(remoteOnly, []);
});

test('tolerates a missing/undefined list without throwing', () => {
  assert.deepEqual(groupSubmoduleBranchVersions(undefined), { local: [], remoteOnly: [] });
  assert.deepEqual(groupSubmoduleBranchVersions([]), { local: [], remoteOnly: [] });
});

test('a branch row shows the exact tip SHA and subject from the existing response', () => {
  const html = submoduleVersionRowHtml({ name: 'develop', kind: 'branch', revision: 'a5cf438372b9568c', subject: 'test32', author: 'Andrei', date: '2026-09-12', current: true, upstream: 'origin/develop', ahead: 1, behind: 0 });
  assert.match(html, /<code>a5cf4383<\/code>/);
  assert.match(html, /test32/);
  assert.match(html, /origin\/develop/);
});

test('a tag row shows the target commit SHA and subject', () => {
  const html = submoduleVersionRowHtml({ name: 'v1.0.0', kind: 'tag', revision: '72a991227b54aaaf', subject: 'Release desktop UI', author: 'Andrei', date: '2026-09-12', current: false, attached_branch: 'main' });
  assert.match(html, /v1\.0\.0/);
  assert.match(html, /<code>72a99122<\/code>/);
  assert.match(html, /Release desktop UI/);
});

test('identically named refs from different submodules keep their own SHAs', () => {
  const first = submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: '11111111aaaaaaaa', subject: 'First module', author: 'A', date: '2026-09-12' });
  const second = submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: '22222222bbbbbbbb', subject: 'Second module', author: 'B', date: '2026-09-12' });
  assert.match(first, /11111111/);
  assert.doesNotMatch(first, /22222222/);
  assert.match(second, /22222222/);
  assert.doesNotMatch(second, /11111111/);
});

test('row rendering is pure and does not invoke a backend loader', () => {
  let requests = 0;
  global.invoke = () => { requests += 1; };
  submoduleVersionRowHtml({ name: 'main', kind: 'branch', revision: 'abcdef1234567890', subject: 'No I/O', author: 'A', date: '2026-09-12' });
  delete global.invoke;
  assert.equal(requests, 0);
});

test('push stays enabled when an existing origin branch is ahead but has no configured upstream', () => {
  const state = submodulePushDialogState({
    branch: 'develop', local_sha: '2ab9a609e2b7', will_create_remote_branch: false,
    ahead: 2, behind: 0, can_push: true, blocked_reason: null,
    commits: [{ id: 'a5cf4383', subject: 'test32' }, { id: '2ab9a609', subject: 'test 32' }],
  });
  assert.equal(state.canPush, true);
  assert.equal(state.commits.length, 2);
  assert.equal(state.summary, '2 commits to push');
});

test('new remote branch creation is enabled even with no comparison commit list', () => {
  const state = submodulePushDialogState({ branch: 'feature/new', local_sha: '1234567890ab', will_create_remote_branch: true, can_push: true, commits: [] });
  assert.equal(state.canPush, true);
  assert.equal(state.summary, 'Create origin/feature/new');
  assert.match(state.emptyMessage, /create origin\/feature\/new at 12345678/);
});

test('an up-to-date submodule keeps normal push disabled with the backend reason', () => {
  const state = submodulePushDialogState({ branch: 'develop', local_sha: '2ab9a609', will_create_remote_branch: false, can_push: false, blocked_reason: 'Already up to date with origin/develop.', commits: [] });
  assert.equal(state.canPush, false);
  assert.equal(state.commits.length, 0);
  assert.equal(state.emptyMessage, 'Already up to date with origin/develop.');
});
