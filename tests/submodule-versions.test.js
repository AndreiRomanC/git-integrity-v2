// Automated tests for frontend/submodule-versions.js — run with:
//   node --test
// Same zero-dependency setup as tests/graph-model.test.js — see its own
// banner comment for why this never touches the shipped app's own
// "no Node.js/npm runtime or build step" promise.
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { groupSubmoduleBranchVersions } = require('../frontend/submodule-versions.js');

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
