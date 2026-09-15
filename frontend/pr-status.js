(() => {
'use strict';

// Pure pull-request card rendering. Keeping this free of Tauri/GitHub calls
// makes it explicit that reviewer rows use the data returned by the one
// existing on-demand PR request; rendering a person never starts a request.
const PR_LIFECYCLE_LABEL = { draft: 'Draft', open: 'Open', merged: 'Merged', closed: 'Closed' };
const PR_MERGEABLE_LABEL = { mergeable: 'Mergeable', conflicting: 'Conflicting', calculating: 'Calculating…', unknown: 'Unknown' };
const PR_REVIEW_LABEL = { approved: 'Approved', changes_requested: 'Changes requested', review_required: 'Review required', none: 'No reviews yet' };
const PR_REVIEWER_STATE_LABEL = { requested: 'Requested', approved: 'Approved', changes_requested: 'Changes requested', commented: 'Reviewed', dismissed: 'Dismissed', pending: 'Pending' };
const PR_CHECKS_LABEL = { passing: 'Checks passing', failing: 'Checks failing', pending: 'Checks running…', none: 'No checks' };

function escapePrHtml(value = '') {
  return String(value).replace(/[&<>"']/g, char => ({ '&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;' }[char]));
}

function reviewerDisplayName(login) {
  return login === 'copilot-pull-request-reviewer' ? 'Copilot' : `@${login}`;
}

function prReviewerHtml(reviewer, pullRequestUrl) {
  const esc = escapePrHtml;
  const state = String(reviewer?.state || 'commented');
  const label = PR_REVIEWER_STATE_LABEL[state] || state;
  const login = String(reviewer?.login || '').trim();
  if (!login) return '';
  // GraphQL supplies an exact PullRequestReview permalink. GitHub CLI does
  // not currently expose that URL in its JSON shape, so fall back to the PR
  // itself while still showing the same reviewer state.
  const url = String(reviewer?.review_url || pullRequestUrl || '');
  const copilotClass = login === 'copilot-pull-request-reviewer' ? ' pr-reviewer-copilot' : '';
  const content = `<span>${esc(reviewerDisplayName(login))}</span><b>${esc(label)}</b>${url ? '<i>↗</i>' : ''}`;
  return url
    ? `<button class="pr-reviewer pr-reviewer-${esc(state)}${copilotClass}" data-open-url="${esc(url)}" title="${reviewer?.review_url ? 'Open this review on GitHub' : 'Open this pull request on GitHub'}">${content}</button>`
    : `<span class="pr-reviewer pr-reviewer-${esc(state)}${copilotClass}">${content}</span>`;
}

function prCardHtml(pr = {}) {
  const esc = escapePrHtml;
  const reviewers = Array.isArray(pr.reviewers) ? pr.reviewers : [];
  const reviewerRows = reviewers.map(reviewer => prReviewerHtml(reviewer, pr.url)).join('');
  const reviewerSection = reviewerRows
    ? `<div class="pr-reviewers"><span class="pr-reviewers-label">Reviewer activity</span><div>${reviewerRows}</div></div>`
    : '';
  return `<div class="pr-card">
    <div class="pr-card-top"><span class="pr-number">#${pr.number}</span><span class="pr-badge pr-lifecycle-${esc(pr.state)}">${esc(PR_LIFECYCLE_LABEL[pr.state] || pr.state)}</span></div>
    <div class="pr-title">${esc(pr.title || '(no title)')}</div>
    <div class="pr-branches"><code>${esc(pr.source_branch)}</code><span class="pr-branch-arrow">→</span><code>${esc(pr.target_branch)}</code></div>
    <div class="pr-badges">
      <span class="pr-badge pr-mergeable-${esc(pr.mergeable)}">${esc(PR_MERGEABLE_LABEL[pr.mergeable] || pr.mergeable)}</span>
      <span class="pr-badge pr-review-${esc(pr.review_summary)}">${esc(PR_REVIEW_LABEL[pr.review_summary] || pr.review_summary)}</span>
      <span class="pr-badge pr-checks-${esc(pr.checks_status)}">${esc(PR_CHECKS_LABEL[pr.checks_status] || pr.checks_status)}</span>
    </div>
    ${reviewerSection}
    <button class="pr-open-link" data-open-url="${esc(pr.url)}" ${pr.url ? '' : 'disabled'}>Open pull request ↗</button>
  </div>`;
}

const api = { prCardHtml, prReviewerHtml, reviewerDisplayName };
if (typeof window !== 'undefined') window.GitDrillDownPr = api;
if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
