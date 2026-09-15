// ---- Submodule branch/tag selector data shaping ---------------------------
// Pulled out of app.js into its own file, loaded the same two ways
// graph-model.js already is (see that file's own banner for why): as a plain
// global script in the webview, and as a plain CommonJS module from Node's
// built-in test runner. No build step either way.
//
// Submodule-branch-selector report, point 3: "Avoid confusing duplicate
// rows. A local branch and its tracking remote should be visually grouped.
// Put branches existing only on the remote in a separate 'Remote only'
// section." The backend (submodule_versions) already lists local branches
// and remote-tracking branches as two separate flat entries with no relation
// between them beyond a shared commit — this is the pure logic that decides
// which remote-tracking entries are "the same branch, already shown via its
// local upstream" (dropped) versus genuinely remote-only (kept, in their own
// section), so it's checkable the same way graph-model.js's own selection
// functions already are.
//
// A local branch's own `upstream` field (set by submodule_versions,
// "<remote>/<branch>" shorthand) is the only signal used to match it to a
// remote-tracking entry's `name` — never a guess from the branch's own name,
// since a local branch can track a differently-named remote one.
function groupSubmoduleBranchVersions(versions) {
  const all = versions || [];
  const local = all.filter(version => version.kind === 'branch');
  const remote = all.filter(version => version.kind === 'remote');
  const trackedRemoteNames = new Set(local.filter(version => version.upstream).map(version => version.upstream));
  const remoteOnly = remote.filter(version => !trackedRemoteNames.has(version.name));
  return { local, remoteOnly };
}

// Pure row renderer: all branch/tag data is supplied by the one existing
// submodule_versions response. Keeping this helper free of invoke/fetch/Git
// calls makes the "show the pointed-to commit" feature effectively free per
// row and lets the exact display contract be covered by the Node tests.
function escapeSubmoduleVersionHtml(value = '') {
  return String(value).replace(/[&<>"']/g, char => ({ '&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;' }[char]));
}

function submoduleCurrentPresentation(data = {}) {
  const revision = String(data.current_revision || '');
  const parentRevision = String(data.parent_revision || '');
  const short = revision.slice(0, 8) || 'unknown';
  const parentShort = parentRevision.slice(0, 8);
  const matchesProject = Boolean(revision && parentRevision && revision === parentRevision);
  if (!data.current_branch && matchesProject) {
    return {
      text: `Detached HEAD @ ${short}`,
      help: 'The parent project records this exact commit. Branch rows are saved pointers, not the active checkout. Use Checkout to attach HEAD to one.',
    };
  }
  if (!data.current_branch) {
    return {
      text: `Detached HEAD @ ${short}`,
      help: parentShort
        ? `This detached commit is active; the project records ${parentShort}. Branches below are inactive until Checkout.`
        : 'This detached commit is active. Branches below are inactive until Checkout.',
    };
  }
  return {
    text: `${data.current_branch} @ ${short}`,
    help: matchesProject
      ? 'This branch is active and currently matches the version recorded by the parent project.'
      : parentShort
        ? `This branch is active; the parent project still records ${parentShort}. Stage the submodule only when you want to update that project reference.`
        : 'This branch is active. Use explicit actions below to switch or match its remote.',
  };
}

function submoduleCurrentContextHtml(data = {}) {
  const esc = escapeSubmoduleVersionHtml;
  const revision = String(data.current_revision || '');
  const short = revision.slice(0, 8) || 'unknown';
  const parent = String(data.parent_revision || '');
  const containing = Array.isArray(data.current_containing_branches) ? data.current_containing_branches : [];
  const versions = Array.isArray(data.versions) ? data.versions : [];
  const currentCommit = versions.find(item => item.kind === 'commit' && item.revision === revision) || {};
  const exactTips = versions.filter(item => ['branch', 'remote'].includes(item.kind) && item.revision === revision).map(item => item.name);
  let relation;
  if (data.current_branch) relation = `Attached to branch ${data.current_branch}.`;
  else if (exactTips.length) relation = `Detached at the tip of: ${exactTips.join(', ')}. Checkout a local branch to attach HEAD.`;
  else if (containing.length) relation = `Detached inside the history of: ${containing.join(', ')}. The branch tips are at newer commits.`;
  else relation = 'Detached and not reachable from currently known local or remote-tracking branches.';
  const projectRelation = parent && parent === revision
    ? 'The parent project records this exact commit.'
    : parent ? `The parent project currently records ${parent.slice(0, 8)}.` : 'The parent project version could not be determined.';
  return `<section class="version-current-context">
    <div><span>ACTIVE CHECKOUT</span><strong>${esc(short)}</strong><b>${data.current_branch ? `BRANCH · ${esc(data.current_branch)}` : 'DETACHED HEAD'}</b></div>
    ${currentCommit.subject ? `<p>${esc(currentCommit.subject)}</p>` : ''}
    <small>${esc(relation)} ${esc(projectRelation)}</small>
  </section>`;
}

function matchesSubmoduleVersion(item = {}, query = '') {
  const needle = String(query).trim().toLowerCase();
  if (!needle) return true;
  return [item.name, item.revision, item.subject, item.author, item.date, item.attached_branch, item.upstream]
    .filter(Boolean).join(' ').toLowerCase().includes(needle);
}

function submoduleVersionRowHtml(item) {
  const esc = escapeSubmoduleVersionHtml;
  const kindLabel = { branch: 'BRANCH', remote: 'REMOTE BRANCH', tag: 'TAG', commit: 'COMMIT (detached)' };
  const symbol = { commit: '●', tag: '◆' };
  const revision = String(item.revision || '');
  // Spell the relationship out instead of showing only arrows. In a
  // detached checkout these rows describe their own branch tips, not HEAD;
  // wording such as "LOCAL + ORIGIN" / "ORIGIN NEWER" makes that boundary
  // visible and avoids reading a branch's counts as the current checkout's.
  let upstreamState = '';
  if (item.kind === 'branch' && item.upstream) {
    const ahead = Number(item.ahead) || 0;
    const behind = Number(item.behind) || 0;
    let relation;
    if (ahead === 0 && behind === 0) relation = 'BRANCH TIP · matches upstream';
    else if (ahead > 0 && behind === 0) relation = `BRANCH TIP AHEAD · ${ahead} commit${ahead === 1 ? '' : 's'} to push`;
    else if (ahead === 0 && behind > 0) relation = `BRANCH TIP BEHIND · ${behind} commit${behind === 1 ? '' : 's'} to pull`;
    else relation = `BRANCH DIVERGED · ${ahead} ahead / ${behind} behind`;
    upstreamState = `<span class="version-upstream-state" title="Branch ${esc(item.name)} compared with ${esc(item.upstream)}">${relation} · ${esc(item.upstream)}</span>`;
  } else if (item.kind === 'branch') {
    upstreamState = '<span class="version-upstream-state version-no-upstream">LOCAL · no upstream configured</span>';
  }
  const tagContext = item.kind === 'tag'
    ? `<span class="version-attached-branch">${item.attached_branch ? `on ⑂ ${esc(item.attached_branch)}` : 'no branch here (detached)'}</span>`
    : upstreamState;
  // Destructive reset is only offered for the active branch. Uncommitted
  // work belongs to the current working tree, not to an inactive branch row;
  // making the user Checkout first keeps the target and consequence explicit.
  // The backend repeats this safety check so stale UI data cannot bypass it.
  const canResetToUpstream = item.kind === 'branch' && item.current && item.upstream
    && ((Number(item.ahead) || 0) > 0 || (Number(item.behind) || 0) > 0);
  const actions = `<span class="version-row-actions">
    ${canResetToUpstream ? `<button type="button" class="version-reset-upstream" data-reset-upstream data-name="${esc(item.name)}" data-upstream="${esc(item.upstream)}" data-ahead="${Number(item.ahead) || 0}" data-behind="${Number(item.behind) || 0}" title="Destructive recovery for the active branch: discard its local-only commits and current uncommitted work, then replace it with ${esc(item.upstream)}">Discard local work…</button>` : ''}
    ${item.kind === 'branch' && item.checkout_detached ? '<span class="version-inactive-label">INACTIVE</span>' : ''}
    ${item.current ? '<span class="current-label">CURRENT</span>' : `<button type="button" class="version-checkout" data-switch-version>Checkout</button>`}
  </span>`;
  return `<div class="version-row ${item.current ? 'current' : ''}" data-revision="${esc(revision)}" data-version-kind="${esc(item.kind)}" data-name="${esc(item.name)}">
    <span class="version-symbol">${symbol[item.kind] || '⑂'}</span>
    <span class="version-sha"><code>${esc(revision.slice(0, 8))}</code><span class="version-copy-sha" role="button" tabindex="0" title="Copy full SHA" data-copy-sha="${esc(revision)}">⧉</span></span>
    <span class="version-name">${esc(item.name)}<b class="version-kind-badge">${esc(kindLabel[item.kind] || item.kind)}</b>${tagContext}</span>
    <span class="version-copy"><span class="version-subject">${esc(item.subject)}</span><span class="version-meta">${esc(item.author)} · ${esc(item.date)}</span></span>
    ${actions}
  </div>`;
}

// Keep Push-button eligibility independent from the number of displayed
// commits. Creating a new remote branch can legitimately transfer no new
// objects, and the backend may still safely allow the ref creation.
function submodulePushDialogState(preview) {
  const commits = Array.isArray(preview?.commits) ? preview.commits : [];
  const willCreate = Boolean(preview?.will_create_remote_branch);
  const branch = String(preview?.branch || 'branch');
  const revision = String(preview?.local_sha || '').slice(0, 8);
  return {
    commits,
    canPush: preview?.can_push === true,
    summary: willCreate ? `Create origin/${branch}` : `${commits.length} commit${commits.length === 1 ? '' : 's'} to push`,
    emptyMessage: willCreate
      ? `The remote branch does not exist yet. Push will create origin/${branch} at ${revision}.`
      : String(preview?.blocked_reason || 'Nothing to push — already up to date.'),
  };
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = { groupSubmoduleBranchVersions, submoduleCurrentPresentation, submoduleCurrentContextHtml, matchesSubmoduleVersion, submoduleVersionRowHtml, submodulePushDialogState };
}
