'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const api = require('../frontend/submodule-compare-details.js');
const app = fs.readFileSync(require.resolve('../frontend/app.js'), 'utf8');
const source = name => app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))[0];
const clone = value => JSON.parse(JSON.stringify(value));
function element() {
  return { handlers: {}, open: false, innerHTML: '', disabled: false,
    addEventListener(name, fn) { this.handlers[name] = fn; },
    showModal() { this.open = true; },
    close() { this.open = false; this.handlers.close?.(); },
    querySelectorAll() { return []; },
  };
}
const details = id => ({ repository_path: '/parent', relative_path: 'modules/dep', local_path: '/parent/modules/dep',
  compare_repository_path: '/parent/modules/dep', compare_note: 'Read only',
  left: { kind: 'submodule', revision: `${id}-old`, message: 'Old <message>', source: 'old-parent', tags: ['v1'], branches: [] },
  right: { kind: 'submodule', revision: `${id}-new`, message: 'New\n\nFull body', source: 'new-parent', tags: [], branches: ['feature'] },
});
test('details show separate snapshots, full messages, tags, URLs and safe text', () => {
  const data = details('pair'); data.right.configured_url = 'https://host/other.git';
  const html = api.detailsHtml(data);
  for (const text of ['pair-old', 'pair-new', 'New\n\nFull body', 'v1', 'feature', 'https://host/other.git', '/parent/modules/dep']) assert.ok(html.includes(text), text);
  assert.match(html, /Old &lt;message&gt;/);
  assert.match(html, /current local references, not historical/);
  data.right = { kind: 'absent', source: 'after' };
  assert.match(api.detailsHtml(data), /Not present in this version/);
});
test('submodule rows open details only on double click or Enter, not first click', () => {
  const node = element(); let count = 0;
  api.bindRow(node, () => count++);
  assert.equal(node.handlers.click, undefined);
  node.handlers.dblclick(); node.handlers.keydown({ key: 'Enter', preventDefault() {} });
  node.handlers.keydown({ key: ' ' });
  assert.equal(count, 2);
  for (const [start, end] of [['function renderCommander()', 'let submoduleDetailsPanel'], ['function renderSubmoduleCompare()', 'const openCommanderDirectoryGuard']]) {
    const render = app.slice(app.indexOf(start), app.indexOf(end, app.indexOf(start)));
    assert.match(render, /SubmoduleCompareDetails.bindRow\(rowNode, \(\) => openSubmoduleComparisonDetails\(row/);
  }
});
function harness() {
  const dialog = element(), body = element(), compareButton = element(), pending = [], comparisons = [];
  const controller = api.create({ dialog, body, compareButton,
    load: args => new Promise((resolve, reject) => pending.push({ args, resolve, reject })),
    onCompare: async (data, active) => { if (active()) comparisons.push(data); },
  });
  return { dialog, body, compareButton, pending, comparisons, controller };
}
test('late responses, errors and closed dialogs cannot replace current details', async () => {
  const h = harness();
  const first = h.controller.open({ leftRef: 'A' });
  assert.equal(h.compareButton.disabled, true);
  const next = h.controller.open({ leftRef: 'B' });
  h.pending[1].resolve(details('current')); await next;
  h.pending[0].resolve(details('stale')); await first;
  assert.match(h.body.innerHTML, /current-old/); assert.doesNotMatch(h.body.innerHTML, /stale/);
  await h.compareButton.handlers.click();
  assert.equal(h.comparisons[0].left.revision, 'current-old');
  assert.equal(h.dialog.open, false);
  const closed = h.controller.open({ leftRef: 'closed' }); h.dialog.close();
  h.pending[2].resolve(details('closed')); await closed;
  assert.equal(h.compareButton.disabled, true);
  const failed = h.controller.open({}); h.pending[3].reject('Object <missing>'); await failed;
  assert.match(h.body.innerHTML, /Object &lt;missing&gt;/); assert.equal(h.compareButton.disabled, true);
});
test('changed comparison and unavailable objects disable contents comparison', async () => {
  const h = harness(); let current = true;
  const pending = h.controller.open({}, () => current); current = false;
  h.pending[0].resolve(details('old')); await pending;
  assert.match(h.body.innerHTML, /comparison changed/);
  assert.equal(h.compareButton.disabled, true);
  const next = h.controller.open({});
  const unavailable = details('missing'); unavailable.compare_repository_path = null;
  h.pending[1].resolve(unavailable); await next;
  assert.equal(h.compareButton.disabled, true);
  await h.compareButton.handlers.click(); assert.equal(h.comparisons.length, 0);
});
test('integration pins displayed parent commits and opens contents in the exact child repository', async () => {
  const calls = [], nodes = {}, state = { repository: { path: '/parent' }, comparisonContext: {}, comparisonSession: 2, view: 'commander',
    commanderComparisonResult: { left_revision: 'parent-A', right_revision: 'parent-B', remote_revision: 'parent-remote' } };
  const c = { state, submoduleDetailsPanel: null, invoke: true, SubmoduleCompareDetails: { create: options => {
    c.options = options; return { open: (args, current) => { calls.push(clone(args)); c.current = current; } };
  } }, $: id => nodes[id] ||= { open: true }, commanderComparisonSpec: () => ({ repositoryPath: '/parent', workspace: false }),
    ensureComparisonContext: async target => { c.target = target; }, openRepositoryRevisionCompare: (...args) => { c.compared = args; } };
  vm.createContext(c); vm.runInContext(source('openSubmoduleComparisonDetails'), c);
  c.openSubmoduleComparisonDetails({ relative_path: 'modules/dep', local: { revision: 'child-old' } });
  assert.deepEqual(calls[0], { repositoryPath: '/parent', relativePath: 'modules/dep', leftRef: 'parent-A', rightRef: 'parent-B', workspaceRevision: null });
  await c.options.onCompare(details('child'), () => true);
  assert.equal(c.compared[0], 'child-old'); assert.equal(c.compared[1], 'child-new');
  assert.equal(c.compared[4].repositoryPath, '/parent/modules/dep'); assert.equal(state.repository.path, '/parent');
  assert.equal(c.compared[4].integration,undefined,'workspace/non-immutable sources do not claim parent authorship');
  const pinned=details('child'); pinned.left.source='a'.repeat(40); pinned.right.source='b'.repeat(40);
  await c.options.onCompare(pinned,()=>true);
  assert.deepEqual(clone(c.compared[4].integration),{repositoryPath:'/parent',relativePath:'modules/dep',leftRevision:'a'.repeat(40),rightRevision:'b'.repeat(40),childLeft:'child-old',childRight:'child-new'});
  c.commanderComparisonSpec = () => ({ repositoryPath: '/parent', workspace: true });
  c.openSubmoduleComparisonDetails({ relative_path: 'modules/dep', local: { revision: 'child-local' } });
  assert.equal(calls.at(-1).leftRef, null); assert.equal(calls.at(-1).rightRef, 'parent-remote');
  assert.equal(calls.at(-1).workspaceRevision, 'child-local');
  state.compareMode = 'submodule';
  state.submoduleCompare = { repositoryPath: '/parent/modules/dep', leftRevision: 'child-old', rightRevision: 'child-new' };
  c.openSubmoduleComparisonDetails({ relative_path: 'nested', local: { revision: 'grandchild' } }, true);
  assert.equal(calls.at(-1).repositoryPath, '/parent/modules/dep');
  assert.equal(calls.at(-1).leftRef, 'child-old'); assert.equal(c.current(), true);
  state.submoduleCompare.loading = {}; assert.equal(c.current(), false);
});
test('closing or replacing a details request during context loading cancels navigation', async () => {
  let resolve, active = true, navigated = false;
  const c = { state: { repository: { path: '/parent' }, view: 'commander', commanderComparisonResult: { left_revision: 'A', right_revision: 'B' } },
    submoduleDetailsPanel: null, invoke: true, commanderComparisonSpec: () => ({ repositoryPath: '/parent' }), $: () => ({}),
    SubmoduleCompareDetails: { create: options => { c.options = options; return { open() {} }; } },
    ensureComparisonContext: () => new Promise(r => { resolve = r; }), openRepositoryRevisionCompare: () => { navigated = true; } };
  vm.createContext(c); vm.runInContext(source('openSubmoduleComparisonDetails'), c);
  c.openSubmoduleComparisonDetails({ relative_path: 'dep' });
  const pending = c.options.onCompare(details('child'), () => active);
  active = false; resolve(); await pending;
  assert.equal(navigated, false);
});
