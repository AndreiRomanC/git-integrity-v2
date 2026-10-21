const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const repository = fs.readFileSync(path.join(__dirname, '../src-tauri/src/repository.rs'), 'utf8');
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
function functionSource(name, async = false) {
  const start = app.indexOf(`${async ? 'async ' : ''}function ${name}(`);
  assert.ok(start >= 0, `missing ${name}`);
  return app.slice(start, app.indexOf('\n}', start) + 2);
}
const tick = () => new Promise(resolve => setImmediate(resolve));
function harness() {
  const entry = name => ({ name, relative_path: `modules/${name}`, kind: 'submodule', tracked: true, status_known: false, status: '', size: 0, modified: 0 });
  const state = { repository: { path: '/parent', current_branch: 'main' }, view: 'explorer', currentPath: 'modules', entries: [entry('a'), entry('b')] };
  const calls = [], renders = [], errors = [], histories = [], logs = [], timers = new Map(), section = { innerHTML: '' };
  let timerId = 0;
  const sandbox = { state, repoOpenGeneration: 1, performance: { now: () => 100 }, jsPerfLog: line => logs.push(line), refs: { editorDialog: { open: false } },
    render() { assert.fail('Selecting an entry must not rebuild branches, PR panels or the Working Tree'); },
    renderExplorer() {}, renderCommitScope() {}, clearDetails: text => renders.push({ empty: text }), renderEntryDetails: model => renders.push(model),
    renderEntryLastCommitInner: model => { histories.push(model); return JSON.stringify(model); },
    $: selector => selector === '#entryLastCommitSection' ? section : null,
    invoke(command, args) { const pending = deferred(); calls.push({ command, args, pending }); return pending.promise; },
    handleError: error => errors.push(String(error)),
    setTimeout: fn => { const id = ++timerId; timers.set(id, fn); return id; }, clearTimeout: id => timers.delete(id) };
  vm.createContext(sandbox);
  vm.runInContext('let previousNavigationAt = null;\n' + functionSource('navigationTraceFields'), sandbox);
  vm.runInContext(functionSource('createRequestGuard'), sandbox);
  const start = app.indexOf('let pendingEntryDetailsTimeout = null;');
  vm.runInContext(app.slice(start, app.indexOf('\nfunction selectedScope()', start)), sandbox);
  return { sandbox, state, calls, renders, errors, histories, logs, timers, section,
    select: name => sandbox.selectEntry(`modules/${name}`),
    details: (index, name) => calls[index].pending.resolve({ ...state.entries.find(e => e.name === name), status_known: true, last_commit_id: null, submodule_current_branch: 'feature' }) };
}
test('selection updates only Explorer and the selected commit scope, without rebuilding the shell', async () => {
  const h = harness(), repaints = [];
  h.sandbox.renderExplorer = () => repaints.push('explorer');
  h.sandbox.renderCommitScope = () => repaints.push('scope');
  for (const name of ['a', 'b', 'a']) {
    const index = h.calls.length, pending = h.select(name); h.details(index, name); await pending;
  }
  assert.deepEqual(repaints, ['explorer','scope','explorer','scope','explorer','scope']);
  assert.deepEqual(h.calls.map(c => c.command), ['entry_details','entry_last_commit','entry_details','entry_last_commit','entry_details','entry_last_commit']);
});
test('selected-scope availability still follows selected files, folders and repository root', () => {
  const state = { repository: { name: 'repo' }, selectedEntry: { name: 'a', relative_path: 'modules/a' }, currentPath: 'modules', changes: [{ path: 'modules/a/file.c' }] };
  const button = {}, sandbox = { state, refs: { commitScope: button } };
  vm.runInNewContext(functionSource('renderCommitScope'), sandbox);
  sandbox.renderCommitScope(); assert.equal(button.textContent, 'Commit a'); assert.equal(button.disabled, false);
  state.selectedEntry = { name: 'b', relative_path: 'modules/b' };
  sandbox.renderCommitScope(); assert.equal(button.disabled, true); assert.match(button.title, /Nothing to commit/);
  state.selectedEntry = null; sandbox.renderCommitScope(); assert.equal(button.textContent, 'Commit folder'); assert.equal(button.disabled, false);
  state.currentPath = ''; sandbox.renderCommitScope(); assert.equal(button.textContent, 'Commit repository'); assert.equal(button.disabled, false);
});
test('listing appears before details, then history loads separately with no additional commands', async () => {
  const h = harness(), pending = h.select('a');
  assert.equal(h.renders[0].name, 'a'); assert.equal(h.renders[0].details_loading, true);
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].command, 'entry_details');
  h.details(0, 'a'); await pending;
  assert.equal(h.renders[1].details_loading, undefined); assert.equal(h.renders[1].submodule_current_branch, 'feature');
  assert.equal(h.renders[1].last_commit_id, undefined, 'do not claim No commit while history is pending');
  assert.equal(h.calls.length, 2); assert.equal(h.calls[1].command, 'entry_last_commit');
  h.calls[1].pending.resolve({ id: 'abc', subject: 'Updated module', author: 'Author', date: '2026-10-18' }); await tick();
  assert.equal(h.histories.at(-1).last_commit_id, 'abc');
  assert.deepEqual(h.logs.map(line => line.match(/phase=([^ ]+)/)[1]), ['basic_dom_ready', 'request_started', 'details_dom_ready', 'history_dom_ready']);
  assert.ok(h.logs.every(line => !line.includes('/parent') && !line.includes('modules/a') && !line.includes('Updated module')));
});
test('A → B → A rejects the first A even though the selected path matches again', async () => {
  const h = harness(), a1 = h.select('a'), b = h.select('b'), a2 = h.select('a');
  h.details(2, 'a'); await a2; const count = h.renders.length;
  h.details(0, 'a'); await a1; h.details(1, 'b'); await b;
  assert.equal(h.renders.length, count); assert.equal(h.calls.filter(c => c.command === 'entry_last_commit').length, 1);
});
test('late history success or failure cannot replace another selection', async () => {
  for (const fail of [false, true]) {
    const h = harness(), a = h.select('a'); h.details(0, 'a'); await a;
    const b = h.select('b');
    if (fail) h.calls[1].pending.reject('old history error'); else h.calls[1].pending.resolve({ id: 'old' });
    await tick(); assert.equal(h.histories.length, 0); assert.equal(h.errors.length, 0);
    h.details(2, 'b'); await b;
  }
});
test('repository changes, reloads, navigation, or leaving Explorer reject old details', async () => {
  const changes = [h => { h.state.repository.path = '/other'; }, h => { h.sandbox.repoOpenGeneration++; },
    h => { h.state.currentPath = 'other'; }, h => { h.state.view = 'graph'; }];
  for (const change of changes) {
    const h = harness(), pending = h.select('a'); change(h); h.details(0, 'a'); await pending;
    assert.equal(h.renders.length, 1); assert.equal(h.calls.length, 1);
  }
});
test('errors for obsolete selections stay silent; current errors remain explicitly unverified', async () => {
  const h = harness(), a = h.select('a'), b = h.select('b');
  h.calls[0].pending.reject('old error'); await a; assert.equal(h.errors.length, 0);
  h.calls[1].pending.reject('current error'); await b;
  assert.equal(h.renders.at(-1).details_loading, true); assert.equal(h.renders.at(-1).details_error, true);
  assert.deepEqual(h.errors, ['current error']); assert.equal(h.calls.length, 2);
});
test('deferred selection shows basics immediately but does not query an abandoned folder', async () => {
  const h = harness(); await h.sandbox.selectEntry('modules/a', { deferMs: 250 });
  assert.equal(h.renders[0].details_loading, true); assert.equal(h.calls.length, 0);
  h.state.currentPath = 'modules/a'; const run = [...h.timers.values()][0]; run();
  assert.equal(h.calls.length, 0);
});
test('a current history failure is unavailable, not No commit', async () => {
  const h = harness(), pending = h.select('a'); h.details(0, 'a'); await pending;
  h.calls[1].pending.reject('history error'); await tick(); assert.equal(h.histories.at(-1).last_commit_error, true);
});
test('deleted items keep their existing history-only path and an empty selection clears the old panel', async () => {
  const h = harness(); h.state.entries[0].kind = 'deleted-submodule'; await h.select('a');
  assert.deepEqual(h.calls.map(c => c.command), ['entry_last_commit']);
  await h.select('missing'); assert.equal(h.renders.at(-1).empty, 'Select a file or folder');
});
function renderHarness() {
  let buttons = [];
  const details = { html: '', closest: () => ({ classList: { remove() {} } }),
    set innerHTML(html) { this.html = html; buttons = [...html.matchAll(/<button[^>]*data-detail-action="([^"]+)"[^>]*>/g)].map(match => ({
      disabled: /\sdisabled(?:\s|>)/.test(match[0]), dataset: { detailAction: match[1] },
      setAttribute(name, value) { this[name] = value; }, addEventListener(type, fn) { this.click = fn; } })); },
    querySelectorAll: selector => selector === '[data-detail-action]' ? buttons : [] };
  const sandbox = { refs: { details }, SubmoduleStateModel: { presentation: () => ({ short: 'Tracked, clean', actionable: false }) },
    esc: value => String(value), entryGitSummary: () => 'Tracked, clean', formatSize: () => '0 B', formatModified: () => 'date',
    submoduleBoundaryFor: () => null, renderPersonalNoteSection: () => '', commitSubjectHtml: value => value,
    specDetailRows: () => '', tagListHtml: () => '', untrackedItemLabel: () => 'Untracked', handleDetailAction() { throw new Error('An unverified action must not execute'); } };
  vm.createContext(sandbox);
  for (const name of ['entryGitSummary', 'contextActionSectionHtml', 'submoduleRepositoryRowsHtml', 'submoduleCheckoutBadgeHtml', 'renderEntryLastCommitInner', 'renderEntryDetails']) vm.runInContext(functionSource(name), sandbox);
  return { sandbox, details, buttons: () => buttons };
}
test('pending and failed submodule panels never claim detached, uninitialized, missing remote, or missing commit', () => {
  for (const [failed, statusKnown] of [[false, false], [true, false], [false, true], [true, true]]) {
    const h = renderHarness();
    h.sandbox.renderEntryDetails({ name: 'module', relative_path: 'modules/module', kind: 'submodule', details_loading: true,
      details_error: failed, tracked: true, status_known: statusKnown, submodule_state: '', last_commit_id: undefined });
    assert.match(h.details.html, failed ? /Details could not be verified/ : /Checking current Git details/);
    // Check status/commit fields, not disabled buttons' pre-existing descriptive tooltips.
    assert.doesNotMatch(h.details.html, /<strong>(?:No commit|Detached HEAD|Not initialized|Not configured|Tracked, clean)<\/strong>/);
    assert.ok(h.buttons().length > 0);
    for (const button of h.buttons()) { assert.equal(button.disabled, true); button.click(); assert.match(button['data-tooltip'], /Checking|unavailable/); }
  }
});
test('complete details preserve existing checkout and action behavior', () => {
  const h = renderHarness();
  h.sandbox.renderEntryDetails({ name: 'module', relative_path: 'modules/module', kind: 'submodule', tracked: true, status: '',
    submodule_initialized: true, submodule_current_branch: 'feature', submodule_state: 'clean', submodule_commit_id: 'abc', last_commit_id: undefined });
  assert.match(h.details.html, /On branch · feature/); assert.doesNotMatch(h.details.html, /Checking current Git details/);
  assert.equal(h.buttons().find(b => b.dataset.detailAction === 'subcommit').disabled, false);
  assert.match(h.details.html, /Loading…/);
});
test('backend only adds phase timings, without splitting the command or adding scans', () => {
  const start = repository.indexOf('fn entry_details_inner('), end = repository.indexOf('// The "last commit touching this path" lookup', start);
  const body = repository.slice(start, end);
  for (const phase of ['filesystem', 'metadata', 'submodule_urls', 'state_and_remote_relation', 'push_summary', 'unpushed_commits', 'head_commit', 'head_tags', 'total']) assert.ok(body.includes(`log_phase("${phase}"`));
  assert.equal((body.match(/cached_git_metadata\(/g) || []).length, 1);
  assert.equal((body.match(/inspect_submodule_state_in\(/g) || []).length, 1);
  assert.doesNotMatch(body, /refresh_status|git\([^\n]*fetch/);
});
test('submodule properties reuse one selected lookup and strict open without enumerating every submodule', () => {
  const lookup = repository.slice(repository.indexOf('fn submodule_url_and_branch_in('), repository.indexOf('fn submodule_value('));
  const browser = repository.slice(repository.indexOf('fn submodule_browser_base('), repository.indexOf('fn launch_browser('));
  const details = repository.slice(repository.indexOf('fn entry_details_inner('), repository.indexOf('// The "last commit touching this path" lookup'));
  assert.doesNotMatch(lookup + browser + details, /\.submodules\(/);
  assert.match(lookup, /find_submodule\(path\)/);
  assert.match(lookup, /normalized\(item\.path\(\)\) ===? path/);
  assert.equal((details.match(/submodule_url_and_branch_in\(/g) || []).length, 1);
  assert.equal((details.match(/internal_submodule_repository\(/g) || []).length, 1);
  assert.match(details, /submodule_browser_base_in\(repo, sub_repo\.as_ref\(\)\.ok\(\), url\.as_deref\(\)\)/);
  assert.doesNotMatch(browser, /git\(|fetch\(|statuses\(|cached_|OnceLock/);
});
