const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const { create } = require('../frontend/navigation-refresh.js');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const source = name => app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))[0];
const tick = () => new Promise(resolve => setImmediate(resolve));
function gate() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }

function harness() {
  const calls = [], messages = [], paints = [], logs = [], fallbacks = [];
  const oldChanges = [{ path: 'src/local.c', status: 'M', staged: true }];
  const entry = { relative_path: 'src/local.c', name: 'local.c' };
  const state = { repository: { path: '/repo', head_oid: 'head', current_branch: 'main' }, statusReady: true,
    changes: oldChanges, currentPath: 'src', entries: [entry], selectedEntry: entry, view: 'explorer',
    changesScope: 'global', branches: [{ name: 'main', current: true }], commits: [{ id: 'old' }], allCommits: [],
    stashes: [], remotes: [], submodule_paths: [], historyScope: '', commanderPath: 'compare/folder', commanderRows: ['comparison'],
    repositoryOrigin: { parentPath: '/parent', repositoryPath: '/repo', submoduleName: 'a' } };
  const metadata = { repository: { ...state.repository, name: 'repo' }, branches: [{ name: 'main', current: true }, { name: 'origin/new', remote: true }],
    commits: [{ id: 'head', refs: [{ name: 'new-tag', kind: 'tag' }] }, { id: 'incoming' }], stashes: [], submodule_paths: ['modules/a'], commits_truncated: false };
  const refs = { commitMessage: { value: 'Keep my note\n\nOEM-1' }, changesDrawer: { classList: { contains: () => true } } };
  const context = { state, refs, repoOpenGeneration: 1, explorerRequestSeq: 1, performance,
    directoryCache: new Map([['src', state.entries]]), window: { GitDrillDownNavigationRefresh: { create } },
    anonymizeForLog: () => 'opaque', jsPerfLog: (text, ms) => logs.push({ text, ms }),
    syncNavigationRefreshNotice: () => { state.navigationStatusPending = context.jobs().length > 0; },
    updateChangeBadge() {}, renderChanges() {}, render: () => paints.push({ phase: 'metadata', changes: state.changes, commits: state.commits }),
    updatePublishIndicator() { context.publishUpdates++; }, publishUpdates: 0,
    checkForMergeConflicts: async () => { context.conflictChecks++; }, conflictChecks: 0,
    submoduleBoundaryFor: () => '',
    flushPendingTogglesNow: async () => {},
    openDirectory: async (folder, options) => paints.push({ phase: 'folder', folder, options }),
    status: (message, kind) => messages.push({ message, kind }),
    showOperationToast: (message, kind) => messages.push({ message, kind, toast: true }),
    handleError: error => messages.push({ message: String(error), kind: 'error' }), diagnosticErrorCategory: () => 'unclassified',
    beginButtonOperation: () => () => { context.finishes++; }, finishes: 0,
    refreshAfterCompletedAction: async (...args) => { fallbacks.push(args); return true; },
    invoke: (command, args) => {
      const g = gate(); calls.push({ command, args, ...g });
      if (command === 'navigation_status') g.resolve({ head_oid: 'head', branch: 'main', fresh: false });
      if (command === 'list_remotes') g.resolve([{ name: 'origin' }]);
      if (command === 'fetch_remote' || command === 'fetch_all_remotes') g.resolve();
      if (command === 'fetch_project') g.resolve({ parent_fetched: true, submodules_fetched: 2, submodules_total: 3, submodules_skipped: 0,
        warnings: [], errors: ['One child unavailable'] });
      return g.promise;
    },
  };
  vm.createContext(context);
  for (const name of ['showCompletedActionRefreshFailure', 'refreshAfterFetch', 'ensureNavigationStatus', 'fetchRemote', 'fetchAllRemotes', 'limitedFetchProjectDetails', 'fetchProjectAndSubmodules']) vm.runInContext(source(name), context);
  const controllerStart = app.indexOf('const navigationRefresh =');
  vm.runInContext(app.slice(controllerStart, app.indexOf('\nfunction syncNavigationRefreshNotice', controllerStart)) + '\nglobalThis.jobs = () => navigationRefresh.states();', context);
  return { context, state, refs, calls, messages, paints, logs, fallbacks, metadata, oldChanges,
    call: name => { const call = calls.find(call => call.command === name); assert.ok(call, name); return call; },
    commands: () => calls.map(call => call.command),
  };
}

test('refs become visible before slow status, without clearing rows, draft, selection or compare state', async () => {
  const h = harness(), entry = h.state.selectedEntry, origin = h.state.repositoryOrigin;
  let done = false;
  const refresh = h.context.refreshAfterFetch('/repo', 'Fetch completed.').then(result => { done = true; return result; });
  await tick(); assert.equal(h.state.navigationStatusPending, true); assert.equal(h.state.changes, h.oldChanges);
  assert.equal(h.state.entries[0], entry); assert.equal(h.paints.length, 0);
  h.call('open_repository_fast').resolve(h.metadata); await tick();
  assert.equal(h.state.branches, h.metadata.branches); assert.equal(h.state.commits, h.metadata.commits);
  assert.equal(h.state.commits[0].refs[0].name, 'new-tag');
  assert.equal(done, false); assert.equal(h.state.navigationStatusPending, true);
  assert.equal(h.state.changes, h.oldChanges); assert.equal(h.state.selectedEntry, entry);
  assert.equal(h.state.repositoryOrigin, origin); assert.equal(h.state.currentPath, 'src');
  assert.equal(h.state.commanderPath, 'compare/folder'); assert.deepEqual(h.state.commanderRows, ['comparison']);
  assert.equal(h.refs.commitMessage.value, 'Keep my note\n\nOEM-1');
  assert.ok(h.logs.some(log => /references_visible/.test(log.text)));
  const fresh = [{ path: 'src/local.c', status: 'M', staged: true }, { path: 'external.c', status: '??', staged: false }];
  h.call('refresh_navigation_status').resolve(fresh);
  assert.equal(await refresh, true); assert.equal(h.state.changes, fresh); assert.equal(h.state.navigationStatusPending, false);
  assert.equal(h.context.publishUpdates, 1); assert.equal(h.context.conflictChecks, 1);
  const repaint = h.paints.find(paint => paint.phase === 'folder');
  assert.equal(repaint.folder, 'src'); assert.equal(repaint.options.keepVisible, true); assert.equal(repaint.options.preserveSelection, true);
  assert.ok(!h.commands().includes('load_repository')); assert.equal(h.call('refresh_navigation_status').args.forceFresh, false);
});

test('folder navigation joins the pending root check and owns its repaint, instead of being sent back', async () => {
  const h = harness(), refresh = h.context.refreshAfterFetch('/repo', 'Fetch completed.'); await tick();
  h.state.currentPath = 'other'; h.context.explorerRequestSeq++;
  const navigation = h.context.ensureNavigationStatus('other'); await tick();
  assert.equal(h.commands().filter(name => name === 'navigation_status').length, 1);
  h.call('open_repository_fast').resolve(h.metadata); h.call('refresh_navigation_status').resolve([]);
  await Promise.all([refresh, navigation]);
  assert.equal(h.state.currentPath, 'other'); assert.equal(h.paints.filter(paint => paint.phase === 'folder').length, 0);
  assert.equal(h.commands().filter(name => name === 'refresh_navigation_status').length, 1);
});

test('new folder history or another view opened during metadata reading is not replaced', async () => {
  for (const view of ['graph', 'commander', 'remotes']) {
    const h = harness(), refresh = h.context.refreshAfterFetch('/repo', 'Fetch completed.'); await tick();
    h.state.view = view;
    if (view === 'graph') { h.state.historyScope = 'src'; h.state.commits = [{ id: 'scoped' }]; }
    h.call('open_repository_fast').resolve(h.metadata); h.call('refresh_navigation_status').resolve([]);
    assert.equal(await refresh, true); assert.equal(h.state.view, view);
    assert.equal(h.paints.filter(paint => paint.phase === 'folder').length, 0);
    if (view === 'graph') { assert.equal(h.state.commits[0].id, 'scoped'); assert.equal(h.state.allCommits, h.metadata.commits); }
  }
});

test('status may finish before metadata without erasing the freshly verified changes', async () => {
  const h = harness(), refresh = h.context.refreshAfterFetch('/repo', 'Fetch completed.'); await tick();
  const fresh = [{ path: 'external', staged: true }]; h.call('refresh_navigation_status').resolve(fresh); await tick();
  assert.equal(h.state.navigationStatusPending, true, 'writes remain gated while metadata is still pending');
  h.call('open_repository_fast').resolve(h.metadata); assert.equal(await refresh, true);
  assert.equal(h.state.changes, fresh); assert.equal(h.state.navigationStatusPending, false);
});

test('failed status preserves old rows, leaves writes blocked and never reports Fetch as failed', async () => {
  const h = harness(), refresh = h.context.fetchAllRemotes(); await tick();
  h.call('open_repository_fast').resolve(h.metadata); h.call('refresh_navigation_status').reject(new Error('PRIVATE scan failure'));
  await refresh;
  assert.equal(h.state.changes, h.oldChanges); assert.equal(h.state.navigationStatusPending, true);
  assert.ok(h.messages.some(item => /Fetch from all remotes completed.*Display refresh failed/s.test(item.message)));
  assert.ok(!h.messages.some(item => item.kind === 'success'));
  assert.equal(h.commands().filter(name => name === 'fetch_all_remotes').length, 1);
  assert.doesNotMatch(h.logs.map(log => log.text).join('\n'), /PRIVATE/);
});

test('failed metadata still observes status settlement and keeps completed Fetch distinct from refresh failure', async () => {
  const h = harness(), refresh = h.context.fetchRemote('origin'); await tick();
  h.call('open_repository_fast').reject(new Error('PRIVATE refs failure')); await tick();
  assert.equal(h.state.navigationStatusPending, true);
  h.call('refresh_navigation_status').resolve([]); await refresh;
  assert.ok(h.messages.some(item => /Fetch from origin completed.*Display refresh failed/s.test(item.message)));
  assert.ok(!h.messages.some(item => item.message === 'origin updated'));
  assert.equal(h.state.commits[0].id, 'old'); assert.doesNotMatch(h.logs.map(log => log.text).join('\n'), /PRIVATE/);
});

test('partial project-Fetch errors survive a successful background refresh', async () => {
  const h = harness(), refresh = h.context.fetchProjectAndSubmodules(); await tick();
  h.call('open_repository_fast').resolve(h.metadata); h.call('refresh_navigation_status').resolve([]); await refresh;
  assert.ok(h.messages.some(item => item.kind === 'error' && /2\/3 submodules fetched.*One child unavailable/s.test(item.message)));
  assert.ok(!h.messages.some(item => item.kind === 'success')); assert.equal(h.context.finishes, 1);
});

test('changed HEAD uses a full, forced refresh instead of patching a different checkout', async () => {
  const h = harness(), refresh = h.context.refreshAfterFetch('/repo', 'Fetch completed.'); await tick();
  h.call('open_repository_fast').resolve({ ...h.metadata, repository: { ...h.metadata.repository, head_oid: 'other', current_branch: 'feature' } });
  h.call('refresh_navigation_status').reject(new Error('HEAD changed'));
  assert.equal(await refresh, true); assert.equal(h.fallbacks.length, 1);
  assert.equal(h.fallbacks[0][2].force, true); assert.equal(h.state.commits[0].id, 'old');
  assert.equal(h.paints.length, 0); assert.equal(h.messages.some(item => /Display refresh failed/.test(item.message)), false);
});

test('specialized folder/submodule histories retain the conservative full refresh', async () => {
  for (const field of ['historyScope', 'submoduleGraph']) {
    const h = harness(); h.state[field] = field === 'historyScope' ? 'src' : { relativePath: 'modules/a' };
    assert.equal(await h.context.refreshAfterFetch('/repo', 'Fetch completed.'), true);
    assert.equal(h.fallbacks.length, 1); assert.equal(h.calls.length, 0);
  }
});

test('switch/reopen during refresh discards late success and errors without altering the new UI', async () => {
  for (const differentPath of [false, true]) for (const failure of [false, true]) {
    const h = harness(), refresh = h.context.refreshAfterFetch('/repo', 'Fetch completed.'); await tick();
    h.context.repoOpenGeneration++; if (differentPath) h.state.repository.path = '/other';
    const fresh = [{ path: 'new session' }]; h.state.changes = fresh; const before = h.messages.length;
    if (failure) { h.call('open_repository_fast').reject(new Error('late refs')); h.call('refresh_navigation_status').reject(new Error('late status')); }
    else { h.call('open_repository_fast').resolve(h.metadata); h.call('refresh_navigation_status').resolve([]); }
    assert.equal(await refresh, false); assert.equal(h.messages.length, before); assert.equal(h.paints.length, 0);
    assert.equal(h.state.changes, fresh); assert.equal(h.fallbacks.length, 0);
  }
});

test('staging must settle before refresh begins, and another open during that wait wins', async () => {
  const h = harness(), staging = gate(); h.context.flushPendingTogglesNow = () => staging.promise;
  const refresh = h.context.refreshAfterFetch('/repo', 'Fetch completed.');
  assert.equal(h.calls.length, 0); h.context.repoOpenGeneration++; staging.resolve();
  assert.equal(await refresh, false); assert.equal(h.calls.length, 0);
});
