const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');

function harness() {
  const calls = [], displayed = [], confirms = [];
  const button = { disabled: false };
  const c = {
    commandHistoryRepositoryPath: '/repo', commandHistoryGeneration: 2,
    activeRepositoryContext: () => ({ path: '/repo' }),
    refs: { commandHistoryDialog: { open: true }, commandHistoryList: { querySelector: () => null, insertAdjacentHTML() {}, scrollTop: 100 } },
    $: () => button,
    beginButtonOperation: b => { b.disabled = true; return () => { b.disabled = false; }; },
    status: (...args) => displayed.push(args), commandHistoryStatusHtml: () => '',
    invoke: async (cmd, args) => {
      calls.push([cmd, args]);
      return cmd === 'inspect_index_lock'
        ? { lock_path: '/repo/.git/index.lock', bytes: 33, age_seconds: 180, ticket: 'checked-file' }
        : { backup_path: '/repo/.git/recovery/index.lock.backup' };
    },
    customConfirm: async (message, options) => { confirms.push([message, options]); options.checkbox.checked = true; return true; },
  };
  vm.createContext(c);
  vm.runInContext(app.match(/async function recoverIndexLockFromConsole\([^]*?\n}/)[0], c);
  return { c, calls, displayed, confirms, button };
}

test('recovery is explicit, scoped, separately confirmed and never retries the destructive action', async () => {
  const { c, calls, displayed, confirms, button } = harness();
  await c.recoverIndexLockFromConsole();
  assert.deepEqual(calls.map(([cmd]) => cmd), ['inspect_index_lock', 'recover_index_lock']);
  assert.equal(calls[1][1].repositoryPath, '/repo');
  assert.equal(calls[1][1].ticket, 'checked-file');
  assert.equal(calls[1][1].confirmed, true);
  assert.equal(confirms[0][1].danger, true);
  assert.match(confirms[0][0], /does not prove that it is abandoned/);
  assert.match(confirms[0][0], /will NOT restart automatically/);
  assert.match(displayed.at(-1)[0], /index.lock.backup/);
  assert.match(displayed.at(-1)[0], /retry your original action manually/);
  assert.equal(button.disabled, false);
});

test('cancel, unchecked acknowledgement and context drift never recover a lock', async () => {
  for (const mode of ['cancel', 'unchecked', 'closed', 'reopened', 'other-repo']) {
    const { c, calls } = harness();
    c.customConfirm = async (_message, options) => {
      options.checkbox.checked = mode !== 'unchecked';
      if (mode === 'closed') c.refs.commandHistoryDialog.open = false;
      if (mode === 'reopened') c.commandHistoryGeneration++;
      if (mode === 'other-repo') c.activeRepositoryContext = () => ({ path: '/other' });
      return mode !== 'cancel';
    };
    await c.recoverIndexLockFromConsole();
    assert.deepEqual(calls.map(([cmd]) => cmd), ['inspect_index_lock'], mode);
  }
});

test('inspection failure displays its actual reason without confirmation or recovery', async () => {
  const { c, confirms, displayed, button } = harness();
  c.invoke = async () => { throw 'A process has the index lock open. Nothing was changed.'; };
  await c.recoverIndexLockFromConsole();
  assert.equal(confirms.length, 0);
  assert.equal(displayed[0][1], 'error');
  assert.match(displayed[0][0], /process has the index lock open/);
  assert.equal(button.disabled, false);
});

test('late inspection and repeated clicks cannot run a recovery against another console', async () => {
  const { c, calls, confirms } = harness();
  let finish;
  c.invoke = async cmd => { calls.push([cmd]); return new Promise(resolve => { finish = resolve; }); };
  const first = c.recoverIndexLockFromConsole();
  await c.recoverIndexLockFromConsole();
  c.commandHistoryGeneration++;
  finish({ ticket: 'old' }); await first;
  assert.equal(calls.length, 1);
  assert.equal(confirms.length, 0);
});

test('backend recovery refusal is not reported as success or automatically retried', async () => {
  const { c, calls, displayed } = harness();
  const invoke = c.invoke;
  c.invoke = async (cmd, args) => {
    if (cmd === 'recover_index_lock') { calls.push([cmd]); throw 'The lock was replaced after inspection.'; }
    return invoke(cmd, args);
  };
  await c.recoverIndexLockFromConsole();
  assert.equal(calls.length, 2);
  assert.equal(displayed.at(-1)[1], 'error');
  assert.match(displayed.at(-1)[0], /replaced/);
});

test('recovery uses the central mutation guards, not background or reset cleanup', () => {
  assert.match(app.split(']);')[0], /'recover_index_lock'/);
  assert.equal((app.match(/invoke\('recover_index_lock'/g) || []).length, 1);
  const backend = fs.readFileSync(path.join(__dirname, '../src-tauri/src/repository/index_lock.rs'), 'utf8');
  assert.doesNotMatch(backend.split('#[cfg(test)]')[0], /fs::remove_file|\bgit\(|restore_exact_checkpoint\(|reset --hard/);
});
