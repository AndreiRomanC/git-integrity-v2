const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { create } = require('../frontend/navigation-refresh.js');
const tick = () => new Promise(resolve => setImmediate(resolve));
function gate() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
const target = { owner: '/repo', repositoryPath: '/repo', generation: 1, expectedHead: { head_oid: 'head', branch: 'main' } };
const snapshot = fresh => ({ fresh, head_oid: 'head', branch: 'main' });
function controller() {
  const calls = [], applied = [], events = [];
  let generation = 1;
  const c = create({ invoke: (command, args) => { const g = gate(); calls.push({ command, args, ...g }); return g.promise; },
    current: t => t.generation === generation, changed() {}, refreshed: (t, changes) => applied.push({ t, changes }),
    log: phase => events.push(phase) });
  return { c, calls, applied, events, reopen: () => generation++ };
}
test('warm navigation performs one cheap probe, no status scan or refresh timer', async () => {
  const h = controller(), p = h.c.ensure(target); await tick();
  assert.equal(h.calls[0].command, 'navigation_status'); h.calls[0].resolve(snapshot(true));
  assert.deepEqual(await p, { refreshed: false }); assert.equal(h.calls.length, 1); assert.equal(h.c.states().length, 0);
});
test('explicit post-Fetch status read applies a warm snapshot without forcing cache invalidation', async () => {
  const h = controller(), p = h.c.ensure(target, { readStatus: true }); await tick();
  h.calls[0].resolve(snapshot(true)); await tick();
  assert.equal(h.calls[1].command, 'refresh_navigation_status'); assert.equal(h.calls[1].args.forceFresh, false);
  assert.equal(h.c.ensure(target), p, 'ordinary navigation joins the post-Fetch read');
  h.calls[1].resolve([{ path: 'cached-current.c' }]); assert.deepEqual(await p, { refreshed: true });
  assert.equal(h.applied[0].changes[0].path, 'cached-current.c'); assert.equal(h.calls.length, 2);
});
test('post-Fetch companion read holds the write barrier even when status finishes first', async () => {
  for (const fails of [false, true]) {
    const h = controller(), metadata = gate();
    const p = h.c.ensure(target, { readStatus: true, completeWith: metadata.promise });
    await tick(); h.calls[0].resolve(snapshot(true)); await tick(); h.calls[1].resolve([]); await tick();
    assert.equal(h.c.states().length, 1, 'status completion must not release the metadata write barrier');
    assert.equal(h.c.ensure(target), p, 'navigation still joins the same job');
    if (fails) metadata.reject(new Error('refs failed')); else metadata.resolve();
    assert.deepEqual(await p, { refreshed: true });
    assert.equal(h.c.states().length, 0, 'a failed refs read is reported by its caller, not a stuck status gate');
    assert.equal(h.calls.length, 2);
  }
});
test('rapid folder/selection requests join one background scan for the repository', async () => {
  const h = controller(), a = h.c.ensure(target), b = h.c.ensure({ ...target });
  assert.equal(a, b); assert.equal(h.c.states()[0].phase, 'checking'); await tick();
  h.calls[0].resolve(snapshot(false)); await tick();
  assert.equal(h.calls[1].command, 'refresh_navigation_status'); assert.equal(h.c.states()[0].phase, 'refreshing');
  assert.equal(h.c.ensure(target), a); h.calls[1].resolve([{ path: 'external.txt' }]);
  await a; assert.equal(h.calls.length, 2); assert.equal(h.applied.length, 1); assert.equal(h.c.states().length, 0);
});
test('submodule refresh targets only its own repository, not all children', async () => {
  const h = controller(), child = { ...target, repositoryPath: '/repo/modules/a', expectedHead: null };
  const p = h.c.ensure(child); await tick(); h.calls[0].resolve({ fresh: false, head_oid: 'child', branch: '' }); await tick();
  assert.deepEqual(h.calls[1].args, { repositoryPath: '/repo/modules/a', headOid: 'child', branch: '', forceFresh: true });
  h.calls[1].resolve([]); await p; assert.equal(h.applied[0].t.repositoryPath, child.repositoryPath);
});
for (const phase of ['probe', 'refresh']) test(`a different repository/open discards late ${phase} results`, async () => {
  const h = controller(), p = h.c.ensure(target); await tick();
  if (phase === 'refresh') { h.calls[0].resolve(snapshot(false)); await tick(); }
  h.reopen(); h.calls.at(-1).resolve(phase === 'probe' ? snapshot(false) : []);
  assert.deepEqual(await p, { obsolete: true }); assert.equal(h.applied.length, 0); assert.equal(h.c.states().length, 0);
});
test('an external branch change requires explicit recovery, never applies status to the old branch', async () => {
  const h = controller(), p = h.c.ensure(target); await tick();
  h.calls[0].resolve({ fresh: true, head_oid: 'other', branch: 'feature' });
  await assert.rejects(p, /changed outside/); assert.equal(h.calls.length, 1);
  assert.equal(h.c.states()[0].phase, 'error'); assert.equal(h.applied.length, 0);
});
test('a changed child HEAD forces fresh status even while its previous snapshot is young', async () => {
  const h = controller(), child = { ...target, repositoryPath: '/repo/dep', expectedHead: null };
  const first = h.c.ensure(child); await tick(); h.calls[0].resolve(snapshot(true)); await tick();
  assert.equal(h.calls[1].args.forceFresh, true); h.calls[1].resolve([]); await first;
  const warm = h.c.ensure(child); await tick(); h.calls[2].resolve(snapshot(true)); await warm;
  assert.equal(h.calls.length, 3);
  const moved = h.c.ensure(child); await tick(); h.calls[3].resolve({ fresh: true, head_oid: 'new-child', branch: 'feature' }); await tick();
  assert.equal(h.calls[4].args.forceFresh, true); assert.equal(h.calls[4].args.headOid, 'new-child');
  h.calls[4].resolve([]); await moved;
});
test('errors stay visible and writes remain unverified; navigation does not start a retry storm', async () => {
  const h = controller(), p = h.c.ensure(target); await tick(); h.calls[0].resolve(snapshot(false)); await tick();
  h.calls[1].reject(new Error('scan failed')); await assert.rejects(p, /scan failed/);
  assert.equal(h.c.states()[0].phase, 'error'); await assert.rejects(h.c.ensure(target), /scan failed/); assert.equal(h.calls.length, 2);
  h.c.retry(); const retry = h.c.ensure(target); await tick(); h.calls[2].resolve(snapshot(true)); await retry;
  assert.equal(h.c.states().length, 0);
});
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
const fn = name => app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))[0];
function viewHarness(cached = true) {
  const old = [{ name: 'old', relative_path: 'folder/old', status: 'M' }], verification = gate(), reads = [];
  const state = { repository: { path: '/repo' }, view: 'explorer', currentPath: 'folder', entries: old, selectedEntry: old[0] };
  const refs = { fileList: { innerHTML: 'existing rows', parentElement: { scrollTop: 200 } } };
  const c = { state, refs, repoOpenGeneration: 1, explorerRequestSeq: 1, directoryCache: new Map(cached ? [['folder', old]] : []),
    performance, jsPerfLog() {}, renderExplorerAfterStatusRefresh() {}, render() {}, status() {}, esc: String, clearDetails() {},
    ensureNavigationStatus: () => verification.promise,
    paintDirectoryFast: async () => { state.entries = [{ name: 'disk', status_known: false }]; },
    selectEntry: path => { c.selected = path; },
    invoke: (command, args) => { const g = gate(); reads.push({ command, args, ...g }); return g.promise; } };
  vm.createContext(c);
  c.navigationTraceFields = () => '';
  for (const name of ['fetchAndRenderDirectory', 'loadNavigableDirectory']) vm.runInContext(fn(name), c);
  return { c, state, refs, old, verification, reads };
}
test('expired cached folder stays usable until replacement; selection and latest scroll survive', async () => {
  const h = viewHarness(), p = h.c.loadNavigableDirectory('folder', 1, {});
  assert.equal(h.state.entries, h.old); assert.equal(h.refs.fileList.innerHTML, 'existing rows');
  h.verification.resolve({ refreshed: true }); await tick();
  assert.equal(h.reads.length, 1); assert.equal(h.refs.fileList.innerHTML, 'existing rows');
  h.refs.fileList.parentElement.scrollTop = 320;
  h.reads[0].resolve([{ name: 'old', relative_path: 'folder/old', status: '' }, { name: 'new' }]); await p;
  assert.equal(h.c.selected, 'folder/old'); assert.equal(h.refs.fileList.parentElement.scrollTop, 320);
  assert.equal(h.state.entries.length, 2);
});
test('unvisited folder gets filesystem rows while Git is still running, not an empty Git loading screen', async () => {
  const h = viewHarness(false), p = h.c.loadNavigableDirectory('folder', 1, {}); await tick();
  assert.equal(h.state.entries[0].name, 'disk'); assert.equal(h.state.entries[0].status_known, false); assert.equal(h.reads.length, 0);
  h.verification.reject(new Error('offline filesystem')); await p;
  assert.equal(h.state.entries[0].name, 'disk');
});
test('navigating away during a slow verification never sends a stale folder read', async () => {
  const h = viewHarness(), p = h.c.loadNavigableDirectory('folder', 1, {});
  h.c.explorerRequestSeq++; h.state.currentPath = 'other'; h.verification.resolve({ refreshed: true }); await p;
  assert.equal(h.reads.length, 0); assert.equal(h.state.currentPath, 'other');
});
test('repository switch rejects a folder read even if its request number happens to match', async () => {
  const h = viewHarness(), p = h.c.fetchAndRenderDirectory('folder', 1, { force: true, keepVisible: true });
  h.c.repoOpenGeneration++; h.state.repository = { path: '/elsewhere' }; h.reads[0].resolve([{ name: 'wrong' }]); await p;
  assert.equal(h.state.entries, h.old);
});
test('navigation errors preserve the old folder frame', async () => {
  const h = viewHarness(), p = h.c.fetchAndRenderDirectory('folder', 1, { force: true, keepVisible: true });
  h.reads[0].reject(new Error('permission')); await p;
  assert.equal(h.state.entries, h.old); assert.equal(h.refs.fileList.innerHTML, 'existing rows');
});
test('central mutation barrier covers pending or failed navigation verification', () => {
  assert.match(app, /MUTATING_COMMANDS\.has\(command\) && state\.navigationStatusPending/);
  assert.match(fn('syncNavigationRefreshNotice'), /state\.navigationStatusPending = jobs\.length > 0/);
  assert.doesNotMatch(fs.readFileSync(require('node:path').join(__dirname, '../frontend/navigation-refresh.js'), 'utf8'), /setInterval|setTimeout|fetch_all|fetch_submodule/);
});
