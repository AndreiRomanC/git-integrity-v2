'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { prCardHtml, prReviewerHtml, prCheckHtml, reviewerDisplayName } = require('../frontend/pr-status.js');

const pr = {
  number: 282,
  title: 'Implementation of LAH',
  source_branch: 'feature/lah',
  target_branch: 'main',
  state: 'open',
  mergeable: 'mergeable',
  review_summary: 'review_required',
  checks_status: 'passing',
  url: 'https://github.example/eng/repo/pull/282',
};

test('PR card renders requested reviewers without making any request', () => {
  let calls = 0;
  global.fetch = () => { calls += 1; throw new Error('must not fetch'); };
  const html = prCardHtml({ ...pr, reviewers: [{ login: 'alice', state: 'requested', review_url: '' }] });
  assert.match(html, /@alice/);
  assert.match(html, /Requested/);
  assert.match(html, /https:\/\/github\.example\/eng\/repo\/pull\/282/);
  assert.equal(calls, 0);
  delete global.fetch;
});

test('a completed collaborator review links to its exact GitHub permalink', () => {
  const url = 'https://github.example/eng/repo/pull/282#pullrequestreview-1234';
  const html = prReviewerHtml({ login: 'reviewer', state: 'approved', review_url: url }, pr.url);
  assert.match(html, /@reviewer/);
  assert.match(html, /Approved/);
  assert.match(html, /pullrequestreview-1234/);
});

test('Copilot reviewer has an understandable display name', () => {
  assert.equal(reviewerDisplayName('copilot-pull-request-reviewer'), 'Copilot');
  const html = prCardHtml({ ...pr, reviewers: [{ login: 'copilot-pull-request-reviewer', state: 'commented', review_url: '' }] });
  assert.match(html, />Copilot</);
  assert.match(html, /Reviewed/);
});

test('PR cards remain compatible when GitHub returns no reviewer detail', () => {
  const html = prCardHtml(pr);
  assert.match(html, /Review required/);
  assert.doesNotMatch(html, /Reviewer activity/);
});

test('required checks expose GitHub-provided Details links and a comment form', () => {
  const html = prCardHtml({ ...pr, checks: [{ name: 'Collaborator', status: 'pending', details_url: 'https://github.example/eng/repo/runs/42' }] });
  assert.match(html, /Collaborator/);
  assert.match(html, /Pending/);
  assert.match(html, /data-open-check-url="https:\/\/github\.example\/eng\/repo\/runs\/42"/);
  assert.match(html, /data-pr-comment-form/);
  assert.match(html, /Send a pull request comment/);
  assert.match(prCheckHtml({ name: 'build-status', status: 'pending', details_url: '' }), /build-status/);
});

test('the classic browser script exports without leaking names into app.js global scope', () => {
  const context = vm.createContext({ window: {} });
  const source = fs.readFileSync(require.resolve('../frontend/pr-status.js'), 'utf8');
  vm.runInContext(source, context);
  assert.equal(typeof context.window.GitDrillDownPr.prCardHtml, 'function');
  assert.doesNotThrow(() => vm.runInContext('const { prCardHtml } = window.GitDrillDownPr;', context));
});

// Exercise the actual sidebar factory, with only DOM and IPC substituted.
function panelHarness(branch = 'main') {
  const app = fs.readFileSync(require.resolve('../frontend/app.js'), 'utf8');
  const start = app.indexOf('const PR_STATE_LABELS =');
  const end = app.indexOf('// Message D, point 5:', start);
  assert.ok(start >= 0 && end > start);
  const source = app.slice(start, end);
  const listeners = [], calls = [];
  const root = { innerHTML: '', classList: { toggle() {} },
    addEventListener(type, callback) { if (type === 'click') listeners.push(callback); } };
  const current = { path: '/fixture/small', branch };
  const context = vm.createContext({
    window: { GitDrillDownPr: { prCardHtml } }, esc: value => String(value),
    invoke: (command, args) => new Promise((resolve, reject) => calls.push({ command, args, resolve, reject })),
  });
  vm.runInContext(source, context);
  const panel = context.createPrStatusPanel(root, {
    getRepositoryPath: () => current.path, getBranch: () => current.branch, getContextLabel: () => 'parent',
  });
  const refresh = () => listeners.forEach(callback => callback({ target: { closest: selector =>
    selector === '[data-pr-status-action]' ? { dataset: { prStatusAction: 'refresh' } } : null } }));
  return { root, current, panel, calls, refresh };
}
const tick = () => new Promise(resolve => setImmediate(resolve));

test('main shows four incoming PRs as into this branch, never as outgoing or a repository-wide list', async () => {
  const h = panelHarness();
  h.panel.setExpanded(true);
  assert.equal(h.calls.length, 0, 'opening the panel does not authenticate or query');
  h.refresh();
  assert.equal(h.calls[0].command, 'pr_status');
  assert.equal(h.calls[0].args.branch, 'main');
  h.calls[0].resolve({ state: 'ok', branch: 'main', queried_repo: 'github.example/eng/small',
    outgoing_pull_requests: [], incoming_pull_requests: [1, 2, 3, 4].map(number => ({ ...pr, number })) });
  await tick();
  assert.match(h.root.innerHTML, /Pull requests into this branch \(4\)/);
  assert.doesNotMatch(h.root.innerHTML, /Pull requests from this branch/);
});

test('feature PRs remain outgoing and an empty result does not fall back to repository PRs', async () => {
  const h = panelHarness('feature/lah');
  h.refresh();
  h.calls[0].resolve({ state: 'ok', outgoing_pull_requests: [pr], incoming_pull_requests: [] });
  await tick();
  assert.match(h.root.innerHTML, /Pull requests from this branch/);
  assert.doesNotMatch(h.root.innerHTML, /Pull requests into this branch/);
  h.current.branch = 'feature/no-pr';
  h.panel.refreshIfContextChanged();
  h.refresh();
  h.calls[1].resolve({ state: 'no_open_pr', branch: 'feature/no-pr', queried_repo: 'github.example/eng/small',
    outgoing_pull_requests: [], incoming_pull_requests: [] });
  await tick();
  assert.match(h.root.innerHTML, /No open pull request for <code>feature\/no-pr<\/code>/);
  assert.doesNotMatch(h.root.innerHTML, /Implementation of LAH/);
  assert.equal(h.calls.length, 2, 'no repository-wide fallback query');
});

test('an old repository PR response cannot replace the current repository result', async () => {
  const h = panelHarness('feature/lah');
  h.refresh();
  h.current.path = '/fixture/other';
  h.current.branch = 'main';
  h.panel.refreshIfContextChanged();
  h.refresh();
  h.calls[1].resolve({ state: 'no_open_pr', branch: 'main', queried_repo: 'github.example/eng/other' });
  await tick();
  h.calls[0].resolve({ state: 'ok', outgoing_pull_requests: [pr], incoming_pull_requests: [] });
  await tick();
  assert.match(h.root.innerHTML, /github\.example\/eng\/other/);
  assert.doesNotMatch(h.root.innerHTML, /Implementation of LAH/);
});
