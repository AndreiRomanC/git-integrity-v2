const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const branches = fs.readFileSync(path.join(__dirname, '../src-tauri/src/repository/branches.rs'), 'utf8').split('#[cfg(test)]')[0];

function harness() {
  const confirmations = [], calls = [], messages = [];
  const c = { activeRepositoryContext: () => ({path: '/repo', name: 'repo'}),
    customConfirm: async (...args) => { confirmations.push(args); return true; },
    invoke: async (...args) => { calls.push(args); }, refreshAfterGraphCommitAction: async () => {},
    status() {}, showOperationToast: message => messages.push(message), handleError: error => messages.push(String(error)),
  };
  vm.createContext(c);
  vm.runInContext(app.match(/async function restoreExactCheckpointFromGraphCommit\([^]*?\n}/)[0], c);
  return { c, confirmations, calls, messages };
}

test('both Clean checkout targets warn about permanent deletion with no temporary copy', async () => {
  for (const branch of ['', 'feature']) {
    const h = harness();
    await h.c.restoreExactCheckpointFromGraphCommit('a'.repeat(40), branch);
    assert.equal(h.confirmations[0][1].danger, true);
    assert.match(h.confirmations[0][0], /No backup or temporary copy is created/);
    assert.match(h.confirmations[0][0], /already deleted files are not restored/);
    assert.match(h.confirmations[0][0], /Other ignored build artifacts are not removed/);
    assert.equal(h.calls.length, 1);
    assert.equal(h.calls[0][0], branch ? 'restore_exact_checkpoint_branch' : 'restore_exact_checkpoint');
    assert.equal(h.calls[0][1].repositoryPath, '/repo');
  }
});

test('cancelled Clean deletes nothing, and failed Clean reports the error without automatic retry', async () => {
  const cancelled = harness(); cancelled.c.customConfirm = async () => false;
  await cancelled.c.restoreExactCheckpointFromGraphCommit('a'.repeat(40));
  assert.equal(cancelled.calls.length, 0);
  const failed = harness(); let attempts = 0;
  failed.c.invoke = async () => { attempts++; throw Error('Clean is incomplete. No backup was created and no files were restored.'); };
  await failed.c.restoreExactCheckpointFromGraphCommit('a'.repeat(40));
  assert.equal(attempts, 1); assert.equal(failed.messages.length, 1);
  assert.match(failed.messages[0], /Clean is incomplete/);
});

test('Clean production flow cannot create or restore a source quarantine', () => {
  assert.doesNotMatch(branches, /gdd-restore|RestoreCheckpointQuarantine|quarantine|rollback_restore|move_untracked/);
  assert.match(branches, /\["clean", "-ffd"\]/);
  const flow = branches.slice(branches.indexOf('fn restore_exact_checkpoint_locked'), branches.indexOf('pub struct CleanUpstreamResult'));
  assert.ok(flow.indexOf('restore_checkpoint_clean_targets') < flow.indexOf('restore_checkpoint_clean_untracked'));
  assert.match(flow, /restore_checkpoint_check_indexes\(&targets, "before-deinit"\)/);
  assert.match(flow, /invalidate_git_metadata\(repository_path\)/);
  assert.match(flow, /No backup was created and no files were restored/);
  assert.doesNotMatch(flow, /fs::rename|fs::copy|create_dir|rollback|cleanup after failed|optional_step|let _ =/);
});
