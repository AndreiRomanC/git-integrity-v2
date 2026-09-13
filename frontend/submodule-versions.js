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
    if (ahead === 0 && behind === 0) relation = 'LOCAL + ORIGIN · in sync';
    else if (ahead > 0 && behind === 0) relation = `LOCAL AHEAD · ${ahead} commit${ahead === 1 ? '' : 's'} to push`;
    else if (ahead === 0 && behind > 0) relation = `ORIGIN NEWER · ${behind} commit${behind === 1 ? '' : 's'} to pull`;
    else relation = `DIVERGED · ${ahead} ahead / ${behind} behind`;
    upstreamState = `<span class="version-upstream-state" title="Branch ${esc(item.name)} compared with ${esc(item.upstream)}">${relation} · ${esc(item.upstream)}</span>`;
  } else if (item.kind === 'branch') {
    upstreamState = '<span class="version-upstream-state version-no-upstream">LOCAL · no upstream configured</span>';
  }
  const tagContext = item.kind === 'tag'
    ? `<span class="version-attached-branch">${item.attached_branch ? `on ⑂ ${esc(item.attached_branch)}` : 'no branch here (detached)'}</span>`
    : upstreamState;
  return `<button class="version-row ${item.current ? 'current' : ''}" data-revision="${esc(revision)}" data-version-kind="${esc(item.kind)}" data-name="${esc(item.name)}">
    <span class="version-symbol">${symbol[item.kind] || '⑂'}</span>
    <span class="version-sha"><code>${esc(revision.slice(0, 8))}</code><span class="version-copy-sha" role="button" tabindex="0" title="Copy full SHA" data-copy-sha="${esc(revision)}">⧉</span></span>
    <span class="version-name">${esc(item.name)}<b class="version-kind-badge">${esc(kindLabel[item.kind] || item.kind)}</b>${tagContext}</span>
    <span class="version-copy"><span class="version-subject">${esc(item.subject)}</span><span class="version-meta">${esc(item.author)} · ${esc(item.date)}</span></span>
    ${item.current ? '<span class="current-label">CURRENT</span>' : ''}
  </button>`;
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
  module.exports = { groupSubmoduleBranchVersions, submoduleVersionRowHtml, submodulePushDialogState };
}
