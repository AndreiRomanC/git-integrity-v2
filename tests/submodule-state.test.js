const test = require('node:test');
const assert = require('node:assert/strict');
const { presentation } = require('../frontend/submodule-state.js');

test('submodule workflow states have short, unambiguous next-action labels', () => {
  assert.equal(presentation('changes_inside').short, 'Changes inside · commit needed');
  assert.equal(presentation('local_commit_push_needed').short, 'Local commit · push needed');
  assert.equal(presentation('on_origin_stage_project').short, 'On origin · stage project');
  assert.equal(presentation('on_origin_commit_project').short, 'On origin · commit project');
  assert.equal(presentation('project_commit_push_needed').short, 'Project commit · push needed');
  assert.equal(presentation('on_origin_commit_project').row, 'On origin · commit');
});

test('detached local commits never masquerade as ordinary unpushed branch commits', () => {
  assert.equal(presentation('detached_choose_branch').short, 'Choose branch before push');
  assert.match(presentation('detached_choose_branch').detail, /Switch to or create a branch/);
});

test('synced is the only non-actionable submodule workflow state', () => {
  assert.equal(presentation('synced').actionable, false);
  for (const state of ['changes_inside', 'local_commit_push_needed', 'sync_needed', 'detached_choose_branch', 'origin_missing', 'on_origin_stage_project', 'on_origin_commit_project', 'project_commit_push_needed']) {
    assert.equal(presentation(state).actionable, true, state);
  }
});
