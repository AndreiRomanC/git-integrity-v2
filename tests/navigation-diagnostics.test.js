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
    directoryCache: new Map([['secret-folder', entries]]), refs: { fileList: {} }, explorerRequestSeq: 1, repoOpenGeneration: 1,
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
    refs: { fileList: {} }, explorerRequestSeq: 1, repoOpenGeneration: 1, performance, jsPerfLog() {},
    invoke: () => new Promise(done => { resolve = done; }), render: () => assert.fail('stale render'),
  });
  vm.runInContext(source('fetchAndRenderDirectory'), c);
  const pending = c.fetchAndRenderDirectory('folder', 1, {});
  c.explorerRequestSeq = 2; resolve(['stale']); await pending;
  assert.equal(c.directoryCache.size, 0);
});

test('fast-open status diagnostics still perform exactly one request and do not log repository paths', async () => {
  const calls = [], logs = [], changes = [{ path: 'private-file', status: 'M' }];
  const c = { state: { repository: { path: '/private/repo' }, view: 'history', commits: [] }, repoOpenGeneration: 4,
    ipcDiagnostics: { applied: result => assert.equal(result, changes) },
    jsPerfLog: line => logs.push(line), invoke: async (command, args) => { calls.push({ command, args }); return changes; },
    updateChangeBadge() {}, render() {}, status() {}, handleError: error => { throw error; } };
  vm.createContext(c); vm.runInContext(source('completeRepositoryOpenStatus'), c);
  await c.completeRepositoryOpenStatus('/private/repo', 4);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, 'refresh_status');
  assert.equal(calls[0].args.repositoryPath, '/private/repo');
  assert.equal(c.state.changes, changes);
  assert.equal(c.state.statusReady, true);
  assert.deepEqual(logs, ['statusRefresh requested reason=fast_repository_open']);
});

test('elapsed feedback changes only text and stops its one timer after verification or failure', () => {
  const text = {}, button = {}, label = {}, timers = new Map(); let sequence = 0;
  const jobs = [{ phase: 'refreshing', refreshStartedAt: 1700000000000 }];
  const notice = { classList: { toggle() {} }, querySelector: selector => selector === 'span' ? text : button };
  const entry = { relative_path: 'module', details_loading: true, details_phase: 'waiting_status', details_wait_started: 1700000000000 };
  const c = { state: { repository: { path: '/repo' }, view: 'explorer', selectedEntry: { relative_path: 'module' } },
    refs: { details: { displayedEntry: entry, displayedRepositoryPath: '/repo', querySelector: () => label } },
    navigationRefresh: { states: () => jobs }, $: () => notice, anonymizeForLog: () => 'repo',
    setTimeout: fn => { const id = ++sequence; timers.set(id, fn); return id; }, clearTimeout: id => timers.delete(id),
    invoke: () => assert.fail('the feedback clock must not query Git') };
  const time = clock(c);
  vm.runInContext('let navigationFeedbackTimer = null;\n' + source('entryDetailsProgressText') + '\n' + source('syncNavigationRefreshNotice'), c);
  c.syncNavigationRefreshNotice(); assert.equal(timers.size, 1); assert.match(text.textContent, /0 s/);
  time.advance(44000); c.syncNavigationRefreshNotice();
  assert.equal(timers.size, 1); assert.match(text.textContent, /44 s/); assert.match(label.textContent, /Waiting for repository status verification · 44 s/);
  assert.equal(c.state.navigationStatusPending, true);
  jobs[0].phase = 'error'; jobs[0].error = 'scan failed'; jobs[0].target = { repositoryPath: '/repo' }; entry.details_error = true;
  c.syncNavigationRefreshNotice(); assert.equal(timers.size, 0); assert.equal(button.hidden, false);
  assert.equal(c.state.navigationStatusPending, true, 'failed verification must still block writes');
  jobs.length = 0; entry.details_loading = false; c.syncNavigationRefreshNotice();
  assert.equal(timers.size, 0); assert.equal(notice.hidden, true); assert.equal(c.state.navigationStatusPending, false);
});

test('details feedback distinguishes repository verification from reading the selected entry', () => {
  const c = {}; vm.runInNewContext(source('entryDetailsProgressText'), c);
  assert.match(c.entryDetailsProgressText({ details_phase: 'waiting_status', details_wait_started: 1000 }, 19000), /verification · 18 s/);
  assert.match(c.entryDetailsProgressText({ details_phase: 'reading_inspection', details_wait_started: 1000 }, 19000), /Reading local submodule information/);
  assert.match(c.entryDetailsProgressText({ details_phase: 'reading_details', details_wait_started: 19000 }, 19000), /Checking current Git details · 0 s/);
  assert.doesNotMatch(c.entryDetailsProgressText({ details_phase: 'waiting_status', details_wait_started: 19000 }, 1000), /-18/);
});
