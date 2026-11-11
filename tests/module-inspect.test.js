const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const main = { kind: 'branch', name: 'main', revision: 'a'.repeat(40) };
const release = { kind: 'tag', name: 'release', revision: 'b'.repeat(40) };
function node() {
  return { dataset: {}, value: '', parentElement: {}, classList: { toggle() {} }, style: { setProperty() {} },
    addEventListener(name, callback) { this[name] = callback; }, querySelectorAll() { return []; },
    append() {}, setAttribute() {}, focus() {} };
}
function harness() {
  const nodes = new Map();
  const find = key => { if (!nodes.has(key)) nodes.set(key, node()); return nodes.get(key); };
  const tabs = ['branch', 'tag', 'history'].map(tab => Object.assign(find(tab), { dataset: { inspectTab: tab } }));
  const dialog = { ...node(), querySelector: find, querySelectorAll: key => key === '[data-inspect-tab]' ? tabs : [],
    addEventListener() {}, showModal() { this.open = true; }, close() { this.open = false; } };
  const ctx = vm.createContext({ document: { createElement: node }, LANE_WIDTH: 30, drawGraphOverlay() {} });
  for (const file of ['graph-model.js', 'submodule-versions.js', 'module-inspect.js']) {
    vm.runInContext(fs.readFileSync(path.join(__dirname, '../frontend', file), 'utf8'), ctx);
  }
  const calls = [], used = [], pairs = [];
  const controller = ctx.createModuleInspector(dialog, {
    readHistory: args => new Promise((resolve, reject) => calls.push({ args, resolve, reject })),
    useRevision: ref => used.push(ref), compare: refs => pairs.push(refs),
  });
  const source = { repo: { name: 'module', full_name: 'eng/module', owner: 'eng', default_branch: 'main' },
    repositoryPath: '/parent', valid: () => true, ready: () => true, loading: () => false, versions: () => [main, release] };
  controller.open(source);
  const choose = (ref, extend = false) => {
    const row = { dataset: { revision: ref.revision, versionKind: ref.kind, name: ref.name } };
    find('#inspectRows').click({ target: { closest: key => key === '[data-revision]' ? row : null }, metaKey: extend });
  };
  return { controller, find, calls, used, pairs, source, choose };
}
const settle = () => new Promise(resolve => setImmediate(resolve));
const response = (id, subject, more = false) => ({ commits: [{ id, subject, parents: [], author: 'Name', date: '2026-11-11' }], has_more: more });

test('Inspect opens and filters loaded refs without I/O, supports cross-tab pairs, and Use returns the exact ref', () => {
  const { controller, find, calls, used, pairs, source, choose } = harness();
  assert.match(find('#inspectRows').innerHTML, /Default branch/);
  assert.doesNotMatch(find('#inspectRows').innerHTML, /data-switch-version|no upstream|data-reset/);
  choose(main); find('tag').click(); choose(release, true);
  assert.equal(find('#inspectUse').disabled, true);
  find('#inspectCompare').click();
  assert.deepEqual(Array.from(pairs[0]), [main, release]);
  controller.open(source); find('tag').click();
  find('#inspectSearch').value = 'missing'; find('#inspectSearch').input();
  assert.doesNotMatch(find('#inspectRows').innerHTML, /data-revision=/);
  find('#inspectSearch').value = ''; find('#inspectSearch').input(); choose(release);
  find('#inspectUse').click();
  assert.deepEqual(used, [release]); assert.equal(calls.length, 0);
});

test('Inspect pins and pages history on demand, graph includes all advertised tips, and late responses cannot cross sessions', async () => {
  const { controller, find, calls, source, choose } = harness();
  find('tag').click(); choose(release); find('history').click();
  assert.deepEqual(Array.from(calls[0].args.revisions), [release.revision]);
  find('branch').click(); find('history').click(); assert.equal(calls.length, 1);
  calls[0].resolve(response(release.revision, 'Release notes', true)); await settle();
  assert.match(find('#inspectRows').innerHTML, /Release notes/);
  find('#inspectMore').click(); assert.equal(calls[1].args.offset, 1);
  find('#inspectGraph').click();
  assert.deepEqual(Array.from(calls[2].args.revisions), [main.revision, release.revision]);
  calls[1].reject(new Error('older history unavailable')); await settle();
  assert.doesNotMatch(find('#inspectRows').innerHTML, /older history unavailable/);
  controller.close(); controller.open({ ...source, repo: { ...source.repo, name: 'other' } });
  calls[2].resolve(response(main.revision, 'From previous repository')); await settle();
  assert.doesNotMatch(find('#inspectRows').innerHTML, /From previous repository/);
  find('history').click(); assert.deepEqual(Array.from(calls[3].args.revisions), [main.revision]);
  calls[3].reject(new Error('credentials unavailable')); await settle();
  assert.equal(find('#inspectMore').hidden, false); find('#inspectMore').click();
  assert.equal(calls[4].args.repositoryName, 'other'); assert.equal(calls[4].args.offset, 0);
});
