'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const app = fs.readFileSync(require('node:path').join(__dirname, '../frontend/app.js'), 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function harness() {
  const calls = [], messages = [], logs = [], handled = [];
  const data = { repository: { path: '/repo', name: 'repo' }, branches: [], commits: [], changes: [], stashes: [] };
  const projectResult = { parent_fetched: true, submodules_fetched: 2, submodules_total: 3, submodules_skipped: 1,
    warnings: ['One module not initialized'], errors: ['One remote unavailable'] };
  let commitClick, buttonFinishes = 0;
  const context = {
    state: { repository: { path: '/repo' }, currentPath: 'src', view: 'explorer', changesScope: 'global',
      changes: [{ path: 'src/a.txt', staged: true }], remotes: [], entries: [], historyScope: 'src' },
    refs: { search: { value: '' }, commitMessage: { value: 'My commit' }, defaultCommitMessage: { value: 'Global task' },
      commitButton: { addEventListener: (_, handler) => { commitClick = handler; } },
      scopeCommitMessage: { value: 'Folder commit' }, confirmScopeCommit: {}, commitScopeDialog: { close() {} } },
    repoOpenGeneration: 1, diagnosticRequestSequence: 0, commitWriteRunning: false,
    activeStagingOperation: null, pendingToggleFlight: null, pendingToggles: new Map(),
    drawerPolarionReferences: null, scopePolarionReferences: null, performance,
    directoryCache: new Map(), flushPendingTogglesNow: async () => {},
    setRepositoryOriginForPath() {}, ensureDrillDownNotesLoaded: async () => {}, updateStashUI() {},
    openDirectory: async () => {}, addRecentRepo() {}, updatePublishIndicator() {}, checkForMergeConflicts: async () => {},
    resetFooterForContextChange() {}, clearDetails() {}, render() {}, renderChanges() {},
    blockDetachedHeadWork: () => false, scopeHasChanges: () => true, selectedScope: () => ({ path: 'src', name: 'src' }),
    customPrompt: async () => 'Module commit', submoduleCommitReferenceContext: () => null,
    beginButtonOperation: () => () => { buttonFinishes++; },
    status: (message, kind) => messages.push({ message, kind, surface: 'footer' }),
    showOperationToast: (message, kind) => messages.push({ message, kind, surface: 'toast' }),
    handleError: error => { handled.push(error); messages.push({ message: String(error), kind: 'error' }); return String(error); },
    diagnosticErrorCategory: () => 'unclassified', jsPerfLog: text => logs.push(text),
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (context.override) { const result = context.override(command, args); if (result !== undefined) return result; }
      if (command === 'load_repository') return data;
      if (command === 'list_remotes') return [{ name: 'origin' }];
      if (command === 'fetch_project') return projectResult;
      return { push_detail: 'Publish separately.' };
    },
  };
  context.loadRemotesGuard = () => {
    const generation = context.repoOpenGeneration, repository = context.state.repository.path;
    return () => generation === context.repoOpenGeneration && repository === context.state.repository.path;
  };
  vm.createContext(context);
  for (const name of ['loadRepository', 'showCompletedActionRefreshFailure', 'refreshAfterCompletedAction', 'refreshAfterFetch', 'loadRemotes',
    'fetchRemote', 'fetchAllRemotes', 'limitedFetchProjectDetails', 'fetchProjectAndSubmodules', 'commitSubmoduleChanges', 'commitSelectedScope']) {
    const source = app.match(new RegExp(`(?:async )?function ${name}\\([^]*?\\n}`))?.[0];
    assert.ok(source, name); vm.runInContext(source, context);
  }
  vm.runInContext(app.slice(app.indexOf("refs.commitButton.addEventListener('click'"), app.indexOf('// Actions and Terminal —')), context);
  return { context, calls, messages, logs, handled, projectResult, commit: () => commitClick(), finishes: () => buttonFinishes,
    commands: () => calls.map(call => call.command) };
}

test('loadRepository keeps legacy error handling, while post-action refresh receives the original failure', async () => {
  for (const throwOnError of [false, true]) {
    const h = harness(), error = new Error('PRIVATE_READ_ERROR');
    h.context.override = command => { if (command === 'load_repository') throw error; };
    const load = h.context.loadRepository('/repo', { throwOnError });
    if (throwOnError) { await assert.rejects(load, actual => actual === error); assert.deepEqual(h.handled, []); }
    else { assert.equal(await load, false); assert.deepEqual(h.handled, [error]); }
    assert.deepEqual(h.commands(), ['load_repository']);
  }
});

test('superseded reload success or failure does not report a false post-action failure', async () => {
  for (const reject of [false, true]) {
    const h = harness(), pending = deferred();
    h.context.override = command => command === 'load_repository' ? pending.promise : undefined;
    const refresh = h.context.refreshAfterCompletedAction('/repo', 'Commit succeeded.');
    await tick(); h.context.repoOpenGeneration++;
    if (reject) pending.reject(new Error('late failure')); else pending.resolve({});
    assert.equal(await refresh, false);
    assert.ok(!h.messages.some(item => /Display refresh failed/.test(item.message)));
    assert.deepEqual(h.handled, []);
  }
});

for (const kind of ['staged', 'folder', 'submodule', 'scope']) {
  test(`${kind} commit success + failed refresh is explicit, without another commit or push`, async () => {
    const h = harness();
    h.context.override = command => { if (command === 'load_repository') throw new Error('PRIVATE disk busy'); };
    if (kind === 'folder') h.context.state.changesScope = 'folder';
    const action = kind === 'submodule' ? () => h.context.commitSubmoduleChanges({ kind: 'submodule', name: 'dep', relative_path: 'modules/dep', submodule_current_branch: 'topic' })
      : kind === 'scope' ? () => h.context.commitSelectedScope({ preventDefault() {} }) : h.commit;
    await action();
    const operation = { staged: 'commit_staged', folder: 'commit_files', submodule: 'commit_submodule', scope: 'commit_path' }[kind];
    assert.deepEqual(h.commands(), [operation, 'load_repository']);
    assert.ok(h.messages.some(item => item.surface === 'footer' && item.kind === 'notice' && /commit.*succeeded/i.test(item.message)
      && /Do not repeat the commit/.test(item.message) && /Display refresh failed/.test(item.message) && /disk busy/.test(item.message)));
    assert.ok(!h.messages.some(item => item.kind === 'success' || /Commit failed:/.test(item.message)));
    assert.doesNotMatch(h.logs.join('\n'), /PRIVATE/);
    assert.deepEqual(h.handled, [], 'UI refresh errors must not trigger generic mutation recovery');
    assert.equal(h.context.refs.defaultCommitMessage.value, 'Global task');
    if (kind === 'staged' || kind === 'folder') {
      assert.equal(h.context.refs.commitMessage.value, ''); assert.equal(h.context.commitWriteRunning, false);
      assert.equal(h.context.refs.commitButton.textContent, 'Commit changes');
    }
    if (kind === 'submodule') assert.equal(h.calls[0].args.alsoPush, false);
  });
}

test('a rejected commit preserves the draft and never claims commit success or reloads', async () => {
  const h = harness(); h.context.override = command => { if (command === 'commit_staged') throw new Error('index.lock'); };
  await h.commit();
  assert.deepEqual(h.commands(), ['commit_staged']);
  assert.equal(h.context.refs.commitMessage.value, 'My commit');
  assert.ok(h.messages.some(item => /Commit failed:/.test(item.message)));
  assert.ok(!h.messages.some(item => /Commit succeeded/.test(item.message)));
});

test('successful commit keeps the existing command and refresh sequence and success message', async () => {
  const h = harness(); await h.commit();
  assert.deepEqual(h.commands(), ['commit_staged', 'load_repository']);
  assert.equal(h.calls[0].args.message, 'My commit');
  assert.ok(h.messages.some(item => item.kind === 'success' && /Committed 1 file/.test(item.message)));
});

for (const [command, run] of [
  ['fetch_remote', h => h.context.fetchRemote('origin')],
  ['fetch_all_remotes', h => h.context.fetchAllRemotes()],
  ['fetch_project', h => h.context.fetchProjectAndSubmodules()],
]) {
  test(`${command}, scoped history: failed repository/remotes refresh cannot become an updated confirmation`, async () => {
    for (const failing of ['load_repository', 'list_remotes']) {
      const h = harness(); h.context.override = name => { if (name === failing) throw new Error('PRIVATE read failure'); };
      await run(h);
      assert.deepEqual(h.commands(), [command, 'load_repository', ...(failing === 'list_remotes' ? ['list_remotes'] : [])]);
      assert.ok(h.messages.some(item => item.kind === 'notice' && /Fetch/.test(item.message) && /Display refresh failed/.test(item.message)));
      assert.ok(!h.messages.some(item => item.kind === 'success'));
      assert.deepEqual(h.handled, []); assert.equal(h.finishes(), 1);
      assert.doesNotMatch(h.logs.join('\n'), /PRIVATE/);
      if (command === 'fetch_project') assert.ok(h.messages.some(item => /2\/3 submodules fetched/.test(item.message)
        && /One remote unavailable/.test(item.message) && /One module not initialized/.test(item.message)));
    }
  });

  test(`${command}, scoped history: real failure does not refresh or report completion; success keeps the full fallback reads`, async () => {
    const failed = harness(); failed.context.override = name => { if (name === command) throw new Error('fetch rejected'); };
    await run(failed); assert.deepEqual(failed.commands(), [command]);
    assert.ok(!failed.messages.some(item => /Fetch .*completed|Display refresh failed/.test(item.message)));
    assert.equal(failed.finishes(), 1);
    const good = harness(); await run(good);
    assert.deepEqual(good.commands(), [command, 'load_repository', 'list_remotes']);
    assert.ok(good.messages.some(item => /updated/.test(item.message)));
    if (command === 'fetch_project') assert.ok(good.messages.some(item => item.kind === 'error' && /One remote unavailable/.test(item.message)));
  });

  test(`${command}: switching or reopening the repository during fetch cannot refresh another context`, async () => {
    for (const otherRepo of [false, true]) for (const reject of [false, true]) {
      const h = harness(), pending = deferred(); h.context.override = name => name === command ? pending.promise : undefined;
      const fetching = run(h); await tick(); h.context.repoOpenGeneration++;
      if (otherRepo) h.context.state.repository.path = '/other';
      const before = h.messages.length;
      if (reject) pending.reject(new Error('PRIVATE late failure')); else pending.resolve(h.projectResult);
      await fetching;
      assert.deepEqual(h.commands(), [command]); assert.equal(h.messages.length, before); assert.equal(h.finishes(), 1);
      assert.doesNotMatch(h.logs.join('\n'), /PRIVATE/);
    }
  });
}

test('late remotes failure after a newer open stays silent', async () => {
  const h = harness(), pending = deferred(); h.context.override = command => command === 'list_remotes' ? pending.promise : undefined;
  const fetching = h.context.fetchAllRemotes(); await tick(); h.context.repoOpenGeneration++;
  pending.reject(new Error('late remotes failure')); await fetching;
  assert.ok(!h.messages.some(item => /Display refresh failed/.test(item.message)));
  assert.deepEqual(h.handled, []);
});
