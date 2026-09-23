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
// "<remote>/<branch>" shorthand) is the authoritative signal used to match it
// to a remote-tracking entry's `name`. There is one extra UX-only grouping:
// when a local branch has no configured upstream yet but `origin/<same-name>`
// points to the exact same commit, show that as one logical row. Git users read
// "remote only" as "there is no local branch"; duplicating the same commit in a
// remote-only section after Checkout made a successful switch look unfinished.
function groupSubmoduleBranchVersions(versions) {
  const all = versions || [];
  const local = all.filter(version => version.kind === 'branch');
  const remote = all.filter(version => version.kind === 'remote');
  const remoteBySameLocalName = new Map();
  remote.forEach(version => {
    const name = String(version.name || '');
    const slash = name.indexOf('/');
    if (slash > 0 && slash < name.length - 1) remoteBySameLocalName.set(name.slice(slash + 1), version);
  });
  const groupedLocal = local.map(version => {
    if (version.upstream) return version;
    const sameNameRemote = remoteBySameLocalName.get(version.name);
    if (!sameNameRemote || sameNameRemote.revision !== version.revision) return version;
    return {
      ...version,
      same_name_remote: sameNameRemote.name,
      same_name_remote_revision: sameNameRemote.revision,
    };
  });
  const representedRemoteNames = new Set(groupedLocal.flatMap(version => [
    version.upstream,
    version.same_name_remote,
  ].filter(Boolean)));
  const remoteOnly = remote.filter(version => !representedRemoteNames.has(version.name));
  return { local: groupedLocal, remoteOnly };
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

function submoduleContainingBranchCandidates(data = {}) {
  const revision = String(data.current_revision || '');
  const containingNames = new Set(Array.isArray(data.current_containing_branches) ? data.current_containing_branches : []);
  const all = (Array.isArray(data.versions) ? data.versions : []).filter(item =>
    ['branch', 'remote'].includes(item.kind) && (item.contains_current === true || containingNames.has(item.name))
  );
  // A local branch row already names its upstream. Do not repeat that same
  // logical branch as a second remote candidate in the compact action list.
  const representedRemotes = new Set(all.filter(item => item.kind === 'branch' && item.upstream).map(item => item.upstream));
  return all.filter(item => item.kind !== 'remote' || !representedRemotes.has(item.name)).sort((left, right) => {
    const distance = item => item.commits_after_current != null && Number.isFinite(Number(item.commits_after_current))
      ? Number(item.commits_after_current) : (item.revision === revision ? 0 : Number.MAX_SAFE_INTEGER);
    const leftDistance = distance(left);
    const rightDistance = distance(right);
    const kindOrder = (left.kind === 'branch' ? 0 : 1) - (right.kind === 'branch' ? 0 : 1);
    return leftDistance - rightDistance || kindOrder || String(left.name).localeCompare(String(right.name));
  });
}

function submoduleCurrentContextHtml(data = {}) {
  const esc = escapeSubmoduleVersionHtml;
  const revision = String(data.current_revision || '');
  const short = revision.slice(0, 8) || 'unknown';
  const parent = String(data.parent_revision || '');
  const candidates = submoduleContainingBranchCandidates(data);
  const versions = Array.isArray(data.versions) ? data.versions : [];
  const currentCommit = versions.find(item => item.kind === 'commit' && item.revision === revision) || {};
  const exactTips = candidates.filter(item => item.revision === revision).map(item => item.name);
  let relation;
  if (data.current_branch) relation = `Attached to branch ${data.current_branch}.`;
  else if (exactTips.length) relation = `Detached at the tip of: ${exactTips.join(', ')}. Checkout a local branch to attach HEAD.`;
  else if (candidates.length) relation = `Detached inside ${candidates.length} known branch${candidates.length === 1 ? '' : 'es'}. Choose explicitly which branch tip to switch to.`;
  else relation = 'Detached and not reachable from currently known local or remote-tracking branches.';
  const projectRelation = parent && parent === revision
    ? 'The parent project records this exact commit.'
    : parent ? `The parent project currently records ${parent.slice(0, 8)}.` : 'The parent project version could not be determined.';
  const visibleCandidates = candidates.slice(0, 5);
  const candidateSection = (title, items, extraClass = '') => items.length ? `<div class="version-containing-group ${extraClass}">
    <h5>${esc(title)}</h5>
    ${items.map(item => {
      const distance = item.commits_after_current != null && Number.isFinite(Number(item.commits_after_current))
        ? Number(item.commits_after_current) : (item.revision === revision ? 0 : null);
      const position = distance === 0 ? 'tip is this commit' : distance == null ? 'contains this commit' : `tip +${distance} commit${distance === 1 ? '' : 's'}`;
      const remote = item.kind === 'remote';
      const action = remote ? 'Checkout remote' : distance === 0 ? 'Attach' : 'Checkout tip';
      return `<div class="version-containing-branch ${remote ? 'remote' : 'local'}" data-revision="${esc(item.revision)}" data-version-kind="${esc(item.kind)}" data-name="${esc(item.name)}">
        <span><strong title="${esc(item.name)}">${esc(item.name)}</strong><small>${esc(remote ? 'remote ref' : 'local branch')} · ${esc(position)}</small></span>
        <button type="button" data-switch-version>${esc(action)}</button>
      </div>`;
    }).join('')}</div>` : '';
  const localCandidates = visibleCandidates.filter(item => item.kind === 'branch');
  const remoteCandidates = visibleCandidates.filter(item => item.kind === 'remote');
  const candidateRows = !data.current_branch && visibleCandidates.length ? `<div class="version-containing-branches">
    <h4>DETACHED COMMIT — SAFE PLACES TO ATTACH OR CHECKOUT</h4>
    ${candidateSection('Local branches', localCandidates, 'local')}
    ${candidateSection('Remote refs', remoteCandidates, 'remote')}
    ${candidates.length > visibleCandidates.length ? `<p class="version-more-branches">+${candidates.length - visibleCandidates.length} more refs in the Branches list below.</p>` : ''}
  </div>` : '';
  return `<section class="version-current-context">
    <div><span>ACTIVE CHECKOUT</span><strong>${esc(short)}</strong><b>${data.current_branch ? `BRANCH · ${esc(data.current_branch)}` : 'DETACHED HEAD'}</b></div>
    ${currentCommit.subject ? `<p>${esc(currentCommit.subject)}</p>` : ''}
    <small>${esc(relation)} ${esc(projectRelation)}</small>
    ${candidateRows}
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
    upstreamState = `<span class="version-upstream-state ${item.upstream === 'origin/main' ? 'version-primary-upstream' : ''}" title="Branch ${esc(item.name)} compared with ${esc(item.upstream)}">${relation} · ${esc(item.upstream)}</span>`;
  } else if (item.kind === 'branch' && item.same_name_remote) {
    upstreamState = `<span class="version-upstream-state version-no-upstream" title="A same-named remote branch exists at this exact commit, but this local branch is not configured to track it yet">LOCAL + ORIGIN · same commit · ${esc(item.same_name_remote)} · upstream not configured</span>`;
  } else if (item.kind === 'branch') {
    upstreamState = '<span class="version-upstream-state version-no-upstream">LOCAL · no upstream configured</span>';
  }
  // Backend-computed for every commit row (not just the active checkout) —
  // which known branch(es), if any, actually contain this exact commit. The
  // question this answers ("what branch is this old commit even on?") only
  // makes sense while looking at History; a branch/tag row already names
  // itself, so this is commit-only.
  const commitBranches = Array.isArray(item.containing_branches) ? item.containing_branches : [];
  const commitBranchContext = item.kind === 'commit'
    ? `<span class="version-attached-branch">${commitBranches.length ? `contained in ⑂ ${esc(commitBranches.slice(0, 2).join(', '))}${commitBranches.length > 2 ? ` +${commitBranches.length - 2}` : ''}` : 'no known branch contains this commit'}</span>`
    : '';
  const rowContext = item.kind === 'tag'
    ? `<span class="version-attached-branch">${item.attached_branch ? `on ⑂ ${esc(item.attached_branch)}` : 'no branch here (detached)'}</span>`
    : item.kind === 'commit' ? commitBranchContext : upstreamState;
  // Report: "matches upstream" next to a destructive "Discard local work…"
  // button reads as a contradiction — nothing here said *why* the button was
  // still offered. It's this: canResetToUpstream below also fires on a
  // purely dirty working tree, which "matches upstream" (a branch-tip
  // comparison) says nothing about either way. Surface that plainly instead
  // of leaving it to the button's own hover tooltip to explain.
  const dirtyNote = item.dirty ? '<span class="version-dirty-note" title="Uncommitted changes on disk in this submodule right now — independent of whether the commit above has been pushed anywhere">● Uncommitted changes present</span>' : '';
  // Destructive reset is only offered for the active branch. Uncommitted
  // work belongs to the current working tree, not to an inactive branch row;
  // making the user Checkout first keeps the target and consequence explicit.
  // The backend repeats this safety check so stale UI data cannot bypass it.
  // A dirty working tree needs this exactly as much as committed ahead/behind
  // divergence does — the backend already discards it (see
  // reset_submodule_branch_to_upstream_inner's own dirty handling) whenever
  // the target is the currently active branch; this button just wasn't
  // offered for that case before, even though there was nothing else in the
  // UI that could reach it either.
  const canResetToUpstream = item.kind === 'branch' && item.current && item.upstream
    && ((Number(item.ahead) || 0) > 0 || (Number(item.behind) || 0) > 0 || !!item.dirty);
  // Report: "BRANCH TIP AHEAD · N commits to push" told the story, but the
  // only button on the row was the destructive "Discard local work…" —
  // reading as if throwing the new commit away were the suggested move.
  // Offer the actual constructive counterpart right here too, ahead of (to
  // the left of) Discard, whenever there is something to send.
  const aheadCount = Number(item.ahead) || 0;
  const canPushAhead = item.kind === 'branch' && item.current && item.upstream && aheadCount > 0;
  const actions = `<span class="version-row-actions">
    ${item.kind === 'commit' ? `<button type="button" class="version-tag-commit" data-tag-version title="Create a tag pointing exactly at commit ${esc(revision.slice(0, 8))}">Tag this commit…</button>` : ''}
    ${canPushAhead ? `<button type="button" class="version-push-ahead" data-push-version title="Send ${aheadCount} local commit${aheadCount === 1 ? '' : 's'} on ${esc(item.name)} to ${esc(item.upstream)}">Push</button>` : ''}
    ${canResetToUpstream ? `<button type="button" class="version-reset-upstream" data-reset-upstream data-name="${esc(item.name)}" data-upstream="${esc(item.upstream)}" data-ahead="${Number(item.ahead) || 0}" data-behind="${Number(item.behind) || 0}" title="Destructive recovery for the active branch: discard its local-only commits and current uncommitted work (including a purely dirty working tree with no divergent commits), then replace it with ${esc(item.upstream)}">Discard local work…</button>` : ''}
    ${item.kind === 'branch' && item.checkout_detached ? '<span class="version-inactive-label">INACTIVE</span>' : ''}
    ${item.current ? '<span class="current-label">CURRENT</span>' : `<button type="button" class="version-checkout" data-switch-version>Checkout</button>`}
  </span>`;
  return `<div class="version-row version-kind-${esc(item.kind)} ${item.name === 'origin/main' ? 'primary-remote' : ''} ${item.current ? 'current' : ''}" data-revision="${esc(revision)}" data-version-kind="${esc(item.kind)}" data-name="${esc(item.name)}">
    <span class="version-symbol">${symbol[item.kind] || '⑂'}</span>
    <span class="version-sha"><code>${esc(revision.slice(0, 8))}</code><span class="version-copy-sha" role="button" tabindex="0" title="Copy full SHA" data-copy-sha="${esc(revision)}">⧉</span></span>
    <span class="version-name">${esc(item.name)}<b class="version-kind-badge">${esc(kindLabel[item.kind] || item.kind)}</b>${rowContext}${dirtyNote}</span>
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
  module.exports = { groupSubmoduleBranchVersions, submoduleCurrentPresentation, submoduleContainingBranchCandidates, submoduleCurrentContextHtml, matchesSubmoduleVersion, submoduleVersionRowHtml, submodulePushDialogState };
}
