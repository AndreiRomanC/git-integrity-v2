'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const section = (start, end) => app.slice(app.indexOf(start), app.indexOf(end, app.indexOf(start)));
const turn = () => new Promise(resolve => setImmediate(resolve));

function harness() {
  const entry = { kind: 'submodule', relative_path: 'modules/engine', name: 'engine' };
  const graph = { relativePath: entry.relative_path, parentRepositoryPath: 'parent', name: 'engine', repository: { path: 'parent/modules/engine', current_branch: 'main' }, commits: [{ id: 'old' }] };
  const state = { repository: { path: 'parent', current_branch: 'main' }, submoduleGraph: graph, view: 'graph', currentPath: 'modules' };
  const calls = [], errors = [], listeners = {}, events = [];
  const context = {
    state, submoduleMenuEntry: entry, submoduleMenuData: null,
    refs: { submoduleMenu: { hidden: false }, currentSubmoduleVersion: {}, submoduleVersions: {} },
    invoke: (command, args) => new Promise((resolve, reject) => calls.push({ command, args, resolve, reject })),
    beginButtonOperation: () => () => events.push('button-finished'),
    renderSubmoduleVersions: () => events.push('versions-rendered'), render: () => events.push('graph-rendered'),
    status: text => events.push(text), showOperationToast() {}, handleError: error => errors.push(error),
    clearDetails() {}, jsPerfLog() {}, anonymizeForLog: value => value, GRAPH_COMMIT_WINDOW: 500,
    esc: String, resetFooterForContextChange() {}, openDirectory() {},
    loadRepository: () => { throw new Error('must not reload the superproject'); },
    fetchRemote: () => { throw new Error('must not fetch the superproject'); },
    $: selector => ({ addEventListener: (event, callback) => { listeners[selector] = callback; } }),
  };
  vm.createContext(context);
  vm.runInContext([
    section('function createRequestGuard()', 'let explorerRequestSeq'),
    section('const openSubmoduleMenuGuard', 'async function openSubmoduleMenu('),
    section('async function fetchSubmodule(', 'async function removeEntryFromGit('),
    section('let submoduleGraphGeneration =', '// The one explicit source of truth'),
    section('async function refreshRepository(', '// The fast, read-only first phase'),
    section("$('#fetchCurrent').addEventListener", "$('#fetchAll').addEventListener"),
  ].join('\n'), context);
  return { context, state, calls, errors, events, listeners, entry, graph };
}

const data = () => ({ repository: { path: 'parent/modules/engine', current_branch: 'main', head_oid: 'unchanged' }, branches: [{ name: 'origin/new' }], commits: [{ id: 'new', subject: 'New server version' }], changes: [] });

test('fetch updates the same open submodule graph and version list, not the parent', async () => {
  const h = harness();
  const pending = h.context.fetchSubmodule(h.entry);
  assert.equal(h.calls[0].command, 'fetch_submodule');
  assert.equal(h.calls[0].args.repositoryPath, 'parent');
  h.calls[0].resolve(); await turn();
  assert.equal(h.calls[1].command, 'submodule_repository');
  h.calls[1].resolve(data()); await turn();
  assert.equal(h.calls[2].command, 'submodule_versions');
  h.calls[2].resolve({ versions: [{ name: 'new-tag' }] }); await pending;
  assert.equal(h.state.repository.path, 'parent');
  assert.equal(h.state.submoduleGraph.commits[0].id, 'new');
  assert.equal(h.context.submoduleMenuData.versions[0].name, 'new-tag');
  assert.equal(h.errors.length, 0);
});

test('topbar Fetch targets the active submodule rather than the superproject', async () => {
  const h = harness(); h.context.refs.submoduleMenu.hidden = true;
  const pending = h.listeners['#fetchCurrent']({ currentTarget: {} });
  assert.equal(h.calls[0].command, 'fetch_submodule');
  assert.equal(h.calls[0].args.relativePath, 'modules/engine');
  h.calls[0].resolve(); await turn();
  h.calls[1].resolve(data()); await pending;
});

for (const navigation of ['repository', 'submodule', 'leave']) {
  test(`slow fetch cannot reopen the old graph after changing ${navigation}`, async () => {
    const h = harness(); h.context.refs.submoduleMenu.hidden = true;
    const pending = h.context.fetchSubmodule(h.entry);
    if (navigation === 'repository') h.state.repository = { path: 'other', current_branch: 'main' };
    if (navigation === 'submodule') h.state.submoduleGraph = { ...h.graph, relativePath: 'modules/other' };
    if (navigation === 'leave') h.context.leaveSubmoduleGraph();
    h.calls[0].resolve(); await pending;
    assert.equal(h.calls.length, 1);
    assert.notEqual(h.state.submoduleGraph?.commits?.[0]?.id, 'new');
  });
}

test('local Refresh stays in the submodule and does not fetch', async () => {
  const h = harness();
  h.graph.primaryBranch = 'branch:release'; h.graph.refFilter = 'branches';
  const pending = h.context.refreshRepository();
  assert.equal(h.calls[0].command, 'submodule_repository');
  h.calls[0].resolve(data()); await pending;
  assert.equal(h.state.submoduleGraph.commits[0].id, 'new');
  assert.equal(h.calls.length, 1);
  assert.equal(h.state.repository.path, 'parent');
  assert.equal(h.state.submoduleGraph, h.graph, 'Story/Full Graph context identity must survive refresh');
  assert.equal(h.state.submoduleGraph.primaryBranch, 'branch:release');
  assert.equal(h.state.submoduleGraph.refFilter, 'branches');
});

test('leaving during graph refresh rejects its late response', async () => {
  const h = harness();
  const pending = h.context.refreshRepository();
  h.context.leaveSubmoduleGraph();
  h.calls[0].resolve(data()); await pending;
  assert.equal(h.state.submoduleGraph, null);
  assert.equal(h.state.view, 'explorer');
});

test('failed fetch recaptures partial ref updates and keeps the original error', async () => {
  const h = harness();
  const pending = h.context.fetchSubmodule(h.entry, {});
  const error = new Error('one ref failed after other refs were updated');
  h.calls[0].reject(error); await turn();
  assert.equal(h.calls[1].command, 'submodule_repository');
  h.calls[1].resolve(data()); await turn();
  assert.equal(h.calls[2].command, 'submodule_versions');
  h.calls[2].resolve({ versions: [{ name: 'updated-tag', revision: 'new' }] }); await pending;
  assert.equal(h.errors.length, 1);
  assert.equal(h.errors[0], error);
  assert.equal(h.state.submoduleGraph, h.graph);
  assert.equal(h.graph.commits[0].id, 'new');
  assert.equal(h.context.submoduleMenuData.versions[0].revision, 'new');
  assert.ok(h.events.includes('button-finished'));
  assert.equal(h.calls.length, 3);
  assert.ok(!h.events.some(event => /branches and tags fetched/.test(event)), 'failure must not become a success notice');
});

test('updated server tags are explained without suggesting a checkout', async () => {
  const h = harness();
  const pending = h.context.fetchSubmodule(h.entry);
  h.calls[0].resolve({ updated_tags: 1 }); await turn();
  h.calls[1].resolve(data()); await turn();
  h.calls[2].resolve({ versions: [{ name: 'moved-tag', revision: 'new' }] }); await pending;
  assert.ok(h.events.some(event => /1 existing tag updated from the server; previous targets saved as recovery refs/.test(event)));
  assert.ok(h.events.some(event => /Checkout unchanged/.test(event)));
  assert.equal(h.errors.length, 0);
});

for (const navigation of ['repository', 'submodule', 'leave']) {
  test(`failed fetch cannot refresh its old graph after changing ${navigation}`, async () => {
    const h = harness(); h.context.refs.submoduleMenu.hidden = true;
    const pending = h.context.fetchSubmodule(h.entry);
    if (navigation === 'repository') h.state.repository = { path: 'other', current_branch: 'main' };
    if (navigation === 'submodule') h.state.submoduleGraph = { ...h.graph, relativePath: 'modules/other' };
    if (navigation === 'leave') h.context.leaveSubmoduleGraph();
    h.calls[0].reject(new Error('old fetch failed')); await pending;
    assert.equal(h.calls.length, 1);
    assert.notEqual(h.state.submoduleGraph?.commits?.[0]?.id, 'new');
  });
}

test('a failed fetch without an open graph still refreshes the same version menu', async () => {
  const h = harness(); h.state.submoduleGraph = null;
  const pending = h.context.fetchSubmodule(h.entry);
  h.calls[0].reject('partial fetch'); await turn();
  assert.equal(h.calls[1].command, 'submodule_versions');
  h.calls[1].resolve({ versions: [{ name: 'new-tag' }] }); await pending;
  assert.equal(h.context.submoduleMenuData.versions[0].name, 'new-tag');
  assert.deepEqual(h.errors, ['partial fetch']);
});

test('older local version requests cannot replace newer results', async () => {
  const h = harness();
  const old = h.context.refreshSubmoduleMenu(), latest = h.context.refreshSubmoduleMenu();
  h.calls[1].resolve({ versions: [{ name: 'latest' }] }); await latest;
  h.calls[0].resolve({ versions: [{ name: 'old' }] }); await old;
  assert.equal(h.context.submoduleMenuData.versions[0].name, 'latest');
});

test('local version refresh captures repository scope and rejects a late result', async () => {
  const h = harness();
  const pending = h.context.refreshSubmoduleMenu();
  h.state.repository = { path: 'other', current_branch: 'main' };
  h.calls[0].resolve({ versions: [{ name: 'wrong' }] }); await pending;
  assert.equal(h.calls[0].args.repositoryPath, 'parent');
  assert.equal(h.context.submoduleMenuData, null);
});
