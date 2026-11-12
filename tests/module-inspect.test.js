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
  const tabs = ['branch', 'tag', 'commit', 'complete'].map(tab => Object.assign(find(tab), { dataset: { versionFilter: tab } }));
  find('.version-history-picker').querySelector = () => find('select');
  const dialog = { ...node(), querySelector: find, querySelectorAll: key => key === '[data-version-filter]' ? tabs : [],
    addEventListener() {}, showModal() { this.open = true; }, close() { this.open = false; } };
  const ctx = vm.createContext({ document: { createElement: node }, LANE_WIDTH: 30, drawGraphOverlay() {} });
  for (const file of ['graph-model.js', 'submodule-versions.js', 'module-inspect.js']) {
    vm.runInContext(fs.readFileSync(path.join(__dirname, '../frontend', file), 'utf8'), ctx);
  }
  const calls = [], used = [], pairs = [], detailCalls = [];
  const controller = ctx.createModuleInspector(dialog, {
    readHistory: args => new Promise((resolve, reject) => calls.push({ args, resolve, reject })),
    readDetails: args => new Promise((resolve, reject) => detailCalls.push({ args, resolve, reject })),
    useRevision: ref => used.push(ref), compare: refs => pairs.push(refs),
  });
  const source = { repo: { name: 'module', full_name: 'eng/module', owner: 'eng', default_branch: 'main' },
    repositoryPath: '/parent', valid: () => true, ready: () => true, loading: () => false, versions: () => [main, release] };
  controller.open(source);
  const choose = (ref, extend = false) => {
    const row = { dataset: { revision: ref.revision, versionKind: ref.kind, name: ref.name } };
    find('.version-list').click({ target: { closest: key => key === '[data-revision]' ? row : null }, metaKey: extend });
  };
  return { controller, find, calls, used, pairs, source, choose, detailCalls };
}
const settle = () => new Promise(resolve => setImmediate(resolve));
const response = (id, subject, more = false) => ({ commits: [{ id, subject, parents: [], author: 'Name', date: '2026-11-11' }], has_more: more });

test('Inspect opens and filters loaded refs without I/O, supports cross-tab pairs, and Use returns the exact ref', () => {
  const { controller, find, calls, used, pairs, source, choose } = harness();
  assert.match(find('.version-list').innerHTML, /Default branch/);
  assert.doesNotMatch(find('.version-list').innerHTML, /data-switch-version|no upstream|data-reset/);
  choose(main); find('tag').click(); choose(release, true);
  assert.equal(find('#inspectUse').disabled, true);
  find('#inspectCompare').click();
  assert.deepEqual(Array.from(pairs[0]), [main, release]);
  controller.open(source); find('tag').click();
  find('.version-search').value = 'missing'; find('.version-search').input();
  assert.doesNotMatch(find('.version-list').innerHTML, /data-revision=/);
  find('.version-search').value = ''; find('.version-search').input(); choose(release);
  find('#inspectUse').click();
  assert.deepEqual(used, [release]); assert.equal(calls.length, 0);
});

test('Inspect pins and pages history on demand, graph includes all advertised tips, and late responses cannot cross sessions', async () => {
  const { controller, find, calls, source, choose } = harness();
  find('tag').click(); choose(release); find('commit').click();
  assert.deepEqual(Array.from(calls[0].args.revisions), [release.revision]);
  find('branch').click(); find('commit').click(); assert.equal(calls.length, 1);
  calls[0].resolve(response(release.revision, 'Release notes', true)); await settle();
  assert.match(find('.version-list').innerHTML, /Release notes/);
  find('#inspectMore').click(); assert.equal(calls[1].args.offset, 1);
  find('[data-version-map]').click();
  assert.deepEqual(Array.from(calls[2].args.revisions), [main.revision, release.revision]);
  calls[1].reject(new Error('older history unavailable')); await settle();
  assert.doesNotMatch(find('.version-list').innerHTML, /older history unavailable/);
  controller.close(); controller.open({ ...source, repo: { ...source.repo, name: 'other' } });
  calls[2].resolve(response(main.revision, 'From previous repository')); await settle();
  assert.doesNotMatch(find('.version-list').innerHTML, /From previous repository/);
  find('commit').click(); assert.deepEqual(Array.from(calls[3].args.revisions), [main.revision]);
  calls[3].reject(new Error('credentials unavailable')); await settle();
  assert.equal(find('#inspectMore').hidden, false); find('#inspectMore').click();
  assert.equal(calls[4].args.repositoryName, 'other'); assert.equal(calls[4].args.offset, 0);
});

test('tag/branch links reveal the exact named ref and keep a comparison pair across tabs', () => {
  const { controller, source, find, choose, calls, pairs } = harness();
  const alias = { ...release, name: 'v/also', revision: main.revision };
  const secondTag = { ...alias, name: 'v&special' };
  controller.open({ ...source, versions: () => [main, alias, secondTag, release] });
  const rows = find('.version-list');
  let focused = 0, scrolled = 0;
  rows.querySelector = () => ({ focus() { focused++; }, scrollIntoView() { scrolled++; } });
  const follow = ref => rows.click({ target: { closest: key => key === '[data-related-version]'
    ? { dataset: { relatedKind: ref.kind, relatedName: ref.name } } : null } });
  assert.match(rows.innerHTML, /Tags · 2/);
  assert.match(rows.innerHTML, /data-related-name="v&amp;special"/);
  find('.version-search').value = 'main'; find('.version-search').input();
  follow(secondTag);
  assert.equal(find('.version-search').value, '');
  assert.match(rows.innerHTML, /version-cross-selected[^>]*data-version-kind="tag" data-name="v&amp;special"/);
  assert.match(rows.innerHTML, /data-related-kind="branch" data-related-name="main"/);
  choose(release, true); follow(main);
  assert.equal(focused, 2); assert.equal(scrolled, 2);
  find('#inspectCompare').click();
  assert.deepEqual(Array.from(pairs[0]), [secondTag, release]); assert.equal(calls.length, 0);
});

test('switching history branches discards late display updates, reuses aliases and keeps tags selectable', async () => {
  const { controller, source, find, calls, choose } = harness();
  const side = { ...main, name: 'feature/side', revision: 'c'.repeat(40) };
  const alias = { ...side, name: 'side-alias' };
  controller.open({ ...source, versions: () => [main, side, alias, release] });
  assert.equal(find('#inspectUse').disabled, true);
  find('tag').click(); choose(release);
  find('commit').click();
  const select = find('select');
  assert.match(select.innerHTML, /Tag · release/);
  select.selectedIndex = 2; select.change(); // selected tag, main, side, alias
  assert.equal(calls[1].args.revisions[0], side.revision);
  calls[0].resolve(response(release.revision, 'Old tag')); await settle();
  assert.doesNotMatch(find('.version-list').innerHTML, /Old tag/);
  calls[1].resolve(response(side.revision, 'Side history')); await settle();
  const aliasIndex = 2; // the picker now contains only main, side and side-alias
  select.selectedIndex = aliasIndex; select.change();
  assert.equal(calls.length, 2); assert.match(find('#inspectScope').textContent, /side-alias/);
  assert.match(find('.version-list').innerHTML, /Side history/);
});

test('tip details are one explicit batch and preserve exact-SHA links and searchable metadata', async () => {
  const { controller, source, find, detailCalls, calls } = harness();
  controller.open({ ...source, versions: () => [main, { ...main, kind: 'tag', name: 'same-commit' }, release] });
  assert.equal(detailCalls.length, 0);
  find('#inspectDetails').click();
  assert.deepEqual(Array.from(detailCalls[0].args.revisions), [main.revision, release.revision]);
  detailCalls[0].resolve([{ id: main.revision, subject: 'Branch message', message: 'Body', author: 'Robin', date: '2026-11-10' }]);
  await settle();
  find('.version-search').value = 'Robin'; find('.version-search').input();
  assert.match(find('.version-list').innerHTML, /Branch message/);
  assert.match(find('.version-list').innerHTML, /data-related-name="same-commit"/);
  assert.equal(calls.length, 0);
});
