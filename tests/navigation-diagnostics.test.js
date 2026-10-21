const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
function source(name) {
  const code = app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))?.[0];
  assert.ok(code, name); return code;
}
function clock(context = {}) {
  let now = 1700000000000;
  class TestDate extends Date { static now() { return now; } }
  Object.assign(context, { Date: TestDate });
  vm.createContext(context);
  vm.runInContext('let previousNavigationAt = null;\n' + source('navigationTraceFields'), context);
  return { context, advance: delta => { now += delta; } };
}
test('navigation diagnostic measures wall-clock gaps without timers, refreshes or a first-call false positive', () => {
  const f = clock();
  assert.match(f.context.navigationTraceFields(), /since_previous_navigation_ms=none after_navigation_gap=false/);
  f.advance(299999);
  assert.match(f.context.navigationTraceFields(), /since_previous_navigation_ms=299999 after_navigation_gap=false/);
  f.advance(300000);
  assert.match(f.context.navigationTraceFields(), /since_previous_navigation_ms=300000 after_navigation_gap=true/);
  f.advance(-2000);
  assert.match(f.context.navigationTraceFields(), /since_previous_navigation_ms=0 after_navigation_gap=false/);
  assert.doesNotMatch(source('navigationTraceFields'), /invoke|setTimeout|setInterval|refresh|repository|relativePath/);
});
test('directory logging distinguishes cache reuse, frontend bypass and actual Git invalidation without adding reads', async () => {
  const entries = [{ name: 'existing' }], calls = [], logs = [];
  const { context: c } = clock({
    state: { repository: { path: '/private-repo' }, view: 'explorer', statusReady: true },
    directoryCache: new Map([['secret-folder', entries]]), refs: { fileList: {} }, explorerRequestSeq: 1,
    performance, jsPerfLog: line => logs.push(line), render() {}, renderExplorerAfterStatusRefresh() {},
    invoke: async (name, args) => { calls.push({ name, args }); return entries; },
  });
  vm.runInContext(source('fetchAndRenderDirectory'), c);
  await c.fetchAndRenderDirectory('secret-folder', 1, {});
  assert.equal(calls.length, 0); assert.equal(c.state.entries, entries);
  assert.match(logs[0], /source=directory_cache force=false invalidate_git=false/);
  assert.doesNotMatch(logs[0], /private-repo|secret-folder/);
  await c.fetchAndRenderDirectory('secret-folder', 1, { force: true });
  assert.equal(calls.length, 1); assert.equal(calls[0].args.force, false);
  await c.fetchAndRenderDirectory('secret-folder', 1, { force: true, invalidateGit: true });
  assert.equal(calls.length, 2); assert.equal(calls[1].args.force, true);
  assert.ok(logs.some(line => /source=backend force=true invalidate_git=false/.test(line)));
  assert.ok(logs.some(line => /source=backend force=true invalidate_git=true/.test(line)));
});
test('instrumented directory loading still rejects late responses', async () => {
  let resolve;
  const { context: c } = clock({
    state: { repository: { path: '/repo' }, statusReady: true }, directoryCache: new Map(),
    refs: { fileList: {} }, explorerRequestSeq: 1, performance, jsPerfLog() {},
    invoke: () => new Promise(done => { resolve = done; }), render: () => assert.fail('stale render'),
  });
  vm.runInContext(source('fetchAndRenderDirectory'), c);
  const pending = c.fetchAndRenderDirectory('folder', 1, {});
  c.explorerRequestSeq = 2; resolve(['stale']); await pending;
  assert.equal(c.directoryCache.size, 0);
});
