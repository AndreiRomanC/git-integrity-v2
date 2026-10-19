const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
function promptHarness() {
  const nodes = new Map();
  function $(id) {
    if (!nodes.has(id)) nodes.set(id, { handlers: {}, value: '', hidden: false, disabled: false,
      addEventListener(type, fn) { this.handlers[type] = fn; }, removeEventListener(type) { delete this.handlers[type]; },
      showModal() { this.open = true; }, close() { this.open = false; }, focus() {}, select() {} });
    return nodes.get(id);
  }
  let notify, context, allowed = true, disposed = 0;
  const sandbox = { $, invoke: () => { throw new Error('No Git or server operation expected'); }, jsPerfLog() {},
    createPolarionCommitReferences(options) { notify = options.onChange; return { setContext(value) { context = value; }, canSubmit: () => allowed,
      message: base => base + '\n\nPolarion: OEM-1', dispose: () => { disposed++; } }; } };
  vm.createContext(sandbox); vm.runInContext(app.match(/function customPrompt\([^]*?\n}/)[0], sandbox);
  return { $, sandbox, refs: value => { allowed = value; notify(); }, context: () => context, disposed: () => disposed };
}
test('detached submodule dialog opens with StructDoc controls, but neither reference changes nor Ctrl+Enter can submit', async () => {
  const h = promptHarness();
  const pending = h.sandbox.customPrompt('Commit module', '', { multiline: true, blockingReason: 'Detached HEAD — create a branch first.', polarionContext: { moduleName: 'errm_adpdg' } });
  assert.equal(h.$('#appPromptDialog').open, true); assert.equal(h.context().moduleName, 'errm_adpdg');
  assert.equal(h.$('#appPromptBlocker').hidden, false); assert.match(h.$('#appPromptBlocker').textContent, /Detached HEAD/);
  h.$('#appPromptTextarea').value = 'Draft'; h.refs(false); h.refs(true);
  assert.equal(h.$('#appPromptOk').disabled, true);
  h.$('#appPromptTextarea').handlers.keydown({ key: 'Enter', ctrlKey: true, preventDefault() {} });
  h.$('#appPromptOk').handlers.click(); assert.equal(h.$('#appPromptDialog').open, true);
  h.$('#appPromptCancel').handlers.click(); assert.equal(await pending, null); assert.equal(h.disposed(), 1);
  const normal = h.sandbox.customPrompt('Normal commit', 'Draft', { multiline: true });
  assert.equal(h.$('#appPromptBlocker').hidden, true); assert.equal(h.$('#appPromptOk').disabled, false);
  h.$('#appPromptOk').handlers.click(); assert.equal(await normal, 'Draft\n\nPolarion: OEM-1');
});
test('Commit submodule opens inspection while detached without a Git write; an attached branch keeps the existing commit operation', async () => {
  const calls = [], prompts = [], defaults = [], state = { repository: { path: '/parent' }, currentPath: 'modules' };
  const sandbox = { state, refs: { defaultCommitMessage: { value: ' P:OEM-123 - Selected task ' } },
    customPrompt: async (message, value, options) => { prompts.push(options); defaults.push(value); return 'Draft'; },
    submoduleCommitReferenceContext: entry => ({ moduleName: entry.name }), invoke: async (name, args) => { calls.push({ name, args }); return { push_detail: '' }; },
    status() {}, directoryCache: { clear() {} }, loadRepository: async () => {}, showOperationToast() {}, handleError(error) { throw error; } };
  vm.createContext(sandbox); vm.runInContext(app.match(/async function commitSubmoduleChanges\([^]*?\n}/)[0], sandbox);
  const entry = { kind: 'submodule', name: 'errm_adpdg', relative_path: 'modules/errm_adpdg', submodule_current_branch: '' };
  await sandbox.commitSubmoduleChanges(entry);
  assert.equal(prompts.length, 1); assert.match(prompts[0].blockingReason, /Detached HEAD/); assert.equal(calls.length, 0);
  assert.equal(defaults[0], 'P:OEM-123 - Selected task', 'detached inspection also displays the selected base message');
  await sandbox.commitSubmoduleChanges({ ...entry, submodule_current_branch: 'feature' });
  assert.equal(prompts[1].blockingReason, ''); assert.equal(calls.length, 1);
  assert.equal(calls[0].name, 'commit_submodule'); assert.equal(calls[0].args.alsoPush, false);
  assert.equal(defaults[1], defaults[0]);
  assert.match(app, /canCommitInsideSubmodule = entry\.kind === 'submodule' && entry\.submodule_initialized !== false/);
});
test('submodule commit starts with the top message and appends optional StructDoc IDs without changing the global message', async () => {
  const { polarionCommitMessageWithReferences } = require('../frontend/polarion.js');
  for (const withReferences of [false, true]) {
    const calls = [], refs = { defaultCommitMessage: { value: 'P:OEM-123 - My base description\nMore detail' } };
    const ids = ['OEM-85585', 'OEM-85586', 'OEM-85587'];
    const sandbox = { refs, state: { repository: { path: '/parent' }, currentPath: 'modules' },
      customPrompt: async (message, value, options) => {
        assert.equal(value, refs.defaultCommitMessage.value); assert.equal(options.multiline, true);
        assert.equal(options.polarionContext.moduleName, 'errm_adpdg');
        // The user may edit the prefilled base; references must preserve that too.
        const edited = value + '\nAdditional module note';
        return withReferences ? polarionCommitMessageWithReferences(edited, ids) : edited;
      }, submoduleCommitReferenceContext: entry => ({ moduleName: entry.name }),
      invoke: async (command, args) => { calls.push({ command, args }); return { push_detail: '' }; },
      status() {}, directoryCache: { clear() {} }, loadRepository: async () => {}, showOperationToast() {}, handleError(error) { throw error; } };
    vm.createContext(sandbox); vm.runInContext(app.match(/async function commitSubmoduleChanges\([^]*?\n}/)[0], sandbox);
    await sandbox.commitSubmoduleChanges({ kind: 'submodule', name: 'errm_adpdg', relative_path: 'modules/errm_adpdg', submodule_current_branch: 'feature' });
    assert.equal(calls.length, 1); assert.equal(calls[0].command, 'commit_submodule');
    const base = refs.defaultCommitMessage.value + '\nAdditional module note';
    assert.equal(calls[0].args.message, withReferences ? base + '\n\n' + ids.join('\n') : base);
    assert.equal(refs.defaultCommitMessage.value, 'P:OEM-123 - My base description\nMore detail');
  }
});
test('an empty top message keeps the submodule commit editable without creating an automatic commit', async () => {
  const calls = [], defaults = [];
  const sandbox = { refs: { defaultCommitMessage: { value: '  ' } }, state: { repository: { path: '/parent' } },
    customPrompt: async (message, value) => { defaults.push(value); return null; }, submoduleCommitReferenceContext: () => null,
    invoke: async command => calls.push(command) };
  vm.createContext(sandbox); vm.runInContext(app.match(/async function commitSubmoduleChanges\([^]*?\n}/)[0], sandbox);
  await sandbox.commitSubmoduleChanges({ kind: 'submodule', name: 'module', relative_path: 'modules/module', submodule_current_branch: 'feature' });
  assert.deepEqual(defaults, ['']); assert.deepEqual(calls, []);
});
