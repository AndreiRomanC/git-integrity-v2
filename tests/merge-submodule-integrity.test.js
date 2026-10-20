'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const app = fs.readFileSync(path.join(__dirname, '../frontend/app.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../frontend/index.html'), 'utf8');
const section = (start, end) => app.slice(app.indexOf(start), app.indexOf(end, app.indexOf(start)));

function harness() {
  const events = [], messages = [], calls = [];
  const target = { targetPath: '', isSubmodule: false };
  const review = { target, items: [{ path: 'vendor/dep', result: 'new', selectedRevision: 'new' }] };
  const state = { repository: { path: 'parent' }, submoduleMergeReview: review, conflictSession: { updatePromptShown: false } };
  const refs = {
    conflictsDialog: { open: true }, conflictsCommitMessage: { value: 'Merge after checking' },
    confirmCompleteMerge: { click: () => events.push('automatic-commit-click') },
    submoduleMergeReviewDialog: { close: () => events.push('review-closed') },
    submoduleMergeReviewStatus: { textContent: '' },
    applySubmoduleMergeReviewOnly: {}, confirmSubmoduleMergeReview: {},
    conflictsList: { innerHTML: '', querySelectorAll: () => [] },
  };
  const context = {
    state, refs, repoOpenGeneration: 1, esc: value => String(value),
    status: text => messages.push(text), showOperationToast() {},
    customConfirm: async () => { events.push('confirm-update'); return true; },
    initAndUpdateSubmodulesFromActions: async () => { events.push('update'); return { success: true }; },
    invoke: async (command, args) => { calls.push({ command, args }); },
    refreshAfterMerge: async () => events.push('refresh'),
    handleError: error => messages.push(String(error)),
    openSubmoduleMergeReviewDialog: async () => { events.push('review-opened'); return true; },
    currentConflictItems: () => [],
  };
  vm.createContext(context);
  vm.runInContext(section('function shortRevision(', 'async function openSubmoduleMergeReviewDialog('), context);
  vm.runInContext(section('async function maybeOfferSubmoduleUpdateBeforeMergeCommit(', 'function shortRevision('), context);
  vm.runInContext(section('async function applySubmoduleMergeReviewChoices(', 'async function openPendingMergeFromBanner('), context);
  vm.runInContext(section('async function maybeOfferSubmoduleReviewAfterConflictsResolved(', 'async function refreshConflictsDialog('), context);
  return { context, state, refs, review, target, events, messages, calls };
}

test('resolved conflicts open pointer review once and never ask for or trigger a commit', async () => {
  const h = harness();
  await h.context.maybeOfferSubmoduleReviewAfterConflictsResolved(h.target, []);
  await h.context.maybeOfferSubmoduleReviewAfterConflictsResolved(h.target, []);
  assert.deepEqual(h.events, ['review-opened']);
  assert.equal(h.calls.length, 0);
});

test('stash, unresolved conflicts and child-repository conflicts do not start the parent update flow', async () => {
  const h = harness();
  await h.context.maybeOfferSubmoduleReviewAfterConflictsResolved({ kind: 'stash' }, []);
  await h.context.maybeOfferSubmoduleReviewAfterConflictsResolved(h.target, [{ path: 'conflict' }]);
  await h.context.maybeOfferSubmoduleReviewAfterConflictsResolved({ isSubmodule: true }, []);
  assert.equal(h.events.length, 0);
});

test('Apply preserves a prepared removal, then offers update without making a commit', async () => {
  const h = harness();
  h.review.items = [{ path: 'vendor/removed', result: null, selectedRevision: null }];
  await h.context.applySubmoduleMergeReviewOnly();
  assert.equal(h.calls.length, 0, 'a prepared deletion is not replaced by an old pointer');
  assert.deepEqual(h.events, ['review-closed', 'refresh', 'confirm-update', 'update']);
  assert.match(h.messages.at(-1), /Merge is still pending/);
});

test('missing registration blocks every choice before partial writes or an update can start', async () => {
  const h = harness();
  h.review.items = [
    { path: 'vendor/valid', result: 'old', selectedRevision: 'new' },
    { path: 'vendor/orphan', result: 'old', selectedRevision: 'old', registration_issue: true },
  ];
  await h.context.applySubmoduleMergeReviewOnly();
  assert.equal(h.calls.length, 0);
  assert.equal(h.events.length, 0);
  assert.match(h.refs.submoduleMergeReviewStatus.textContent, /vendor\/orphan.*Update\/init cannot fix/);
});

test('an explicit orphan removal sends only the empty gitlink choice before offering update', async () => {
  const h = harness();
  h.review.items = [{ path: 'vendor/orphan', result: 'old', selectedRevision: null, registration_issue: true }];
  await h.context.applySubmoduleMergeReviewOnly();
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].command, 'apply_submodule_merge_revision');
  assert.equal(h.calls[0].args.revision, '');
  assert.ok(h.events.indexOf('refresh') < h.events.indexOf('update'));
});

test('a failed or incomplete update is never overwritten with a success or commit message', async () => {
  for (const result of [{ success: false }, { success: true, refresh_failed: true }]) {
    const h = harness();
    h.context.initAndUpdateSubmodulesFromActions = async () => {
      h.messages.push('Update incomplete; inspect the error');
      return result;
    };
    await h.context.maybeOfferSubmoduleUpdateBeforeMergeCommit(h.review);
    assert.equal(h.messages.at(-1), 'Update incomplete; inspect the error');
    assert.equal(h.calls.length, 0);
  }
});

test('declining update leaves the merge pending without running Git', async () => {
  const h = harness();
  h.context.customConfirm = async () => false;
  await h.context.maybeOfferSubmoduleUpdateBeforeMergeCommit(h.review);
  assert.equal(h.events.length, 0);
  assert.match(h.messages.at(-1), /Merge is still pending.*were not updated/);
});

test('a late update confirmation cannot run against another repository', async () => {
  const h = harness();
  h.context.customConfirm = async () => { h.state.repository.path = 'other'; return true; };
  await h.context.maybeOfferSubmoduleUpdateBeforeMergeCommit(h.review);
  assert.equal(h.events.length, 0);
});

test('an old pointer-review response cannot open against a different repository', async () => {
  const h = harness();
  vm.runInContext(section('async function openSubmoduleMergeReviewDialog(', 'async function completeMergeAfterSubmoduleReview('), h.context);
  let finish;
  h.context.invoke = () => new Promise(resolve => { finish = resolve; });
  const pending = h.context.openSubmoduleMergeReviewDialog(h.target);
  h.state.repository.path = 'other';
  finish({ items: [{ path: 'dep' }] });
  assert.equal(await pending, false);
  assert.equal(h.events.length, 0);
});

test('changing repository during choices cannot send the next choice to that repository', async () => {
  const h = harness();
  h.review.items = ['one', 'two'].map(name => ({ path: `vendor/${name}`, result: 'old', selectedRevision: 'new' }));
  h.context.invoke = async (command, args) => {
    h.calls.push({ command, args });
    h.state.repository.path = 'other';
  };
  await assert.rejects(h.context.applySubmoduleMergeReviewChoices(h.review), /Repository context changed/);
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].args.repositoryPath, 'parent');
});

test('the normal refresh after a successful update can change the generation without hiding pending-merge status', async () => {
  const h = harness();
  h.context.initAndUpdateSubmodulesFromActions = async () => { h.context.repoOpenGeneration++; return { success: true }; };
  await h.context.maybeOfferSubmoduleUpdateBeforeMergeCommit(h.review);
  assert.match(h.messages.at(-1), /Submodules updated\. Merge is still pending/);
});

test('submodule deletion sides are selectable, while missing ordinary-file sides remain disabled', () => {
  const h = harness();
  vm.runInContext(section('function renderConflictsList(', 'function conflictRepositoryPath('), h.context);
  h.context.currentConflictItems = () => [{ path: 'dep', kind: 'submodule', has_ours: true, has_theirs: false }];
  h.context.renderConflictsList(h.target);
  assert.match(h.refs.conflictsList.innerHTML, /data-resolve="theirs"\s+data-tooltip=.*Remove gitlink · Incoming/);
  h.context.currentConflictItems = () => [{ path: 'file', kind: 'file', has_ours: true, has_theirs: false }];
  h.context.renderConflictsList(h.target);
  assert.match(h.refs.conflictsList.innerHTML, /data-resolve="theirs" disabled/);
});

test('Apply/update is primary; committing remains an explicit separate action', () => {
  assert.match(html, /id="applySubmoduleMergeReviewOnly" class="confirm">Apply · update submodules/);
  assert.match(html, /id="confirmSubmoduleMergeReview">Commit merge/);
  assert.doesNotMatch(app, /Complete the merge commit now\?|confirmCompleteMerge\.click\(/);
});

test('the result label describes the actual selected pointer, not the clicked source name', () => {
  const h = harness();
  const item = { current: 'aaaa', incoming: 'bbbb', result: 'bbbb', selectedRevision: 'bbbb', selectedSource: 'result' };
  assert.equal(h.context.submoduleReviewChoiceLabel(item), 'Result: Incoming');
  item.selectedRevision = 'aaaa';
  item.selectedSource = 'custom';
  assert.equal(h.context.submoduleReviewChoiceLabel(item), 'Result: Current');
  item.incoming = 'aaaa';
  assert.equal(h.context.submoduleReviewChoiceLabel(item), 'Result: Current = Incoming');
  item.selectedRevision = 'cccc';
  assert.equal(h.context.submoduleReviewChoiceLabel(item), 'Result: Other commit');
  assert.equal(h.context.submoduleReviewChoiceClass(item), 'custom');
});

test('an absent selected side is clearly a removal, even when Current has a pointer', () => {
  const h = harness();
  const item = { path: 'vendor/removed', current: 'aaaa', incoming: null, result: 'aaaa', selectedRevision: null, selectedSource: 'incoming' };
  const row = h.context.submoduleReviewRowHtml(item);
  assert.match(row, /Result: Remove submodule/);
  assert.match(row, /Not applied yet/);
  assert.match(row, /data-review-action="incoming" data-revision-none="true" class="active" aria-pressed="true"/);
  assert.equal(h.context.submoduleReviewChoiceClass(item), 'removed');
});

test('prepared results are distinguished from pending choices without implying a new write', () => {
  const h = harness();
  const item = { path: 'vendor/dep', current: 'aaaa', incoming: 'bbbb', result: 'bbbb', selectedRevision: 'bbbb', selectedSource: 'result' };
  assert.match(h.context.submoduleReviewRowHtml(item), /Prepared in index/);
  item.selectedSource = 'incoming';
  assert.match(h.context.submoduleReviewRowHtml(item), /Already matches index/);
  item.selectedRevision = 'aaaa';
  item.selectedSource = 'current';
  assert.match(h.context.submoduleReviewRowHtml(item), /Not applied yet/);
  assert.equal(h.calls.length, 0);
});

test('registration warnings remain outside collapsed details and never silently remove a pointer', () => {
  const h = harness();
  const item = { path: 'vendor/orphan', current: 'aaaa', result: 'aaaa', selectedRevision: 'aaaa', selectedSource: 'result', registration_issue: true, suspicious: true };
  const row = h.context.submoduleReviewRowHtml(item);
  assert.ok(row.indexOf('Blocked: no matching entry') < row.indexOf('<details'));
  assert.match(row, /<details class="submodule-review-details">/);
  assert.equal(item.selectedRevision, 'aaaa');
  item.selectedRevision = null;
  assert.match(h.context.submoduleReviewRowHtml(item), /Removal selected to resolve/);
});

test('prepared-result diagnostics stay identified separately from a new selection', () => {
  const h = harness();
  const item = { path: 'vendor/dep', current: 'aaaa', incoming: 'bbbb', result: 'bbbb', selectedRevision: 'aaaa', selectedSource: 'current', status: 'Incoming applied', local_checkout: null };
  const row = h.context.submoduleReviewRowHtml(item);
  assert.match(row, /Result: Current/);
  assert.match(row, /Prepared: Incoming applied/);
  assert.match(row, /Folder on disk<\/span><code title="Not available">Not available/);
});
