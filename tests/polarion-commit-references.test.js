const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const api = require('../frontend/polarion.js');
const item = (id, title, item_type, status = 'released') => ({ id: 'VWAQ4/' + id, title, item_type, status, kind: 'workitem' });
const elements = [item('VWAQ4-85585', 'errm_adpdg 1.2.1.5_0GH0D', 'simulinkitem'), item('VWAQ4-85586', 'errm_adpdg 2.0.0', 'designelement'),
  item('VWAQ4-85587', 'errm_adpdg 2.0.0', 'solutionelement'), item('VWAQ4-85588', 'errm_dcmng 1.0.0', 'simulinkitem'),
  item('VWAQ4-85591', 'errm_dcmngai 1.0.0', 'simulinkitem')];
const workspace = () => ({ document: { id: 'VWAQ4/Structure Information/ERRM', title: 'VWAQ4-SW_ERRM_StructDoc_2.0.0', kind: 'document', fields: {} }, elements,
  elements_saved_at: '2026-10-18T10:00:00Z', elements_complete: true, elements_revision: '' });

test('module matching is case/separator insensitive but never mistakes another module prefix or an ID for its name', () => {
  assert.deepEqual(api.polarionModuleElements(elements, 'ERRM-ADPDG').map(i => i.id.split('/').pop()), ['VWAQ4-85585','VWAQ4-85586','VWAQ4-85587']);
  assert.equal(api.polarionModuleElements(elements, 'errm_dcmng').length, 1);
  assert.equal(api.polarionModuleElements(elements, 'errm_dcmngai').length, 1);
  assert.equal(api.polarionModuleElements(elements, 'errm').length, 5, 'a real separate name token may match; caller uses the full submodule name');
  assert.equal(api.polarionModuleElements(elements, '').length, 0);
  assert.equal(api.polarionModuleElements([...elements, elements[0]], 'errm_adpdg').length, 3);
  assert.equal(api.polarionModuleElements([item('VWAQ4-1', 'Another module uses errm_adpdg', 'designelement')], 'errm_adpdg').length, 0, 'a mention of a dependency is not a module identity');
});
test('local element filter supports inner fragments, technical types and module names typed with spaces', () => {
  assert.equal(api.polarionFilterElements(elements, 'ERRM ADPDG').length, 3);
  assert.equal(api.polarionFilterElements(elements, '85586').length, 1);
  assert.equal(api.polarionFilterElements(elements, 'adpdg', 'DesignElement').length, 1);
  assert.equal(api.polarionFilterElements(elements, 'does not exist').length, 0);
});
test('only a matching document in the selected project is a local search hit', () => {
  assert.equal(api.polarionSavedDocumentMatches(workspace(), 'VWAQ4', 'errm').length, 1);
  assert.equal(api.polarionSavedDocumentMatches(workspace(), 'Other', 'errm').length, 0);
  assert.equal(api.polarionSavedDocumentMatches(workspace(), 'VWAQ4', 'missing').length, 0);
});
test('reference context exists only for a submodule commit, never a general parent-project message', () => {
  const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
  const state = { repository: { path: '/parent' }, selectedEntry: { kind: 'submodule', name: 'errm_adpdg' } };
  const context = { state, submoduleNameFromPath: value => value.split('/').pop() }; vm.createContext(context);
  vm.runInContext(app.match(/function submoduleCommitReferenceContext\([^]*?\n}/)[0], context);
  assert.equal(context.submoduleCommitReferenceContext(), null);
  assert.equal(context.submoduleCommitReferenceContext({ kind: 'submodule', name: 'errm_adpdg', relative_path: 'modules/errm_adpdg' }).repositoryPath, '/parent');
  state.repository = { path: '/parent/modules/errm_adpdg' };
  state.repositoryOrigin = { repositoryPath: state.repository.path, parentPath: '/parent', submodulePath: 'modules/errm_adpdg', submoduleName: 'errm_adpdg' };
  assert.equal(context.submoduleCommitReferenceContext().repositoryPath, '/parent');
  state.repository.path = '/unrelated'; assert.equal(context.submoduleCommitReferenceContext(), null);
  const commit = app.match(/async function commitSubmoduleChanges\([^]*?\n}/)[0];
  assert.match(commit, /polarionContext: submoduleCommitReferenceContext\(entry\)/);
  assert.doesNotMatch(commit, /recordDefaultCommitMessage|addMessage|alsoPush: true/);
  const drawer = app.match(/function syncDrawerPolarionReferences\([^]*?\n}/)[0];
  assert.match(drawer, /editor: refs\.commitMessage/);
  assert.match(drawer, /setContext\(submoduleCommitReferenceContext\(\)\)/);
  const css = fs.readFileSync(path.join(__dirname, '../frontend/polarion.css'), 'utf8');
  assert.match(css, /\.commit-area:has\(> \.polarion-commit-references:not\(\[hidden\]\)\) \{ min-height: 0; overflow-y: auto/);
});
test('scoped commits show references inside a submodule and in its full repository, never for a parent gitlink bump or normal folder', () => {
  const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
  const state = { repository: { path: '/parent' } };
  const context = { state, submoduleNameFromPath: value => value.split('/').pop(), submoduleBoundaryFor: path => path.startsWith('modules/errm_adpdg') ? 'modules/errm_adpdg' : null };
  vm.createContext(context);
  for (const name of ['submoduleCommitReferenceContext','scopeCommitReferenceContext']) vm.runInContext(app.match(new RegExp('function ' + name + '\\([^]*?\\n}'))[0], context);
  assert.equal(context.scopeCommitReferenceContext({ path: 'src/file.c' }), null);
  assert.equal(context.scopeCommitReferenceContext({ path: 'modules/errm_adpdg' }), null, 'root gitlink is a parent commit, not an inner commit');
  assert.equal(context.scopeCommitReferenceContext({ path: 'modules/errm_adpdg/src/file.c' }).moduleName, 'errm_adpdg');
  state.repository = { path: '/parent/modules/errm_adpdg' };
  state.repositoryOrigin = { repositoryPath: state.repository.path, parentPath: '/parent', submoduleName: 'errm_adpdg' };
  assert.equal(context.scopeCommitReferenceContext({ path: 'src/file.c' }).repositoryPath, '/parent');
  assert.equal(context.scopeCommitReferenceContext({ path: '' }).moduleName, 'errm_adpdg');
  assert.match(app.match(/function openScopeCommit\([^]*?\n}/)[0], /editor: refs\.scopeCommitMessage/);
  assert.match(app.match(/async function commitSelectedScope\([^]*?\n}/)[0], /!scopePolarionReferences\.canSubmit\(\)/);
});
test('opening P forwards the same parent StructDoc source as commit, including an independently selected submodule graph', () => {
  const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
  const state = { repository: { path: '/parent' }, currentPath: 'modules', selectedEntry: { kind: 'submodule', name: 'errm_confi0', relative_path: 'modules/errm_confi0' } };
  let handler, opened, active = { isSubmodule: false, path: '/parent' };
  const context = { state, document: { querySelectorAll: () => [{ dataset: { polarionSearch: 'context' }, addEventListener: (_event, fn) => { handler = fn; } }] },
    submoduleNameFromPath: value => value.split('/').pop(), activeRepositoryContext: () => active, polarionTaskPicker: { open: (...args) => { opened = args; } } };
  vm.createContext(context);
  vm.runInContext(app.match(/function submoduleCommitReferenceContext\([^]*?\n}/)[0], context);
  const start = app.indexOf("document.querySelectorAll('[data-polarion-search]')");
  vm.runInContext(app.slice(start, app.indexOf('\n}));', start) + 5), context);
  handler(); assert.equal(opened[0], '/parent'); assert.equal(opened[1], 'modules/errm_confi0');
  assert.equal(opened[2].repositoryPath, '/parent'); assert.equal(opened[2].moduleName, 'errm_confi0');
  active = { isSubmodule: true, path: '/parent/modules/errm_adpdg', parentPath: '/parent', name: 'errm_adpdg', relativePath: 'modules/errm_adpdg' };
  handler(); assert.equal(opened[0], active.path); assert.equal(opened[1], '');
  assert.equal(opened[2].repositoryPath, '/parent'); assert.equal(opened[2].moduleName, 'errm_adpdg', 'graph context, not an old explorer selection');
});
test('references use short work-item IDs and preserve the base message without adding duplicates', () => {
  assert.equal(api.polarionCommitMessageWithReferences('Checkpoint', ['VWAQ4-85585','VWAQ4-85586','VWAQ4-85585']), 'Checkpoint\n\nVWAQ4-85585\nVWAQ4-85586');
  assert.equal(api.polarionCommitMessageWithReferences('P:VWAQ4-85585 - task', ['VWAQ4-85585', 'VWAQ4/VWAQ4-85586', 'bad']), 'P:VWAQ4-85585 - task');
  assert.equal(api.polarionCommitMessageWithReferences('Keep whitespace  ', []), 'Keep whitespace  ');
  assert.equal(api.polarionCommitMessageWithReferences('P:OEM-42 - Top selected description\nMore detail', ['VWAQ4-85585','VWAQ4-85586','VWAQ4-85587']),
    'P:OEM-42 - Top selected description\nMore detail\n\nVWAQ4-85585\nVWAQ4-85586\nVWAQ4-85587');
});
test('metadata-only selection and changed revisions are distinct from complete or legitimately empty saved elements', () => {
  assert.equal(api.polarionElementsState({}), 'no_document');
  assert.equal(api.polarionElementsState({ ...workspace(), elements_complete: false, elements: [] }), 'not_loaded');
  assert.equal(api.polarionElementsState({ ...workspace(), elements_revision: 'old', document: { ...workspace().document, fields: { revision: 'new' } } }), 'revision_changed');
  assert.equal(api.polarionElementsState(workspace()), 'ready');
  assert.equal(api.polarionElementsState({ ...workspace(), elements: [] }), 'ready');
});
function fixture(knownWorkspace = null, editor = null) {
  class Node {
    constructor() { this.children = []; this.listeners = {}; this.checked = false; this.hidden = false; }
    append(...nodes) { this.children.push(...nodes); }
    replaceChildren() { this.children = []; }
    addEventListener(event, fn) { this.listeners[event] = fn; }
  }
  const calls = [], logs = [], host = new Node(); let changes = 0;
  const context = { document: { createElement: () => new Node() } };
  vm.createContext(context); vm.runInContext(fs.readFileSync(path.join(__dirname, '../frontend/polarion.js'), 'utf8'), context);
  const controller = context.createPolarionCommitReferences({ host, editor, invoke: (command, args) => new Promise((resolve, reject) => calls.push({ command, args, resolve, reject })),
    onChange: () => { changes++; }, log: text => logs.push(text) });
  if (knownWorkspace) context.polarionRememberDocument('/parent', knownWorkspace);
  const toggle = checked => { const box = host.children[0].children[0]; box.checked = checked; box.listeners.change(); };
  controller.setContext({ repositoryPath: '/parent', moduleName: 'errm_adpdg', commitKey: 'modules/errm_adpdg' });
  return { controller, host, calls, logs, toggle, changes: () => changes };
}
const tick = () => new Promise(resolve => setImmediate(resolve));
const content = node => [node.textContent || '', ...(node.children || []).map(content)].join('\n');
function messageEditor(value = 'My base description') {
  const listeners = new Map();
  return { value, addEventListener: (name, fn) => listeners.set(name, fn), removeEventListener: name => listeners.delete(name),
    type(value) { this.value = value; listeners.get('input')?.(); }, listeners };
}
test('selected StructDoc IDs appear in the actual editable commit message, never appended again on submit', async () => {
  const editor = messageEditor('P:OEM-1 - Selected description\nMy module note'), f = fixture(null, editor);
  f.toggle(true); f.calls[0].resolve(workspace()); await tick();
  assert.equal(editor.value, 'P:OEM-1 - Selected description\nMy module note\n\nVWAQ4-85585\nVWAQ4-85586\nVWAQ4-85587');
  assert.equal(f.controller.message(editor.value), editor.value);
  editor.type(editor.value.replace('My module note', 'Edited module note'));
  const list = f.host.children.find(n => n.className === 'polarion-commit-reference-list');
  const box = list.children[1].children[0]; box.checked = false; box.listeners.change();
  assert.match(editor.value, /Edited module note/); assert.doesNotMatch(editor.value, /85586/);
  f.toggle(false); assert.equal(editor.value, 'P:OEM-1 - Selected description\nEdited module note');
  f.controller.dispose(); assert.equal(editor.listeners.size, 0);
});
test('manually edited or removed reference text stays authoritative, including cancel, reset and submit', async () => {
  const editor = messageEditor(), f = fixture(null, editor);
  f.toggle(true); f.calls[0].resolve(workspace()); await tick();
  editor.type(editor.value.replace('VWAQ4-85586', 'OEM-99 - manual note'));
  const manual = editor.value;
  assert.equal(f.controller.message(manual), manual, 'do not restore a removed checked ID at submit');
  f.toggle(false); assert.equal(editor.value, manual, 'uncheck must not erase user edits inside the former inserted block');
  f.controller.reset(); assert.equal(editor.value, manual);
});
test('context changes remove only untouched generated references, and stale local reads cannot inject into another module draft', async () => {
  const editor = messageEditor(), f = fixture(null, editor);
  f.toggle(true); f.calls[0].resolve(workspace()); await tick();
  f.controller.setContext(null); assert.equal(editor.value, 'My base description');
  f.controller.setContext({ repositoryPath: '/parent', moduleName: 'errm_adpdg' }); f.toggle(true);
  f.toggle(false); f.calls[1].resolve(workspace()); await tick();
  assert.equal(editor.value, 'My base description');
});
test('unchecking restores original trailing whitespace and preserves notes typed after generated references', async () => {
  const editor = messageEditor('Base  \n'), f = fixture(null, editor);
  f.toggle(true); f.calls[0].resolve(workspace()); await tick();
  f.toggle(false); assert.equal(editor.value, 'Base  \n');
  editor.type('Base'); f.toggle(true); f.calls[1].resolve(workspace()); await tick();
  editor.type(editor.value + '\nAdditional manual note');
  f.toggle(false); assert.equal(editor.value, 'Base\nAdditional manual note');
});
test('commit shows the remembered selected StructDoc without IO, then verifies the source and previews IDs on opt-in', async () => {
  const f = fixture(workspace());
  assert.match(content(f.host), /StructDoc: VWAQ4-SW_ERRM_StructDoc_2.0.0/);
  assert.match(content(f.host), /VWAQ4\/Structure Information\/ERRM.*remembered selection/);
  assert.equal(f.calls.length, 0);
  f.toggle(true);
  f.calls[0].resolve({ ...workspace(), document: { ...workspace().document, id: 'P/S/New', title: 'New selected document' } }); await tick();
  assert.match(content(f.host), /StructDoc: New selected document/);
  assert.doesNotMatch(content(f.host), /StructDoc: VWAQ4-SW_ERRM/);
  assert.match(content(f.host), /P\/S\/New.*saved source verified/);
  assert.match(content(f.host), /3 selected \/ 3 matching elements/);
  assert.match(content(f.host), /Saved snapshot: 2026-10-18T10:00:00Z/);
  const list = f.host.children.find(node => node.className === 'polarion-commit-reference-list');
  const choice = list.children[0].children[0]; choice.checked = false; choice.listeners.change();
  assert.match(content(f.host), /2 selected \/ 3 matching elements/);
  const preview = f.host.children.find(node => node.className === 'polarion-commit-preview');
  assert.doesNotMatch(content(preview), /VWAQ4-85585/);
  assert.equal(f.calls.length, 1, 'changing element checkboxes does not read local files or the server');
});
test('four unique matching types use the exact IDs from any project, not a fixed set of three or a VWAQ4 prefix', async () => {
  const f = fixture(); f.toggle(true);
  const data = { ...workspace(), document: { id: 'OEM/S/StructDoc', kind: 'document', fields: {} },
    elements: ['simulinkitem', 'designelement', 'solutionelement', 'verificationelement'].map((type, index) => ({
      ...item('unused', 'ERRM-ADPDG 2.0.0', type), id: 'OEM/OEM_PLATFORM-AQ4-' + (index + 100),
    })) };
  f.calls[0].resolve(data); await tick();
  assert.match(content(f.host), /StructDoc: OEM\/S\/StructDoc/);
  assert.match(content(f.host), /4 selected \/ 4 matching elements/);
  assert.equal(f.controller.message('My description'), 'My description\n\nOEM_PLATFORM-AQ4-100\nOEM_PLATFORM-AQ4-101\nOEM_PLATFORM-AQ4-102\nOEM_PLATFORM-AQ4-103');
  assert.equal(f.calls.length, 1);
});
test('mounting never searches; ticking loads only the saved parent snapshot and applies three references to this message', async () => {
  const f = fixture(); assert.equal(f.calls.length, 0); assert.equal(f.controller.message('Base'), 'Base');
  f.toggle(true); assert.equal(f.calls.length, 1); assert.equal(f.calls[0].command, 'polarion_workspace');
  assert.equal(f.calls[0].args.repositoryPath, '/parent'); assert.equal(f.calls[0].args.action, 'load');
  assert.equal(f.controller.canSubmit(), false); assert.throws(() => f.controller.message('Base'), /not ready/);
  f.calls[0].resolve(workspace()); await tick();
  assert.equal(f.controller.canSubmit(), true);
  assert.equal(f.controller.message('Base'), 'Base\n\nVWAQ4-85585\nVWAQ4-85586\nVWAQ4-85587');
  f.controller.setContext({ repositoryPath: '/parent', moduleName: 'errm_adpdg', commitKey: 'modules/errm_adpdg' });
  assert.equal(f.calls.length, 1, 'ordinary re-render must not reload or reset the choice');
  f.toggle(false); assert.equal(f.controller.message('Base'), 'Base');
  assert.doesNotMatch(f.logs.join('\n'), /VWAQ4|errm_adpdg|85585/);
});
test('ambiguous variants of the same type are not automatically included; obsolete status stays visible', async () => {
  const f = fixture(); f.toggle(true);
  f.calls[0].resolve({ ...workspace(), elements: [...elements, item('VWAQ4-999', 'errm_adpdg 3.0', 'designelement', 'obsolete')] }); await tick();
  assert.equal(f.controller.message('Base'), 'Base\n\nVWAQ4-85585\nVWAQ4-85587');
  const list = f.host.children.find(node => node.className === 'polarion-commit-reference-list');
  assert.ok(list.children.some(row => row.children[2].textContent.includes('obsolete')));
  const extra = list.children.at(-1).children[0]; extra.checked = true; extra.listeners.change();
  assert.match(f.controller.message('Base'), /VWAQ4-999/);
});
test('missing/outdated snapshots and lookup failures never silently produce a commit with missing requested references', async () => {
  for (const data of [{}, { ...workspace(), elements_complete: false }, { ...workspace(), elements_revision: 'old', document: { ...workspace().document, fields: { revision: 'new' } } }]) {
    const f = fixture(); f.toggle(true); f.calls[0].resolve(data); await tick();
    assert.equal(f.controller.canSubmit(), false); assert.throws(() => f.controller.message('Base'), /not ready/);
    f.toggle(false); assert.equal(f.controller.canSubmit(), true); assert.equal(f.controller.message('Base'), 'Base');
    assert.equal(f.calls.length, 1, 'no implicit server reads or authentication');
  }
  const f = fixture(); f.toggle(true); f.calls[0].reject(new Error('Read failed')); await tick();
  assert.equal(f.controller.canSubmit(), false);
});
test('diagnostics distinguish an unloaded document from a name mismatch without logging IDs or document contents', async () => {
  const f = fixture(); f.toggle(true); f.calls[0].resolve({ ...workspace(), elements_complete: false, elements: [] }); await tick();
  assert.match(content(f.host), /document is selected, but its elements have not been loaded/);
  assert.match(f.logs.join('\n'), /reason=not_loaded/);
  f.controller.setContext({ repositoryPath: '/parent', moduleName: 'errm_confi0' }); f.toggle(true);
  f.calls[1].resolve(workspace()); await tick();
  assert.match(content(f.host), /No saved element title matches the module name errm_confi0/);
  assert.equal(f.controller.message('Base description'), 'Base description', 'never reuse adpdg references for confi0');
  assert.match(f.logs.join('\n'), /total=5 missing_details=0 candidates=0 selected=0/);
  assert.doesNotMatch(f.logs.join('\n'), /VWAQ4|errm|85585|Base description/);
});
test('no match means no invented references; cancel/context changes reject late responses and clear selections after commit', async () => {
  const f = fixture(); f.toggle(true); f.calls[0].resolve({ ...workspace(), elements: [] }); await tick();
  assert.equal(f.controller.canSubmit(), true); assert.equal(f.controller.message('Base'), 'Base');
  f.toggle(true); f.toggle(false); f.calls[1].resolve(workspace()); await tick();
  assert.equal(f.controller.message('Base'), 'Base');
  f.toggle(true); f.controller.setContext(null); f.calls[2].resolve(workspace()); await tick();
  assert.equal(f.host.hidden, true); assert.equal(f.controller.message('Parent commit'), 'Parent commit');
  f.controller.setContext({ repositoryPath: '/parent', moduleName: 'errm_adpdg' }); f.toggle(true); f.calls[3].resolve(workspace()); await tick();
  f.controller.reset(); assert.equal(f.controller.message('Next commit'), 'Next commit');
  f.toggle(true); f.controller.dispose(); f.calls[4].resolve(workspace()); await tick();
  assert.equal(f.host.hidden, true); assert.equal(f.host.children.length, 0);
});
