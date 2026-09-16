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
