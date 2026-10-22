'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const versions = require('../frontend/submodule-versions.js');
const app = fs.readFileSync(require.resolve('../frontend/app.js'), 'utf8');
const section = (a, b) => app.slice(app.indexOf(a), app.indexOf(b, app.indexOf(a)));

test('version selection toggles unique SHAs, keeps order and never selects over two', () => {
  const select = versions.selectSubmoduleCompareRevision;
  assert.deepEqual(select([], 'old'), ['old']);
  assert.deepEqual(select(['old'], 'new', true), ['old', 'new']);
  assert.deepEqual(select(['old', 'new'], 'old', true), ['new']);
  assert.deepEqual(select(['old', 'new'], 'third', true), ['new', 'third']);
  assert.deepEqual(select(['old', 'new'], 'third'), ['third']);
  assert.deepEqual(select(['old'], 'old', true), []);
});

test('full commit message, SHA and metadata are escaped on hover without expanding rows', () => {
  const html = versions.submoduleVersionRowHtml({ kind: 'tag', name: 'release', revision: '0123456789abcdef', subject: 'Subject', message: 'Subject\n\nBody <script> & "detail"', author: 'Author', date: '2026-10-22' });
  assert.match(html, /title="0123456789abcdef\nAuthor · 2026-10-22\nSubject\n\nBody &lt;script&gt; &amp; &quot;detail&quot;"/);
  assert.match(html, /class="version-subject">Subject<\/span>/);
  assert.doesNotMatch(html, /<script>/);
});

function harness() {
  const nodes = { '#compareSelectedVersions': {}, '#submoduleVersionSelection': {} }, calls = [];
  const rows = ['old', 'new', 'other'].map(revision => ({ dataset: { revision, name: revision }, selected: false, listeners: {},
    addEventListener(name, callback) { this.listeners[name] = callback; }, closest: () => null,
    classList: { toggle(name, value) { rows.find(row => row.dataset.revision === revision).selected = value; } }, setAttribute() {} }));
  const context = { ...versions, state: { repository: { path: 'parent' } }, submoduleMenuRepositoryPath: 'parent',
    submoduleMenuEntry: { kind: 'submodule', name: 'dep', relative_path: 'vendor/dep' },
    submoduleMenuData: { versions: [{ revision: 'old' }, { revision: 'new' }] }, submoduleCompareSelection: ['old', 'new'],
    refs: { submoduleMenu: { hidden: false }, submoduleVersions: { querySelectorAll: () => rows } },
    $: key => nodes[key], document: { querySelector: () => ({ setAttribute() {} }) },
    showFloatingMenu: (_event, items) => { context.menu = items; },
    closeSubmoduleMenu: () => { context.refs.submoduleMenu.hidden = true; context.submoduleMenuData = null; context.submoduleCompareSelection = []; },
    openSubmoduleCompareFromEntry: (...args) => calls.push(args),
  };
  vm.createContext(context);
  vm.runInContext(section('function bindSubmoduleVersionSelection()', 'function placeSubmoduleMenu('), context);
  return { context, calls, rows, nodes };
}

test('Ctrl/Cmd-click and keyboard select only; embedded actions do not alter the pair', () => {
  const { context: c, calls, rows } = harness();
  c.submoduleCompareSelection = [];
  c.bindSubmoduleVersionSelection();
  const event = (row, extra = {}) => ({ target: row, preventDefault() {}, stopPropagation() {}, ...extra });
  rows[0].listeners.click(event(rows[0]));
  rows[1].listeners.click(event(rows[1], { ctrlKey: true }));
  assert.deepEqual([...c.submoduleCompareSelection], ['old', 'new']);
  rows[1].listeners.click(event(rows[1], { metaKey: true }));
  assert.deepEqual([...c.submoduleCompareSelection], ['old']);
  rows[2].listeners.keydown(event(rows[2], { key: ' ', metaKey: true }));
  assert.deepEqual([...c.submoduleCompareSelection], ['old', 'other']);
  const button = { closest: () => true };
  rows[1].listeners.click(event(button, { ctrlKey: true }));
  rows[1].listeners.keydown(event(button, { key: 'Enter' }));
  assert.deepEqual([...c.submoduleCompareSelection], ['old', 'other']);
  assert.equal(calls.length, 0, 'selection alone does not compare or checkout');
});

test('two selected versions reuse the existing read-only compare with cached version data', () => {
  const { context: c, calls, rows, nodes } = harness();
  const data = c.submoduleMenuData;
  c.syncSubmoduleCompareSelection();
  assert.deepEqual(rows.map(r => r.selected), [true, true, false]);
  assert.equal(nodes['#compareSelectedVersions'].disabled, false);
  c.showSubmoduleVersionCompareMenu({}, 'new');
  c.menu[0].run();
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0].relative_path, 'vendor/dep');
  assert.equal(calls[0][1].leftRef, 'old');
  assert.equal(calls[0][1].rightRef, 'new');
  assert.equal(calls[0][1].versionsData, data);
  assert.equal(c.refs.submoduleMenu.hidden, true);
});

test('right-click on an unselected row clears the previous pair and disables Compare', () => {
  const { context: c, calls, nodes } = harness();
  c.showSubmoduleVersionCompareMenu({}, 'other');
  assert.equal(c.menu[0].disabled, true);
  assert.equal(nodes['#compareSelectedVersions'].disabled, true);
  c.compareSelectedSubmoduleVersions();
  assert.equal(calls.length, 0);
});

test('footer Compare cannot reuse versions from a different repository', () => {
  const { context: c, calls } = harness();
  c.state.repository.path = 'another-parent';
  c.compareSelectedSubmoduleVersions();
  assert.equal(calls.length, 0);
});

for (const change of ['repository', 'data', 'selection', 'closed']) test(`stale context action is rejected after ${change} changes`, () => {
  const { context: c, calls } = harness();
  c.showSubmoduleVersionCompareMenu({}, 'old');
  if (change === 'repository') c.state.repository.path = 'other';
  if (change === 'data') c.submoduleMenuData = { versions: [] };
  if (change === 'selection') c.submoduleCompareSelection = ['old', 'other'];
  if (change === 'closed') c.closeSubmoduleMenu();
  c.menu[0].run();
  assert.equal(calls.length, 0);
});

test('drill-down and list show both exact gitlink SHAs and preserve missing sides', () => {
  const c = { esc: String, iconFor: () => '', formatSize: String };
  vm.createContext(c);
  vm.runInContext(section('function commanderSide(', 'function flatCompareRowHtml('), c);
  const old = { kind: 'submodule', name: 'dep', relative_path: 'vendor/dep', revision: '0123456789abcdef' };
  const next = { ...old, revision: 'fedcba9876543210' };
  for (const render of [c.commanderSide, c.flatCompareSide]) {
    assert.match(render(old, 'local'), /title="0123456789abcdef"/);
    assert.match(render(old, 'local'), /submodule · 01234567/);
    assert.match(render(next, 'remote'), /submodule · fedcba98/);
    assert.match(render(null, 'local'), /not present|not on left/);
    assert.match(render(null, 'remote'), /not present|not on right/);
  }
});
