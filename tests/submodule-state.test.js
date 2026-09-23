const test = require('node:test');
const assert = require('node:assert/strict');
const { presentation } = require('../frontend/submodule-state.js');

test('submodule workflow states have short, unambiguous next-action labels', () => {
  assert.equal(presentation('changes_inside').short, 'Commit submodule changes');
  assert.equal(presentation('local_commit_push_needed').short, 'Push submodule commit');
  assert.equal(presentation('on_origin_stage_project').short, 'New version on origin · stage project link');
  assert.equal(presentation('on_origin_commit_project').short, 'New version on origin · commit project link');
  assert.equal(presentation('project_commit_push_needed').short, 'Parent project commit · push pending');
  assert.equal(presentation('project_commit_push_needed').row, 'Push project');
  assert.match(presentation('project_commit_push_needed').detail, /submodule itself is already in sync/i);
  assert.equal(presentation('on_origin_commit_project').row, 'Commit project link');
});

test('detached local commits never masquerade as ordinary unpushed branch commits', () => {
  assert.equal(presentation('detached_choose_branch').short, 'Detached HEAD · choose branch before push');
  assert.match(presentation('detached_choose_branch').detail, /Switch to or create a branch/);
});

test('synced is the only non-actionable submodule workflow state', () => {
  assert.equal(presentation('synced').actionable, false);
  for (const state of ['changes_inside', 'local_commit_push_needed', 'sync_needed', 'detached_choose_branch', 'origin_missing', 'on_origin_stage_project', 'on_origin_commit_project', 'project_commit_push_needed']) {
    assert.equal(presentation(state).actionable, true, state);
  }
});
