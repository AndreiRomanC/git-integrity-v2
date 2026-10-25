const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
function fixture() {
  const calls = [], events = [], errors = [];
  const state = { repository: { path: '/parent/', name: 'parent' }, repositoryOrigin: null, statusReady: true,
    branches: [{ name: 'main' }], commits: [{ id: 'parent-commit' }], changes: [{ path: 'local', staged: true }],
    currentPath: 'modules', entries: [{ name: 'dep', kind: 'submodule', relative_path: 'modules/dep' }], stashes: [],
    drillDownNotes: { local: 'Keep note' } };
  const refs = { commitMessage: { value: 'Parent draft' }, search: { value: 'parent search' }, fileList: { parentElement: { scrollTop: 123 } } };
  const context = { state, refs, structuredClone, parentReturnSnapshots: new Map(), directoryCache: new Map([['modules', state.entries]]),
    repoOpenGeneration: 1, explorerRequestSeq: 2, commitWriteRunning: false, inFlightRepositoryWrites: 0, activeStagingOperation: null, pendingToggleFlight: null, pendingToggles: new Map(),
    invoke: (name, args) => new Promise((resolve, reject) => calls.push({ name, args, resolve, reject })),
    flushPendingTogglesNow: async () => {}, closeSubmoduleGraph: () => { state.submoduleGraph = null; }, resetFooterForContextChange() {},
    render: () => events.push('paint'), clearDetails() {}, updateStashUI() {}, updateChangeBadge() {}, jsPerfLog: text => events.push(text), status() {},
    handleError: error => errors.push(error), addRecentRepo() {}, updatePublishIndicator() {},
    openDirectory: async (path, options) => { events.push('folder-paint'); assert.equal(path, 'modules'); assert.equal(options.force, true); },
    openRepositoryFast: async (path, options) => events.push({ fallback: path, options }), parentPathOf: p => p.split('/').slice(0, -1).join('/'),
  };
  vm.createContext(context);
  for (const name of ['rememberParentReturn','returnToParentRepository']) vm.runInContext(source.match(new RegExp(`async function ${name}\\([^]*?\\n}`))[0], context);
  const target = { path: '/parent/modules/dep/', parent_path: '/parent/', submodule_path: 'modules/dep' };
  const enterChild = () => {
    Object.assign(state, { repository: { path: target.path, name: 'dep' }, repositoryOrigin: { repositoryPath: target.path, parentPath: target.parent_path, submodulePath: target.submodule_path },
      branches: [{ name: 'child' }], commits: [{ id: 'child-commit' }], changes: [{ path: 'child.txt' }], currentPath: '', entries: [], stashes: [] });
    refs.commitMessage.value = 'Child draft'; context.directoryCache.clear(); context.directoryCache.set('', [{ name: 'child.txt' }]);
  };
  const remember = async () => { const pending = context.rememberParentReturn(target); calls.at(-1).resolve('stamp'); await pending; enterChild(); };
  return { context, state, refs, calls, events, errors, target, remember, enterChild };
}
test('return paints the saved parent immediately and only resumes its gitlink status, not a full repository open', async () => {
  const f = fixture(); await f.remember();
  const pending = f.context.returnToParentRepository(); await tick();
  assert.equal(f.state.repository.path, '/parent/'); assert.equal(f.state.statusReady, false);
  assert.equal(f.state.commits[0].id, 'parent-commit'); assert.equal(f.refs.commitMessage.value, 'Parent draft');
  assert.equal(f.refs.search.value, 'parent search'); assert.equal(f.refs.fileList.parentElement.scrollTop, 123);
  assert.deepEqual(f.state.drillDownNotes, { local: 'Keep note' }); assert.ok(f.events.includes('paint'));
  assert.equal(f.calls.at(-1).name, 'resume_repository_return'); assert.equal(f.calls.at(-1).args.submodulePath, 'modules/dep');
  const changes = [{ path: 'local', staged: true }, { path: 'modules/dep', staged: false }]; f.calls.at(-1).resolve(changes); await pending;
  assert.equal(f.state.statusReady, true); assert.equal(f.state.changes, changes);
  assert.ok(f.events.includes('folder-paint')); assert.ok(!f.events.some(e => e.fallback));
  assert.ok(!f.calls.some(c => ['refresh_status','load_repository','open_repository_fast'].includes(c.name)));
  assert.equal(f.context.directoryCache.has(''), false, 'child folders cannot contaminate the parent cache');
});
test('metadata change or missing snapshot uses the existing safe open; failed validation keeps mutation guards closed', async () => {
  const f = fixture(); await f.remember(); const pending = f.context.returnToParentRepository(); await tick();
  f.calls.at(-1).resolve(null); await pending; assert.equal(f.events.find(e => e.fallback)?.fallback, '/parent/');
  const g = fixture(); g.enterChild(); await g.context.returnToParentRepository();
  assert.equal(g.events.find(e => e.fallback)?.fallback, '/parent/'); assert.equal(g.calls.length, 0);
  const h = fixture(); await h.remember(); const failure = h.context.returnToParentRepository(); await tick();
  h.calls.at(-1).reject(new Error('read failed')); await failure;
  assert.equal(h.state.statusReady, false); assert.equal(h.errors.length, 1);
});
test('late parent validation cannot overwrite a different repository and console/staging barriers are respected', async () => {
  const f = fixture(); await f.remember(); const pending = f.context.returnToParentRepository(); await tick();
  f.state.repository = { path: '/other/' }; f.context.repoOpenGeneration++; f.calls.at(-1).resolve([{ path: 'obsolete' }]); await pending;
  assert.equal(f.state.repository.path, '/other/'); assert.ok(!f.events.includes('folder-paint'));
  const g = fixture(); await g.remember(); g.state.consoleCommandRunning = true; const before = g.calls.length;
  await g.context.returnToParentRepository(); assert.equal(g.calls.length, before); assert.equal(g.state.repository.path, g.target.path);
  const h = fixture(); await h.remember(); h.context.flushPendingTogglesNow = async () => { throw new Error('stage failed'); };
  await h.context.returnToParentRepository(); assert.equal(h.state.repository.path, h.target.path); assert.equal(h.errors.length, 1);
  for (const flag of ['inFlightRepositoryWrites','activeStagingOperation']) {
    const j = fixture(); await j.remember(); j.context[flag] = 1;
    await j.context.returnToParentRepository(); assert.equal(j.state.repository.path, j.target.path); assert.equal(j.calls.length, 1);
  }
});
test('capture refuses unverified parent state and rejects late stamps after another repository was opened', async () => {
  const f = fixture(); f.state.statusReady = false; await f.context.rememberParentReturn(f.target); assert.equal(f.calls.length, 0);
  f.state.statusReady = true; const pending = f.context.rememberParentReturn(f.target);
  f.state.repository = { path: '/other/' }; f.context.repoOpenGeneration++; f.calls[0].resolve('old-stamp'); await pending;
  assert.equal(f.context.parentReturnSnapshots.size, 0);
});
test('completing validation does not navigate away from a newly selected parent view', async () => {
  const f = fixture(); await f.remember(); const pending = f.context.returnToParentRepository(); await tick();
  f.state.view = 'graph'; f.state.currentPath = 'elsewhere'; f.calls.at(-1).resolve([]); await pending;
  assert.equal(f.state.view, 'graph'); assert.equal(f.state.currentPath, 'elsewhere'); assert.ok(!f.events.includes('folder-paint'));
});
test('stale directory responses are rejected before writing a repository-relative cache entry', () => {
  const body = source.match(/async function fetchAndRenderDirectory\([^]*?\n}/)[0];
  const invocation = body.indexOf("await invoke('load_directory'");
  assert.ok(body.indexOf('if (requestId !== explorerRequestSeq) return;', invocation) < body.indexOf('directoryCache.set(path, entries)', invocation));
});
test('repository switching is blocked during a Git write and the guard is released on success and failure', async () => {
  const calls = [], state = { statusReady: true };
  let syncFailure = false;
  const context = { state, performance, pendingToggles: new Map(), activeStagingOperation: null, pendingToggleFlight: null,
    repoOpenGeneration: 0, jsPerfLog() {}, status() {}, refreshAfterMutationFailure: async () => {},
    window: { __TAURI__: { core: { invoke: command => {
      if (syncFailure) throw new Error('sync failure');
      return new Promise((resolve, reject) => calls.push({ command, resolve, reject }));
    } } } },
  };
  vm.createContext(context); vm.runInContext(source.slice(0, source.indexOf('const $ =')) + '\nglobalThis.call = invoke; globalThis.writes = () => inFlightRepositoryWrites;', context);
  state.navigationStatusPending = true;
  await assert.rejects(context.call('stage_files', {}), /status is being verified/);
  await assert.rejects(context.call('restore_folder', {}), /status is being verified/);
  const read = context.call('list_directory_fast', {}); calls.at(-1).resolve([]); await read;
  assert.equal(context.writes(), 0); calls.length = 0;
  state.navigationStatusPending = false;
  const write = context.call('commit_staged', {}); assert.equal(context.writes(), 1);
  await assert.rejects(context.call('open_repository_fast', {}), /write is still running/);
  calls[0].resolve(); await write; assert.equal(context.writes(), 0);
  const navigation = context.call('open_repository_fast', {}); calls[1].resolve(); await navigation;
  const failed = context.call('merge_branch', {}); calls[2].reject(new Error('failure')); await assert.rejects(failed); assert.equal(context.writes(), 0);
  syncFailure = true; assert.throws(() => context.call('commit_staged', {}), /sync failure/); assert.equal(context.writes(), 0);
});
