'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const rust = fs.readFileSync(path.join(__dirname, '../src-tauri/src/repository.rs'), 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve, reject; const promise = new Promise((a,b) => { resolve=a; reject=b; }); return { promise, resolve, reject }; };

function harness() {
  const calls = [], messages = [], nodes = new Map();
  const node = id => { if (!nodes.has(id)) nodes.set(id, { value: '', disabled: false, innerHTML: '', textContent: '', className: '', classList: { toggle() {} } }); return nodes.get(id); };
  const publish = { branch: 'feature', remote: 'origin', local_tip: 'head', ahead: 2, commits: [] };
  const state = { repository: { path: 'repo-a', head_oid: 'head', current_branch: 'feature' }, publish, publishUpto: null,
    branches: [{ name: 'feature', remote: false, current: true }], commits: [{ id: 'old', refs: [{ name: 'origin/feature', kind: 'remote_branch' }] }, { id: 'head', refs: [] }],
    allCommits: [{ id: 'head', refs: [] }], entries: [], changes: ['staged', 'unstaged'], selectedEntry: null, view: 'explorer' };
  const refs = { publishFastMode: { checked: true }, publishSafeMode: {}, publishBranch: { value: 'feature' }, publishRemote: { value: 'origin' },
    publishDialog: { open: true, close() { this.open = false; } }, publishCommits: { querySelectorAll: () => [] }, details: {} };
  const metadata = { head_oid: 'head', current_branch: 'feature', upstream: 'origin/feature', remote_tip: 'head', unpushed_paths: [] };
  const context = { state, refs, $: node, mode: 'all', repoOpenGeneration: 1, publishInFlight: false, publishRiskGeneration: 0, publishSubmoduleRisksHtml: '', ipcDiagnostics: null,
    UNPUSHED_SUBMODULE_OVERRIDABLE_PREFIX: 'UNPUSHED_SUBMODULE_OVERRIDABLE::', directoryCache: new Map([['folder', []]]),
    document: { querySelector: () => ({ value: context.mode }), querySelectorAll: () => [] },
    invoke: async (command, args) => { calls.push({ command, args }); if (command === 'refresh_publish_metadata') return metadata; return []; },
    customConfirm: async () => true, render() {}, renderPublishCommits() {}, updatePublishModeUi() {}, updatePublishSummary() {},
    renderEntryDetails() {}, submodulePublishRisksHtml: risks => JSON.stringify(risks), submoduleBoundaryFor: p => p.startsWith('dep') ? 'dep' : null,
    updatePublishIndicatorGuard() {}, updatePublishIndicator() { calls.push({command:'indicator'}); },
    refreshPublish: async () => { calls.push({command:'preview'}); },
    loadRepository: async (repo, options) => { calls.push({command:'load_repository', repo, options}); context.repoOpenGeneration++; return true; },
    status: (message, kind) => messages.push({message,kind}), showOperationToast: (message,kind) => messages.push({message,kind}), jsPerfLog() {} };
  vm.createContext(context);
  for (const name of ['refreshPublishRisks', 'refreshAfterPublish', 'confirmPublish', 'executePublish', 'finishPublishUi']) {
    const source = app.match(new RegExp(`async function ${name}\\([^]*?\\n\\}`))?.[0];
    assert.ok(source, name); vm.runInContext(source, context);
  }
  const request = () => ({ publish, repositoryPath: 'repo-a', headOid: 'head', currentBranch: 'feature',
    isCurrent: () => context.repoOpenGeneration === 1 && state.repository.path === 'repo-a' });
  return { context, state, refs, node, metadata, calls, messages, request };
}

test('Fast starts no risk scan; Safe runs it only when explicitly selected and ignores late results after switching back', async () => {
  const h = harness();
  await h.context.refreshPublishRisks(); assert.equal(h.calls.length, 0);
  const pending = deferred(); h.context.invoke = (command, args) => { h.calls.push({command,args}); return pending.promise; };
  h.refs.publishFastMode.checked = false;
  const safe = h.context.refreshPublishRisks(); assert.equal(h.calls[0].command, 'submodule_publish_risks');
  h.refs.publishFastMode.checked = true; await h.context.refreshPublishRisks();
  pending.resolve([{risk:'local_only'}]); await safe;
  assert.equal(h.context.publishSubmoduleRisksHtml, ''); assert.equal(h.calls.length, 1);
});

test('Safe preview errors are explicit; changed selection, closed dialog and Combine Only do not apply old warnings', async () => {
  const h = harness(); h.refs.publishFastMode.checked = false;
  h.context.invoke = async () => { throw new Error('failed'); };
  await h.context.refreshPublishRisks(); assert.match(h.context.publishSubmoduleRisksHtml, /still run the complete safety check/);
  const pending = deferred(); h.context.invoke = () => pending.promise;
  const safe = h.context.refreshPublishRisks(); h.refs.publishDialog.open = false;
  pending.resolve([{risk:'old'}]); await safe; assert.equal(h.context.publishSubmoduleRisksHtml, '');
  h.context.invoke = () => assert.fail('Combine Only must not inspect publish risks');
  h.context.mode = 'combine_only'; await h.context.refreshPublishRisks();
});

test('targeted refresh updates new branch/upstream/ref and partial push badges without changing local state or loaded history', async () => {
  const h = harness(); const changes = h.state.changes;
  h.metadata.remote_tip = 'old'; h.metadata.unpushed_paths = ['src/remaining.txt'];
  h.state.entries = [{relative_path:'src',kind:'folder',unpushed:true,status:'M'}, {relative_path:'done.txt',kind:'file',unpushed:true,status:'M'},
    {relative_path:'dep',kind:'submodule',unpushed:true,submodule_state:'project_commit_push_needed'},
    {relative_path:'dep/inside',kind:'file',unpushed:true,status:'M'}];
  h.state.selectedEntry = h.state.entries[1]; const selected = h.state.selectedEntry;
  await h.context.refreshAfterPublish(h.request(), null);
  assert.equal(h.state.changes, changes); assert.equal(h.state.selectedEntry, selected);
  assert.equal(h.state.entries[0].unpushed, true); assert.equal(h.state.entries[1].unpushed, false);
  assert.equal(h.state.entries[1].status, 'M'); assert.equal(h.state.entries[2].submodule_state, 'synced');
  assert.equal(h.state.entries[3].unpushed, true, 'parent push cannot clear an inner submodule badge');
  assert.equal(h.state.commits.length, 2); assert.equal(h.state.commits[0].refs.length, 1); assert.equal(h.state.commits[1].refs.length, 0);
  assert.equal(h.state.branches[0].upstream, 'origin/feature'); assert.equal(h.state.branches[1].name, 'origin/feature');
  assert.equal(h.context.directoryCache.size, 0);
  assert.deepEqual(h.calls.map(c=>c.command), ['refresh_publish_metadata','indicator']);
});

test('Combine and changed HEAD use full reload, while late responses cannot repaint a different/reopened repository', async () => {
  const combined = harness(); await combined.context.refreshAfterPublish(combined.request(), {revision:'new'});
  assert.deepEqual(combined.calls.map(c=>c.command), ['load_repository']);
  const changed = harness(); changed.metadata.head_oid = 'external'; await changed.context.refreshAfterPublish(changed.request(), null);
  assert.deepEqual(changed.calls.map(c=>c.command), ['refresh_publish_metadata','load_repository']);
  const h = harness(), pending = deferred(); h.context.invoke = () => pending.promise;
  const update = h.context.refreshAfterPublish(h.request(), null); h.context.repoOpenGeneration += 2;
  pending.resolve(h.metadata); await update; assert.equal(h.state.branches.length, 1);
});

test('concurrent folder navigation falls back to existing reload instead of painting previous entries into the new folder', async () => {
  const h = harness(); h.refs.fileList = {querySelector: () => ({})};
  await h.context.refreshAfterPublish(h.request(), null);
  assert.deepEqual(h.calls.map(c=>c.command), ['refresh_publish_metadata','load_repository']);
});

test('targeted refresh updates the visible properties without rereading submodule details or losing its history', async () => {
  const h = harness();
  h.state.selectedEntry = {relative_path:'done.txt', kind:'file', status:'M', unpushed:true};
  h.refs.details.displayedEntry = {...h.state.selectedEntry, last_commit_id:'history'};
  h.node('#entryLastCommitSection').innerHTML = 'Current history';
  let shown;
  h.context.renderEntryDetails = entry => { shown = entry; h.node('#entryLastCommitSection').innerHTML = 'replaced'; };
  await h.context.refreshAfterPublish(h.request(), null);
  assert.equal(shown.unpushed, false); assert.equal(shown.status, 'M'); assert.equal(shown.last_commit_id, 'history');
  assert.equal(h.node('#entryLastCommitSection').innerHTML, 'Current history');
  assert.deepEqual(h.calls.map(c=>c.command), ['refresh_publish_metadata','indicator']);
});

test('normal Fast publish captures its original target, avoids full reload and prevents double submission', async () => {
  const h = harness(), push = deferred();
  const invoke = h.context.invoke; h.context.invoke = (command, args) => command === 'publish_branch'
    ? (h.calls.push({command,args}), push.promise) : invoke(command,args);
  const run = h.context.confirmPublish({preventDefault(){}}); await tick();
  await h.context.confirmPublish({preventDefault(){}});
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].args.skipSubmoduleSafety, true);
  assert.equal(h.calls[0].args.repositoryPath, 'repo-a');
  push.resolve(); await run;
  assert.equal(h.context.publishInFlight, false); assert.equal(h.refs.publishDialog.open, false);
  assert.deepEqual(h.calls.map(c=>c.command), ['publish_branch','refresh_publish_metadata','indicator']);
});

test('push rejection does not report success or perform a repository/status reload; Safe flag is preserved', async () => {
  const h = harness(); h.refs.publishFastMode.checked = false;
  h.context.invoke = async (command,args) => { h.calls.push({command,args}); throw new Error('Push rejected: non-fast-forward'); };
  await h.context.confirmPublish({preventDefault(){}});
  assert.equal(h.calls[0].args.skipSubmoduleSafety, false);
  assert.deepEqual(h.calls.map(c=>c.command), ['publish_branch','preview']);
  assert.equal(h.refs.publishDialog.open, true); assert.ok(h.messages.some(m=>/Push rejected/.test(m.message)));
  assert.ok(!h.messages.some(m=>m.kind==='success')); assert.equal(h.context.publishInFlight, false);
});

test('successful push with failed UI refresh remains successful and does not offer an automatic retry', async () => {
  const h = harness(); h.context.invoke = async command => { h.calls.push({command}); if (command==='refresh_publish_metadata') throw new Error('disk busy'); };
  await h.context.confirmPublish({preventDefault(){}});
  assert.ok(h.messages.some(m=>m.kind==='success' && /Published/.test(m.message)));
  assert.ok(h.messages.some(m=>/Display refresh failed/.test(m.message)));
  assert.equal(h.calls.filter(c=>c.command==='publish_branch').length, 1);
  assert.equal(h.refs.publishDialog.open, false);
});

test('repository switch during push neither loads nor paints the new project; switch during confirmation cancels dispatch', async () => {
  const h = harness(), push = deferred(); h.context.invoke = (command,args) => { h.calls.push({command,args}); return push.promise; };
  const run = h.context.confirmPublish({preventDefault(){}}); await tick();
  h.state.repository = {path:'repo-b'}; h.context.repoOpenGeneration++;
  push.resolve(); await run;
  assert.deepEqual(h.calls.map(c=>c.command), ['publish_branch']); assert.equal(h.state.repository.path, 'repo-b');
  const h2 = harness(), confirmation = deferred(); h2.context.customConfirm = () => confirmation.promise;
  const run2 = h2.context.confirmPublish({preventDefault(){}}); h2.context.repoOpenGeneration++;
  confirmation.resolve(true); await run2; assert.equal(h2.calls.length, 0);
});

test('Combine & Push uses original inputs, combines once and keeps full reload; Combine Only never pushes', async () => {
  for (const mode of ['combine_push', 'combine_only']) {
    const h = harness(); h.context.mode = mode;
    h.node('#publishCombinedMessage').value = 'Combined message';
    h.context.invoke = async (command,args) => { h.calls.push({command,args}); if (command==='combine_local_commits') return {revision:'newhead',count:2,backup_ref:'backup'}; };
    await h.context.confirmPublish({preventDefault(){}});
    assert.deepEqual(h.calls.map(c=>c.command), mode==='combine_push' ? ['combine_local_commits','publish_branch','load_repository'] : ['combine_local_commits','load_repository']);
    assert.equal(h.calls[0].args.message, 'Combined message');
  }
});

test('Safe overridable warning retries only after approval, preserves credentials/target, and does not combine twice', async () => {
  const h = harness(); h.context.mode = 'combine_push'; h.refs.publishFastMode.checked = false;
  h.node('#publishToken').value = 'test-token'; let pushes = 0;
  h.context.invoke = async (command,args) => {
    h.calls.push({command,args});
    if (command==='combine_local_commits') return {revision:'newhead',count:2,backup_ref:'backup'};
    if (command==='publish_branch' && ++pushes===1) throw new Error('placeholder');
  };
  // IPC rejects strings in the actual desktop, including this exact prefix.
  const invoke = h.context.invoke; h.context.invoke = async (command,args) => {
    try { return await invoke(command,args); } catch { throw 'UNPUSHED_SUBMODULE_OVERRIDABLE::Local URL'; }
  };
  await h.context.confirmPublish({preventDefault(){}});
  assert.equal(h.calls.filter(c=>c.command==='combine_local_commits').length, 1);
  const requests = h.calls.filter(c=>c.command==='publish_branch'); assert.equal(requests.length, 2);
  assert.equal(requests[0].args.overrideUnpushedSubmodules, false); assert.equal(requests[1].args.overrideUnpushedSubmodules, true);
  assert.equal(requests[1].args.skipSubmoduleSafety, false); assert.equal(requests[1].args.accessToken, 'test-token');
  assert.equal(requests[1].args.repositoryPath, 'repo-a');
});

test('publish workers and metadata refresh keep Git status/submodule scans out of the targeted path', () => {
  const status = rust.match(/pub async fn publish_status\([^]*?\n\}/)[0];
  assert.match(status, /off_main_thread/); assert.match(status, /publish_status_inner/);
  const metadata = rust.match(/fn refresh_publish_metadata_inner\([^]*?\n\}/)[0];
  assert.doesNotMatch(metadata, /internal_statuses|cached_full_statuses|load_repository|load_directory|fetch|\.index\(/);
  assert.match(metadata, /unpushed_paths\(&repository_path\)/);
});
